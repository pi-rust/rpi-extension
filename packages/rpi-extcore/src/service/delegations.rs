//! Native-owned delegated-child observations. These volatile tails are not
//! independent sessions or replacement transcripts. No call opens a host.
use super::{
    tool_program_access::{self, ChildAccess},
    tool_program_queries::{valid_id, Counter},
    Service,
};
use crate::{
    host::HostSession,
    reply::{failure, ReplyKind, ServiceResult},
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, time::Duration};

const PAGE_BYTES: usize = 131072;
const COMMANDS: [&str; 3] = [
    "get_child_agents",
    "get_child_agent_events",
    "cancel_child_agent",
];

/// Exact native profile; legacy hosts must not be probed optimistically.
pub fn native_profile() -> Value {
    json!({"version":1,"listCommand":COMMANDS[0],"eventsCommand":COMMANDS[1],
        "cancelCommand":COMMANDS[2],"retention":"volatile","maxChildren":32,
        "maxEvents":256,"maxEventBytes":32768,"maxPageCount":32,
        "maxResponseBytes":PAGE_BYTES,"sequence":"childEpochSeq-v1"})
}
/// Parent-scoped metadata, including the retained native sequence window.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Delegation {
    pub child_id: String,
    pub status: DelegationStatus,
    pub turns: u32,
    pub role: String,
    pub task_preview: String,
    pub started_ms: u64,
    pub finished_ms: Option<u64>,
    pub earliest_seq: u64,
    pub last_seq: u64,
}
/// Native execution state, not parent-run settlement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DelegationStatus {
    Running,
    Done,
    Failed,
}
/// Bounded retained roster. Eviction and process restart remove observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationRoster {
    pub version: u32,
    pub parent_session_id: String,
    pub epoch: String,
    pub retention: String,
    pub children: Vec<Delegation>,
}
/// A cursor is meaningful only for this native epoch, parent and child.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationEventQuery {
    pub epoch: String,
    pub after_seq: u64,
    pub limit: usize,
}
/// Native compact wire event; large events have an explicit observation_gap.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationEvent {
    pub seq: u64,
    pub event: Value,
}
/// One bounded native page. resync_required reports evicted observations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationEventPage {
    pub version: u32,
    pub parent_session_id: String,
    pub epoch: String,
    pub child_id: String,
    pub retention: String,
    pub earliest_seq: u64,
    pub last_seq: u64,
    pub after_seq: u64,
    pub next_seq: u64,
    pub has_more: bool,
    pub resync_required: bool,
    pub events: Vec<DelegationEvent>,
}
/// Mutation bound to the roster epoch; stale IDs cannot cancel a new host.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationCancelQuery {
    pub epoch: String,
}
/// Cancellation acknowledgement is not proof of OS-process cleanup completion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DelegationCancelResult {
    pub version: u32,
    pub parent_session_id: String,
    pub epoch: String,
    pub child_id: String,
    pub status: String,
}
fn invalid() -> crate::reply::ServiceError {
    failure(ReplyKind::InvalidInput, "invalid bounded delegation query")
}
fn malformed() -> crate::reply::ServiceError {
    failure(
        ReplyKind::HostUnavailable,
        "invalid native delegation response",
    )
}
fn ensure(ok: bool) -> ServiceResult<()> {
    if ok {
        Ok(())
    } else {
        Err(malformed())
    }
}
fn supported(host: &HostSession) -> bool {
    host.ready()["childAgents"] == native_profile()
        && COMMANDS.iter().all(|c| host.supports_command(c))
}
async fn access(state: &Service, parent: &str) -> ServiceResult<ChildAccess> {
    if !valid_id(parent) {
        return Err(invalid());
    }
    let access = ChildAccess::for_service(state, parent.into()).await?;
    if !supported(&access.host) {
        return Err(failure(
            ReplyKind::Unsupported,
            "native delegated child observations are unsupported",
        ));
    }
    check(&access).await?;
    Ok(access)
}
async fn check(access: &ChildAccess) -> ServiceResult<()> {
    access.check_binding().await?;
    tool_program_access::check_identity(&access.host, &access.id).await?;
    tool_program_access::read_health(&access.host).await
}
async fn request<T: DeserializeOwned>(
    access: &ChildAccess,
    command: &str,
    mut payload: Value,
) -> ServiceResult<T> {
    check(access).await?;
    payload["type"] = json!(command);
    payload["version"] = json!(1);
    payload["parentSessionId"] = json!(access.id);
    payload["id"] = json!(uuid::Uuid::new_v4().to_string());
    let response = access
        .host
        .send_timeout(payload, Duration::from_secs(10))
        .await
        .map_err(|_| {
            failure(
                ReplyKind::HostUnavailable,
                "native delegation request failed",
            )
        })?;
    check(access).await?;
    if response["success"] == false {
        return Err(failure(
            ReplyKind::Conflict,
            "native delegation unavailable or cursor expired; reload roster",
        ));
    }
    ensure(response["success"] == true)?;
    let data = response.get("data").ok_or_else(malformed)?;
    serde_json::to_writer(
        &mut Counter {
            bytes: 0,
            max: PAGE_BYTES,
        },
        data,
    )
    .map_err(|_| malformed())?;
    serde_json::from_value(data.clone()).map_err(|_| malformed())
}
fn binding(
    version: u32,
    parent: &str,
    epoch: &str,
    retention: &str,
    expected: &str,
) -> ServiceResult<()> {
    ensure(version == 1 && parent == expected && valid_id(epoch) && retention == "volatile")
}
/// List only native observations retained for an already live parent host.
pub async fn roster(state: &Service, parent: &str) -> ServiceResult<DelegationRoster> {
    let access = access(state, parent).await?;
    let roster: DelegationRoster = request(&access, COMMANDS[0], json!({})).await?;
    binding(
        roster.version,
        &roster.parent_session_id,
        &roster.epoch,
        &roster.retention,
        parent,
    )?;
    ensure(roster.children.len() <= 32)?;
    let mut ids = HashSet::new();
    for child in &roster.children {
        ensure(
            valid_id(&child.child_id)
                && ids.insert(&child.child_id)
                && child.turns <= 32
                && child.role.len() <= 200
                && child.task_preview.chars().count() <= 200
                && child.earliest_seq <= child.last_seq.saturating_add(1)
                && child.finished_ms.map_or(true, |t| t >= child.started_ms),
        )?;
    }
    Ok(roster)
}
fn validate_query(child: &str, query: &DelegationEventQuery) -> ServiceResult<()> {
    if valid_id(child) && valid_id(&query.epoch) && (1..=32).contains(&query.limit) {
        Ok(())
    } else {
        Err(invalid())
    }
}
fn validate_page(
    parent: &str,
    child: &str,
    q: &DelegationEventQuery,
    p: &DelegationEventPage,
) -> ServiceResult<()> {
    binding(
        p.version,
        &p.parent_session_id,
        &p.epoch,
        &p.retention,
        parent,
    )?;
    ensure(
        p.epoch == q.epoch
            && p.child_id == child
            && p.after_seq == q.after_seq
            && p.earliest_seq <= p.last_seq.saturating_add(1)
            && p.events.len() <= q.limit
            && p.after_seq <= p.last_seq
            && p.next_seq <= p.last_seq
            && p.resync_required == (q.after_seq.saturating_add(1) < p.earliest_seq),
    )?;
    let mut next = q.after_seq;
    let mut expected = q.after_seq.saturating_add(1).max(p.earliest_seq);
    for e in &p.events {
        ensure(
            e.seq == expected
                && e.seq <= p.last_seq
                && e.event["type"].as_str().is_some_and(valid_id)
                && serde_json::to_vec(&e.event).is_ok_and(|b| b.len() <= 32768),
        )?;
        next = e.seq;
        expected = expected.saturating_add(1);
    }
    ensure(
        p.next_seq == next
            && p.has_more == (next < p.last_seq)
            && (!p.has_more || !p.events.is_empty()),
    )
}
async fn read_events(
    access: &ChildAccess,
    child: &str,
    query: &DelegationEventQuery,
) -> ServiceResult<DelegationEventPage> {
    validate_query(child, query)?;
    let mut payload = serde_json::to_value(query).map_err(|_| invalid())?;
    payload["childId"] = json!(child);
    let page = request(access, COMMANDS[1], payload).await?;
    validate_page(&access.id, child, query, &page)?;
    Ok(page)
}
/// Read a resumable native event page without polling or opening a host.
pub async fn events(
    state: &Service,
    parent: &str,
    child: &str,
    query: DelegationEventQuery,
) -> ServiceResult<DelegationEventPage> {
    validate_query(child, &query)?;
    read_events(&access(state, parent).await?, child, &query).await
}
/// Ask the native owner to cancel one child; never kill a process in an adapter.
pub async fn cancel(
    state: &Service,
    parent: &str,
    child: &str,
    query: DelegationCancelQuery,
) -> ServiceResult<DelegationCancelResult> {
    if !valid_id(child) || !valid_id(&query.epoch) {
        return Err(invalid());
    }
    let access = access(state, parent).await?;
    let result: DelegationCancelResult = request(
        &access,
        COMMANDS[2],
        json!({"epoch":query.epoch,"childId":child}),
    )
    .await?;
    ensure(
        result.version == 1
            && result.parent_session_id == parent
            && result.child_id == child
            && result.epoch == query.epoch
            && ["cancelled", "already-finished"].contains(&result.status.as_str()),
    )?;
    Ok(result)
}
/// Neutral stream output; adapters choose the wire event name.
#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DelegationStreamItem {
    Page { page: DelegationEventPage },
    ResyncRequired { code: &'static str },
}
/// Drop cancels the polling future; no detached task or secondary store exists.
pub struct DelegationSubscription {
    access: ChildAccess,
    child: String,
    query: DelegationEventQuery,
    initial: Option<DelegationEventPage>,
    stopped: bool,
}
impl DelegationSubscription {
    /// Yield bounded pages; idle waits do not advance the native cursor.
    pub async fn next(&mut self) -> Option<DelegationStreamItem> {
        if self.stopped {
            return None;
        }
        if check(&self.access).await.is_err() {
            self.stopped = true;
            return Some(DelegationStreamItem::ResyncRequired {
                code: "native_delegation_unavailable",
            });
        }
        loop {
            let page = match self.initial.take() {
                Some(page) => Ok(page),
                None => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    read_events(&self.access, &self.child, &self.query).await
                }
            };
            match page {
                Ok(page) => {
                    self.query.after_seq = page.next_seq;
                    self.stopped = page.resync_required;
                    if !page.events.is_empty() || page.resync_required {
                        return Some(DelegationStreamItem::Page { page });
                    }
                }
                Err(_) => {
                    self.stopped = true;
                    return Some(DelegationStreamItem::ResyncRequired {
                        code: "native_delegation_unavailable",
                    });
                }
            }
        }
    }
}
/// Establish the native cursor before live polling, so no handoff event is lost.
pub async fn subscribe(
    state: &Service,
    parent: &str,
    child: &str,
    query: DelegationEventQuery,
) -> ServiceResult<DelegationSubscription> {
    validate_query(child, &query)?;
    let access = access(state, parent).await?;
    let page = read_events(&access, child, &query).await?;
    Ok(DelegationSubscription {
        access,
        child: child.into(),
        query,
        initial: Some(page),
        stopped: false,
    })
}

#[cfg(test)]
mod tests;
