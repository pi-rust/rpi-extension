//! `rpi --http` entry point: run the API-only HTTP/SSE adapter from `rpi-httpcore`.
//!
//! The plugin keeps the existing ABI contract (`ownsStdio`, `SessionStart`,
//! `SessionShutdown` and `StdioLifecycle` exit reporting). It starts one
//! `rpi-extcore::service::Service` in its own runtime; that service does not
//! share live sessions with the Agent session that loaded the plugin.
use rpi_httpcore::config::{self, Launch};
use rpi_plugin_sdk::{EventTag, PluginApi, RuntimeActionId, StablePluginEvent, StbStringRef};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::io::Write;
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
        let (code, body) = rpi_plugin_sdk::call_action!(self.0, id, &args.to_string());
        if code == 0 {
            serde_json::from_str(&body).ok()
        } else {
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
fn flag_text(value: Value) -> Option<String> {
    match value {
        Value::String(value) if !value.is_empty() => Some(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn flag_value(host: Host, name: &str) -> Option<String> {
    host.flag(name).and_then(flag_text)
}

fn service_options(bind: Option<String>, port: Option<String>) -> Vec<String> {
    let mut args = Vec::new();
    for (name, value) in [("bind", bind), ("port", port)] {
        if let Some(value) = value {
            args.push(format!("--{name}"));
            args.push(value);
        }
    }
    args
}

/// Forward host-parsed listener overrides so CLI values beat the config file.
fn append_service_flags(host: Host, args: &mut Vec<String>) {
    args.extend(service_options(
        flag_value(host, "bind"),
        flag_value(host, "port"),
    ));
}

fn enabled(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => !value.is_empty() && !value.eq_ignore_ascii_case("false"),
        _ => false,
    }
}

/// Map the host `--http` value onto the existing `rpi-httpcore` CLI contract.
fn http_args(mode: Option<Value>) -> Vec<String> {
    match mode {
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("help") => vec!["--help".into()],
        Some(Value::String(mode)) if mode.eq_ignore_ascii_case("true") => Vec::new(),
        Some(Value::String(path)) => vec!["--config".into(), path],
        _ => Vec::new(),
    }
}

fn report(info: rpi_httpcore::Listening) {
    println!("rpi HTTP API listening on {}", info.address);
    let _ = std::io::stdout().flush();
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
    for other in ["webserver", "web", "server", "webui", "acp", "mcp"] {
        if enabled(host.flag(other).as_ref()) {
            eprintln!("[rpi-http] --http cannot be combined with --{other}");
            host.finish(2);
            return 0;
        }
    }
    let mut args = http_args(host.flag("http"));
    append_service_flags(host, &mut args);
    let mut config = match config::from_args(args) {
        Ok(Launch::Run(config)) => *config,
        Ok(Launch::Help) => {
            println!("rpi --http[=CONFIG|help]\n{}", rpi_httpcore::config::HELP);
            host.finish(0);
            return 0;
        }
        Ok(Launch::Version) => {
            host.finish(0);
            return 0;
        }
        Err(error) => {
            eprintln!("[rpi-http] {error}");
            host.finish(2);
            return 0;
        }
    };
    // The native child host must be the same rpi binary that loaded the plugin.
    if config.rpi_bin == std::path::Path::new("rpi") {
        match std::env::current_exe() {
            Ok(path) => config.rpi_bin = path,
            Err(error) => {
                eprintln!("[rpi-http] {error}");
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
            Ok(runtime) => runtime.block_on(rpi_httpcore::serve_until(
                config,
                axum::Router::new(),
                report,
                async move {
                    if !*receiver.borrow_and_update() {
                        let _ = receiver.changed().await;
                    }
                },
            )),
            Err(error) => Err(error.into()),
        };
        let code = match result {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("[rpi-http] {error}");
                1
            }
        };
        host.finish(code);
    });
    match thread {
        Ok(thread) => worker.thread = Some(thread),
        Err(error) => {
            eprintln!("[rpi-http] failed to start service thread: {error}");
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
            "http",
            "Start the API-only HTTP/SSE service; optional configuration path or help",
        ),
        (
            "bind",
            "HTTP listen address (overrides the HTTP configuration), used with --http",
        ),
        (
            "port",
            "HTTP listen port (overrides the HTTP configuration), used with --http",
        ),
    ] {
        if register_flag(
            StbStringRef::from_str(name),
            StbStringRef::from_str(description),
        ) != 0
        {
            return 1;
        }
    }
    if !enabled(host.flag("http").as_ref()) {
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
    // The existing ABI claim: normal model configuration still applies, as it
    // does for `--webserver`; no `requiresAgent:false` bypass is requested.
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

    type Lifecycle = (
        rpi_plugin_sdk::PluginBeginShutdownFn,
        rpi_plugin_sdk::PluginFinishShutdownFn,
        *mut c_void,
    );

    #[derive(Default)]
    struct Registration {
        active: bool,
        reject_start: bool,
        reject_claim: bool,
        claims: usize,
        lifecycles: Vec<Lifecycle>,
        hooks: Vec<(EventTag, rpi_plugin_sdk::EventHandlerFn, *mut c_void)>,
    }

    thread_local! {
        static REGISTRATION: std::cell::RefCell<Registration> = Default::default();
    }

    rpi_plugin_sdk::string_handler! {
        fn fake_action(id, _args, out, _context) {
            let result = match RuntimeActionId::try_from(id) {
                Ok(RuntimeActionId::GetCliFlag) => {
                    REGISTRATION.with(|state| json!({"value": state.borrow().active}))
                }
                Ok(RuntimeActionId::StdioLifecycle) => json!({}),
                _ => return 1,
            };
            out.write(result.to_string());
            0
        }
    }

    extern "C" fn fake_flag(_: StbStringRef, _: StbStringRef) -> i32 {
        0
    }

    extern "C" fn fake_claim(_: StbStringRef) -> i32 {
        REGISTRATION.with(|state| {
            let mut state = state.borrow_mut();
            state.claims += 1;
            i32::from(state.reject_claim)
        })
    }

    extern "C" fn fake_handler(
        tag: EventTag,
        callback: rpi_plugin_sdk::EventHandlerFn,
        context: *mut c_void,
    ) -> i32 {
        REGISTRATION.with(|state| {
            let mut state = state.borrow_mut();
            state.hooks.push((tag, callback, context));
            if tag == EventTag::SessionStart && state.reject_start {
                17
            } else {
                0
            }
        })
    }

    extern "C" fn fake_lifecycle(
        begin: rpi_plugin_sdk::PluginBeginShutdownFn,
        finish: rpi_plugin_sdk::PluginFinishShutdownFn,
        context: *mut c_void,
    ) -> i32 {
        REGISTRATION.with(|state| state.borrow_mut().lifecycles.push((begin, finish, context)));
        0
    }

    #[test]
    fn failed_or_stale_hooks_cannot_touch_the_current_owner() {
        let api = PluginApi {
            abi_version: rpi_plugin_sdk::RPI_PLUGIN_ABI_VERSION_UNIFIED,
            struct_size: std::mem::size_of::<PluginApi>() as u32,
            free_string: rpi_plugin_sdk::callbacks::free_string,
            register_tool: None,
            register_command: None,
            register_shortcut: None,
            register_flag: Some(fake_flag),
            register_provider: None,
            register_message_renderer: None,
            register_markdown_transformer: None,
            register_entry_renderer: None,
            register_event_handler: Some(fake_handler),
            register_resources_discover: None,
            runtime_action: fake_action,
            dispatch_event: None,
            user_data: std::ptr::null_mut(),
            declare: Some(fake_claim),
            register_before_agent_start: None,
            register_oauth_provider: None,
            register_streaming_provider: None,
            register_tool_with_wake: None,
            register_plugin_lifecycle: Some(fake_lifecycle),
            register_tool_prompt: None,
            register_oauth_provider_cancellable: None,
            register_tool_result: None,
            register_autocomplete_provider: None,
            register_command_completions: None,
            register_argument_suggester: None,
        };

        assert_eq!(register(&api), 0);
        assert!(OWNER.lock().unwrap().is_none());

        REGISTRATION.with(|state| {
            let mut state = state.borrow_mut();
            state.active = true;
            state.reject_start = true;
        });
        assert_eq!(register(&api), 17);
        assert!(OWNER.lock().unwrap().is_none());
        REGISTRATION.with(|state| {
            let mut state = state.borrow_mut();
            assert_eq!(state.claims, 0);
            state.reject_start = false;
            state.reject_claim = true;
        });
        assert_eq!(register(&api), 1);
        assert!(OWNER.lock().unwrap().is_none());
        REGISTRATION.with(|state| state.borrow_mut().reject_claim = false);

        assert_eq!(register(&api), 0);
        REGISTRATION.with(|state| state.borrow_mut().active = false);
        assert_eq!(register(&api), 0);
        REGISTRATION.with(|state| state.borrow_mut().active = true);
        // A second active registration cannot claim stdio before the old
        // worker releases its slot. Its failed registration still has hooks.
        assert_eq!(register(&api), 1);
        let (hooks, lifecycles) = REGISTRATION.with(|state| {
            let mut state = state.borrow_mut();
            (
                std::mem::take(&mut state.hooks),
                std::mem::take(&mut state.lifecycles),
            )
        });
        assert_eq!(lifecycles.len(), 4);
        let (begin, finish, current) = lifecycles[2];
        let owner = find_owner(current as usize).unwrap();
        for &(old_begin, old_finish, stale) in lifecycles.iter().filter(|item| item.2 != current) {
            old_begin(stale);
            assert_eq!(old_finish(stale), 0);
        }
        for &(tag, callback, context) in hooks.iter().filter(|hook| hook.2 != current) {
            assert_eq!(callback(StablePluginEvent::empty(tag), context), 0);
        }
        assert!(!*owner.stop.borrow());
        assert!(!owner.worker.lock().unwrap().started);

        let mut receiver = owner.stop.subscribe();
        let (stopped_tx, stopped_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            runtime.block_on(receiver.wait_for(|stop| *stop)).unwrap();
            stopped_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });
        owner.worker.lock().unwrap().thread = Some(worker);
        begin(current);
        stopped_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(start_owner(current as usize), 0);
        assert!(!owner.worker.lock().unwrap().started);
        assert_eq!(register(&api), 1);
        release_tx.send(()).unwrap();
        assert_eq!(finish(current), 0);
        assert!(OWNER.lock().unwrap().is_none());
        assert_eq!(finish(current), 0);

        assert_eq!(register(&api), 0);
        let (new_begin, new_finish, replacement) =
            REGISTRATION.with(|state| *state.borrow().lifecycles.last().unwrap());
        assert_ne!(replacement, current);
        begin(current);
        assert_eq!(finish(current), 0);
        assert!(!*find_owner(replacement as usize).unwrap().stop.borrow());
        new_begin(replacement);
        assert_eq!(new_finish(replacement), 0);
        assert!(OWNER.lock().unwrap().is_none());

        // Older isolated loaders still retire their owner on SessionShutdown.
        let legacy = PluginApi {
            register_plugin_lifecycle: None,
            ..api
        };
        assert_eq!(register(&legacy), 0);
        let (tag, shutdown, legacy_context) = REGISTRATION.with(|state| {
            let state = state.borrow();
            state.hooks[state.hooks.len() - 2]
        });
        assert_eq!(tag, EventTag::SessionShutdown);
        assert_eq!(shutdown(StablePluginEvent::empty(tag), legacy_context), 0);
        assert!(OWNER.lock().unwrap().is_none());
    }

    #[test]
    fn inactive_flags_do_not_start_a_service_in_rpc_children() {
        for value in [
            None,
            Some(Value::Null),
            Some(json!(false)),
            Some(json!("false")),
            Some(json!("")),
        ] {
            assert!(!enabled(value.as_ref()), "{value:?}");
        }
        for value in [
            json!(true),
            json!("true"),
            json!("help"),
            json!("/path/http.json"),
        ] {
            assert!(enabled(Some(&value)), "{value:?}");
        }
    }

    #[test]
    fn http_modes_reuse_the_existing_http_cli_contract() {
        assert!(http_args(None).is_empty());
        assert!(http_args(Some(json!(true))).is_empty());
        assert!(http_args(Some(json!("true"))).is_empty());
        assert_eq!(http_args(Some(json!("help"))), ["--help"]);
        assert_eq!(
            http_args(Some(json!("/path/http.json"))),
            ["--config", "/path/http.json"]
        );
        assert!(matches!(
            config::from_args(http_args(Some(json!("help")))).unwrap(),
            Launch::Help
        ));
        assert!(matches!(
            config::from_args(http_args(Some(json!("true")))).unwrap(),
            Launch::Run(_)
        ));
    }

    #[test]
    fn host_listener_flags_become_http_options() {
        assert_eq!(flag_text(json!("3031")).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(3031)).as_deref(), Some("3031"));
        assert_eq!(flag_text(json!(true)), None);
        assert_eq!(flag_text(json!("")), None);
        let options = service_options(Some("0.0.0.0".into()), Some("3031".into()));
        assert_eq!(options, ["--bind", "0.0.0.0", "--port", "3031"]);
        let Launch::Run(config) = config::from_args(options).unwrap() else {
            panic!("expected a run launch");
        };
        assert_eq!(config.bind, "0.0.0.0");
        assert_eq!(config.port, 3031);
        assert!(service_options(None, None).is_empty());
    }
}
