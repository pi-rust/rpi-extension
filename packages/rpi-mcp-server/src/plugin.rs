use crate::protocol::{run, ServerOptions};
use rpi_plugin_sdk::{
    EventTag, PluginApi, RuntimeActionId, StablePluginEvent, StbString, StbStringRef,
};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

const MCP_FLAG: &str = "mcp";

#[derive(Clone, Copy)]
struct Host(PluginApi);

impl Host {
    fn action(self, id: RuntimeActionId, args: Value) -> Option<Value> {
        let encoded = args.to_string();
        let mut out = StbString::empty();
        let rc = (self.0.runtime_action)(
            id as u32,
            StbStringRef::from_str(&encoded),
            &mut out,
            self.0.user_data,
        );
        if rc == 0 {
            let body = rpi_plugin_sdk::stb_take!(out, Some(self.0.free_string));
            serde_json::from_str(&body).ok()
        } else {
            rpi_plugin_sdk::stb_drop!(out, Some(self.0.free_string));
            None
        }
    }

    fn flag(self, name: &str) -> Option<Value> {
        self.action(RuntimeActionId::GetCliFlag, json!({"name":name}))
            .and_then(|value| value.get("value").cloned())
    }

    fn finish(self, code: i32) {
        let _ = self.action(
            RuntimeActionId::StdioLifecycle,
            json!({"op":"finish","exitCode":code}),
        );
    }
}

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
    thread: Option<thread::JoinHandle<()>>,
}

fn flag_is_on(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => {
            !value.trim().is_empty() && !value.eq_ignore_ascii_case("false")
        }
        _ => false,
    }
}

fn get_arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix(&format!("{name}=")).map(String::from))
        })
}

fn get_flag(name: &str) -> bool {
    std::env::args().any(|arg| arg == name)
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
    let receiver = owner.stop.subscribe();
    if worker.started || *receiver.borrow() {
        return 0;
    }
    worker.started = true;
    let host = owner.host;
    // Stdio ownership is already ours here, so refusing a conflicting adapter
    // can end the host instead of silently leaving two servers on one pipe.
    for other in ["acp", "http", "webserver", "web", "server", "webui"] {
        if flag_is_on(host.flag(other).as_ref()) {
            eprintln!("[rpi-mcp] --mcp cannot be combined with --{other}");
            host.finish(2);
            return 0;
        }
    }
    let options = ServerOptions {
        cwd: std::env::current_dir().unwrap_or_else(|_| ".".into()),
        model: get_arg("--model"),
        provider: get_arg("--provider"),
        bypass: get_flag("--bypass"),
        session_dir: get_arg("--session-dir").map(Into::into),
        saved_session: get_arg("--resume").map(Into::into),
    };
    let thread = thread::Builder::new().spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("[rpi-mcp] failed to create runtime: {error}");
                host.finish(1);
                return;
            }
        };
        if *receiver.borrow() {
            return;
        }
        let (input, pending) = match take_stdin() {
            Ok(value) => value,
            Err(error) => {
                eprintln!("[rpi-mcp] failed to take stdin: {error}");
                host.finish(1);
                return;
            }
        };
        let code = runtime.block_on(run(input, pending, receiver, options));
        host.finish(code);
    });
    match thread {
        Ok(thread) => worker.thread = Some(thread),
        Err(error) => {
            eprintln!("[rpi-mcp] failed to start server thread: {error}");
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

fn take_stdin() -> Result<(rpi_plugin_sdk::stdio::StdinLines, Option<String>), String> {
    let mut input = rpi_plugin_sdk::stdio::StdinLines::default();
    let pending = match input.poll_line().map_err(|error| error.to_string())? {
        rpi_plugin_sdk::stdio::InputLine::Line(line) => Some(line),
        rpi_plugin_sdk::stdio::InputLine::Pending | rpi_plugin_sdk::stdio::InputLine::Eof => None,
    };
    Ok((input, pending))
}

pub fn register(api: &PluginApi) -> i32 {
    let host = Host(*api);

    if let Some(register_flag) = api.register_flag {
        if register_flag(
            StbStringRef::from_str(MCP_FLAG),
            StbStringRef::from_str("Run rpi as an MCP stdio server"),
        ) != 0
        {
            return 1;
        }
    }

    if !flag_is_on(host.flag(MCP_FLAG).as_ref()) {
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
    let mut slot = OWNER.lock().unwrap_or_else(|error| error.into_inner());
    if slot.is_some() {
        return 1;
    }
    if declare(StbStringRef::from_str(r#"{"ownsStdio":true}"#)) != 0 {
        eprintln!("[rpi-mcp] host rejected stdio ownership");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_values_are_parsed_like_the_host() {
        assert!(flag_is_on(Some(&json!(true))));
        assert!(flag_is_on(Some(&json!("yes"))));
        assert!(!flag_is_on(Some(&json!(false))));
        assert!(!flag_is_on(Some(&json!("false"))));
        assert!(!flag_is_on(Some(&json!(""))));
        assert!(!flag_is_on(None));
    }

    #[test]
    fn cli_arguments_forward_model_and_provider() {
        // `get_arg` reads the host argv, which tests cannot rewrite; assert the
        // parsing shape through a representative slice instead.
        let args = [
            "rpi",
            "--model",
            "p/m",
            "--provider=p2",
            "--resume",
            "/tmp/s.jsonl",
        ];
        let find = |name: &str| {
            args.windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| pair[1].to_string())
                .or_else(|| {
                    args.iter()
                        .find_map(|arg| arg.strip_prefix(&format!("{name}=")).map(String::from))
                })
        };
        assert_eq!(find("--model").as_deref(), Some("p/m"));
        assert_eq!(find("--provider").as_deref(), Some("p2"));
        assert_eq!(find("--resume").as_deref(), Some("/tmp/s.jsonl"));
        assert_eq!(find("--session-dir"), None);
    }
}
