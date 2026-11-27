//! `rpi-acp` — run `rpi` as an external **Agent Client Protocol** agent.
//!
//! Zed (and any other ACP client) launches `rpi --acp` and speaks ACP v1
//! JSON-RPC over the process's stdin/stdout. This extension makes that work:
//!
//! 1. During `register` (which the host runs *before* it decides whether to
//!    read piped stdin) it reads the `--acp` flag and, when set, `declare`s
//!    `{"ownsStdio": true}`. The host then defers `read_piped_stdin` and runs
//!    its headless `Rpc` lifecycle, leaving this process's stdio to us.
//! 2. On `SessionStart` it runs the one-shot actions (Zed registration or the
//!    `--setup` credential flow) or starts the ACP server ([`protocol::run`]) on a
//!    dedicated thread. The server handles `initialize` / `session/new` /
//!    `session/prompt` / `session/cancel` / `session/load` / `session/resume`
//!    / standard ACP session methods.
//! 3. Each ACP session is backed by a child `rpi --mode rpc` process rooted at
//!    the session's `cwd`; its event stream is translated into ACP
//!    `session/update` notifications (see [`session`]).
//!
//! The host waits for the exclusive stdio owner to finish. EOF reports the
//! server's exit code through the generic StdioLifecycle action; SessionShutdown
//! stops and joins the server thread before the plugin is unloaded.

#![deny(unsafe_code)]
// The SDK's `export_plugin!` macro expands to the `pub extern "C" fn
// rpi_plugin_register(*const PluginApi)` entrypoint and validates/dereferences
// the pointer via `register_entrypoint`; clippy attributes that generated deref
// here. The ABI signature is fixed, so the lint is not actionable.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

// `pub` so the standalone `rpi-acp` binary (`src/main.rs`) can run the same ACP
// server the extension embeds.
pub mod auth;
pub mod diag;
mod file_io;
mod history;
mod mcp;
pub mod protocol;
mod questions;
pub mod session;
mod session_list;
pub mod zed;

use rpi_plugin_sdk::callbacks::OwnedString;
use rpi_plugin_sdk::{
    EventTag, PluginApi, RuntimeActionId, StablePluginEvent, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

/// The CLI flag that turns an ordinary `rpi` invocation into an ACP agent.
const ACP_FLAG: &str = "acp";
const BYPASS_FLAG: &str = "bypass";
/// One-shot: register rpi in Zed's settings, then exit.
const INSTALL_ZED_FLAG: &str = "install-zed";
/// One-shot: print the Zed settings snippet, then exit.
const PRINT_ZED_FLAG: &str = "print-zed";
/// One-shot: run the interactive credential setup that backs ACP's `terminal`
/// auth method, then exit `0` only once the provider is verified usable.
const SETUP_FLAG: &str = "setup";

// Passive registrations keep no state. The active registration holds the
// slot until its worker has joined, so a replacement cannot overlap stdio.
static OWNER: Mutex<Option<Arc<Owner>>> = Mutex::new(None);
static NEXT_OWNER: AtomicUsize = AtomicUsize::new(1);

struct Owner {
    token: usize,
    host: Host,
    mode: Mode,
    stop: tokio::sync::watch::Sender<bool>,
    worker: Mutex<Worker>,
}

#[derive(Default)]
struct Worker {
    started: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Copy)]
enum Mode {
    Setup,
    Zed { write: bool },
    Serve { bypass: bool },
}

#[derive(Clone, Copy)]
struct Host(PluginApi);

impl Host {
    #[allow(unsafe_code)] // Adopt the string returned by the host ABI.
    fn lifecycle(self, op: &str, code: i32) -> bool {
        let mut out = StbString::empty();
        let rc = (self.0.runtime_action)(
            RuntimeActionId::StdioLifecycle as u32,
            StbStringRef::from_str(&json!({"op": op, "exitCode": code}).to_string()),
            &mut out,
            self.0.user_data,
        );
        // SAFETY: the host transfers the output with its own deallocator.
        let _out = unsafe { OwnedString::from_raw(out, Some(self.0.free_string)) };
        rc == 0
    }

    fn finish(self, code: i32) -> i32 {
        if !self.lifecycle("finish", code) {
            diagln!("[rpi-acp] failed to finish the host stdio lifecycle");
        }
        0
    }

    #[allow(unsafe_code)] // Adopt the string returned by the host ABI.
    fn flag(self, name: &str) -> Option<Value> {
        if self.0.user_data.is_null() {
            return None;
        }
        let mut out = StbString::empty();
        let rc = (self.0.runtime_action)(
            RuntimeActionId::GetCliFlag as u32,
            StbStringRef::from_str(&json!({"name": name}).to_string()),
            &mut out,
            self.0.user_data,
        );
        // SAFETY: the host transfers the output with its own deallocator.
        let out = unsafe { OwnedString::from_raw(out, Some(self.0.free_string)) };
        let result = if rc == 0 {
            serde_json::from_str::<Value>(&out.to_string_lossy()).ok()
        } else {
            None
        };
        result?.get("value").cloned()
    }
}

fn find_owner(token: usize) -> Option<Arc<Owner>> {
    OWNER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .as_ref()
        .filter(|owner| owner.token == token)
        .cloned()
}

extern "C" fn begin_shutdown(context: *mut c_void) {
    rpi_plugin_sdk::guard_or((), || {
        if let Some(owner) = find_owner(context as usize) {
            owner.stop.send_replace(true);
        }
    });
}

extern "C" fn finish_shutdown(context: *mut c_void) -> i32 {
    rpi_plugin_sdk::guard_or(1, || {
        let Some(owner) = find_owner(context as usize) else {
            return 0;
        };
        owner.stop.send_replace(true);
        // Start holds this lock until it publishes the worker handle. Keep the
        // slot occupied until the worker has released the process streams.
        let mut worker = owner
            .worker
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(thread) = worker.thread.take() {
            let _ = thread.join();
        }
        let mut slot = OWNER.lock().unwrap_or_else(|error| error.into_inner());
        if slot
            .as_ref()
            .is_some_and(|current| current.token == owner.token)
        {
            *slot = None;
        }
        0
    })
}

extern "C" fn on_session_shutdown(_: StablePluginEvent, context: *mut c_void) -> i32 {
    finish_shutdown(context)
}

/// Whether a `GetCliFlag` result means the switch is ON. A bare `--flag` yields
/// `true`; `--flag=value` yields a string; unset yields `null`.
fn flag_is_on(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(on)) => *on,
        Some(Value::String(text)) => !text.is_empty() && !text.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// `SessionStart`: run the one-shot Zed actions, or bring up the ACP server.
///
/// Dispatched by the host after it has loaded extensions and (because of the
/// `ownsStdio` claim) decided to run the headless lifecycle with stdio left to us.
///
/// Make Zed able to launch rpi without the user hand-editing `settings.json`.
///
/// Only reached under `--acp`, i.e. when the user has explicitly asked for Zed
/// integration, and a no-op once `agent_servers.rpi` exists (so a Zed-launched
/// child, which is already registered, never writes). The edit preserves the
/// file's comments and formatting and is backed up — see [`zed`].
///
/// `RPI_ACP_NO_AUTO_REGISTER_ZED=1` refuses the write and prints the snippet
/// instead, for users who keep their editor config under version control.
fn ensure_zed_registration() {
    let Some(path) = zed::settings_path() else {
        return;
    };
    let registration = zed::Registration::detect();
    if env_flag("RPI_ACP_NO_AUTO_REGISTER_ZED") {
        let registered = std::fs::read_to_string(&path)
            .map(|text| matches!(zed::ensure_registered(&text, &registration), Ok(None)))
            .unwrap_or(true);
        if !registered {
            diagln!(
                "[rpi-acp] rpi is not registered in {}. Add this to that file:\n{}",
                path.display(),
                registration.snippet()
            );
        }
        return;
    }
    match zed::register_file(&path, &registration, false) {
        Ok(zed::Outcome::AlreadyPresent) => {}
        Ok(zed::Outcome::Created) => {
            diagln!("[rpi-acp] registered rpi in {}", path.display());
        }
        Ok(zed::Outcome::Updated { .. }) => {
            diagln!(
                "[rpi-acp] added rpi to {} (backup: settings.json.rpi-bak)",
                path.display()
            );
        }
        // Never fail an ACP start over this: a user can always edit by hand.
        Err(error) => diagln!("[rpi-acp] could not update {}: {error}", path.display()),
    }
}

/// Treat unset/empty/`0`/`false` as off, anything else as on.
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| {
            let value = value.trim();
            !value.is_empty() && value != "0" && !value.eq_ignore_ascii_case("false")
        })
        .unwrap_or(false)
}

extern "C" fn on_session_start(event: StablePluginEvent, context: *mut c_void) -> i32 {
    if event.tag != EventTag::SessionStart {
        return 0;
    }
    rpi_plugin_sdk::guard_or(1, || start_owner(context as usize))
}

fn start_owner(token: usize) -> i32 {
    let Some(owner) = find_owner(token) else {
        return 0;
    };
    let mut worker = owner
        .worker
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let receiver = owner.stop.subscribe();
    if worker.started || *receiver.borrow() {
        return 0;
    }
    worker.started = true;
    let host = owner.host;
    let bypass = match owner.mode {
        Mode::Setup => {
            let args: Vec<String> = std::env::args().skip(1).collect();
            return host.finish(auth::run(&args));
        }
        Mode::Zed { write } => return host.finish(zed::run_default_action(write)),
        Mode::Serve { bypass } => bypass,
    };

    // Take stdin *here*, synchronously, before spawning the server: a terminal
    // cannot carry ACP, and the host has already parked in its headless
    // lifecycle by now. Failing later (inside the server thread) would print the
    // host's "running in headless mode; Ctrl+C to stop" line and look like a
    // hang; failing here prints one actionable message and exits.
    let (input, pending) = match protocol::acp_stdin() {
        Ok(taken) => taken,
        Err(error) => {
            diagln!("[rpi-acp] {error}");
            diagln!(
                "[rpi-acp] --acp speaks ACP on stdin, so it needs a client (Zed) on the \
                 other end of a pipe."
            );
            diagln!("[rpi-acp] To register rpi in Zed from a terminal, run: rpi --install-zed");
            return host.finish(2);
        }
    };

    diagln!("[rpi-acp] ACP stdio server starting");
    diagln!(
        "[rpi-acp] pid {} = {} (cwd {})",
        std::process::id(),
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".into()),
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "?".into()),
    );
    ensure_zed_registration();
    let server = thread::Builder::new().spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(2)
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                diagln!("[rpi-acp] failed to create tokio runtime: {error}");
                host.finish(2);
                return;
            }
        };

        let code = runtime.block_on(protocol::run_with_bypass(receiver, input, pending, bypass));
        host.finish(code);
    });
    match server {
        Ok(server) => worker.thread = Some(server),
        Err(error) => {
            diagln!("[rpi-acp] failed to start server thread: {error}");
            host.finish(2);
        }
    }
    0
}

fn register_plugin(api: &PluginApi) -> i32 {
    let host = Host(*api);

    // Document the flags. They are always readable via `GetCliFlag` (unknown
    // long flags are retained by the parser), but registering them puts them in
    // the host's flag list/help.
    if let Some(register_flag) = api.register_flag {
        let _ = register_flag(
            StbStringRef::from_str(ACP_FLAG),
            StbStringRef::from_str("Run as an ACP (Agent Client Protocol) stdio agent"),
        );
        let _ = register_flag(
            StbStringRef::from_str(BYPASS_FLAG),
            StbStringRef::from_str(
                "Skip permission-gate approvals for this process and ACP sessions",
            ),
        );
        let _ = register_flag(
            StbStringRef::from_str(INSTALL_ZED_FLAG),
            StbStringRef::from_str("Register rpi in Zed's settings.json, then exit"),
        );
        let _ = register_flag(
            StbStringRef::from_str(PRINT_ZED_FLAG),
            StbStringRef::from_str("Print the Zed settings snippet, then exit"),
        );
        let _ = register_flag(
            StbStringRef::from_str(SETUP_FLAG),
            StbStringRef::from_str("Set up provider credentials, then exit"),
        );
    }

    // Decide NOW, during register: the host only learns whether to defer
    // `read_piped_stdin` from the `ownsStdio` declaration, which must be in the
    // registry before `build()` returns.
    let mode = if flag_is_on(host.flag(SETUP_FLAG).as_ref()) {
        Mode::Setup
    } else if flag_is_on(host.flag(INSTALL_ZED_FLAG).as_ref()) {
        Mode::Zed { write: true }
    } else if flag_is_on(host.flag(PRINT_ZED_FLAG).as_ref()) {
        Mode::Zed { write: false }
    } else if flag_is_on(host.flag(ACP_FLAG).as_ref()) {
        Mode::Serve {
            bypass: flag_is_on(host.flag(BYPASS_FLAG).as_ref()),
        }
    } else {
        return 0;
    };
    for other in ["mcp", "http", "webserver", "web", "server", "webui"] {
        if flag_is_on(host.flag(other).as_ref()) {
            diagln!("[rpi-acp] ACP actions cannot be combined with --{other}");
            return 1;
        }
    }

    if !host.lifecycle("status", 0) {
        diagln!("[rpi-acp] host must support the StdioLifecycle runtime action");
        return 1;
    }
    let Some(register) = api.register_event_handler else {
        diagln!("[rpi-acp] host must support session lifecycle event registration");
        return 1;
    };
    let Some(declare) = api.declare else {
        diagln!(
            "[rpi-acp] this rpi host does not support the `declare` ABI; \
             cannot claim stdio (upgrade rpi)"
        );
        return 1;
    };
    #[allow(deprecated)] // `try_update` requires a newer Rust version.
    let Ok(token) = NEXT_OWNER.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        next.checked_add(1)
    }) else {
        return 1;
    };
    // The host round-trips this token without dereferencing it. Reusing a
    // host pointer as the key would let a stale event address a new owner.
    let context = token as *mut c_void;
    // Older isolated hosts fall back to SessionShutdown. The shared Host
    // supplies loader cleanup even when registration fails or reloads.
    if let Some(lifecycle) = api.register_plugin_lifecycle {
        let code = lifecycle(begin_shutdown, finish_shutdown, context);
        if code != 0 {
            return code;
        }
    }
    for (tag, callback) in [
        (
            EventTag::SessionShutdown,
            on_session_shutdown as rpi_plugin_sdk::EventHandlerFn,
        ),
        (EventTag::SessionStart, on_session_start),
    ] {
        let code = register(tag, callback, context);
        if code != 0 {
            return code;
        }
    }
    // Publish only after both hooks and the stdio claim succeed. A partially
    // installed hook has no matching owner, even if the host calls it.
    let mut slot = OWNER.lock().unwrap_or_else(|error| error.into_inner());
    if slot.is_some() {
        return 1;
    }
    let payload = json!({"ownsStdio": true}).to_string();
    if declare(StbStringRef::from_str(&payload)) != 0 {
        diagln!("[rpi-acp] host rejected the `ownsStdio` declaration");
        return 1;
    }

    *slot = Some(Arc::new(Owner {
        token,
        host,
        mode,
        stop: tokio::sync::watch::channel(false).0,
        worker: Mutex::new(Worker::default()),
    }));

    0
}

// Passive registrations share the image. The active stdio owner retains its
// own callbacks and worker until the host finishes that registration.
#[allow(unsafe_code)] // Export the shared-session marker symbol.
#[no_mangle]
pub extern "C" fn rpi_plugin_shared_sessions_v1() {}

rpi_plugin_sdk::export_plugin!(register_plugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_is_on_handles_bool_string_and_null() {
        assert!(flag_is_on(Some(&json!(true))));
        assert!(flag_is_on(Some(&json!("true"))));
        assert!(flag_is_on(Some(&json!("yes"))));
        assert!(!flag_is_on(Some(&json!(false))));
        assert!(!flag_is_on(Some(&json!("false"))));
        assert!(!flag_is_on(Some(&json!(""))));
        assert!(!flag_is_on(Some(&Value::Null)));
        assert!(!flag_is_on(None));
    }
}
