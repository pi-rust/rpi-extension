//! Required explicit native artifact. Only this integration fixture launches
//! one foreground daemon; shared_host::HostClient itself never spawns anything.
//! No DLL/Provider/Agent execution is claimed by this read-only cross-repo test.
#![cfg(unix)]
use rpi_extcore::shared_host::{
    protocol::{
        AdministrativeAction, AdministrativeRequest, AuthToken, Capability, Command,
        ControlScopeId, ErrorCode, RequestId, Welcome,
    },
    ClientError, ConnectionConfig, Failure, HostClient,
};
use serde_json::{json, Value};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command as ProcessCommand,
};
const DEADLINE: Duration = Duration::from_secs(15);

#[tokio::test]
#[ignore = "requires RPI_SHARED_HOST_BINARY pointing to the explicitly built shared-Host rpi; no fallback or fixture skip"]
async fn shared_client_auth_queries_detach_and_reconnect_to_explicit_real_daemon() {
    let binary = PathBuf::from(
        std::env::var_os("RPI_SHARED_HOST_BINARY").expect("RPI_SHARED_HOST_BINARY is required"),
    );
    assert!(
        binary.is_absolute() && binary.is_file(),
        "explicit native artifact must be an absolute regular executable path"
    );
    let root = tempfile::Builder::new().prefix("he").tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = root.path().join("s");
    let host_id = uuid::Uuid::new_v4().to_string().try_into().unwrap();
    let token = AuthToken::new(uuid::Uuid::new_v4().simple().to_string()).unwrap();
    let config_path = root.path().join("c.json");
    let administrative_scope = ControlScopeId::new();
    let mut private_config = json!({"directory":root.path().join("d"), "socket_path":socket, "host_id":host_id, "token":token});
    std::fs::write(&config_path, private_config.to_string()).unwrap();
    std::fs::set_permissions(&config_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let config = ConnectionConfig {
        socket_path: socket.clone(),
        host_id,
        token: token.clone(),
        timeout: DEADLINE,
    };
    let mut previous_epoch = None;
    for administrative in [false, true] {
        if administrative {
            // A fixed owner policy, default-deny; no peer-supplied grant.
            private_config["administration"] = json!({"scope": administrative_scope});
            std::fs::write(&config_path, private_config.to_string()).unwrap();
        }
        let mut child = ProcessCommand::new(&binary)
            .args(["host", "serve", "--config"])
            .arg(&config_path)
            .env_clear()
            .env("HOME", root.path().join("unused-home"))
            .current_dir(root.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("explicit native artifact must start");
        let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(DEADLINE, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("native daemon must publish readiness, not skip or fall back");
        assert!(!line.contains(&String::from(token.clone())));
        let ready: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(ready["event"], "host_ready");
        let welcome: Welcome = serde_json::from_value(ready["control"].clone()).unwrap();
        let mut expected = vec![
            Capability::Status,
            Capability::List,
            Capability::Inspect,
            Capability::Receipts,
            Capability::Results,
            Capability::Submit,
        ];
        if administrative {
            expected.extend([
                Capability::Profiles,
                Capability::AdministrativeReceipts,
                Capability::AdministrativeCreate,
                Capability::AdministrativeRestore,
                Capability::AdministrativeClose,
                Capability::AdministrativeCancel,
                Capability::AdministrativeStop,
            ]);
        }
        assert_eq!(welcome.capabilities, expected);
        assert_ne!(Some(welcome.epoch), previous_epoch);
        previous_epoch = Some(welcome.epoch);
        let mut client = HostClient::connect(config.clone()).await.unwrap();
        assert_eq!(client.welcome().epoch, welcome.epoch);
        assert_eq!(
            client.call(Command::Status {}).await.unwrap()["registered"],
            0
        );
        assert_eq!(
            client
                .call(Command::List {
                    after: None,
                    limit: 1
                })
                .await
                .unwrap()["agents"],
            json!([])
        );
        assert_eq!(
            client
                .call(Command::Inspect {
                    agent_id: uuid::Uuid::new_v4().to_string().try_into().unwrap()
                })
                .await
                .unwrap_err(),
            ClientError::Rejected(ErrorCode::NotFound)
        );
        // Receipts are now a real, agent-scoped read. The unloaded Agent is
        // NotFound and the query never loads a session or creates admission.
        assert_eq!(
            client
                .call(Command::Receipt {
                    agent_id: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
                    request_id: RequestId::new(),
                })
                .await
                .unwrap_err(),
            ClientError::Rejected(ErrorCode::NotFound)
        );
        // Submit is dispatched, but an unknown Agent is NotFound: the daemon
        // never loads a Session or admits input for it.
        assert_eq!(
            client
                .call(Command::Submit {
                    agent_id: uuid::Uuid::new_v4().to_string().try_into().unwrap(),
                    request_id: RequestId::new(),
                    prompt: "no such agent".into(),
                })
                .await
                .unwrap_err(),
            ClientError::Rejected(ErrorCode::NotFound)
        );
        assert_eq!(
            client
                .call(Command::Stop {
                    request_id: RequestId::new()
                })
                .await
                .unwrap_err(),
            ClientError::NotSent(Failure::UnsupportedCapability)
        );
        let administrative_query = Command::AdministrativeReceipt {
            request: AdministrativeRequest {
                scope: administrative_scope,
                request_id: RequestId::new(),
                epoch: welcome.epoch,
                action: AdministrativeAction::Stop {},
            },
        };
        let agent_id = rpi_extcore::shared_host::protocol::AgentId::new();
        let profile = rpi_extcore::shared_host::protocol::ProfileRef {
            agent_id,
            version: rpi_extcore::shared_host::protocol::ProfileVersion::new(),
        };
        for action in [
            AdministrativeAction::CreateProfile { profile },
            AdministrativeAction::RestoreProfile {
                profile,
                revision: 1,
            },
            AdministrativeAction::Close {
                agent_id,
                revision: 1,
            },
            AdministrativeAction::CancelSubmitted {
                agent_id,
                revision: 1,
                submitted_request_id: RequestId::new(),
            },
            AdministrativeAction::Stop {},
        ] {
            let request = AdministrativeRequest {
                scope: administrative_scope,
                request_id: RequestId::new(),
                epoch: welcome.epoch,
                action,
            };
            let command = match request.action {
                AdministrativeAction::CreateProfile { .. } => {
                    Command::AdministrativeCreate { request }
                }
                AdministrativeAction::RestoreProfile { .. } => {
                    Command::AdministrativeRestore { request }
                }
                AdministrativeAction::Close { .. } => Command::AdministrativeClose { request },
                AdministrativeAction::CancelSubmitted { .. } => {
                    Command::AdministrativeCancel { request }
                }
                _ => Command::AdministrativeStop { request },
            };
            let expected = if administrative {
                ClientError::Rejected(ErrorCode::Unauthorized)
            } else {
                ClientError::NotSent(Failure::UnsupportedCapability)
            };
            assert_eq!(client.call(command).await.unwrap_err(), expected);
        }
        if administrative {
            let profiles = client
                .call(Command::Profiles {
                    after: None,
                    limit: 1,
                })
                .await
                .unwrap();
            assert_eq!(
                profiles,
                json!({"scope": administrative_scope, "profiles": [], "next_cursor": null})
            );
            assert_eq!(
                client.call(administrative_query).await.unwrap_err(),
                ClientError::Rejected(ErrorCode::Unauthorized)
            );
        } else {
            assert_eq!(
                client
                    .call(Command::Profiles {
                        after: None,
                        limit: 1
                    })
                    .await
                    .unwrap_err(),
                ClientError::NotSent(Failure::UnsupportedCapability)
            );
            assert_eq!(
                client.call(administrative_query).await.unwrap_err(),
                ClientError::NotSent(Failure::UnsupportedCapability)
            );
        }
        assert_eq!(
            client.call(Command::Status {}).await.unwrap()["registered"],
            0
        );
        let mut bad = config.clone();
        bad.token = AuthToken::new(uuid::Uuid::new_v4().simple().to_string()).unwrap();
        assert!(HostClient::connect(bad).await.is_err());
        client.detach();
        drop(client);
        assert!(child.try_wait().unwrap().is_none());
        let mut reattached = HostClient::connect(config.clone()).await.unwrap();
        reattached.call(Command::Status {}).await.unwrap();
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let output = tokio::time::timeout(DEADLINE, child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!socket.exists());
        assert!(reattached.call(Command::Status {}).await.is_err());
        assert!(!root.path().join("unused-home").exists());
        assert!(!root.path().join(".rpi").exists());
        assert!(!root.path().join("sessions").exists());
    }
}
