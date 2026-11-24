#[cfg(unix)]
#[tokio::test]
async fn native_launch_opts_in_per_child_without_requiring_new_native_flags() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("rpi-lifecycle-opt-in-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let script = root.join("host");
    // An old-host stand-in ignores the opt-in and advertises no lifecycle
    // capability. It also demonstrates that no unsupported CLI flag is needed.
    std::fs::write(&script, r#"#!/bin/sh
printf '{"type":"ready","protocol":"rpi","childOptIn":"%s"}\n' "$RPI_RPC_EXECUTION_LIFECYCLE"
exec cat >/dev/null
"#).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let session = HostSession::spawn(script, root.clone(), None, "test", root.join("timings"))
        .await.unwrap();
    let ready = session.ready();
    session.terminate().await;
    std::fs::remove_dir_all(root).unwrap();
    assert_eq!(ready["childOptIn"], "correlated-v1");
    assert!(ready["semantics"]["executionLifecycle"].is_null());
}

#[tokio::test]
async fn correlated_projection_keeps_successor_busy_and_ignores_stale_settlement() {
    let (writer, _peer) = tokio::io::duplex(4096);
    let (session, events) = HostSession::test_connection_with_ready(
        writer,
        32,
        json!({"semantics":{"executionLifecycle":"correlated-v1"}}),
    );
    for id in ["old", "current"] {
        events
            .send_projected(json!({"type":"execution_started","executionId":id,
            "requestId":null,"lane":"main"}))
            .await;
    }
    assert_eq!(session.snapshot().await.status, "running");
    events
        .send_projected(json!({"type":"agent_settled","executionId":"old",
        "requestId":null,"lane":"main","outcome":"completed"}))
        .await;
    assert_eq!(session.snapshot().await.status, "running");
    assert!(session.runtime.state.read().await.resource_unsettled);
    let fence = session.resource_fence().unwrap();
    assert!(session.verify_resource_idle(&fence).await.is_err());
    drop(fence);
    assert!(session
        .begin_prompt("blocked successor".into())
        .await
        .is_err());
    let previous = session.snapshot().await.latest_settlement;
    events
        .send_projected(json!({"type":"agent_settled","executionId":"stale",
        "requestId":null,"lane":"main","outcome":"failed","error":"stale error"}))
        .await;
    let snapshot = session.snapshot().await;
    assert_eq!(snapshot.status, "running");
    assert_eq!(snapshot.latest_settlement, previous);
    events
        .send_projected(json!({"type":"agent_settled","executionId":"current",
        "requestId":null,"lane":"main","outcome":"completed"}))
        .await;
    assert_eq!(session.snapshot().await.status, "ready");
    assert!(!session.runtime.state.read().await.resource_unsettled);
    session.terminate().await;
}
