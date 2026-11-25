//! Thin command projection. No retries, request/epoch rewriting or Agent owner.
use rpi_extcore::shared_host::{protocol::*, ClientError, ConnectionConfig, HostClient};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: Command,
    #[serde(rename = "__rpi", default)]
    _context: Option<Value>,
}

pub fn parse(text: &str) -> Result<Command, &'static str> {
    if text.len() > MAX_FRAME_BYTES {
        return Err("Host tool arguments exceed 1 MiB");
    }
    let input: Input = serde_json::from_str(text).map_err(|_| "Invalid Host command; use the documented wire v1.5 shape, with explicit mutation request IDs")?;
    if !input.command.valid(Limits::default())
        || matches!(
            input.command,
            Command::Cancel { .. }
                | Command::Close { .. }
                | Command::Restore { .. }
                | Command::Stop { .. }
        )
    {
        return Err("Invalid or legacy Host command; use a complete guarded administrative intent");
    }
    Ok(input.command)
}

pub fn failure(status: &str, message: &str) -> Value {
    json!({"error":true, "status":status, "message":message})
}

pub fn reconciliation(command: &Command) -> Option<Command> {
    match command {
        Command::Submit {
            agent_id,
            request_id,
            ..
        } => Some(Command::Receipt {
            agent_id: *agent_id,
            request_id: *request_id,
        }),
        Command::AdministrativeCreate { request }
        | Command::AdministrativeRestore { request }
        | Command::AdministrativeClose { request }
        | Command::AdministrativeCancel { request }
        | Command::AdministrativeStop { request } => Some(Command::AdministrativeReceipt {
            request: request.clone(),
        }),
        _ => None,
    }
}

pub(crate) fn unknown(command: &Command, message: &str) -> Value {
    let mut result = failure("outcome_unknown", message);
    result["request_id"] = json!(command.request_id());
    result["reconcile"] = json!({"command":reconciliation(command)});
    result
}

fn client_error(command: &Command, error: ClientError) -> Value {
    match error {
        ClientError::OutcomeUnknown { .. } => unknown(command, &error.to_string()),
        ClientError::Rejected(ErrorCode::OutcomeUnknown | ErrorCode::PersistenceUncertain)
            if command.request_id().is_some() =>
        {
            unknown(command, &error.to_string())
        }
        ClientError::NotSent(_) => failure("not_sent", &error.to_string()),
        ClientError::QueryFailed(_) => failure("query_failed", &error.to_string()),
        ClientError::Rejected(code) => {
            let mut result = failure("rejected", &error.to_string());
            result["code"] = json!(code);
            result
        }
    }
}

fn received(command: &Command, data: Value) -> Value {
    // Keep the native projection intact, but never paint a known non-success
    // lifecycle receipt green just because its transport exchange succeeded.
    let native_state = data
        .pointer("/receipt/status/state")
        .and_then(Value::as_str);
    let error = if matches!(command, Command::Result { .. }) && data["found"] != false {
        !matches!(
            data.pointer("/result/state").and_then(Value::as_str),
            Some("admitted" | "running" | "completed")
        )
    } else {
        native_state.is_some_and(|state| state != "succeeded")
    };
    let mut result = json!({"error":error, "status":if error { "native_not_succeeded" } else { "reply_received" }, "data":data});
    if error {
        result["message"] = json!("Native receipt/result is not succeeded; inspect its state. Do not blindly retry or replace the request identity");
        let query = reconciliation(command).or_else(|| {
            matches!(
                command,
                Command::AdministrativeReceipt { .. } | Command::Result { .. }
            )
            .then(|| command.clone())
        });
        result["reconcile"] = json!({"command":query});
    }
    result
}

pub fn run(config: ConnectionConfig, command: Command, cancelled: Arc<AtomicBool>) -> Value {
    if cancelled.load(Ordering::Acquire) {
        return failure("not_sent", "Local request cancelled before connection");
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => return failure("not_sent", "Cannot create Host transport runtime"),
    };
    runtime.block_on(async {
        let exchange = async {
            let mut client = match HostClient::connect(config).await {
                Ok(client) => client,
                Err(error) => return client_error(&command, error),
            };
            let welcome = client.welcome().clone();
            let mut result = match client.call(command.clone()).await {
                // This is an authenticated transport reply, NOT a locally
                // verified lifecycle success, completed run or saved output.
                Ok(data) => received(&command, data),
                Err(error) => client_error(&command, error),
            };
            result["control"] = json!(welcome);
            result
        };
        tokio::select! {
            biased;
            _ = wait_cancelled(&cancelled) => {
                if command.request_id().is_some() {
                    unknown(&command, "Local request cancelled; remote work may continue. Query the original receipt; do not blindly retry")
                } else { failure("query_failed", "Local query cancelled") }
            }
            result = exchange => result,
        }
    })
    // Runtime destruction waits for its filesystem workers; the outer worker
    // cannot finish, release its slot, or be unmapped before that destruction.
}

async fn wait_cancelled(cancelled: &AtomicBool) {
    while !cancelled.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(test)]
pub(crate) mod tests;
