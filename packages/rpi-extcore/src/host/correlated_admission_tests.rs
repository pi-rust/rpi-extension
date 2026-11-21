use super::*;

fn correlated() -> Arc<StdMutex<ExecutionAdmission>> {
    Arc::new(StdMutex::new(ExecutionAdmission::from_ready(&json!({
        "semantics":{"executionLifecycle":"correlated-v1"}
    }))))
}
fn start(execution: &str, request: Value) -> Value {
    json!({"type":"execution_started","executionId":execution,"requestId":request,
        "lane":"main","runId":"shared-durable-run"})
}
fn settle(execution: Value, request: Value) -> Value {
    json!({"type":"agent_settled","executionId":execution,"requestId":request,
        "lane":"main","runId":"shared-durable-run","outcome":"completed"})
}
#[test]
fn autonomous_execution_blocks_until_its_matching_settlement() {
    let state = correlated();
    state
        .lock()
        .unwrap()
        .observe(&start("external", Value::Null));
    assert!(ExecutionTicket::register(&state, &json!({"type":"prompt","id":"next"})).is_err());
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("old"), Value::Null));
    assert!(state.lock().unwrap().unsettled());
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("external"), Value::Null));
    assert!(!state.lock().unwrap().unsettled());
    assert!(ExecutionTicket::register(&state, &json!({"type":"prompt","id":"next"})).is_ok());
}
#[test]
fn unrelated_and_old_execution_cannot_steal_a_published_request() {
    let state = correlated();
    let ticket = ExecutionTicket::register(&state, &json!({"type":"prompt","id":"mine"}))
        .unwrap()
        .unwrap();
    assert!(ticket.published());
    state
        .lock()
        .unwrap()
        .observe(&start("external", Value::Null));
    state
        .lock()
        .unwrap()
        .observe(&json!({"type":"response","id":"mine","success":true,"data":{"accepted":true}}));
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("external"), Value::Null));
    assert!(state.lock().unwrap().unsettled());
    state
        .lock()
        .unwrap()
        .observe(&start("attempt-2", json!("mine")));
    // Same durable run, different execution: a late previous lease cannot clear it.
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("attempt-1"), json!("mine")));
    state
        .lock()
        .unwrap()
        .observe(&settle(Value::Null, json!("mine")));
    assert!(state.lock().unwrap().unsettled());
    state.lock().unwrap().observe(
        &json!({"type":"execution_released","executionId":"attempt-2",
        "requestId":"mine","lane":"main","recoveryRequired":false}),
    );
    assert!(
        state.lock().unwrap().unsettled(),
        "wrapper still owns the local intent"
    );
    state
        .lock()
        .unwrap()
        .observe(&start("successor", Value::Null));
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("attempt-2"), json!("mine")));
    assert!(
        state.lock().unwrap().unsettled(),
        "autonomous successor remains protected"
    );
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("successor"), Value::Null));
    assert!(!state.lock().unwrap().unsettled());
}
#[test]
fn skipped_request_and_fault_never_clear_external_execution() {
    let state = correlated();
    let ticket = ExecutionTicket::register(&state, &json!({"type":"resume_pending","id":"resume"}))
        .unwrap()
        .unwrap();
    assert!(ticket.published());
    state
        .lock()
        .unwrap()
        .observe(&start("external", Value::Null));
    state.lock().unwrap().observe(
        &json!({"type":"response","command":"resume_pending","id":"resume",
        "success":true,"data":{"resumed":false}}),
    );
    assert!(state.lock().unwrap().current.is_none());
    assert!(state.lock().unwrap().unsettled());
    state.lock().unwrap().observe(
        &json!({"type":"execution_released","executionId":"external",
        "requestId":null,"lane":"main","recoveryRequired":true}),
    );
    state
        .lock()
        .unwrap()
        .observe(&settle(json!("external"), Value::Null));
    assert!(
        state.lock().unwrap().unsettled(),
        "recovery failure is not silently reset"
    );
}
#[test]
fn accepted_without_admission_settles_only_by_request_identity() {
    let state = correlated();
    let ticket = ExecutionTicket::register(&state, &json!({"type":"prompt","id":"cancelled"}))
        .unwrap()
        .unwrap();
    assert!(ticket.published());
    state.lock().unwrap().observe(
        &json!({"type":"response","id":"cancelled","success":true,"data":{"accepted":true}}),
    );
    state
        .lock()
        .unwrap()
        .observe(&settle(Value::Null, json!("other")));
    assert!(state.lock().unwrap().unsettled());
    state
        .lock()
        .unwrap()
        .observe(&settle(Value::Null, json!("cancelled")));
    assert!(!state.lock().unwrap().unsettled());
}

#[test]
fn conflicting_or_unbounded_execution_owners_fail_closed() {
    let state = correlated();
    let mut admission = state.lock().unwrap();
    admission.observe(&start("same", json!("owner")));
    admission.observe(&start("same", json!("impostor")));
    admission.observe(&settle(json!("same"), json!("owner")));
    assert!(admission.recovery_required && admission.unsettled());
    let mut admission = ExecutionAdmission::from_ready(&json!({
        "semantics":{"executionLifecycle":"correlated-v1"}}));
    for index in 0..65 {
        admission.observe(&start(&format!("lease-{index}"), Value::Null));
    }
    assert_eq!(admission.executions.len(), 64);
    for index in 0..65 {
        admission.observe(&settle(json!(format!("lease-{index}")), Value::Null));
    }
    assert!(admission.recovery_required && admission.unsettled());
}
