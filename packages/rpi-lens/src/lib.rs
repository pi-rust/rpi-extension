#![deny(unsafe_code)]

use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

mod kit;

use crate::kit::optional_string;
use serde_json::Value;
#[cfg(windows)]
use wait_timeout::ChildExt;

#[cfg(windows)]
fn wait_for_child(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    child.wait_timeout(timeout)
}

// wait-timeout installs a process-wide SIGCHLD handler on Unix. Polling the
// owned child keeps this image from leaving a signal callback behind on unload.
#[cfg(not(windows))]
fn wait_for_child(child: &mut Child, timeout: Duration) -> std::io::Result<Option<ExitStatus>> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let status = child.try_wait()?;
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if status.is_some() || remaining.is_zero() {
            return Ok(status);
        }
        std::thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

fn code_lens(params: &Value) -> Result<String, String> {
    let check = optional_string(params, "check", "diff");
    let (program, args): (&str, &[&str]) = match check.as_str() {
        "diff" => ("git", &["diff", "--check"]),
        "cargo" => ("cargo", &["check"]),
        "fmt" => ("cargo", &["fmt", "--", "--check"]),
        "clippy" => ("cargo", &["clippy", "--", "-D", "warnings"]),
        _ => return Err("check must be one of: diff, cargo, fmt, clippy".into()),
    };
    let timeout = params
        .get("timeoutSeconds")
        .and_then(Value::as_u64)
        .unwrap_or(60)
        .clamp(1, 120);
    let mut command = Command::new(program);
    command
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let session_cwd = params[rpi_plugin_sdk::HOST_CONTEXT_KEY]["cwd"]
        .as_str()
        .filter(|value| !value.is_empty());
    let requested_cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let Some(cwd) = requested_cwd.or(session_cwd) {
        let cwd = std::path::Path::new(cwd);
        match session_cwd.filter(|_| cwd.is_relative()) {
            Some(base) => command.current_dir(std::path::Path::new(base).join(cwd)),
            None => command.current_dir(cwd),
        };
    }
    let mut child = command
        .spawn()
        .map_err(|err| format!("unable to run {program}: {err}"))?;
    let wait_error = match wait_for_child(&mut child, Duration::from_secs(timeout)) {
        Ok(Some(_)) => None,
        Ok(None) => Some(format!("{check} timed out after {timeout}s")),
        Err(err) => Some(format!("failed while waiting for {check}: {err}")),
    };
    if let Some(message) = wait_error {
        let _ = child.kill();
        let _ = child.wait();
        return Err(message);
    }
    let output = child
        .wait_with_output()
        .map_err(|err| format!("failed to collect {check} output: {err}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if text.len() > 40_000 {
        let mut end = 40_000;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[truncated]");
    }
    Ok(format!(
        "check={check} exit={}\n{}",
        output.status.code().unwrap_or(-1),
        text.trim()
    ))
}

const DESCRIPTION: &str = "Run one local diagnostic, not arbitrary shell commands or automatic fixes. Requires git/cargo (and rustfmt/clippy when selected) on PATH and a suitable repository/project directory. Defaults to git diff --check in the host session workspace when available. Use cwd to select another directory. Returns check name, exit code and stdout followed by stderr, capped after collection at 40,000 bytes plus a truncation marker; a nonzero exit is diagnostic output, not a tool error. Spawn/wait/collection errors and timeout fail the tool. Cargo commands may run build scripts, access the network and write build/cache artifacts; no tests are run. Timeout kills only the direct child, not its descendants; output is buffered, not streamed.";

const PARAMETERS: &str = r#"{
  "type": "object",
  "additionalProperties": true,
  "properties": {
    "check": {
      "type": "string",
      "enum": ["diff", "cargo", "fmt", "clippy"],
      "default": "diff",
      "description": "Optional diagnostic selector; defaults to diff. diff runs git diff --check (unstaged tracked changes only; not a full diff or test suite); cargo runs cargo check; fmt runs cargo fmt -- --check (reports formatting without applying it); clippy runs cargo clippy -- -D warnings. No arbitrary command, flags, shell expansion or automatic fixing. Selected executables/components must be installed."
    },
    "cwd": {
      "type": "string",
      "description": "Optional child working directory: use an absolute repository/project directory for predictable results. Omitted or empty uses the host session workspace when available, otherwise the plugin process directory. A relative directory is resolved from that same base. Must exist and suit the selected check (Git repository for diff, Cargo project for cargo/fmt/clippy); does not change the host directory."
    },
    "timeoutSeconds": {
      "type": "integer",
      "minimum": 1,
      "maximum": 120,
      "default": 60,
      "description": "Optional direct-child wait timeout in seconds; defaults to 60. Runtime clamps unsigned integer values to 1..120; other values fall back to 60. On timeout, kills and waits for the direct child and returns an error without partial output; descendants are not terminated. Not a hard end-to-end deadline or memory/output-production limit."
    }
  }
}"#;

// The tool owns its child and waits inside poll. Its ABI helper stores only
// per-call inputs and no host callback context.
#[allow(unsafe_code)] // Export the optional Host ABI marker.
#[no_mangle]
pub extern "C" fn rpi_plugin_shared_sessions_v1() {}

export_single_tool_plugin!(code_lens, "code_lens", DESCRIPTION, PARAMETERS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_schema_preserves_optional_inputs_and_runtime_limits() {
        let registered = registered_tool_schema();
        assert_eq!(registered.name.to_string_lossy(), "code_lens");
        let description = registered.description.to_string_lossy();
        for constraint in [
            "not arbitrary",
            "nonzero exit",
            "build scripts",
            "direct child",
            "40,000 bytes",
        ] {
            assert!(description.contains(constraint), "missing {constraint}");
        }
        let schema: Value = serde_json::from_str(&registered.parameters.to_string_lossy())
            .expect("registered parameters must be JSON");
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], true); // Host may inject __rpi.
        assert!(schema.get("required").is_none());
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(properties.len(), 3);
        for (name, kind) in [
            ("check", "string"),
            ("cwd", "string"),
            ("timeoutSeconds", "integer"),
        ] {
            assert_eq!(properties[name]["type"], kind);
            assert!(!properties[name]["description"].as_str().unwrap().is_empty());
        }
        assert_eq!(
            properties["check"]["enum"],
            serde_json::json!(["diff", "cargo", "fmt", "clippy"])
        );
        assert_eq!(properties["check"]["default"], "diff");
        assert_eq!(properties["timeoutSeconds"]["default"], 60);
        assert_eq!(properties["timeoutSeconds"]["minimum"], 1);
        assert_eq!(properties["timeoutSeconds"]["maximum"], 120);
        assert!(properties["cwd"].get("default").is_none());
        assert!(properties["cwd"]["description"]
            .as_str()
            .unwrap()
            .contains("host session workspace"));
        assert!(properties["check"]["description"]
            .as_str()
            .unwrap()
            .contains("unstaged tracked"));
    }

    #[test]
    fn rejects_unknown_check() {
        let value = serde_json::json!({"check":"shell"});
        assert!(code_lens(&value).unwrap_err().contains("diff"));
    }
}
