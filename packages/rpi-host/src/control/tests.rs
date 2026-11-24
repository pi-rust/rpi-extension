use super::*;

#[test]
fn input_is_strict_accepts_only_host_metadata_and_preserves_exact_intent() {
    let request = AdministrativeRequest {
        scope: ControlScopeId::new(),
        request_id: RequestId::new(),
        epoch: Epoch::new(),
        action: AdministrativeAction::Close {
            agent_id: AgentId::new(),
            revision: 7,
        },
    };
    let command = Command::AdministrativeClose { request };
    let encoded = json!({"command":command,"__rpi":{"sessionId":"ignored"}}).to_string();
    assert_eq!(
        serde_json::to_value(parse(&encoded).unwrap()).unwrap(),
        serde_json::to_value(&command).unwrap()
    );
    for bad in [
        json!({"command":{"op":"status","extra":true}}),
        json!({"command":{"op":"status"},"token":"must not override"}),
        json!({"command":{"op":"status"},"config":"/must/not/read"}),
        json!({"command":{"op":"submit","agent_id":AgentId::new(),"prompt":"missing explicit request id"}}),
        json!({"command":{"op":"stop","request_id":RequestId::new()}}),
        json!({"command":{"op":"list","limit":129}}),
    ] {
        assert!(parse(&bad.to_string()).is_err(), "invalid input accepted");
    }
    let too_many_bytes = json!({"command":{"op":"submit", "agent_id":AgentId::new(),"request_id":RequestId::new(),"prompt":"文".repeat(MAX_SUBMIT_PROMPT_BYTES / 3 + 1)}});
    assert!(parse(&too_many_bytes.to_string()).is_err());
}

#[test]
fn unknown_mutation_has_original_receipt_without_prompt_or_new_identity() {
    let command = Command::Submit {
        agent_id: AgentId::new(),
        request_id: RequestId::new(),
        prompt: "private prompt not echoed".into(),
    };
    let result = client_error(
        &command,
        ClientError::OutcomeUnknown {
            request_id: command.request_id().unwrap(),
            reason: rpi_extcore::shared_host::Failure::Timeout,
        },
    );
    assert_eq!(result["status"], "outcome_unknown");
    assert_eq!(result["request_id"], json!(command.request_id()));
    assert_eq!(result["reconcile"]["command"]["op"], "receipt");
    assert!(!result.to_string().contains("private prompt"));
    let query = reconciliation(&command).unwrap();
    assert!(query.request_id().is_none());
    assert!(client_error(
        &command,
        ClientError::Rejected(ErrorCode::PersistenceUncertain)
    )["error"]
        .as_bool()
        .unwrap());
}

#[test]
fn result_query_validates_page_identity_and_preserves_native_states() {
    let agent_id = AgentId::new();
    let request_id = RequestId::new();
    let command = Command::Result {
        agent_id,
        request_id,
        run_id: None,
        offset: 0,
        max_bytes: 4096,
    };
    assert!(command.request_id().is_none());
    assert!(parse(&json!({"command":command}).to_string()).is_ok());
    for (state, error) in [
        ("admitted", false),
        ("running", false),
        ("completed", false),
        ("failed", true),
        ("aborted", true),
        ("unknown", true),
        ("future", true),
    ] {
        let data = json!({"found":true,"result":{"state":state}});
        let result = received(&command, data.clone());
        assert_eq!(result["error"], error);
        assert_eq!(result["data"], data);
        if error {
            assert_eq!(result["reconcile"]["command"], json!(command));
        }
    }
    assert_eq!(received(&command, json!({"found":false}))["error"], false);
    assert_eq!(received(&command, json!({"found":true}))["error"], true);
    for (run_id, offset, max_bytes) in [
        (None, 1, 4096),
        (Some(""), 0, 4096),
        (None, 0, 3),
        (None, 0, 16385),
    ] {
        let bad = json!({"command":{"op":"result","agent_id":agent_id,"request_id":request_id,
            "run_id":run_id,"offset":offset,"max_bytes":max_bytes}});
        assert!(parse(&bad.to_string()).is_err());
    }
}

#[cfg(unix)]
pub(crate) fn configuration(root: &tempfile::TempDir) -> ConnectionConfig {
    crate::config::load(&root.path().join("c.json")).unwrap()
}

#[cfg(unix)]
pub(crate) fn endpoint() -> (
    tempfile::TempDir,
    ConnectionConfig,
    std::os::unix::net::UnixListener,
) {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new().prefix("hx").tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = root.path().join("s");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = json!({"socket_path":path,"host_id":HostId::new(),"token":"fixture".repeat(5),"timeout_seconds":1});
    let file = root.path().join("c.json");
    std::fs::write(&file, config.to_string()).unwrap();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = configuration(&root);
    (root, config, listener)
}

#[cfg(unix)]
pub(crate) fn accept(
    listener: &std::os::unix::net::UnixListener,
) -> std::os::unix::net::UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                return stream;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5))
            }
            Err(error) => panic!("fixture accept deadline/failure: {error}"),
        }
    }
}

#[cfg(unix)]
pub(crate) fn read<T: serde::de::DeserializeOwned>(
    stream: &mut std::os::unix::net::UnixStream,
) -> T {
    use std::io::Read;
    let mut length = [0; 4];
    stream.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length <= MAX_FRAME_BYTES);
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}
#[cfg(unix)]
pub(crate) fn write(stream: &mut std::os::unix::net::UnixStream, value: &impl serde::Serialize) {
    use std::io::Write;
    let bytes = serde_json::to_vec(value).unwrap();
    stream
        .write_all(&(bytes.len() as u32).to_be_bytes())
        .unwrap();
    stream.write_all(&bytes).unwrap();
}
#[cfg(unix)]
pub(crate) fn welcome(config: &ConnectionConfig, capabilities: Vec<Capability>) -> Welcome {
    Welcome {
        protocol: PROTOCOL.into(),
        version: Version::default(),
        host_id: config.host_id,
        epoch: Epoch::new(),
        capabilities,
        limits: Limits::default(),
    }
}

#[cfg(unix)]
#[test]
fn authenticated_query_keeps_control_projection_without_exposing_token() {
    let (_root, config, listener) = endpoint();
    let expected = config.clone();
    let peer = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let hello: Hello = read(&mut stream);
        assert!(hello.token.matches(&expected.token));
        write(&mut stream, &welcome(&expected, vec![Capability::Status]));
        let request: Request = read(&mut stream);
        assert!(matches!(request.command, Command::Status {}));
        write(
            &mut stream,
            &Response {
                call_id: request.call_id,
                epoch: request.epoch,
                reply: Reply::Data {
                    data: json!({"registered":0}),
                },
            },
        );
    });
    let result = run(
        config.clone(),
        Command::Status {},
        Arc::new(AtomicBool::new(false)),
    );
    peer.join().unwrap();
    assert_eq!(result["status"], "reply_received");
    assert_eq!(result["data"]["registered"], 0);
    assert_eq!(result["control"]["host_id"], json!(config.host_id));
    assert!(!result.to_string().contains(&String::from(config.token)));
}

#[cfg(unix)]
#[test]
fn lost_mutation_reply_keeps_original_administrative_intent_and_never_reconnects() {
    let (_root, config, listener) = endpoint();
    let expected = config.clone();
    let command = Command::AdministrativeClose {
        request: AdministrativeRequest {
            scope: ControlScopeId::new(),
            request_id: RequestId::new(),
            epoch: Epoch::new(),
            action: AdministrativeAction::Close {
                agent_id: AgentId::new(),
                revision: 5,
            },
        },
    };
    let exact = serde_json::to_value(&command).unwrap();
    let peer = std::thread::spawn(move || {
        let mut stream = accept(&listener);
        let _: Hello = read(&mut stream);
        write(
            &mut stream,
            &welcome(&expected, vec![Capability::AdministrativeClose]),
        );
        let request: Request = read(&mut stream);
        assert_eq!(serde_json::to_value(request.command).unwrap(), exact);
        drop(stream);
        listener
    });
    let result = run(config, command.clone(), Arc::new(AtomicBool::new(false)));
    let listener = peer.join().unwrap();
    listener.set_nonblocking(true).unwrap();
    assert!(
        listener.accept().is_err(),
        "no retry connection may be created"
    );
    assert_eq!(result["status"], "outcome_unknown");
    let Command::AdministrativeClose { request } = command else {
        unreachable!()
    };
    assert_eq!(
        result["reconcile"]["command"],
        json!(Command::AdministrativeReceipt { request })
    );
}

#[cfg(unix)]
#[test]
fn unavailable_capability_is_not_sent_and_unknown_query_is_not_a_mutation() {
    let (_root, config, listener) = endpoint();
    let expected = config.clone();
    let peer = std::thread::spawn(move || {
        use std::io::Read;
        let mut stream = accept(&listener);
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let _: Hello = read(&mut stream);
        write(&mut stream, &welcome(&expected, vec![Capability::Status]));
        assert_eq!(stream.read(&mut [0; 1]).unwrap(), 0);
    });
    let command = Command::Submit {
        agent_id: AgentId::new(),
        request_id: RequestId::new(),
        prompt: "not sent".into(),
    };
    assert_eq!(
        run(config, command, Arc::new(AtomicBool::new(false)))["status"],
        "not_sent"
    );
    peer.join().unwrap();
    let query = Command::AdministrativeReceipt {
        request: AdministrativeRequest {
            scope: ControlScopeId::new(),
            request_id: RequestId::new(),
            epoch: Epoch::new(),
            action: AdministrativeAction::Stop {},
        },
    };
    assert_eq!(
        client_error(
            &query,
            ClientError::QueryFailed(rpi_extcore::shared_host::Failure::Timeout)
        )["status"],
        "query_failed"
    );
}

#[cfg(unix)]
#[test]
fn private_configuration_rejects_symlink_hardlink_permissions_and_secret_echo() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let (root, _config, _listener) = endpoint();
    let file = root.path().join("c.json");
    let link = root.path().join("link");
    symlink(&file, &link).unwrap();
    assert!(crate::config::load(&link).is_err());
    std::fs::remove_file(&link).unwrap();
    std::fs::hard_link(&file, &link).unwrap();
    assert!(crate::config::load(&file).is_err());
    std::fs::remove_file(&link).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert!(crate::config::load(&file).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(
        &file,
        r#"{"token":"sensitive-marker-invalid-configuration"}"#,
    )
    .unwrap();
    let error = crate::config::load(&file).unwrap_err();
    assert!(!error.contains("sensitive-marker"));
    assert!(crate::config::load(std::path::Path::new("relative-config")).is_err());
}

#[test]
fn native_non_success_receipts_are_not_successful_tool_results() {
    let command = Command::AdministrativeClose {
        request: AdministrativeRequest {
            scope: ControlScopeId::new(),
            request_id: RequestId::new(),
            epoch: Epoch::new(),
            action: AdministrativeAction::Close {
                agent_id: AgentId::new(),
                revision: 2,
            },
        },
    };
    for state in [
        "prepared",
        "rejected",
        "uncertain",
        "interrupted",
        "future_unknown_state",
    ] {
        let data = json!({"receipt":{"status":{"state":state}}});
        let result = received(&command, data.clone());
        assert_eq!(result["error"], true);
        assert_eq!(result["status"], "native_not_succeeded");
        assert_eq!(result["data"], data);
        assert_eq!(
            result["reconcile"]["command"],
            json!(reconciliation(&command).unwrap())
        );
    }
    let succeeded = received(
        &command,
        json!({"receipt":{"status":{"state":"succeeded"}}}),
    );
    assert_eq!(succeeded["status"], "reply_received"); // Still not locally verified execution success.
}
