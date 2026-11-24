//! Cross-repository test against pi-rust's production dispatcher with FauxProvider.
//! RPI_ADMISSION_NATIVE_FIXTURE must name the explicitly built, trusted
//! execution_admission_fixture example. No PATH lookup or real provider.
use rpi_extcore::host::HostSession;
use serde_json::json;
use std::{path::PathBuf, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires pi-rust execution_admission_fixture; see native admission docs"]
async fn native_autonomous_execution_and_noop_resume_allow_the_next_prompt() {
    let bin = PathBuf::from(
        std::env::var_os("RPI_ADMISSION_NATIVE_FIXTURE")
            .expect("explicit trusted fixture required"),
    );
    assert!(bin.is_absolute() && bin.is_file());
    let root = std::env::temp_dir().join(format!("admission-native-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let host = HostSession::spawn(
        bin,
        root.clone(),
        Some(root.join("sessions")),
        &uuid::Uuid::new_v4().to_string(),
        root.join("timings"),
    )
    .await
    .unwrap();
    let scenario_host = host.clone();
    let mut scenario = tokio::spawn(async move {
        let host = scenario_host;
        assert_eq!(
            host.ready()["semantics"]["executionLifecycle"],
            "correlated-v1",
            "the parent must opt in before the child's ready handshake"
        );
        let resumed = host
            .command("resume_pending", json!({}), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(resumed["resumed"], false);
        let mut run = host.begin_prompt("first".into()).await.unwrap();
        let mut own = None;
        let mut autonomous = None;
        let mut completed = false;
        while own.is_none() || autonomous.is_none() || !completed {
            let event = run.events.recv().await.unwrap();
            if event["type"] == "agent_settled" {
                assert_eq!(event["outcome"], "completed");
                let id = event["executionId"].as_str().unwrap().to_string();
                if event["requestId"].is_null() {
                    assert_eq!(event["finalText"], "autonomous plugin reply");
                    autonomous = Some(id);
                } else {
                    assert_eq!(event["finalText"], "first RPC reply");
                    own = Some(id);
                }
            }
            if let Some(outcome) = run.tracker.observe(&event, false).outcome {
                assert!(outcome.is_ok(), "{outcome:?}");
                completed = true;
            }
        }
        assert_ne!(own, autonomous);
        drop(run);
        let mut projected = host.subscribe();
        let mut next = host.begin_prompt("next".into()).await.unwrap();
        loop {
            let event = next.events.recv().await.unwrap();
            if let Some(outcome) = next.tracker.observe(&event, false).outcome {
                assert!(outcome.is_ok(), "{outcome:?}");
                assert_eq!(event["finalText"], "next RPC reply");
                break;
            }
        }
        loop {
            let event = projected.recv().await.unwrap();
            if event.value["type"] == "agent_settled" {
                break;
            }
        }
        assert_eq!(host.snapshot().await.status, "ready");
    });
    let result = tokio::time::timeout(Duration::from_secs(20), &mut scenario).await;
    if result.is_err() {
        scenario.abort();
        let _ = scenario.await;
    }
    host.shutdown().await;
    std::fs::remove_dir_all(root).unwrap();
    result
        .expect("native/extcore admission integration timed out")
        .expect("integration scenario failed");
}
