//! Update discovery and installation endpoints for the web UI.
//!
//! The native CLI owns release checking and installation. This module only
//! exposes those operations to the browser, stores the web-only proxy setting
//! in the shared `settings.json`, and keeps short-lived task state for an
//! install that runs in the background.

use axum::{
    extract::{Query, State},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

use crate::server::AppConfig;

const CHECK_CACHE_TTL_MS: u64 = 30 * 60 * 1000;
const MAX_LOG_CHARS: usize = 8_000;
/// Finished tasks stay queryable for this long so a page that polls slowly
/// still observes the outcome, then the map stops growing.
const TASK_RETENTION_MS: u64 = 10 * 60 * 1000;
const MAX_TASKS: usize = 32;

#[derive(Clone)]
pub(crate) struct UpdateState {
    config: Arc<AppConfig>,
    cache: Arc<Mutex<Option<Value>>>,
    tasks: Arc<Mutex<HashMap<String, UpdateTask>>>,
    /// One install at a time: `rpi update` replaces a running binary and the
    /// extension installer rewrites the installed cdylib, so a second
    /// concurrent invocation would race the first on the same files.
    installing: Arc<Mutex<bool>>,
    /// One check at a time. Two tabs opening together, or a "Check now" during
    /// the startup check, would otherwise each spawn a network-bound CLI.
    checking: Arc<tokio::sync::Mutex<()>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateTask {
    state: &'static str,
    target: String,
    message: String,
    log: String,
    restart_required: bool,
    /// Unix ms when the task reached a terminal state; `None` while running.
    #[serde(skip)]
    finished_at: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct CheckQuery {
    /// The Web client sends `?force=1`, while an API caller may send `true`.
    /// serde's `bool` accepts only `true`/`false`, so both spellings are read
    /// here instead of rejecting the refresh outright.
    #[serde(default, deserialize_with = "query_flag")]
    force: bool,
}

fn query_flag<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = String::deserialize(deserializer)?;
    Ok(matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    ))
}

#[derive(Debug, Deserialize)]
struct ProxyRequest {
    /// Empty or absent clears the saved proxy.
    proxy: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InstallRequest {
    target: String,
}

#[derive(Debug, Deserialize)]
struct StatusQuery {
    task_id: String,
}

pub(crate) fn routes(config: Arc<AppConfig>) -> Router {
    let state = Arc::new(UpdateState {
        config,
        cache: Arc::new(Mutex::new(None)),
        tasks: Arc::new(Mutex::new(HashMap::new())),
        installing: Arc::new(Mutex::new(false)),
        checking: Arc::new(tokio::sync::Mutex::new(())),
    });
    Router::new()
        .route("/api/v1/updates/check", get(check))
        .route("/api/v1/updates/proxy", post(set_proxy))
        .route("/api/v1/updates/install", post(install))
        .route("/api/v1/updates/status", get(status))
        .with_state(state)
}

async fn check(
    State(state): State<Arc<UpdateState>>,
    Query(query): Query<CheckQuery>,
) -> Json<Value> {
    if !query.force {
        if let Some(cached) = cached_inventory(&state) {
            return Json(cached);
        }
    }
    // Serialize checks, then re-read the cache: a concurrent request that just
    // finished populates it, so the second caller reuses that answer instead of
    // spawning another CLI.
    let _checking = state.checking.clone().lock_owned().await;
    if let Some(cached) = cached_inventory(&state) {
        return Json(cached);
    }
    let refresh = run_cli_json(&state, &["updates", "refresh", "--json"]).await;
    let (inventory, mut error) = match refresh {
        Ok(value) => (Some(value), None),
        Err(message) => {
            let cached = run_cli_json(&state, &["updates", "check", "--json"]).await;
            match cached {
                Ok(value) => (
                    Some(value),
                    Some(format!("Could not refresh update information: {message}")),
                ),
                Err(cached_error) => {
                    let stale = cached_inventory(&state);
                    return Json(unavailable_response(
                        stale,
                        format!("{message}; cached check also failed: {cached_error}"),
                    ));
                }
            }
        }
    };
    let mut response = inventory.unwrap_or_else(|| json!({}));
    normalize_inventory(&mut response);
    if let Some(object) = response.as_object_mut() {
        object.insert("checkedAt".into(), json!(now_ms()));
        if let Some(message) = error.take() {
            object.insert("error".into(), json!(message));
        }
        let proxy = saved_proxy();
        object.insert("proxyConfigured".into(), json!(proxy.is_some()));
        object.insert("proxy".into(), json!(proxy));
    }
    remember_inventory(&state, &response);
    Json(response)
}

/// Guarantee the shape the Web client reads.
///
/// A CLI that implements only part of the inventory, or any other JSON object
/// on stdout, would otherwise reach the browser without `host` or
/// `extensions`, and the settings panel reads both.
fn normalize_inventory(value: &mut Value) {
    if !value.is_object() {
        *value = json!({});
    }
    let object = value
        .as_object_mut()
        .expect("normalize_inventory guarantees an object");
    if !object.get("host").is_some_and(Value::is_object) {
        object.insert(
            "host".into(),
            json!({ "name": "rpi", "current": "", "updatable": false }),
        );
    }
    if !object.get("extensions").is_some_and(Value::is_array) {
        object.insert("extensions".into(), json!([]));
    }
}

async fn set_proxy(
    State(_state): State<Arc<UpdateState>>,
    Json(request): Json<ProxyRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let proxy = request
        .proxy
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    if let Some(value) = &proxy {
        if let Err(message) = validate_proxy(value) {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": message })),
            ));
        }
    }
    if let Err(message) = write_saved_proxy(proxy.as_deref()) {
        return Err((
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": message })),
        ));
    }
    Ok(Json(json!({
        "proxy": proxy,
        "proxyConfigured": proxy.is_some(),
    })))
}

async fn install(
    State(state): State<Arc<UpdateState>>,
    Json(request): Json<InstallRequest>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let args: &[&str] = match request.target.as_str() {
        "host" => &["update"],
        "extension" => &["extension", "update", "rpi-web"],
        other => {
            return Err((
                axum::http::StatusCode::BAD_REQUEST,
                Json(json!({ "error": format!("unsupported update target `{other}`") })),
            ))
        }
    };
    {
        let mut installing = state.installing.lock().expect("update install gate");
        if *installing {
            return Err((
                axum::http::StatusCode::CONFLICT,
                Json(json!({ "error": "an update is already running" })),
            ));
        }
        *installing = true;
    }
    let task_id = Uuid::new_v4().to_string();
    {
        let mut tasks = state.tasks.lock().expect("update task store poisoned");
        evict_tasks(&mut tasks);
        tasks.insert(
            task_id.clone(),
            UpdateTask {
                state: "running",
                target: request.target.clone(),
                message: "更新正在运行…".into(),
                log: String::new(),
                restart_required: false,
                finished_at: None,
            },
        );
    }
    let runtime = state.clone();
    let owned_args: Vec<String> = args.iter().map(|value| (*value).to_owned()).collect();
    let owned_id = task_id.clone();
    tokio::spawn(async move {
        let output = run_cli_command(&runtime, &owned_args).await;
        let (state_name, message, log) = match output {
            Ok(output) if output.status.success() => (
                "succeeded",
                "更新已完成，请重启 rpi 宿主后生效。".to_owned(),
                combined_log(&output.stdout, &output.stderr),
            ),
            Ok(output) => (
                "failed",
                format!("更新失败，退出码 {}", output.status),
                combined_log(&output.stdout, &output.stderr),
            ),
            Err(message) => ("failed", message, String::new()),
        };
        if let Some(task) = runtime
            .tasks
            .lock()
            .expect("update task store poisoned")
            .get_mut(&owned_id)
        {
            task.state = state_name;
            task.message = message;
            task.log = log;
            task.restart_required = state_name == "succeeded";
            task.finished_at = Some(now_ms());
        }
        // Release the gate before the next install may start.
        *runtime.installing.lock().expect("update install gate") = false;
    });
    Ok(Json(json!({ "taskId": task_id })))
}

async fn status(
    State(state): State<Arc<UpdateState>>,
    Query(query): Query<StatusQuery>,
) -> Result<Json<Value>, (axum::http::StatusCode, Json<Value>)> {
    let task = state
        .tasks
        .lock()
        .expect("update task store poisoned")
        .get(&query.task_id)
        .cloned();
    match task {
        Some(task) => Ok(Json(
            serde_json::to_value(task).unwrap_or_else(|_| json!({})),
        )),
        None => Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown update task" })),
        )),
    }
}

/// Bound the task map: drop expired finished tasks, then the oldest finished
/// ones if an unusual burst still exceeds the cap. Running tasks are never
/// removed, so a caller that already holds an id keeps its status.
fn evict_tasks(tasks: &mut HashMap<String, UpdateTask>) {
    let now = now_ms();
    tasks.retain(|_, task| match task.finished_at {
        Some(finished) => now.saturating_sub(finished) < TASK_RETENTION_MS,
        None => true,
    });
    if tasks.len() < MAX_TASKS {
        return;
    }
    let mut finished: Vec<(u64, String)> = tasks
        .iter()
        .filter_map(|(id, task)| task.finished_at.map(|at| (at, id.clone())))
        .collect();
    finished.sort();
    for (_, id) in finished
        .into_iter()
        .take(tasks.len().saturating_sub(MAX_TASKS) + 1)
    {
        tasks.remove(&id);
    }
}

fn cached_inventory(state: &Arc<UpdateState>) -> Option<Value> {
    let cache = state.cache.lock().expect("update cache poisoned");
    let value = cache.as_ref()?.clone();
    let checked_at = value
        .get("checkedAt")
        .and_then(Value::as_u64)
        .unwrap_or_default();
    let fresh = now_ms().saturating_sub(checked_at) < CHECK_CACHE_TTL_MS;
    fresh.then_some(value)
}

fn remember_inventory(state: &Arc<UpdateState>, value: &Value) {
    let mut cache = state.cache.lock().expect("update cache poisoned");
    *cache = Some(value.clone());
}

fn unavailable_response(stale: Option<Value>, error: String) -> Value {
    let mut response = stale.unwrap_or_else(|| json!({}));
    normalize_inventory(&mut response);
    if let Some(object) = response.as_object_mut() {
        object.insert("error".into(), json!(error));
        let proxy = saved_proxy();
        object.insert("proxyConfigured".into(), json!(proxy.is_some()));
        object.insert("proxy".into(), json!(proxy));
    }
    response
}

/// Run an inventory command and require a JSON object.
///
/// A native CLI without the `updates` subcommand does not fail: the argument
/// falls through to the prompt path, which answers with prose and exit 0. An
/// object check is therefore the only honest support test — anything else is
/// reported as an unsupported binary instead of surfacing model text as an
/// update inventory.
async fn run_cli_json(state: &Arc<UpdateState>, args: &[&str]) -> Result<Value, String> {
    let owned_args: Vec<String> = args.iter().map(|value| (*value).to_owned()).collect();
    let output = run_cli_command(state, &owned_args).await?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("`rpi {}` exited with {}", args.join(" "), output.status)
        } else {
            stderr
        });
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        format!(
            "`rpi {}` did not return JSON; this rpi build has no update inventory ({error})",
            args.join(" ")
        )
    })?;
    if !value.is_object() {
        return Err(format!(
            "`rpi {}` did not return a JSON object; this rpi build has no update inventory",
            args.join(" ")
        ));
    }
    Ok(value)
}

async fn run_cli_command(
    state: &Arc<UpdateState>,
    args: &[String],
) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new(&state.config.rpi_bin);
    command.args(args).current_dir(&state.config.project);
    if let Some(proxy) = saved_proxy() {
        // An explicit shell proxy wins, matching the native CLI's own rule.
        for key in ["HTTP_PROXY", "HTTPS_PROXY"] {
            if std::env::var_os(key).is_none() {
                command.env(key, &proxy);
            }
        }
    }
    command
        .output()
        .await
        .map_err(|error| format!("could not run `rpi {}`: {error}", args.join(" ")))
}

fn combined_log(stdout: &[u8], stderr: &[u8]) -> String {
    let mut log = String::new();
    for bytes in [stdout, stderr] {
        let text = String::from_utf8_lossy(bytes);
        if text.trim().is_empty() {
            continue;
        }
        if !log.is_empty() {
            log.push('\n');
        }
        log.push_str(text.trim());
    }
    if log.chars().count() > MAX_LOG_CHARS {
        log = log.chars().take(MAX_LOG_CHARS).collect();
        log.push_str("\n… output truncated");
    }
    log
}

fn agent_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(crate::config::AGENT_DIR_ENV) {
        return Some(PathBuf::from(dir));
    }
    let home = if cfg!(windows) {
        std::env::var_os("USERPROFILE")
    } else {
        std::env::var_os("HOME")
    }?;
    Some(PathBuf::from(home).join(".rpi").join("agent"))
}

fn settings_path() -> Result<PathBuf, String> {
    agent_dir()
        .map(|dir| dir.join("settings.json"))
        .ok_or_else(|| "could not resolve the rpi agent directory".to_owned())
}

fn saved_proxy() -> Option<String> {
    let path = settings_path().ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value
        .get("httpProxy")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn write_saved_proxy(proxy: Option<&str>) -> Result<(), String> {
    let path = settings_path()?;
    let mut value = match std::fs::read_to_string(&path) {
        Ok(text) if text.trim().is_empty() => json!({}),
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let object = value
        .as_object_mut()
        .ok_or_else(|| format!("{} must contain a JSON object", path.display()))?;
    match proxy {
        Some(proxy) => {
            object.insert("httpProxy".into(), json!(proxy));
        }
        None => {
            object.remove("httpProxy");
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("cannot serialize settings: {error}"))?;
    write_atomically(&path, &bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Replace `path` without ever leaving a truncated settings file.
///
/// The native CLI reads this file at startup; a plain `fs::write` that is
/// interrupted, or that races the CLI's own save, would hand it half a JSON
/// document. Write a sibling temp file, then rename over the target.
fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "settings.json".into());
    let temp = path.with_file_name(format!(
        ".{file_name}.{}.tmp",
        uuid::Uuid::new_v4().simple()
    ));
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // The native file is private; keep the replacement private too.
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        use std::io::Write as _;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            Err(error)
        }
    }
}

fn validate_proxy(value: &str) -> Result<(), String> {
    let (scheme, rest) = value
        .split_once("://")
        .ok_or_else(|| "代理地址需要包含 http:// 或 https://".to_owned())?;
    if !matches!(scheme, "http" | "https") {
        return Err("仅支持 http:// 或 https:// 代理地址".into());
    }
    if rest.is_empty() || rest.starts_with('/') {
        return Err("代理地址缺少主机名".into());
    }
    if rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return Err("请填写代理监听地址，不要填写订阅链接".into());
    }
    // The native CLI exports this value as HTTP_PROXY/HTTPS_PROXY, so control
    // characters or whitespace would corrupt the child environment.
    if rest
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '@' || c == '\\')
    {
        return Err("代理地址包含非法字符".into());
    }
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{normalize_inventory, validate_proxy, write_atomically};
    use serde_json::json;

    #[test]
    fn proxy_validation_accepts_http_and_https_and_rejects_subscription_urls() {
        assert!(validate_proxy("http://127.0.0.1:7890").is_ok());
        assert!(validate_proxy("https://proxy.example.com").is_ok());
        assert!(validate_proxy("socks5://127.0.0.1:1080").is_err());
        assert!(validate_proxy("https://example.com/sub.yaml").is_err());
        assert!(validate_proxy("127.0.0.1:7890").is_err());
        // A subscription link often carries a query string too.
        assert!(validate_proxy("https://example.com/api?token=1").is_err());
        // Credential-bearing URLs and control characters must not reach the
        // child process environment as HTTP_PROXY.
        assert!(validate_proxy("http://user:pass@proxy:8080").is_err());
        assert!(validate_proxy("http://proxy:8080\nHTTPS_PROXY=x").is_err());
    }

    #[test]
    fn force_is_parsed_from_the_query_string_the_client_sends() {
        use axum::extract::Query;
        // The client asks for a forced refresh with `?force=1`, while a bare
        // `/updates/check` must stay a cached read.
        let forced: Query<super::CheckQuery> = Query::try_from_uri(&"/?force=1".parse().unwrap())
            .expect("the client's force flag must parse");
        assert!(forced.0.force);
        let cached: Query<super::CheckQuery> = Query::try_from_uri(&"/".parse().unwrap()).unwrap();
        assert!(!cached.0.force);
    }

    #[test]
    fn a_partial_inventory_still_answers_with_the_fields_the_client_reads() {
        // An older CLI that answers `{}`, or any JSON object without the
        // inventory fields, must not reach the panel with a missing `host`.
        for mut value in [
            json!({}),
            json!({"host": null}),
            json!({"extensions": "no"}),
        ] {
            normalize_inventory(&mut value);
            assert!(value["host"].is_object(), "{value}");
            assert!(value["extensions"].is_array(), "{value}");
            assert_eq!(value["host"]["updatable"], false, "{value}");
        }
        // A well-formed inventory keeps every value the CLI reported.
        let mut value = json!({
            "host": {"name": "rpi", "current": "0.31.0", "latest": "0.32.0", "updatable": true},
            "extensions": [{"name": "rpi-web", "current": "0.1.95", "updatable": true}]
        });
        normalize_inventory(&mut value);
        assert_eq!(value["host"]["latest"], "0.32.0");
        assert_eq!(value["extensions"][0]["name"], "rpi-web");
    }

    #[test]
    fn atomic_settings_write_replaces_the_target_and_leaves_no_temp_file() {
        let root =
            std::env::temp_dir().join(format!("rpi-web-updates-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        std::fs::write(&path, b"{\"theme\":\"dark\"}").unwrap();
        write_atomically(&path, b"{\"theme\":\"light\"}").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"theme\":\"light\"}"
        );
        let names: Vec<String> = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["settings.json".to_string()]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn combined_log_skips_empty_streams_and_truncates_long_output() {
        let log = super::combined_log(b"stdout\n", b"stderr\n");
        assert!(log.contains("stdout"));
        assert!(log.contains("stderr"));
        let long = vec![b'x'; 9_000];
        let log = super::combined_log(&long, &[]);
        assert!(log.contains("output truncated"));
        assert!(log.chars().count() < 9_000);
    }
}
