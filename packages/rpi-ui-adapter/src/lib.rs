//! `--webui=<UI>` selects an independent UI adapter. The host and the ACP
//! implementation remain untouched; the local relay only carries ACP frames.
#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod config;
mod openwebui;
mod server;

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
    let launch = match config::resolve(|name| host.flag(name)) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("[rpi-ui-adapter] {error}");
            host.finish(2);
            return 0;
        }
    };
    let thread = std::thread::Builder::new().spawn(move || {
        if *receiver.borrow() {
            return;
        }
        let result = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime.block_on(server::run(launch, receiver)),
            Err(error) => Err(error.to_string()),
        };
        let code = if let Err(error) = result {
            eprintln!("[rpi-ui-adapter] {error}");
            1
        } else {
            0
        };
        host.finish(code);
    });
    match thread {
        Ok(thread) => worker.thread = Some(thread),
        Err(error) => {
            eprintln!("[rpi-ui-adapter] failed to start service thread: {error}");
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
    if let Some(register_flag) = api.register_flag {
        for (name, description) in config::FLAGS {
            let code = register_flag(
                StbStringRef::from_str(name),
                StbStringRef::from_str(description),
            );
            if code != 0 {
                return code;
            }
        }
    }
    if matches!(
        host.flag("webui"),
        None | Some(Value::Null) | Some(Value::Bool(false))
    ) {
        return 0;
    }
    if host.flag("acp").is_some_and(|v| !v.is_null() && v != false) {
        eprintln!("[rpi-ui-adapter] --webui and --acp are separate entrypoints; choose one");
        return 1;
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
