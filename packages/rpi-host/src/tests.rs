use super::*;
#[cfg(unix)]
use rpi_extcore::shared_host::protocol::*;
use std::sync::Mutex;
static WORKER_TEST: Mutex<()> = Mutex::new(());

#[test]
fn registered_schema_has_all_supported_commands_and_no_endpoint_override() {
    let schema = schema();
    assert_eq!(schema.name.to_string_lossy(), "host");
    let parameters: Value = serde_json::from_str(&schema.parameters.to_string_lossy()).unwrap();
    assert_eq!(parameters["required"], json!(["command"]));
    assert_eq!(parameters["additionalProperties"], false);
    assert_eq!(parameters["x-rpi-result-envelope"], 1);
    let operations: Vec<_> = parameters["properties"]["command"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["properties"]["op"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(operations.len(), 13);
    for op in [
        "status",
        "list",
        "inspect",
        "profiles",
        "receipt",
        "result",
        "submit",
        "administrative_receipt",
        "administrative_create",
        "administrative_restore",
        "administrative_close",
        "administrative_cancel",
        "administrative_stop",
    ] {
        assert!(operations.contains(&op));
    }
    assert!(parameters["properties"].get("token").is_none());
    assert!(parameters["properties"].get("config").is_none());
    let description = schema.description.to_string_lossy();
    for guidance in [
        "wire v1.5",
        "outcome_unknown",
        "remote work may continue",
        "generic error without a structured result",
        "original agent_id and request_id",
        "administrative_receipt with the complete original request",
        "not an OS sandbox",
        "joined",
    ] {
        assert!(description.contains(guidance));
    }
}

#[test]
fn missing_configuration_is_not_sent_and_starts_no_worker() {
    let mut drive = Drive::start(
        r#"{"command":{"op":"status"}}"#,
        &Err("operator configuration required"),
    );
    assert!(drive.worker.is_none());
    assert_eq!(drive.poll().unwrap()["status"], "not_sent");
}

#[cfg(unix)]
#[test]
fn poll_is_pending_and_destroy_joins_cancelled_transport_before_releasing_slot() {
    let _serial = WORKER_TEST.lock().unwrap();
    let (root, config, listener) = control::tests::endpoint();
    let (received, wait_received) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        use std::io::Read;
        let mut stream = control::tests::accept(&listener);
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let _: Hello = control::tests::read(&mut stream);
        control::tests::write(
            &mut stream,
            &control::tests::welcome(&config, vec![Capability::Status]),
        );
        let _: Request = control::tests::read(&mut stream);
        received.send(()).unwrap();
        assert_eq!(
            stream.read(&mut [0; 1]).unwrap(),
            0,
            "destroy must close transport"
        );
    });
    let config = control::tests::configuration(&root);
    let mut drive = Drive::start(r#"{"command":{"op":"status"}}"#, &Ok(config));
    wait_received
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    assert!(drive.poll().is_none());
    assert_eq!(ACTIVE.load(Ordering::Acquire), 1);
    drop(drive);
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    peer.join().unwrap();
}

#[test]
fn local_worker_limit_is_reserved_until_physical_join() {
    let _serial = WORKER_TEST.lock().unwrap();
    let permits: Vec<_> = (0..MAX_WORKERS)
        .map(|_| Permit::acquire().unwrap())
        .collect();
    assert!(Permit::acquire().is_none());
    drop(permits);
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
}

#[cfg(unix)]
fn stalled_handle(
    command: Command,
) -> (tempfile::TempDir, StepHandle, std::thread::JoinHandle<()>) {
    let (root, config, listener) = control::tests::endpoint();
    let expected = config.clone();
    let capability = command.capability();
    let (received, wait_received) = std::sync::mpsc::channel();
    let peer = std::thread::spawn(move || {
        use std::io::Read;
        let mut stream = control::tests::accept(&listener);
        let _: Hello = control::tests::read(&mut stream);
        control::tests::write(
            &mut stream,
            &control::tests::welcome(&expected, vec![capability]),
        );
        let _: Request = control::tests::read(&mut stream);
        received.send(()).unwrap();
        assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    });
    let handle = HANDLES.insert(Drive::start(
        &json!({"command":command}).to_string(),
        &Ok(config),
    ));
    wait_received
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    (root, handle, peer)
}

#[cfg(unix)]
#[test]
fn abi_cancel_after_mutation_send_returns_original_receipt_then_destroy_joins() {
    let _serial = WORKER_TEST.lock().unwrap();
    let request_id = RequestId::new();
    let command = Command::Submit {
        agent_id: AgentId::new(),
        request_id,
        prompt: "must not be replayed".into(),
    };
    let (_root, handle, peer) = stalled_handle(command.clone());
    struct HandleGuard(StepHandle);
    impl Drop for HandleGuard {
        fn drop(&mut self) {
            destroy(self.0);
        }
    }
    let guard = HandleGuard(handle);
    cancel(handle);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let report = loop {
        match rpi_plugin_sdk::take_step_result!(poll(handle, None, std::ptr::null_mut())) {
            rpi_plugin_sdk::callbacks::Step::Pending(_) => {
                assert!(std::time::Instant::now() < deadline);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            rpi_plugin_sdk::callbacks::Step::Done(text) => {
                break serde_json::from_str::<Value>(&text).unwrap()
            }
            error => panic!("unexpected ABI step: {error:?}"),
        }
    };
    assert_eq!(report["$rpiToolResult"], 1);
    assert_eq!(report["isError"], true);
    assert_eq!(report["result"]["details"]["status"], "outcome_unknown");
    assert_eq!(report["result"]["details"]["request_id"], json!(request_id));
    assert_eq!(
        report["result"]["details"]["reconcile"]["command"],
        json!(control::reconciliation(&command).unwrap())
    );
    drop(guard);
    peer.join().unwrap();
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
}

#[cfg(unix)]
#[test]
fn abi_destroy_joins_even_while_a_late_observer_retains_the_handle_entry() {
    let _serial = WORKER_TEST.lock().unwrap();
    let (_root, handle, peer) = stalled_handle(Command::Status {});
    let late_observer = HANDLES.get(handle).unwrap();
    destroy(handle);
    assert!(HANDLES.get(handle).is_none());
    assert!(late_observer.lock().completed);
    assert_eq!(ACTIVE.load(Ordering::Acquire), 0);
    peer.join().unwrap();
}
