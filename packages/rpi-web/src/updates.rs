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

#[derive(Clone)]
pub(crate) struct UpdateState {
    config: Arc<AppConfig>,
    cache: Arc<Mutex<Option<Value>>>,
    tasks: Arc<Mutex<HashMap<String, UpdateTask>>>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateTask {
    state: &'static str,
    target: String,
    message: String,
    log: String,
    restart_required: bool,
}

#[derive(Debug, Deserialize)]
struct CheckQuery {
    #[serde(default)]
    force: bool,
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
    let task_id = Uuid::new_v4().to_string();
    {
        let mut tasks = state.tasks.lock().expect("update task store poisoned");
        tasks.insert(
            task_id.clone(),
            UpdateTask {
                state: "running",
                target: request.target.clone(),
                message: "更新正在运行…".into(),
                log: String::new(),
                restart_required: false,
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
        }
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
        Some(task) => Ok(Json(serde_json::to_value(task).unwrap_or_else(|_| json!({})))),
        None => Err((
            axum::http::StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown update task" })),
        )),
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
    let mut response = stale.unwrap_or_else(|| {
        json!({
            "host": { "name": "rpi", "updatable": false, "latest": null },
            "extensions": []
        })
    });
    if let Some(object) = response.as_object_mut() {
        object.insert("error".into(), json!(error));
        let proxy = saved_proxy();
        object.insert("proxyConfigured".into(), json!(proxy.is_some()));
        object.insert("proxy".into(), json!(proxy));
    }
    response
}

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
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid `rpi {}` output: {error}", args.join(" ")))
}

async fn run_cli_command(
    state: &Arc<UpdateState>,
    args: &[String],
) -> Result<std::process::Output, String> {
    let mut command = tokio::process::Command::new(&state.config.rpi_bin);
    command.args(args).current_dir(&state.config.project);
    if let Some(proxy) = saved_proxy() {
        command.env("HTTP_PROXY", &proxy).env("HTTPS_PROXY", &proxy);
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
    std::fs::write(&path, bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
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
    if rest.contains('/') {
        return Err("请填写代理监听地址，不要填写订阅链接".into());
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
    use super::validate_proxy;

    #[test]
    fn proxy_validation_accepts_http_and_https_and_rejects_subscription_urls() {
        assert!(validate_proxy("http://127.0.0.1:7890").is_ok());
        assert!(validate_proxy("https://proxy.example.com").is_ok());
        assert!(validate_proxy("socks5://127.0.0.1:1080").is_err());
        assert!(validate_proxy("https://example.com/sub.yaml").is_err());
        assert!(validate_proxy("127.0.0.1:7890").is_err());
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
