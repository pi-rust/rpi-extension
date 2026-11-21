#[test]
fn correlated_tracker_ignores_autonomous_progress_and_stale_settlements() {
    for kind in [RunKind::Prompt, RunKind::Compact] {
        let mut tracker = RunTracker::for_kind(json!("request"), kind);
        tracker.require_execution_identity(true);
        tracker.observe(
            &json!({"type":"response","id":"request","success":true,
            "data":{"accepted":true}}),
            false,
        );
        for event in [
            json!({"type":"execution_started","executionId":"external","requestId":null,"lane":"main"}),
            json!({"type":"agent_settled","executionId":"external","requestId":null,"lane":"main",
                "outcome":"completed","finalText":"wrong text"}),
            json!({"type":"message_update","executionId":"external","requestId":null,"lane":"main",
                "eventType":"text_delta","delta":"wrong text"}),
        ] {
            let progress = tracker.observe(&event, false);
            assert!(progress.outcome.is_none());
            assert!(progress.deltas.is_empty());
        }
        tracker.observe(
            &json!({"type":"execution_started","executionId":"current",
            "requestId":"request","lane":"main"}),
            false,
        );
        for execution in [json!("old"), Value::Null] {
            assert!(tracker
                .observe(
                    &json!({"type":"agent_settled","executionId":execution,
                "requestId":"request","lane":"main","outcome":"completed"}),
                    false
                )
                .outcome
                .is_none());
        }
        let terminal = json!({"type":"agent_settled","executionId":"current","requestId":"request",
            "lane":"main","outcome":if kind == RunKind::Compact {"declined"} else {"completed"}});
        assert_eq!(
            tracker.observe(&terminal, false).outcome,
            Some(Ok(RunOutcome::Completed))
        );
        assert!(
            tracker.observe(&terminal, false).outcome.is_none(),
            "terminal is idempotent"
        );
    }
}
