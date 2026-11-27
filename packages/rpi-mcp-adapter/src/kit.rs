//! Nonblocking tool ABI bridge. Workers are cancelled and joined before destroy.
use crate::control::Control;
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{StbString, StepHandle, StepResult, ToolPartialCb};
use serde_json::{json, Value};
use std::ffi::c_void;
use std::sync::{
    mpsc::{self, Receiver},
    Arc, Mutex, Weak,
};
use std::thread::JoinHandle;

struct State {
    receiver: Receiver<Result<Value, String>>,
    completed: bool,
}
static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    worker: Arc<Worker>,
    state: Mutex<State>,
}

struct Worker {
    control: Control,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Worker {
    fn join(&self) {
        if let Some(thread) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = thread.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct Workers(Mutex<WorkerSet>);

#[derive(Default)]
struct WorkerSet {
    stopping: bool,
    live: Vec<Weak<Worker>>,
}

impl Workers {
    pub(crate) fn is_stopping(&self) -> bool {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).stopping
    }

    pub(crate) fn stop(&self) {
        let mut set = self.0.lock().unwrap_or_else(|p| p.into_inner());
        set.stopping = true;
        for worker in set.live.iter().filter_map(Weak::upgrade) {
            worker.control.cancel();
        }
    }

    pub(crate) fn join(&self) {
        let mut set = self.0.lock().unwrap_or_else(|p| p.into_inner());
        for worker in set.live.drain(..).filter_map(|worker| worker.upgrade()) {
            worker.join();
        }
    }
}

pub fn execute(
    text: String,
    workers: &Workers,
    builder: impl FnOnce(&Value, &Control) -> Result<Value, String> + Send + 'static,
) -> StepHandle {
    let mut set = workers.0.lock().unwrap_or_else(|p| p.into_inner());
    if set.stopping {
        return std::ptr::null_mut();
    }
    let control = Control::default();
    let worker_control = control.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    let Ok(thread) = std::thread::Builder::new().spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker_control.check()?;
            let params: Value = serde_json::from_str(&text)
                .map_err(|e| format!("invalid MCP tool parameters: {e}"))?;
            if !params.is_object() {
                return Err("MCP tool parameters must be an object".into());
            }
            builder(&params, &worker_control)
        }))
        .unwrap_or_else(|_| Err("MCP worker panicked".into()));
        let _ = sender.send(result);
    }) else {
        return std::ptr::null_mut();
    };
    let worker = Arc::new(Worker {
        control,
        thread: Mutex::new(Some(thread)),
    });
    set.live.retain(|worker| worker.strong_count() != 0);
    set.live.push(Arc::downgrade(&worker));
    HANDLES.insert(Drive {
        worker,
        state: Mutex::new(State {
            receiver,
            completed: false,
        }),
    })
}

pub fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null MCP tool handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let drive = entry.lock();
    let mut state = match drive.state.lock() {
        Ok(state) => state,
        Err(_) => return StepResult::err(StbString::from_string("MCP tool state poisoned".into())),
    };
    if state.completed {
        return StepResult::err(StbString::from_string(
            "MCP tool polled after completion".into(),
        ));
    }
    match state.receiver.try_recv() {
        Ok(result) => {
            state.completed = true;
            // The worker only has sender destruction left; joining belongs in destroy,
            // not poll, whose ABI contract is nonblocking.
            match result {
                Ok(value) => StepResult::done(StbString::from_string(value.to_string())),
                Err(error) => StepResult::err(StbString::from_string(error)),
            }
        }
        Err(mpsc::TryRecvError::Empty) => StepResult::pending(StbString::empty()),
        Err(mpsc::TryRecvError::Disconnected) => {
            state.completed = true;
            StepResult::err(StbString::from_string("MCP worker disconnected".into()))
        }
    }
}
pub fn cancel(handle: StepHandle) {
    if let Some(entry) = HANDLES.get(handle) {
        entry.cancel();
        entry.lock().worker.control.cancel();
    }
}
pub fn destroy(handle: StepHandle) {
    if let Some(entry) = HANDLES.remove(handle) {
        let drive = entry.lock();
        drive.worker.control.cancel();
        drive.worker.join();
    }
}
pub fn text_result(text: impl Into<String>) -> Value {
    json!({"content":[{"type":"text","text":text.into()}]})
}
pub fn schema(name: &str, description: &str, parameters: &str) -> rpi_plugin_sdk::StableToolSchema {
    rpi_plugin_sdk::StableToolSchema {
        name: StbString::from_string(name.into()),
        description: StbString::from_string(description.into()),
        parameters: StbString::from_string(parameters.into()),
    }
}
pub use rpi_plugin_sdk::callbacks::free_string as plugin_free_string;
pub fn string_param(params: &Value, name: &str) -> Result<String, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string parameter `{name}`"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    #[test]
    fn poll_is_nonblocking_and_cancel_joins_worker() {
        fn wait(_: &Value, control: &Control) -> Result<Value, String> {
            loop {
                control.check()?;
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let handle = execute("{}".into(), &Workers::default(), wait);
        let start = Instant::now();
        let result = poll(handle, None, std::ptr::null_mut());
        assert_eq!(result.tag, rpi_plugin_sdk::StepResultTag::Pending);
        assert!(start.elapsed() < Duration::from_millis(100));
        cancel(handle);
        loop {
            let result = poll(handle, None, std::ptr::null_mut());
            if result.tag == rpi_plugin_sdk::StepResultTag::Err {
                let rpi_plugin_sdk::callbacks::Step::Err(error) =
                    rpi_plugin_sdk::take_step!(result)
                else {
                    unreachable!()
                };
                assert!(error.contains("cancelled"));
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(2));
            std::thread::sleep(Duration::from_millis(5));
        }
        destroy(handle);
    }
}
