//! Package-local ABI helpers and registration-owned tool workers.
//!
//! Each tool runs its builder on a **worker thread** so `poll` stays
//! non-blocking: the host can then stream progress and, crucially, cancel a call
//! whose worker is waiting on a child process (see [`crate::control`]). This
//! mirrors `rpi-mcp-adapter`'s non-blocking bridge.

use crate::control::Control;
use crate::runtime::{self, Registration};
use rpi_plugin_sdk::{
    FreeStringFn, StbString, StbStringRef, StepHandle, StepResult, ToolCancelFn, ToolDestroyFn,
    ToolExecuteFn, ToolPartialCb, ToolPollFn,
};
use serde_json::Value;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;

pub use rpi_plugin_sdk::callbacks::free_string as plugin_free_string;

/// One message from a worker to `poll`.
pub enum Update {
    /// A partial result, shaped like a tool result (`{"content":[...]}`).
    Progress(String),
    /// The final result (or error message).
    Done(Result<String, String>),
}

/// Half of a worker's channel, handed to the builder so it can report progress.
pub type Progress = Sender<Update>;

/// A tool builder: parameters, cancellation token, progress channel → result JSON.
type Builder = fn(&Registration, &Value, &Control, &Progress) -> Result<String, String>;

/// Send a progress partial (best-effort; a closed channel is ignored).
pub fn progress(sender: &Progress, text: &str) {
    let _ = sender.send(Update::Progress(content_only(text)));
}

pub fn schema(name: &str, description: &str, parameters: &str) -> rpi_plugin_sdk::StableToolSchema {
    rpi_plugin_sdk::StableToolSchema {
        name: StbString::from_string(name.to_string()),
        description: StbString::from_string(description.to_string()),
        parameters: StbString::from_string(parameters.to_string()),
    }
}

pub fn string_param(params: &Value, name: &str) -> Result<String, String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string parameter `{name}`"))
}

pub fn optional_string(params: &Value, name: &str, default: &str) -> String {
    params
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or(default)
        .to_string()
}

/// Read an optional string param, treating blank/whitespace as absent so an empty
/// value never becomes a bare CLI flag.
pub fn optional_trimmed(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// A result with only text content (no state to replay).
pub fn content_only(text: &str) -> String {
    serde_json::json!({"content":[{"type":"text","text":text}]}).to_string()
}

/// A result that also carries `details`, which the host persists on the tool
/// result so the extension can reconstruct its state by replaying the branch.
pub fn result_with_details(text: &str, details: Value) -> String {
    serde_json::json!({"content":[{"type":"text","text":text}],"details":details}).to_string()
}

/// Tokens are unique across registrations, including after slot reuse.
static NEXT_HANDLE: AtomicUsize = AtomicUsize::new(1);

#[derive(Default)]
pub(crate) struct Workers(Mutex<HashMap<usize, Worker>>);

struct Worker {
    control: Control,
    receiver: Receiver<Update>,
    thread: JoinHandle<()>,
    completed: bool,
}

impl Worker {
    fn stop(self) {
        self.control.cancel();
        let _ = self.thread.join();
    }

    fn poll(&mut self) -> StepResult {
        if self.completed {
            return step_error("package tool polled after completion");
        }
        let mut latest = None;
        loop {
            match self.receiver.try_recv() {
                Ok(Update::Progress(json)) => latest = Some(json),
                Ok(Update::Done(result)) => {
                    self.completed = true;
                    return match result {
                        Ok(json) => StepResult::done(StbString::from_string(json)),
                        Err(message) => StepResult::err(StbString::from_string(message)),
                    };
                }
                Err(mpsc::TryRecvError::Empty) => {
                    return StepResult::pending(
                        latest.map_or_else(StbString::empty, StbString::from_string),
                    );
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.completed = true;
                    return step_error("package tool worker disconnected");
                }
            }
        }
    }
}

impl Workers {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<usize, Worker>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn finish(&self) {
        let workers = std::mem::take(&mut *self.lock());
        for worker in workers.into_values() {
            worker.stop();
        }
    }
}

fn step_error(message: &str) -> StepResult {
    StepResult::err(StbString::from_string(message.into()))
}

fn execute(
    slot: usize,
    params: StbString,
    free: Option<FreeStringFn>,
    builder: Builder,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    let Some(registration) = runtime::get(slot) else {
        return std::ptr::null_mut();
    };
    if registration.stop.is_cancelled() {
        return std::ptr::null_mut();
    }
    #[allow(deprecated)] // `try_update` is newer than the workspace MSRV.
    let Ok(handle) = NEXT_HANDLE.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        next.checked_add(1)
    }) else {
        return std::ptr::null_mut();
    };
    let control = registration.stop.for_worker();
    let worker_control = control.clone();
    let worker_registration = registration.clone();
    let (sender, receiver) = mpsc::channel();
    let Ok(thread) = std::thread::Builder::new().spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker_control.check()?;
            let params = serde_json::from_str(&text).unwrap_or(Value::Null);
            builder(&worker_registration, &params, &worker_control, &sender)
        }))
        .unwrap_or_else(|_| Err("tool worker panicked".into()));
        let _ = sender.send(Update::Done(result));
    }) else {
        return std::ptr::null_mut();
    };
    registration.workers.lock().insert(
        handle,
        Worker {
            control,
            receiver,
            thread,
            completed: false,
        },
    );
    handle as StepHandle
}

macro_rules! tool_execute {
    ($name:ident, $builder:path) => {
        extern "C" fn $name<const SLOT: usize>(
            _: StbStringRef,
            params: StbString,
            free: Option<FreeStringFn>,
        ) -> StepHandle {
            execute(SLOT, params, free, $builder)
        }
    };
}

tool_execute!(subagent, crate::subagent::subagent);
tool_execute!(plan, crate::plan::plan);
tool_execute!(dispatch, crate::dispatch::dispatch);

extern "C" fn poll<const SLOT: usize>(
    handle: StepHandle,
    _: Option<ToolPartialCb>,
    _: *mut c_void,
) -> StepResult {
    runtime::get(SLOT)
        .and_then(|registration| {
            registration
                .workers
                .lock()
                .get_mut(&(handle as usize))
                .map(Worker::poll)
        })
        .unwrap_or_else(|| step_error("invalid or destroyed tool handle"))
}

extern "C" fn cancel<const SLOT: usize>(handle: StepHandle) {
    if let Some(registration) = runtime::get(SLOT) {
        if let Some(worker) = registration.workers.lock().get(&(handle as usize)) {
            worker.control.cancel();
        }
    }
}

extern "C" fn destroy<const SLOT: usize>(handle: StepHandle) {
    if let Some(registration) = runtime::get(SLOT) {
        let worker = registration.workers.lock().remove(&(handle as usize));
        if let Some(worker) = worker {
            worker.stop();
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Callbacks {
    pub executes: [ToolExecuteFn; 3],
    pub poll: ToolPollFn,
    pub cancel: ToolCancelFn,
    pub destroy: ToolDestroyFn,
}

const fn callbacks<const SLOT: usize>() -> Callbacks {
    Callbacks {
        executes: [subagent::<SLOT>, plan::<SLOT>, dispatch::<SLOT>],
        poll: poll::<SLOT>,
        cancel: cancel::<SLOT>,
        destroy: destroy::<SLOT>,
    }
}

// The tool ABI has no registration data parameter. Each callback set binds
// to a slot that can be reclaimed only after the host drains its callbacks.
#[rustfmt::skip]
const SHARED_CALLBACKS: [Callbacks; runtime::SHARED_SLOTS] = [
    callbacks::<0>(), callbacks::<1>(), callbacks::<2>(), callbacks::<3>(), callbacks::<4>(), callbacks::<5>(), callbacks::<6>(), callbacks::<7>(),
    callbacks::<8>(), callbacks::<9>(), callbacks::<10>(), callbacks::<11>(), callbacks::<12>(), callbacks::<13>(), callbacks::<14>(), callbacks::<15>(),
    callbacks::<16>(), callbacks::<17>(), callbacks::<18>(), callbacks::<19>(), callbacks::<20>(), callbacks::<21>(), callbacks::<22>(), callbacks::<23>(),
    callbacks::<24>(), callbacks::<25>(), callbacks::<26>(), callbacks::<27>(), callbacks::<28>(), callbacks::<29>(), callbacks::<30>(), callbacks::<31>(),
    callbacks::<32>(), callbacks::<33>(), callbacks::<34>(), callbacks::<35>(), callbacks::<36>(), callbacks::<37>(), callbacks::<38>(), callbacks::<39>(),
    callbacks::<40>(), callbacks::<41>(), callbacks::<42>(), callbacks::<43>(), callbacks::<44>(), callbacks::<45>(), callbacks::<46>(), callbacks::<47>(),
    callbacks::<48>(), callbacks::<49>(), callbacks::<50>(), callbacks::<51>(), callbacks::<52>(), callbacks::<53>(), callbacks::<54>(), callbacks::<55>(),
    callbacks::<56>(), callbacks::<57>(), callbacks::<58>(), callbacks::<59>(), callbacks::<60>(), callbacks::<61>(), callbacks::<62>(), callbacks::<63>(),
    callbacks::<64>(), callbacks::<65>(), callbacks::<66>(), callbacks::<67>(), callbacks::<68>(), callbacks::<69>(), callbacks::<70>(), callbacks::<71>(),
    callbacks::<72>(), callbacks::<73>(), callbacks::<74>(), callbacks::<75>(), callbacks::<76>(), callbacks::<77>(), callbacks::<78>(), callbacks::<79>(),
    callbacks::<80>(), callbacks::<81>(), callbacks::<82>(), callbacks::<83>(), callbacks::<84>(), callbacks::<85>(), callbacks::<86>(), callbacks::<87>(),
    callbacks::<88>(), callbacks::<89>(), callbacks::<90>(), callbacks::<91>(), callbacks::<92>(), callbacks::<93>(), callbacks::<94>(), callbacks::<95>(),
    callbacks::<96>(), callbacks::<97>(), callbacks::<98>(), callbacks::<99>(), callbacks::<100>(), callbacks::<101>(), callbacks::<102>(), callbacks::<103>(),
    callbacks::<104>(), callbacks::<105>(), callbacks::<106>(), callbacks::<107>(), callbacks::<108>(), callbacks::<109>(), callbacks::<110>(), callbacks::<111>(),
    callbacks::<112>(), callbacks::<113>(), callbacks::<114>(), callbacks::<115>(), callbacks::<116>(), callbacks::<117>(), callbacks::<118>(), callbacks::<119>(),
    callbacks::<120>(), callbacks::<121>(), callbacks::<122>(), callbacks::<123>(), callbacks::<124>(), callbacks::<125>(), callbacks::<126>(), callbacks::<127>(),
];

pub(crate) fn callbacks_for(slot: usize) -> Callbacks {
    if slot == runtime::LEGACY_SLOT {
        callbacks::<{ runtime::LEGACY_SLOT }>()
    } else {
        SHARED_CALLBACKS[slot]
    }
}

#[cfg(test)]
#[path = "registration_tests.rs"]
mod registration_tests;
