//! Retained session retirement owners. Transport cancellation must not release
//! lifecycle exclusion while native close or filesystem deletion is in flight.
use super::*;

pub(super) fn deleting_error() -> (ReplyKind, Value) {
    (
        ReplyKind::Conflict,
        json!({"error":"Session deletion is pending; retry DELETE", "code":"session_deleting"}),
    )
}

impl Service {
    /// Call under session_lifecycle before any same-ID native writer admission.
    /// Resource configuration adapters must use this gate too, not live_session
    /// filtering: treating a deleting host as absent could spawn a replacement.
    pub(crate) async fn check_session_admission(&self, id: &str) -> ServiceResult<()> {
        let registry = self.registry.read().await;
        check_session_admission(&registry, id)
    }
}

pub(super) fn check_session_admission(registry: &Registry, id: &str) -> ServiceResult<()> {
    if registry.deletion(id).is_some() {
        return Err(deleting_error());
    }
    if registry.preferences(id).archived {
        return Err((
            ReplyKind::Conflict,
            json!({"error":"Session is archived; restore it before opening", "code":"session_archived"}),
        ));
    }
    Ok(())
}

fn owner_error() -> (ReplyKind, Value) {
    (
        ReplyKind::Unavailable,
        json!({"error":"Session retirement worker failed", "code":"session_retirement_unavailable"}),
    )
}

async fn idle_close(state: &Service, id: &str) -> ServiceResult<()> {
    let session = state.sessions.read().await.get(id).cloned();
    if let Some(session) = session {
        if session.is_shared_area() {
            return Err((
                ReplyKind::Conflict,
                json!({"error":"Session lifetime belongs to its native Host area", "code":"shared_host_session"}),
            ));
        }
        session.shutdown_if_idle().await.map_err(|error| match error {
            crate::host::IdleCloseError::Busy => (ReplyKind::Conflict, json!({"error":"Session is busy", "code":"session_busy"})),
            crate::host::IdleCloseError::Unavailable(_) => (ReplyKind::Unavailable, json!({"error":"Session idle state could not be confirmed", "code":"session_idle_unknown"})),
        })?;
        // The host has confirmed child exit, not merely accepted stop.
        state.sessions.write().await.remove(id);
        state.session_activity.write().await.remove(id);
    }
    Ok(())
}

/// Retire live native hosts that have not received a Web/API request for the
/// configured timeout. The session registry and native transcript are kept;
/// only the in-memory host is released.
pub(super) async fn reap_idle(state: &Service, timeout: Duration) {
    let now = Instant::now();
    let candidates = state
        .session_activity
        .read()
        .await
        .iter()
        .filter(|(_, last)| now.duration_since(**last) >= timeout)
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return;
    }
    // Do not make ordinary requests wait behind a long native idle probe.
    let Ok(_lifecycle) = state.session_lifecycle.try_lock() else {
        return;
    };
    for id in candidates {
        if state.closing.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let expired = state
            .session_activity
            .read()
            .await
            .get(&id)
            .is_some_and(|last| last.elapsed() >= timeout);
        if !expired
            || state
                .live_session(&id)
                .await
                .is_some_and(|session| session.is_shared_area())
        {
            continue;
        }
        match idle_close(state, &id).await {
            Ok(()) => {}
            // A session can stay `awaiting_input` indefinitely when a
            // `ui_dialog_request` (ask_user/permission gate) is never
            // answered: the native run lease is still held, so the ordinary
            // idle proof above correctly returns Busy and the host process is
            // never released. After the full idle budget has elapsed with no
            // request at all, cancel those dialogs through the normal command
            // path instead of killing the run. The plugin then settles the tool
            // call, the run returns to `ready`, and a later reaper pass closes
            // the host with the existing idle proof. Never touch a `running`
            // session: `cancel_stale_dialogs` is a no-op unless the projection
            // says `awaiting_input` with pending dialogs.
            Err((ReplyKind::Conflict, _)) => {
                let Some(session) = state.sessions.read().await.get(&id).cloned() else {
                    continue;
                };
                // Only a session that is actually waiting on a projected
                // dialog is eligible. `cancel_stale_dialogs` itself is a no-op
                // for other states, but checking here keeps ordinary Busy
                // sessions (active run, held lease) off this path entirely.
                let snapshot = session.snapshot().await;
                if snapshot.status != "awaiting_input" || snapshot.pending_dialogs.is_empty() {
                    continue;
                }
                if session.cancel_stale_dialogs().await.is_err() {
                    // Back off when the cancellation itself could not be
                    // delivered; retrying every tick against a broken native
                    // host would just spin.
                    state
                        .session_activity
                        .write()
                        .await
                        .insert(id, Instant::now());
                }
            }
            // Unknown native state is backed off so a broken host cannot
            // consume the reaper.
            Err((ReplyKind::Unavailable, _)) => {
                state
                    .session_activity
                    .write()
                    .await
                    .insert(id, Instant::now());
            }
            Err(_) => {}
        }
    }
}

pub(super) async fn metadata(
    state: &Service,
    id: String,
    mut request: MetadataRequest,
) -> (ReplyKind, Value) {
    // Reject malformed metadata before probing or closing a live host.
    if let Some(tags) = request.tags.take() {
        request.tags = match crate::registry::normalize_tags(tags) {
            Ok(tags) => Some(tags),
            Err(_) => {
                return (
                    ReplyKind::InvalidInput,
                    json!({"error":"Invalid session tags", "code":"invalid_session_tags"}),
                )
            }
        };
    }
    let permit = match state.retirement_workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return (
                ReplyKind::Conflict,
                json!({"error":"Session retirement is at capacity; try again later", "code":"session_retirement_capacity"}),
            )
        }
    };
    let owner = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        let _lifecycle = owner.session_lifecycle.lock().await;
        if owner.closing.load(std::sync::atomic::Ordering::Acquire) {
            return owner_error();
        }
        {
            let registry = owner.registry.read().await;
            if registry.get(&id).is_none() {
                return (ReplyKind::NotFound, json!({"error":"session not found"}));
            }
            if registry.deletion(&id).is_some() {
                return deleting_error();
            }
        }
        if request.archived == Some(true) {
            if let Err(error) = idle_close(&owner, &id).await {
                return error;
            }
        }
        // Restore and ordinary metadata updates never spawn native processes.
        let mut registry = owner.registry.write().await;
        match registry.update_preferences(&id, request.pinned, request.archived, request.tags) {
            Ok(prefs) => (ReplyKind::Success, json!({"sessionId":id,"pinned":prefs.pinned,"archived":prefs.archived,"tags":prefs.tags})),
            Err(_) => (ReplyKind::Internal, json!({"error":"Session preferences could not be saved"})),
        }
    }).await.unwrap_or_else(|_| owner_error())
}

fn bulk_skip(id: &str, error: (ReplyKind, Value)) -> Value {
    let mut entry = serde_json::Map::new();
    entry.insert("sessionId".into(), json!(id));
    entry.insert(
        "code".into(),
        json!(match error.1.get("code").and_then(Value::as_str) {
            Some(code) => code.to_string(),
            None => match error.0 {
                ReplyKind::NotFound => "session_not_found".to_string(),
                ReplyKind::Conflict => "session_conflict".to_string(),
                ReplyKind::Unavailable => "session_unavailable".to_string(),
                _ => "session_metadata_failed".to_string(),
            },
        }),
    );
    entry.insert(
        "error".into(),
        error
            .1
            .get("error")
            .cloned()
            .unwrap_or_else(|| json!("Session metadata could not be updated")),
    );
    Value::Object(entry)
}

pub(super) async fn bulk_metadata(
    state: &Service,
    mut request: super::BulkMetadataRequest,
) -> (ReplyKind, Value) {
    if request.session_ids.is_empty() {
        return (
            ReplyKind::InvalidInput,
            json!({"error":"sessionIds must not be empty", "code":"invalid_session_ids"}),
        );
    }
    if request.session_ids.len() > super::MAX_BULK_METADATA_SESSIONS {
        return (
            ReplyKind::InvalidInput,
            json!({"error":"Too many sessions in one metadata request", "code":"bulk_metadata_limit"}),
        );
    }
    let mut seen = std::collections::HashSet::with_capacity(request.session_ids.len());
    request.session_ids.retain(|id| seen.insert(id.clone()));
    if let Some(tags) = request.patch.tags.take() {
        request.patch.tags = match crate::registry::normalize_tags(tags) {
            Ok(tags) => Some(tags),
            Err(_) => {
                return (
                    ReplyKind::InvalidInput,
                    json!({"error":"Invalid session tags", "code":"invalid_session_tags"}),
                )
            }
        };
    }
    let owner = state.clone();
    let ids = request.session_ids;
    let patch = request.patch;
    tokio::spawn(async move {
        let _lifecycle = owner.session_lifecycle.lock().await;
        if owner.closing.load(std::sync::atomic::Ordering::Acquire) {
            return owner_error();
        }
        let mut updated_ids = Vec::with_capacity(ids.len());
        let mut updated = Vec::with_capacity(ids.len());
        let mut skipped = Vec::new();
        let mut skipped_ids = std::collections::HashSet::new();
        {
            let registry = owner.registry.read().await;
            for id in &ids {
                let skip = if registry.get(id).is_none() {
                    Some(bulk_skip(
                        id,
                        (ReplyKind::NotFound, json!({"error":"session not found"})),
                    ))
                } else if registry.deletion(id).is_some() {
                    Some(bulk_skip(id, deleting_error()))
                } else {
                    None
                };
                if let Some(entry) = skip {
                    skipped_ids.insert(id.clone());
                    skipped.push(entry);
                }
            }
        }
        if patch.archived == Some(true) {
            let close_ids: Vec<String> = ids
                .iter()
                .filter(|id| !skipped_ids.contains(*id))
                .cloned()
                .collect();
            for id in close_ids {
                if let Err(error) = idle_close(&owner, &id).await {
                    skipped_ids.insert(id.clone());
                    skipped.push(bulk_skip(&id, error));
                }
            }
        }
        let candidates: Vec<String> = ids
            .iter()
            .filter(|id| !skipped_ids.contains(*id))
            .cloned()
            .collect();
        if candidates.is_empty() {
            return (
                ReplyKind::Success,
                json!({"updated":[], "updatedSessions":[], "skipped":skipped}),
            );
        }
        let prefs = {
            let mut registry = owner.registry.write().await;
            match registry.update_preferences_bulk(
                &candidates,
                patch.pinned,
                patch.archived,
                patch.tags,
            ) {
                Ok(prefs) => prefs,
                Err(_) => {
                    return (
                        ReplyKind::Internal,
                        json!({"error":"Session preferences could not be saved"}),
                    )
                }
            }
        };
        for (id, prefs) in candidates.into_iter().zip(prefs) {
            updated_ids.push(id.clone());
            updated.push(json!({
                "sessionId": id,
                "pinned": prefs.pinned,
                "archived": prefs.archived,
                "tags": prefs.tags,
            }));
        }
        (
            ReplyKind::Success,
            json!({"updated":updated_ids, "updatedSessions":updated, "skipped":skipped}),
        )
    })
    .await
    .unwrap_or_else(|_| owner_error())
}

pub(super) async fn delete(state: &Service, id: String) -> ServiceReply {
    let permit = match state.retirement_workers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return (ReplyKind::Conflict, json!({"error":"Session retirement is at capacity; try again later", "code":"session_retirement_capacity"})).into_reply(),
    };
    let owner = state.clone();
    tokio::spawn(async move {
        let _permit = permit;
        delete_owned(&owner, id).await
    })
    .await
    .unwrap_or_else(|_| owner_error().into_reply())
}

async fn delete_owned(state: &Service, id: String) -> ServiceReply {
    let _lifecycle = state.session_lifecycle.lock().await;
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return owner_error().into_reply();
    }
    let pending = state.registry.read().await.deletion(&id).cloned();
    let execution = if let Some(plan) = pending {
        // Exact persisted manifest only. Never probe/spawn native or rediscover
        // a new plan after a partial removal or service restart.
        tokio::task::spawn_blocking(move || crate::deletion::execute(&plan))
    } else {
        let live = state.sessions.read().await.get(&id).cloned();
        let remembered = state.registry.read().await.get(&id).is_some();
        if !remembered && live.is_none() {
            return (
                ReplyKind::NotFound,
                json!({"error":"unknown session", "code":"session_not_found"}),
            )
                .into_reply();
        }
        if let Err(error) = idle_close(state, &id).await {
            return error.into_reply();
        }
        // Normally publication remembers every live host. Retain the legacy
        // live-only deletion contract without weakening begin_deletion's
        // require-known invariant or changing metadata on a Busy rejection.
        if !remembered {
            let snapshot = live
                .as_ref()
                .expect("known or live checked")
                .snapshot()
                .await;
            if state
                .registry
                .write()
                .await
                .upsert(RegistryEntry {
                    session_id: id.clone(),
                    project_id: Some(state.default_project_id.clone()),
                    title: snapshot.title,
                    parent_session_id: None,
                    fork_entry_id: None,
                    relation: None,
                    created_at: snapshot.created_at,
                    updated_at: snapshot.updated_at,
                    last_turn_ended_at: None,
                })
                .is_err()
            {
                return (ReplyKind::Unavailable, json!({"error":"Session deletion intent could not be saved", "code":"session_deletion_storage"})).into_reply();
            }
        }
        let project = match session_project(state, &id).await {
            Ok(project) => project,
            Err(_) => {
                return (
                    ReplyKind::Unavailable,
                    json!({"error":"Session project is unavailable"}),
                )
                    .into_reply()
            }
        };
        let configured = state.config.session_dir.clone();
        let timings = state.config.registry.with_extension("tool-timings");
        let preparing_id = id.clone();
        let prepared = match tokio::task::spawn_blocking(move || {
            let root = crate::run_timing::session_root(&project.root, configured)?;
            crate::deletion::prepare_locked(&root, &preparing_id, &timings)
        })
        .await
        {
            Ok(Ok(plan)) => plan,
            Ok(Err(error)) => return error.into_reply(),
            Err(_) => return owner_error().into_reply(),
        };
        if state
            .registry
            .write()
            .await
            .begin_deletion(&id, prepared.plan().clone())
            .is_err()
        {
            return (ReplyKind::Unavailable, json!({"error":"Session deletion intent could not be saved", "code":"session_deletion_storage"})).into_reply();
        }
        // The retained owner holds the native lock through registry persistence;
        // move the same lock into execution, without a release/reacquire gap.
        tokio::task::spawn_blocking(move || crate::deletion::execute_prepared(prepared))
    };
    match execution.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return error.into_reply(),
        Err(_) => return owner_error().into_reply(),
    }
    // Registry failure retains the exact manifest, so DELETE can safely retry.
    if state.registry.write().await.remove(&id).is_err() {
        return (ReplyKind::Unavailable, json!({"error":"Session deletion completion could not be saved", "code":"session_deletion_storage"})).into_reply();
    }
    ReplyKind::Empty.into_reply()
}
