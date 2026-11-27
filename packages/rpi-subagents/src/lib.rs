#![forbid(unsafe_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

mod kit;

use crate::kit::{plugin_free_string, schema};
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    RuntimeActionId, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const GET_EXECUTION_CONTEXT: u32 = RuntimeActionId::GetExecutionContext as u32;
const INDEPENDENT_RUN: u32 = RuntimeActionId::IndependentRun as u32;
const MAX_OUTPUT_BYTES: usize = 4 * 1024 * 1024;
const MAX_PARTIAL_EVENTS: usize = 8192;
static JOBS: ToolHandles<Job> = ToolHandles::new();
// Only process-lifetime host function pointers are global. Never retain a
// registration's user_data: a candidate reload may be rolled back and freed.
#[derive(Clone, Copy)]
struct RuntimeFns {
    action: extern "C" fn(u32, StbStringRef, *mut StbString, *mut std::ffi::c_void) -> i32,
    free: rpi_plugin_sdk::FreeStringFn,
}
// The native host uses a process-lifetime trampoline and allocator for all
// registrations of this image. A different table must never replace them.
static HOST: OnceLock<RuntimeFns> = OnceLock::new();

fn install_host(host: RuntimeFns) -> bool {
    let installed = HOST.get_or_init(|| host);
    installed.action as usize == host.action as usize
        && installed.free as usize == host.free as usize
}

enum JobMessage {
    Partial(String),
    Done(String),
    Error(String),
}
struct Job {
    receiver: Receiver<JobMessage>,
    // The worker queues bounded deltas; tool progress consumers replace the
    // displayed result, so poll emits a cumulative snapshot instead.
    progress: String,
    completed: bool,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Request {
    task: String,
    role: String,
    max_turns: u64,
    timeout_seconds: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct BudgetPolicy {
    max_turns: Option<u64>,
    timeout_seconds: Option<u64>,
}

fn parse_request(params: &Value) -> Result<Request, String> {
    parse_request_with_budget_policy(params, configured_budget_policy()?)
}

/// Independent operator policies. Neither changes native cancellation, output
/// bounds or ownership; zero disables only the turn guard.
fn configured_budget_policy() -> Result<BudgetPolicy, String> {
    Ok(BudgetPolicy {
        max_turns: configured_limit("RPI_SUBAGENT_MAX_TURNS", 0, 32)?,
        timeout_seconds: configured_limit("RPI_SUBAGENT_TIMEOUT_SECONDS", 10, 1800)?,
    })
}

fn configured_limit(name: &str, minimum: u64, maximum: u64) -> Result<Option<u64>, String> {
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => Ok(None),
        Ok(value) => parse_operator_limit(name, &value, minimum, maximum).map(Some),
        Err(_) => Err(format!("{name} must be valid UTF-8")),
    }
}

fn parse_operator_limit(
    name: &str,
    value: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, String> {
    value
        .trim()
        .parse::<u64>()
        .ok()
        .filter(|value| (minimum..=maximum).contains(value))
        .ok_or_else(|| format!("{name} must be an integer between {minimum} and {maximum}"))
}

fn parse_request_with_budget_policy(
    params: &Value,
    policy: BudgetPolicy,
) -> Result<Request, String> {
    let task = params
        .get("task")
        .and_then(Value::as_str)
        .ok_or("missing string parameter `task`")?;
    let role = params
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("worker")
        .trim();
    if task.trim().is_empty() || task.len() > 20_000 {
        return Err("task must contain 1-20000 bytes".into());
    }
    if role.is_empty() || role.len() > 200 {
        return Err("role must contain 1-200 bytes".into());
    }
    Ok(Request {
        task: task.into(),
        role: role.into(),
        max_turns: policy.max_turns.unwrap_or_else(|| {
            params
                .get("maxTurns")
                .and_then(Value::as_u64)
                .unwrap_or(8)
                .clamp(0, 32)
        }),
        timeout_seconds: policy.timeout_seconds.unwrap_or_else(|| {
            params
                .get("timeoutSeconds")
                .and_then(Value::as_u64)
                .unwrap_or(300)
                .clamp(10, 1800)
        }),
    })
}

enum ActionError {
    UnknownAction,
    Failed(String),
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownAction => f.write_str("unsupported runtime action"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

type Action = dyn Fn(u32, Value) -> Result<Value, ActionError> + Send + Sync;

fn host_action(
    host: &RuntimeFns,
    runtime_handle: usize,
    action: u32,
    args: Value,
) -> Result<Value, ActionError> {
    let encoded = args.to_string();
    let mut out = StbString::empty();
    let rc = (host.action)(
        action,
        StbStringRef::from_str(&encoded),
        &mut out,
        runtime_handle as *mut std::ffi::c_void,
    );
    let text = rpi_plugin_sdk::stb_take!(out, Some(host.free));
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| ActionError::Failed(format!("invalid runtime response (status {rc}): {e}")))?;
    if rc != 0 {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("runtime action failed");
        return Err(
            if rc == 2 && message == format!("unknown runtime action id {action}") {
                ActionError::UnknownAction
            } else {
                ActionError::Failed(message.into())
            },
        );
    }
    Ok(value)
}

/// Check the invocation bridge against the host-injected tool context.
fn validate_context(expected: &Value, actual: &Value) -> Result<(), String> {
    if let Some(id) = expected.get("sessionId").and_then(Value::as_str) {
        if actual.get("sessionId").and_then(Value::as_str) == Some(id) {
            return Ok(());
        }
    } else if let Some(cwd) = expected.get("cwd").and_then(Value::as_str) {
        if actual.get("cwd").and_then(Value::as_str) == Some(cwd) {
            return Ok(());
        }
    }
    Err("subagent host context does not match the calling project/session; reload the extension in this session".into())
}

/// Always release the host handle, including malformed replies and local cancellation.
struct Child<'a> {
    action: &'a Action,
    id: String,
    session_id: Value,
}
impl<'a> Child<'a> {
    fn start(action: &'a Action, request: &Request, tools: &[Value]) -> Result<Self, String> {
        let Request {
            task,
            role,
            max_turns,
            timeout_seconds,
        } = request;
        let budget = if *max_turns == 0 {
            "Complete the task within the host timeout.".to_owned()
        } else {
            format!("Complete the task in at most {max_turns} turns.")
        };
        let input = format!("You are a delegated {role} subagent. {budget} Do not delegate, spawn, or ask another agent to do work. Work only in the current project and return a concise final report.\n\nTask:\n{task}");
        // The extension owns its tool selection. The host checks the subset and
        // also prevents re-entry through aliases using invocation provenance.
        let tools: Vec<_> = tools
            .iter()
            .filter_map(Value::as_str)
            .filter(|name| *name != "delegate_task")
            .collect();
        let mut reply = action(
            INDEPENDENT_RUN,
            json!({
                "op": "start", "input": input, "tools": tools,
                "maxTurns": max_turns, "timeoutSeconds": timeout_seconds,
            }),
        )
        .map_err(|error| match error {
            ActionError::UnknownAction => {
                "Upgrade to an rpi host supporting IndependentRun to use delegate_task.".into()
            }
            ActionError::Failed(message) => {
                format!("Cannot start a host-managed subagent: {message}")
            }
        })?;
        let id = reply
            .get("runId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or("run start returned no runId")?
            .to_owned();
        let session_id = reply.get_mut("sessionId").map_or(Value::Null, Value::take);
        Ok(Self {
            action,
            id,
            session_id,
        })
    }

    fn call(&self, op: &str) -> Result<Value, String> {
        (self.action)(INDEPENDENT_RUN, json!({"op":op,"runId":self.id}))
            .map_err(|error| error.to_string())
    }
}
impl Drop for Child<'_> {
    fn drop(&mut self) {
        let _ = self.call("dispose");
    }
}

fn run_request(
    request: Request,
    tools: &[Value],
    action: &Action,
    output: Sender<JobMessage>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    if cancelled.load(Ordering::Acquire) {
        return Err("subagent cancelled".into());
    }
    let child = Child::start(action, &request, tools)?;
    let deadline = Instant::now() + Duration::from_secs(request.timeout_seconds + 3);
    let mut bytes = 0usize;
    let mut events = 0usize;
    loop {
        if cancelled.load(Ordering::Acquire) {
            let _ = child.call("cancel");
            return Err("subagent cancelled".into());
        }
        if Instant::now() >= deadline {
            return Err("host-managed subagent timed out".into());
        }
        let value = child.call("poll")?;
        if let Some(deltas) = value.get("deltas").and_then(Value::as_array) {
            for delta in deltas {
                let text = delta.as_str().ok_or("invalid child text delta")?;
                bytes = bytes.saturating_add(text.len());
                events += 1;
                if bytes > MAX_OUTPUT_BYTES || events > MAX_PARTIAL_EVENTS {
                    return Err("subagent output limit exceeded".into());
                }
                output
                    .send(JobMessage::Partial(text.into()))
                    .map_err(|_| "subagent receiver closed")?;
            }
        }
        match value.get("status").and_then(Value::as_str) {
            Some("running") => thread::sleep(Duration::from_millis(25)),
            Some("failed") => {
                return Err(value
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("child failed")
                    .into())
            }
            Some("done") => {
                let result = value
                    .get("result")
                    .ok_or("child completed without a result")?;
                let report = result.get("text").and_then(Value::as_str).unwrap_or("");
                if bytes.saturating_add(report.len()) > MAX_OUTPUT_BYTES {
                    return Err("subagent output limit exceeded".into());
                }
                if result.get("status").and_then(Value::as_str) != Some("completed") {
                    return Err(format!(
                        "subagent {}: {}",
                        result
                            .get("status")
                            .and_then(Value::as_str)
                            .unwrap_or("failed"),
                        result
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or(report)
                    ));
                }
                output.send(JobMessage::Done(json!({"kind":"rpi-delegation", "task":request.task,
                    "role":request.role, "maxTurns":request.max_turns, "timeoutSeconds":request.timeout_seconds,
                    "childId":child.id, "sessionId":child.session_id, "status":"completed",
                    "turns":result.get("turns"), "turnLimited":result.get("turnLimited"), "report":report}).to_string()))
                    .map_err(|_| "subagent receiver closed")?;
                return Ok(());
            }
            _ => return Err("invalid child lifecycle status".into()),
        }
    }
}

fn tool_result(text: &str) -> String {
    json!({"content":[{"type":"text","text":text}]}).to_string()
}

rpi_plugin_sdk::execute_handler!(fn execute(_tool_call_id, params, free_params) {
    rpi_plugin_sdk::guard(|| {
        let text = rpi_plugin_sdk::stb_take!(params, free_params);
        let value = serde_json::from_str::<Value>(&text).unwrap_or(Value::Null);
        let expected_context = value.get("__rpi").cloned().unwrap_or(Value::Null);
        // The worker uses its invocation handle through disposal.
        let host = HOST.get().copied();
        let (sender, receiver) = mpsc::channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = match parse_request(&value) {
            Ok(request) => {
                let token = cancelled.clone();
                Some(thread::spawn(move || {
                    let result = rpi_plugin_sdk::guard(|| {
                        let host = host.ok_or("subagent host runtime is unavailable")?;
                        let runtime_handle = expected_context.get("runtimeHandle").and_then(Value::as_u64)
                            .and_then(|id| usize::try_from(id).ok()).filter(|id| *id != 0)
                            .ok_or("IndependentRun requires a host-provided tool invocation runtimeHandle; upgrade the host")?;
                        let actual = host_action(&host, runtime_handle, GET_EXECUTION_CONTEXT, json!({})).map_err(|error|
                            format!("Cannot read execution context: {error}; upgrade to a host supporting GetExecutionContext"))?;
                        validate_context(&expected_context, &actual)?;
                        let runtime_key = expected_context.get("runtimeKey").and_then(Value::as_str)
                            .ok_or("IndependentRun requires a host-provided invocation runtimeKey; upgrade the host")?.to_owned();
                        let action = move |action_id, mut args: Value| {
                            args["__rpiRuntimeKey"] = json!(runtime_key);
                            host_action(&host, runtime_handle, action_id, args)
                        };
                        let tools = actual["tools"].as_array().map(Vec::as_slice).unwrap_or_default();
                        run_request(request, tools, &action, sender.clone(), token)
                    });
                    match result {
                        Ok(Ok(())) => {},
                        Ok(Err(error)) => { let _ = sender.send(JobMessage::Error(error)); },
                        Err(_) => { let _ = sender.send(JobMessage::Error("subagent worker panicked".into())); },
                    }
                }))
            }
            Err(error) => { let _ = sender.send(JobMessage::Error(error)); None }
        };
        JOBS.insert_with_cancel(Job { receiver, progress: String::new(), completed: false, worker }, cancelled)
    }).unwrap_or(std::ptr::null_mut())
});

extern "C" fn poll(
    handle: StepHandle,
    partial_cb: Option<ToolPartialCb>,
    user_data: *mut std::ffi::c_void,
) -> StepResult {
    rpi_plugin_sdk::guard(|| {
        let Some(entry) = JOBS.get(handle) else {
            return StepResult::err(StbString::from_string("invalid subagent handle".into()));
        };
        if entry.cancelled.load(Ordering::Acquire) {
            return StepResult::err(StbString::from_string("subagent cancelled".into()));
        }
        let (progress, terminal) = {
            let mut job = entry.lock();
            if job.completed {
                return StepResult::err(StbString::from_string(
                    "subagent polled after completion".into(),
                ));
            }
            let previous_len = job.progress.len();
            let mut terminal = None;
            while let Ok(message) = job.receiver.try_recv() {
                match message {
                    JobMessage::Partial(text) => job.progress.push_str(&text),
                    JobMessage::Done(text) => {
                        job.completed = true;
                        terminal = Some(Ok(text));
                        break;
                    }
                    JobMessage::Error(error) => {
                        job.completed = true;
                        terminal = Some(Err(error));
                        break;
                    }
                }
            }
            // Coalesce this poll's deltas before cloning/encoding. Do not queue
            // cumulative strings or emit one growing snapshot for every delta.
            let progress = (partial_cb.is_some() && job.progress.len() != previous_len)
                .then(|| job.progress.clone());
            (progress, terminal)
        };
        // Never call foreign code while holding the job lock.
        if let (Some(callback), Some(text)) = (partial_cb, progress) {
            callback(StbString::from_string(tool_result(&text)), user_data);
        }
        match terminal {
            Some(Ok(text)) => StepResult::done(StbString::from_string(tool_result(&text))),
            Some(Err(error)) => StepResult::err(StbString::from_string(error)),
            None => StepResult::pending(StbString::empty()),
        }
    })
    .unwrap_or_else(|_| StepResult::err(StbString::from_string("subagent poll panicked".into())))
}
extern "C" fn cancel(handle: StepHandle) {
    JOBS.cancel(handle);
}
extern "C" fn destroy(handle: StepHandle) {
    // Never let a detached plugin worker outlive the DLL/host bridge keepalive.
    if let Some(entry) = JOBS.remove(handle) {
        let worker = entry.lock().worker.take();
        if let Some(worker) = worker {
            let _ = worker.join();
        }
    }
}

/// Keep registration and schema tests on the same metadata path.
fn delegate_schema() -> rpi_plugin_sdk::StableToolSchema {
    delegate_schema_with_budget_policy(configured_budget_policy().unwrap_or_default())
}

fn delegate_schema_with_budget_policy(policy: BudgetPolicy) -> rpi_plugin_sdk::StableToolSchema {
    let mut registered = schema(
        "delegate_task",
        "Delegate one bounded, non-recursive task to a host-managed in-memory child session, not a subagent subprocess or isolated worktree. The child inherits the current model/provider, tools, cwd and approval hooks; it shares the workspace and extension state. Supply necessary context, exact scope/file ownership, constraints, acceptance checks and expected report in task; do not assume the child knows the parent conversation. Coordinate overlapping writes. role is a label, not a tool/permission profile. The child cannot delegate further. Streams progress and returns a JSON report; review the report and verify acceptance, including turnLimited. Requires a host supporting IndependentRun.",
        r#"{
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "description": "Required self-contained task: objective, relevant context/paths, allowed scope and write ownership, constraints, acceptance checks and expected report. Must contain non-whitespace text and be at most 20000 UTF-8 bytes (not characters); original text is forwarded unchanged. Shared workspace, not a sandbox or isolated checkout."
                },
                "role": {
                    "type": "string",
                    "default": "worker",
                    "description": "Optional descriptive role label, not a permission/tool selector. Trimmed value must contain 1-200 UTF-8 bytes; default worker."
                },
                "maxTurns": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": 32,
                    "default": 8,
                    "description": "Per-child turn budget: 0 disables the turn limit and requires host support; 1-32 selects a finite budget; default 8 unless overridden by operator policy. A completed report may be turnLimited; verify task acceptance."
                },
                "timeoutSeconds": {
                    "type": "integer",
                    "minimum": 10,
                    "maximum": 1800,
                    "default": 300,
                    "description": "Child total wall-clock timeout in seconds, 10-1800; default 300 unless overridden by operator policy. Includes model generation, tools and waiting; independent of maxTurns. Timeout/cancellation triggers host-managed cleanup."
                }
            },
            "required": ["task"],
            "additionalProperties": true
        }"#,
    );
    let mut parameters: Value =
        serde_json::from_str(&rpi_plugin_sdk::stb_text!(registered.parameters))
            .expect("static delegate schema");
    if let Some(turns) = policy.max_turns {
        parameters["properties"]["maxTurns"] = json!({"type":"integer", "enum":[turns], "default":turns,
            "description":if turns == 0 { "Operator policy disables the turn limit. Timeout and cancellation still apply; omit maxTurns or use 0.".to_string() } else { format!("Operator policy fixes the per-child turn budget at {turns}.") }});
    }
    if let Some(seconds) = policy.timeout_seconds {
        parameters["properties"]["timeoutSeconds"] = json!({"type":"integer", "enum":[seconds], "default":seconds,
            "description":format!("Operator policy fixes the child total wall-clock timeout at {seconds} seconds. Includes model generation, tools and waiting; independent of maxTurns.")});
    }
    registered.parameters = StbString::from_string(parameters.to_string());
    registered
}

fn register_plugin(api: &rpi_plugin_sdk::PluginApi) -> i32 {
    if std::env::var("RPI_SUBAGENT_CHILD").ok().as_deref() == Some("1") {
        return 0;
    }
    let Some(register) = api.register_tool else {
        return 1;
    };
    if !install_host(RuntimeFns {
        action: api.runtime_action,
        free: api.free_string,
    }) {
        return 1;
    }
    let schema = Box::new(delegate_schema());
    let result = register(&*schema, execute, poll, cancel, destroy, plugin_free_string);
    drop(schema);
    result
}

// Opt in to sharing the library image across native Host registrations.

rpi_plugin_sdk::export_shared_plugin!(register_plugin);

#[cfg(test)]
mod tests;
