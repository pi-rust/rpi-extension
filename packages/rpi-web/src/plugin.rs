//! Optional host entry point: `rpi --webserver`, sharing the standalone server.
use crate::config::{self, Launch};
use rpi_plugin_sdk::{
    EventTag, PluginApi, RuntimeActionId, StablePluginEvent, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

// Passive registrations keep no state. The active registration holds the
// slot until its worker has joined, so a replacement cannot overlap stdio.
static OWNER: Mutex<Option<Arc<Owner>>> = Mutex::new(None);
static NEXT_OWNER: AtomicUsize = AtomicUsize::new(1);

struct Owner {
    token: usize,
    host: Host,
    stop: tokio::sync::watch::Sender<bool>,
    worker: Mutex<Worker>,
}

#[derive(Default)]
struct Worker {
    started: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Clone, Copy)]
struct Host(PluginApi);

impl Host {
    fn action(self, id: RuntimeActionId, args: Value) -> Option<Value> {
        let mut out = StbString::empty();
        let code = (self.0.runtime_action)(
            id as u32,
            StbStringRef::from_str(&args.to_string()),
            &mut out,
            self.0.user_data,
        );
        if code == 0 {
            let body = rpi_plugin_sdk::stb_take!(out, Some(self.0.free_string));
            serde_json::from_str(&body).ok()
        } else {
            rpi_plugin_sdk::stb_drop!(out, Some(self.0.free_string));
            None
        }
    }

    fn flag(self, name: &str) -> Option<Value> {
        self.action(RuntimeActionId::GetCliFlag, json!({"name":name}))
            .and_then(|v| v.get("value").cloned())
    }

    fn finish(self, code: i32) {
        self.action(
            RuntimeActionId::StdioLifecycle,
            json!({"op":"finish","exitCode":code}),
        );
    }
}

/// A flag carrying a value: `--port 3031` and `--port=3031` both yield a string.
fn flag_value(host: Host, name: &str) -> Option<String> {
    host.flag(name).and_then(flag_text)
}

/// Normalize a parsed host flag into the text form of a `--flag VALUE` option.
fn flag_text(value: Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

/// Append options the host CLI already parsed, so `rpi --webserver --port N`
/// behaves like `rpi-web --port N` (CLI overrides every config file).
fn append_service_flags(host: Host, args: &mut Vec<String>) {
    args.extend(service_options(
        flag_value(host, "bind"),
        flag_value(host, "port"),
        flag_value(host, "session-timeout"),
    ));
    if let Some(area) = host.flag("host-area").filter(|value| enabled(Some(value))) {
        args.push("--host-area".into());
        if let Some(name) = flag_text(area) {
            args.push(name);
        }
    }
}

fn service_options(
    bind: Option<String>,
    port: Option<String>,
    session_timeout: Option<String>,
) -> Vec<String> {
    let mut args = Vec::new();
    for (name, value) in [
        ("bind", bind),
        ("port", port),
        ("session-timeout", session_timeout),
    ] {
        if let Some(value) = value {
            args.push(format!("--{name}"));
            args.push(value);
        }
    }
    args
}

fn enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty() && !value.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

fn service_mode(primary: Option<Value>, legacy: Option<Value>) -> Result<Option<Value>, String> {
    let primary = primary.filter(|value| enabled(Some(value)));
    let legacy = legacy.filter(|value| enabled(Some(value)));
    match (primary, legacy) {
        (Some(primary), Some(legacy)) if primary != legacy => {
            Err("--webserver and --web specify different values; use only one entry point".into())
        }
        (Some(value), _) | (_, Some(value)) => Ok(Some(value)),
        _ => Ok(None),
    }
}

extern "C" fn start(_: StablePluginEvent, context: *mut c_void) -> i32 {
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
    let mut receiver = owner.stop.subscribe();
    if worker.started || *receiver.borrow() {
        return 0;
    }
    worker.started = true;
    let host = owner.host;
    for other in ["acp", "mcp", "webui", "server"] {
        if enabled(host.flag(other).as_ref()) {
            eprintln!("[rpi-web] --webserver (--web) cannot be combined with --{other}");
            host.finish(2);
            return 0;
        }
    }
    let mode = match service_mode(host.flag("webserver"), host.flag("web")) {
        Ok(mode) => mode,
        Err(error) => {
            eprintln!("[rpi-web] {error}");
            host.finish(2);
            return 0;
        }
    };
    let args = match mode {
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("true") => vec![],
        Some(Value::String(mode)) if mode == "init" => vec!["--init".into()],
        Some(Value::String(mode)) if mode == "help" => vec!["--help".into()],
        Some(Value::String(path)) => vec!["--config".into(), path],
        _ => vec![],
    };
    let mut args = args;
    append_service_flags(host, &mut args);
    let (mut config, area) = match config::from_args_with_host_area(args) {
        Ok((Launch::Run(config), area)) => (*config, area),
        Ok((Launch::Initialized(path), _)) => {
            println!("Created {}", path.display());
            host.finish(0);
            return 0;
        }
        Ok((Launch::Help, _)) => {
            println!(
                "rpi --webserver[=CONFIG|init|help]\nAlias: --web\n{}",
                config::HELP
            );
            host.finish(0);
            return 0;
        }
        Ok((Launch::Version, _)) => {
            host.finish(0);
            return 0;
        }
        Err(error) => {
            eprintln!("[rpi-web] {error}");
            host.finish(2);
            return 0;
        }
    };
    // Children receive neither service flag, preventing recursive Web servers.
    if config.rpi_bin == std::path::Path::new("rpi") {
        match std::env::current_exe() {
            Ok(path) => config.rpi_bin = path,
            Err(error) => {
                eprintln!("[rpi-web] {error}");
                host.finish(2);
                return 0;
            }
        }
    }
    let thread = std::thread::Builder::new().spawn(move || {
        if *receiver.borrow() {
            return;
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build();
        let result = match runtime {
            Ok(runtime) => runtime.block_on(crate::server::run_until(config, area, async move {
                if !*receiver.borrow_and_update() {
                    let _ = receiver.changed().await;
                }
            })),
            Err(error) => Err(error.into()),
        };
        let code = match result {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("[rpi-web] {error}");
                1
            }
        };
        host.finish(code);
    });
    match thread {
        Ok(thread) => worker.thread = Some(thread),
        Err(error) => {
            eprintln!("[rpi-web] failed to start service thread: {error}");
            host.finish(1);
        }
    }
    0
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

extern "C" fn stop(_: StablePluginEvent, context: *mut c_void) -> i32 {
    finish_shutdown(context)
}

fn register(api: &PluginApi) -> i32 {
    let host = Host(*api);
    let Some(register_flag) = api.register_flag else {
        return 1;
    };
    for (name, description) in [
        (
            "webserver",
            "Start Web server; optional configuration path, init or help",
        ),
        ("web", "Compatibility alias for --webserver"),
    ] {
        if register_flag(
            StbStringRef::from_str(name),
            StbStringRef::from_str(description),
        ) != 0
        {
            return 1;
        }
    }
    if let Some(register_flag) = api.register_flag {
        let _ = register_flag(
            StbStringRef::from_str("port"),
            StbStringRef::from_str(
                "Web server listen port (overrides web.json), used with --webserver",
            ),
        );
        let _ = register_flag(
            StbStringRef::from_str("bind"),
            StbStringRef::from_str(
                "Web server listen address (overrides web.json), used with --webserver",
            ),
        );
        let _ = register_flag(
            StbStringRef::from_str("host-area"),
            StbStringRef::from_str("Join the native Host area, optionally passing its name"),
        );
        let _ = register_flag(
            StbStringRef::from_str("session-timeout"),
            StbStringRef::from_str(
                "Idle Web session timeout in seconds (overrides web.json), used with --webserver",
            ),
        );
    }
    // RPC children do not receive --webserver, but a loaded Web extension can
    // still format file references. This hook never starts or owns a server.
    if let Some(register_prompt) = api.register_before_agent_start {
        let code = register_prompt(
            crate::file_links::before_agent_start,
            rpi_plugin_sdk::callbacks::free_string,
            api.user_data,
        );
        if code != 0 {
            return code;
        }
    }
    if !enabled(host.flag("webserver").as_ref()) && !enabled(host.flag("web").as_ref()) {
        return 0;
    }
    if host
        .action(RuntimeActionId::StdioLifecycle, json!({"op":"status"}))
        .is_none()
    {
        return 1;
    }
    let (Some(declare), Some(handler)) = (api.declare, api.register_event_handler) else {
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
            stop as rpi_plugin_sdk::EventHandlerFn,
        ),
        (EventTag::SessionStart, start),
    ] {
        let code = handler(tag, callback, context);
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
    if declare(StbStringRef::from_str("{\"ownsStdio\":true}")) != 0 {
        return 1;
    }
    *slot = Some(Arc::new(Owner {
        token,
        host,
        stop: tokio::sync::watch::channel(false).0,
        worker: Mutex::new(Worker::default()),
    }));
    0
}

// Passive registrations share the image. The active stdio owner retains its
// own callbacks and worker until the host finishes that registration.
rpi_plugin_sdk::export_shared_plugin!(register);

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_entry_points_preserve_modes_and_reject_ambiguous_configuration() {
        for value in [
            json!(true),
            json!("true"),
            json!("init"),
            json!("help"),
            json!("/path/web.json"),
        ] {
            assert_eq!(
                service_mode(Some(value.clone()), None).unwrap(),
                Some(value.clone())
            );
            assert_eq!(
                service_mode(None, Some(value.clone())).unwrap(),
                Some(value.clone())
            );
            assert_eq!(
                service_mode(Some(json!(false)), Some(value.clone())).unwrap(),
                Some(value)
            );
        }
        assert_eq!(service_mode(None, None).unwrap(), None);
        assert_eq!(
            service_mode(Some(json!(false)), Some(json!("false"))).unwrap(),
            None
        );
        assert_eq!(
            service_mode(Some(json!("init")), Some(json!("init"))).unwrap(),
            Some(json!("init"))
        );
        assert!(service_mode(Some(json!("init")), Some(json!("other.json"))).is_err());
    }

    #[test]
    fn inactive_flags_do_not_start_a_server_in_rpc_children() {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(false)),
            Some(json!("false")),
            Some(json!("")),
        ] {
            assert!(!enabled(value.as_ref()));
        }
        for value in [json!(true), json!("init"), json!("/path/web.json")] {
            assert!(enabled(Some(&value)));
        }
    }

    #[test]
    fn host_service_flags_become_web_options() {
        assert_eq!(flag_text(json!("3031")).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(3031)).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(true)), None);
        assert_eq!(flag_text(json!(null)), None);
        assert_eq!(flag_text(json!("")), None);
        let options = service_options(
            Some("0.0.0.0".into()),
            Some("3031".into()),
            Some("120".into()),
        );
        assert_eq!(
            options,
            [
                "--bind",
                "0.0.0.0",
                "--port",
                "3031",
                "--session-timeout",
                "120"
            ]
        );
        // The forwarded options parse as ordinary Web CLI overrides.
        let (bind, port, session_timeout) = match crate::config::from_args(options).unwrap() {
            crate::config::Launch::Run(config) => {
                (config.bind, config.port, config.session_timeout_seconds)
            }
            other => panic!("expected a run launch, got {other:?}"),
        };
        assert_eq!(bind, "0.0.0.0");
        assert_eq!(port, 3031);
        assert_eq!(session_timeout, Some(120));
        assert!(service_options(None, None, None).is_empty());
    }
}
