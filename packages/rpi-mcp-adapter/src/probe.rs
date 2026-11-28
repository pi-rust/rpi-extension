use crate::{Control, CoreRequestContext, McpRuntime};
use serde_json::{json, Value};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const PAGE_SIZE: usize = 8;

pub(crate) fn list(
    runtime: &McpRuntime,
    context: &CoreRequestContext,
    params: &Value,
    offset: usize,
    control: &Control,
) -> Result<Value, String> {
    let seconds = match params.get("timeoutSeconds") {
        None => 15,
        Some(value) => value
            .as_u64()
            .filter(|n| (1..=300).contains(n))
            .ok_or("timeoutSeconds must be an integer from 1 to 300")?,
    };
    let listed = runtime.list(context, params, control)?;
    let servers = listed["servers"]
        .as_array()
        .ok_or("MCP config returned no server list")?;
    let page: Vec<_> = servers
        .iter()
        .skip(offset)
        .take(PAGE_SIZE)
        .cloned()
        .collect();
    let deadline = Instant::now() + Duration::from_secs(seconds);

    let results = std::thread::scope(|scope| {
        let probe_control = Control::default();
        let (done, wait) = mpsc::channel::<()>();
        let timer_control = probe_control.clone();
        scope.spawn(move || loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if control.check().is_err() || remaining.is_zero() {
                timer_control.cancel();
                break;
            }
            match wait.recv_timeout(remaining.min(Duration::from_millis(20))) {
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                _ => break,
            }
        });

        let jobs: Vec<_> = page
            .into_iter()
            .map(|mut server| {
                let probe_control = probe_control.clone();
                scope.spawn(move || {
                    if server["enabled"] != true {
                        server["status"] = json!("disabled");
                        return server;
                    }
                    let target =
                        json!({"configPath":params["configPath"],"server":server["server"]});
                    match runtime.list(context, &target, &probe_control) {
                        Ok(result) => {
                            server["status"] = json!("ready");
                            server["toolCount"] =
                                json!(result["tools"].as_array().map_or(0, Vec::len));
                        }
                        Err(error) => {
                            server["status"] = json!("failed");
                            let (reason, stderr) = error
                                .split_once("\nMCP server stderr:\n")
                                .unwrap_or((&error, ""));
                            server["error"] =
                                if Instant::now() >= deadline && probe_control.check().is_err() {
                                    json!(format!("MCP probe timed out after {seconds} seconds"))
                                } else {
                                    json!(first_line(reason))
                                };
                            if !stderr.is_empty() {
                                server["stderr"] = json!(first_line(stderr));
                            }
                        }
                    }
                    server
                })
            })
            .collect();
        let results: Vec<_> = jobs
            .into_iter()
            .map(|job| job.join().expect("MCP probe worker panicked"))
            .collect();
        // Dropping the sender wakes the timer even when every probe succeeded early.
        drop(done);
        results
    });

    control.check()?;
    let mut result = json!({"servers":results});
    if offset + PAGE_SIZE < servers.len() {
        result["nextOffset"] = json!(offset + PAGE_SIZE);
    }
    Ok(result)
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(160)
        .collect()
}
