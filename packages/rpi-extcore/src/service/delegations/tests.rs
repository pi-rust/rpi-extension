use super::*;
use crate::service::BackendConfig;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader};
struct Fixture {
    service: Arc<Service>,
    root: tempfile::TempDir,
    worker: tokio::task::JoinHandle<()>,
    requests: Arc<Mutex<Vec<Value>>>,
    override_data: Arc<Mutex<Option<Value>>>,
}
fn page(after: u64) -> Value {
    let events = if after == 0 {
        json!([{"seq":1,"event":{"type":"message_end","message":{"role":"assistant","content":"native child"}}}])
    } else {
        json!([])
    };
    json!({"version":1,"parentSessionId":"parent","childId":"child","epoch":"epoch",
        "retention":"volatile","earliestSeq":1,"lastSeq":1,"afterSeq":after,
        "nextSeq":1,"hasMore":false,"resyncRequired":false,"events":events})
}
impl Fixture {
    async fn new(capable: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let service = Service::new(BackendConfig {
            project: root.path().into(),
            registry: root.path().join("registry.json"),
            rpi_bin: root.path().join("must-not-launch"),
            ..Default::default()
        })
        .unwrap();
        let (writer, peer) = tokio::io::duplex(8192);
        let (host, replies) = HostSession::test_connection_with_ready(
            writer,
            16,
            json!({"sessionId":"parent",
            "childAgents":if capable {native_profile()} else {Value::Null},"supportedCommands":COMMANDS}),
        );
        service.sessions.write().await.insert("parent".into(), host);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let override_data = Arc::new(Mutex::new(None::<Value>));
        let seen = requests.clone();
        let data = override_data.clone();
        let worker = tokio::spawn(async move {
            let mut lines = BufReader::new(peer).lines();
            while let Some(line) = lines.next_line().await.unwrap() {
                let q: Value = serde_json::from_str(&line).unwrap();
                if q["type"] == "stop" {
                    break;
                }
                seen.lock().unwrap().push(q.clone());
                let result=data.lock().unwrap().clone().unwrap_or_else(|| match q["type"].as_str().unwrap() {
                    "get_child_agents"=>json!({"version":1,"parentSessionId":"parent","epoch":"epoch","retention":"volatile","children":[]}),
                    "get_child_agent_events"=>page(q["afterSeq"].as_u64().unwrap()),
                    "cancel_child_agent"=>json!({"version":1,"parentSessionId":"parent","epoch":"epoch","childId":"child","status":"cancelled"}),
                    _=>panic!("unexpected command"),
                });
                replies
                    .send(json!({"type":"response","id":q["id"],"success":true,"data":result}))
                    .unwrap();
            }
        });
        Self {
            service,
            root,
            worker,
            requests,
            override_data,
        }
    }
    async fn close(self) {
        self.service.shutdown().await;
        self.worker.abort();
        let _ = self.worker.await;
        drop(self.service);
        drop(self.root);
    }
}
fn query(after: u64) -> DelegationEventQuery {
    DelegationEventQuery {
        epoch: "epoch".into(),
        after_seq: after,
        limit: 32,
    }
}
#[tokio::test]
async fn queries_forward_parent_and_epoch_without_launching_or_replaying() {
    let f = Fixture::new(true).await;
    assert_eq!(roster(&f.service, "parent").await.unwrap().epoch, "epoch");
    let p = events(&f.service, "parent", "child", query(0))
        .await
        .unwrap();
    assert_eq!(p.next_seq, 1);
    assert_eq!(p.events[0].event["type"], "message_end");
    assert_eq!(
        cancel(
            &f.service,
            "parent",
            "child",
            DelegationCancelQuery {
                epoch: "epoch".into()
            }
        )
        .await
        .unwrap()
        .status,
        "cancelled"
    );
    let requests = f.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|q| q["parentSessionId"] == "parent" && q["version"] == 1));
    assert_eq!(requests[1]["afterSeq"], 0);
    assert_eq!(requests[2]["epoch"], "epoch");
    f.close().await;
}
#[tokio::test]
async fn unsupported_hosts_and_bad_limits_fail_before_native_requests() {
    let f = Fixture::new(false).await;
    assert_eq!(
        roster(&f.service, "parent").await.unwrap_err().0,
        ReplyKind::Unsupported
    );
    assert!(f.requests.lock().unwrap().is_empty());
    f.close().await;
    let f = Fixture::new(true).await;
    let mut q = query(0);
    q.limit = 33;
    assert_eq!(
        events(&f.service, "parent", "child", q)
            .await
            .unwrap_err()
            .0,
        ReplyKind::InvalidInput
    );
    assert!(f.requests.lock().unwrap().is_empty());
    f.close().await;
}
#[tokio::test]
async fn foreign_binding_bad_sequence_and_oversized_events_fail_closed() {
    let f = Fixture::new(true).await;
    for bad in [
        {
            let mut p = page(0);
            p["parentSessionId"] = json!("other");
            p
        },
        {
            let mut p = page(0);
            p["epoch"] = json!("stale");
            p
        },
        {
            let mut p = page(0);
            p["events"][0]["seq"] = json!(2);
            p
        },
        {
            let mut p = page(0);
            p["events"][0]["event"]["content"] = json!("x".repeat(32768));
            p
        },
    ] {
        *f.override_data.lock().unwrap() = Some(bad);
        assert_eq!(
            events(&f.service, "parent", "child", query(0))
                .await
                .unwrap_err()
                .0,
            ReplyKind::HostUnavailable
        );
    }
    f.close().await;
}
#[tokio::test]
async fn subscription_keeps_snapshot_cursor_and_stops_on_host_replacement() {
    let f = Fixture::new(true).await;
    let mut subscription = subscribe(&f.service, "parent", "child", query(0))
        .await
        .unwrap();
    let Some(DelegationStreamItem::Page { page }) = subscription.next().await else {
        panic!("missing initial native page")
    };
    assert_eq!(page.next_seq, 1);
    f.service.sessions.write().await.remove("parent");
    assert!(matches!(
        subscription.next().await,
        Some(DelegationStreamItem::ResyncRequired { .. })
    ));
    assert!(subscription.next().await.is_none());
    assert_eq!(f.requests.lock().unwrap().len(), 1);
    drop(subscription);
    f.close().await;
}
#[tokio::test]
async fn native_eviction_is_explicit_and_terminates_this_subscription() {
    let f = Fixture::new(true).await;
    let mut p = page(0);
    p["earliestSeq"] = json!(3);
    p["lastSeq"] = json!(3);
    p["nextSeq"] = json!(3);
    p["resyncRequired"] = json!(true);
    p["events"][0]["seq"] = json!(3);
    *f.override_data.lock().unwrap() = Some(p);
    let mut s = subscribe(&f.service, "parent", "child", query(0))
        .await
        .unwrap();
    assert!(
        matches!(s.next().await,Some(DelegationStreamItem::Page {page}) if page.resync_required)
    );
    assert!(s.next().await.is_none());
    drop(s);
    f.close().await;
}

#[tokio::test]
async fn replacing_host_before_initial_delivery_requires_resync() {
    let f = Fixture::new(true).await;
    let mut s = subscribe(&f.service, "parent", "child", query(0))
        .await
        .unwrap();
    f.service.sessions.write().await.remove("parent");
    assert!(matches!(
        s.next().await,
        Some(DelegationStreamItem::ResyncRequired { .. })
    ));
    assert!(s.next().await.is_none());
    drop(s);
    f.close().await;
}

#[tokio::test]
async fn roster_exposes_parent_tool_call_binding_and_rejects_invalid_ids() {
    let f = Fixture::new(true).await;
    let mut data = json!({"version":1,"parentSessionId":"parent","epoch":"epoch","retention":"volatile",
        "children":[{"childId":"child","status":"running","turns":0,"role":"worker","taskPreview":"task",
        "parentToolCallId":"call-42","startedMs":1,"finishedMs":null,"earliestSeq":1,"lastSeq":0}]});
    *f.override_data.lock().unwrap() = Some(data.clone());
    let observed = roster(&f.service, "parent").await.unwrap();
    assert_eq!(
        observed.children[0].parent_tool_call_id.as_deref(),
        Some("call-42")
    );
    assert_eq!(native_profile()["parentToolCallId"], true);
    data["children"][0]["parentToolCallId"] = json!("bad\ncall");
    *f.override_data.lock().unwrap() = Some(data);
    assert!(roster(&f.service, "parent").await.is_err());
    f.close().await;
}
