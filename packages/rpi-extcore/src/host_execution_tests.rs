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
