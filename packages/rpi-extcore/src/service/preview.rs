use super::*;
use serde::Serialize;

const PREVIEW_TIMEOUT: Duration = Duration::from_secs(3);
const PREVIEW_OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionPreview {
    session_id: String,
    messages: Vec<Value>,
    has_more: bool,
}

#[derive(PartialEq, Eq)]
struct PreviewRoute {
    project_id: String,
    project_root: PathBuf,
    created_at: u64,
    resource_revision: Option<u64>,
}

async fn preview_route(state: &Service, id: &str) -> ServiceResult<Option<PreviewRoute>> {
    if state.closing.load(std::sync::atomic::Ordering::Acquire) {
        return Ok(None);
    }
    // Follow list_sessions' lock order. These are metadata guards only: open
    // holds session_lifecycle during startup and must be free to race this read.
    let sessions = state.sessions.read().await;
    let registry = state.registry.read().await;
    retirement::check_session_admission(&registry, id)?;
    state
        .check_resource_storage(id)
        .map_err(resources::reply_error)?;
    let record = registry.resource_configuration(id);
    if record.is_some_and(crate::resources::ResourceConfiguration::blocked) {
        return Err(crate::reply::failure(
            ReplyKind::Conflict,
            "resource operation pending or blocked",
        ));
    }
    if sessions
        .get(id)
        .is_some_and(|host| host.is_shared_area() || !*host.closed_receiver().borrow())
    {
        return Ok(None);
    }
    let entry = registry
        .get(id)
        .ok_or_else(|| crate::reply::failure(ReplyKind::NotFound, "unknown session"))?;
    let project_id = entry
        .project_id
        .as_deref()
        .unwrap_or(&state.default_project_id);
    let projects = state.projects.read().await;
    let project = projects
        .entries
        .iter()
        .find(|project| project.id == project_id && !project.archived)
        .ok_or_else(|| {
            crate::reply::failure(ReplyKind::Unavailable, "session project is unavailable")
        })?;
    Ok(Some(PreviewRoute {
        project_id: project.id.clone(),
        project_root: project.root.clone(),
        created_at: entry.created_at,
        resource_revision: record.map(|record| record.revision),
    }))
}

/// Optional saved transcript for display while the client opens the live host.
/// Only the live snapshot supplies runtime status and an event sequence boundary.
pub async fn session_preview(state: &Service, id: String) -> ServiceReply {
    if uuid::Uuid::parse_str(&id).is_err() {
        return crate::reply::failure(ReplyKind::InvalidInput, "session ID must be a UUID")
            .into_reply();
    }
    let route = match preview_route(state, &id).await {
        Ok(Some(route)) => route,
        Ok(None) => return ReplyKind::Empty.into_reply(),
        Err(error) => return error.into_reply(),
    };
    let Ok(permit) = state.preview_workers.clone().try_acquire_owned() else {
        return ReplyKind::Empty.into_reply();
    };
    let bin = state.config.rpi_bin.clone();
    let configured = state.config.session_dir.clone();
    let project = route.project_root.clone();
    let native_id = id.clone();
    let read = tokio::task::spawn_blocking(move || {
        // The blocking owner retains its child and permit through cleanup even
        // if the HTTP caller navigates away or disconnects.
        let _permit = permit;
        let root = crate::run_timing::session_root(&project, configured).ok()?;
        let root = std::fs::canonicalize(root).ok()?;
        if !root.is_dir() {
            return None;
        }
        // Older native releases accept unknown long flags as extension
        // options. Probe the standalone help before invoking the reader.
        let started = std::time::Instant::now();
        let (ok, help, _) = crate::resources::local::invoke(
            &bin,
            &project,
            &["--help".into()],
            128 * 1024,
            PREVIEW_TIMEOUT,
        )
        .ok()?;
        if !ok
            || !std::str::from_utf8(&help)
                .ok()?
                .lines()
                .any(|line| line.split_whitespace().next() == Some("--session-preview"))
        {
            return None;
        }
        let remaining = PREVIEW_TIMEOUT.checked_sub(started.elapsed())?;
        let (ok, stdout, _) = crate::resources::local::invoke(
            &bin,
            &project,
            &[
                "--session-preview".into(),
                native_id.clone().into(),
                "--session-dir".into(),
                root.into_os_string(),
            ],
            PREVIEW_OUTPUT_LIMIT,
            remaining,
        )
        .ok()?;
        if !ok {
            return None;
        }
        let preview: SessionPreview = serde_json::from_slice(&stdout).ok()?;
        (preview.session_id == native_id && preview.messages.iter().all(Value::is_object))
            .then_some(preview)
    })
    .await;
    let Ok(Some(preview)) = read else {
        return ReplyKind::Empty.into_reply();
    };
    match preview_route(state, &id).await {
        Ok(Some(current)) if current == route => json!(preview).into_reply(),
        Ok(_) => ReplyKind::Empty.into_reply(),
        Err(error) => error.into_reply(),
    }
}
