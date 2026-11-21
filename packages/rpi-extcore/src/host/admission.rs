//! Local execution intent exists before command publication. Correlated native
//! rejection can settle it; timeout, acceptance and caller loss cannot. Writer
//! publication and unsent cancellation share one mutex so an unsent rollback
//! can never race a later write of the same command.
use super::*;
#[cfg(test)]
#[path = "correlated_admission_tests.rs"]
mod correlated_tests;
#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;

#[derive(Default)]
pub(super) struct ExecutionAdmission {
    current: Option<Intent>,
    native_unsettled: bool,
    native_run: Option<String>,
    correlated: bool,
    executions: HashMap<String, Option<String>>,
    recovery_required: bool,
}
#[derive(Clone, Copy)]
pub(super) struct ExecutionProjection {
    pub busy: bool,
    pub settlement: bool,
}
struct Intent {
    kind: String,
    execution: Option<String>,
    id: String,
    token: String,
    published: bool,
    accepted: bool,
    run: Option<String>,
}
pub(super) struct ExecutionTicket {
    state: Arc<StdMutex<ExecutionAdmission>>,
    token: String,
    cancel_queued: bool,
}
impl Clone for ExecutionTicket {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            token: self.token.clone(),
            cancel_queued: true,
        }
    }
}
impl ExecutionTicket {
    pub(super) fn register(
        state: &Arc<StdMutex<ExecutionAdmission>>,
        value: &Value,
    ) -> Result<Option<Self>, String> {
        if !matches!(
            value["type"].as_str(),
            Some("prompt" | "compact" | "resume_pending")
        ) {
            return Ok(None);
        }
        let id = value["id"]
            .as_str()
            .ok_or("execution requires a string request ID")?
            .to_owned();
        let mut admission = state.lock().expect("execution admission");
        if admission.unsettled() {
            return Err("execution admission remains unsettled".into());
        }
        let token = uuid::Uuid::new_v4().to_string();
        admission.current = Some(Intent {
            kind: value["type"].as_str().unwrap().to_string(),
            execution: None,
            id,
            token: token.clone(),
            published: false,
            accepted: false,
            run: None,
        });
        Ok(Some(Self {
            state: state.clone(),
            token,
            cancel_queued: true,
        }))
    }
    pub(super) fn published(&self) -> bool {
        let mut admission = self.state.lock().expect("execution admission");
        if let Some(intent) = admission
            .current
            .as_mut()
            .filter(|intent| intent.token == self.token)
        {
            intent.published = true;
            true
        } else {
            false
        }
    }
    pub(super) fn retain_queued(&mut self) {
        self.cancel_queued = false;
    }
}
impl Drop for ExecutionTicket {
    fn drop(&mut self) {
        if !self.cancel_queued {
            return;
        }
        let mut admission = self.state.lock().expect("execution admission");
        if admission
            .current
            .as_ref()
            .is_some_and(|intent| intent.token == self.token && !intent.published)
        {
            admission.current = None;
        }
    }
}
fn matches_run(expected: Option<&str>, event: Option<&str>) -> bool {
    match expected {
        Some(expected) => event == Some(expected),
        None => true,
    }
}
impl ExecutionAdmission {
    pub(super) fn from_ready(ready: &Value) -> Self {
        Self {
            correlated: ready["semantics"]["executionLifecycle"] == "correlated-v1",
            ..Default::default()
        }
    }
    pub(super) fn unsettled(&self) -> bool {
        self.current.is_some()
            || self.native_unsettled
            || !self.executions.is_empty()
            || self.recovery_required
    }
    pub(super) fn observe(&mut self, value: &Value) -> Option<ExecutionProjection> {
        if self.correlated {
            return Some(self.observe_correlated(value));
        }
        self.observe_legacy(value);
        None
    }
    fn observe_correlated(&mut self, value: &Value) -> ExecutionProjection {
        let execution = value["executionId"].as_str();
        let request = value["requestId"].as_str();
        let mut settlement = false;
        match value["type"].as_str() {
            Some("response") => self.observe_response(value),
            Some("execution_started") if value["lane"] == "main" => {
                if let Some(execution) = execution {
                    let conflicting = self
                        .executions
                        .get(execution)
                        .is_some_and(|owner| owner.as_deref() != request);
                    let overflow =
                        self.executions.len() >= 64 && !self.executions.contains_key(execution);
                    if conflicting || overflow {
                        // Broken/missing lifecycle frames cannot create an
                        // unbounded owner table or rebind somebody else's lease.
                        self.recovery_required = true;
                    } else {
                        self.executions
                            .entry(execution.into())
                            .or_insert_with(|| request.map(str::to_owned));
                        if let Some(intent) = self.current.as_mut().filter(|intent| {
                            intent.published && request == Some(intent.id.as_str())
                        }) {
                            intent.execution = Some(execution.into());
                        }
                    }
                } else {
                    self.recovery_required = true;
                }
            }
            Some("execution_released") if value["lane"] == "main" => {
                if value["recoveryRequired"] == true {
                    self.recovery_required = true;
                }
                // An owned request keeps its local ticket until command cleanup
                // and agent_settled. Autonomous leases remain until settlement.
                if request.is_some() && value["recoveryRequired"] == false {
                    if let Some(id) = execution.filter(|id| {
                        self.executions
                            .get(*id)
                            .is_some_and(|owner| owner.as_deref() == request)
                    }) {
                        self.executions.remove(id);
                    }
                }
            }
            Some("agent_settled") if value["lane"] == "main" => {
                if let Some(id) = execution.filter(|id| {
                    self.executions
                        .get(*id)
                        .is_some_and(|owner| owner.as_deref() == request)
                }) {
                    self.executions.remove(id);
                    settlement = true;
                }
                if self.current.as_ref().is_some_and(|intent| {
                    intent.accepted
                        && request == Some(intent.id.as_str())
                        && intent.execution.as_deref() == execution
                }) {
                    self.current = None;
                    settlement = true;
                }
            }
            _ => {}
        }
        ExecutionProjection {
            busy: self.unsettled(),
            settlement,
        }
    }
    fn observe_response(&mut self, value: &Value) {
        let Some(intent) = self
            .current
            .as_mut()
            .filter(|intent| Some(intent.id.as_str()) == value["id"].as_str())
        else {
            return;
        };
        let skipped_resume = intent.kind == "resume_pending"
            && value["command"] == "resume_pending"
            && value["success"] == true
            && value["data"]["resumed"] == false
            && value["data"]["accepted"] != true;
        if !intent.accepted && (value["success"] == false || skipped_resume) {
            self.current = None;
        } else if value["success"] == true && value["data"]["accepted"] == true {
            intent.accepted = true;
            if !self.correlated {
                self.native_unsettled = true;
                self.native_run = intent.run.clone();
            }
        }
    }
    fn observe_legacy(&mut self, value: &Value) {
        let run = value["runId"].as_str();
        match value["type"].as_str() {
            Some("response") => self.observe_response(value),
            Some("agent_start") => {
                self.native_unsettled = true;
                self.native_run = run.map(str::to_owned);
                if let Some(intent) = self.current.as_mut().filter(|intent| intent.published) {
                    intent.run = self.native_run.clone();
                }
            }
            Some("agent_settled") => {
                if matches_run(self.native_run.as_deref(), run) {
                    self.native_unsettled = false;
                    self.native_run = None;
                }
                if self.current.as_ref().is_some_and(|intent| {
                    intent.accepted && matches_run(intent.run.as_deref(), run)
                }) {
                    self.current = None;
                }
            }
            _ => {}
        }
    }
}
