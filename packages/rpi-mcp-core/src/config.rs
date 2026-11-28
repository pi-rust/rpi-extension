//! Pi-compatible MCP server configuration owned by the extension.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq)]
pub struct McpConfig {
    #[serde(default, rename = "mcpServers")]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(untagged)]
pub enum McpServerConfig {
    Stdio(StdioServer),
    Http(HttpServer),
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct StdioServer {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub exposure: Exposure,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct HttpServer {
    pub url: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub exposure: Exposure,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Exposure {
    #[default]
    Codemode,
    Deferred,
    Direct,
    Hidden,
}

fn default_true() -> bool {
    true
}
fn default_timeout() -> u64 {
    60
}

impl McpConfig {
    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self =
            serde_json::from_str(text).map_err(|error| format!("invalid mcp config: {error}"))?;
        for (name, server) in &config.servers {
            validate_name(name)?;
            match server {
                McpServerConfig::Stdio(server) if server.command.trim().is_empty() => {
                    return Err(format!("MCP server {name:?} has an empty command"));
                }
                McpServerConfig::Http(server) if server.url.trim().is_empty() => {
                    return Err(format!("MCP server {name:?} has an empty URL"));
                }
                _ => {}
            }
        }
        Ok(config)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ToolPolicy {
    #[serde(default)]
    deny: Vec<String>,
    read_only: Option<Vec<String>>,
    preset: Option<PolicyPreset>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum PolicyPreset {
    Database,
}

impl ToolPolicy {
    fn check(&self, server: &str, tool: &str) -> Result<(), String> {
        let matches = |patterns: &[String]| -> Result<bool, String> {
            let mut matched = false;
            for pattern in patterns {
                let pattern = glob::Pattern::new(pattern)
                    .map_err(|e| format!("invalid MCP toolPolicy glob for {server:?}: {e}"))?;
                matched |= pattern.matches(tool);
            }
            Ok(matched)
        };
        let denied = matches(&self.deny)?;
        let allowed = match &self.read_only {
            Some(patterns) => matches(patterns)?,
            None => true,
        };
        let database_write = self.preset.is_some()
            && ["*_execute_*", "*_open_table", "*_rollback_*"]
                .iter()
                .any(|pattern| {
                    glob::Pattern::new(pattern)
                        .expect("valid preset glob")
                        .matches(tool)
                });
        if denied || !allowed || database_write {
            return Err(format!(
                "MCP tool {tool:?} on server {server:?} is denied by toolPolicy; execute it manually if intended"
            ));
        }
        Ok(())
    }
}

pub(crate) fn check_tool_policy(text: &str, server: &str, tool: &str) -> Result<(), String> {
    let document: Value =
        serde_json::from_str(text).map_err(|e| format!("invalid mcp config: {e}"))?;
    let Some(policy) = document["mcpServers"][server].get("toolPolicy") else {
        return Ok(());
    };
    let policy: ToolPolicy = serde_json::from_value(policy.clone())
        .map_err(|e| format!("invalid MCP toolPolicy for {server:?}: {e}"))?;
    policy.check(server, tool)
}

fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(format!("invalid MCP server name {name:?}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pi_mcp_servers_shape() {
        let config = McpConfig::parse(
            r#"{
          "mcpServers": {
            "docs": {"type":"stdio", "command":"node", "args":["server.js"]},
            "remote": {"url":"https://example.com/mcp", "exposure":"direct"}
          }
        }"#,
        )
        .unwrap();
        assert_eq!(config.servers.len(), 2);
        assert!(matches!(config.servers["docs"], McpServerConfig::Stdio(_)));
        assert!(matches!(config.servers["remote"], McpServerConfig::Http(_)));
    }

    #[test]
    fn rejects_invalid_server_name() {
        let error = McpConfig::parse(r#"{"mcpServers":{"bad.name":{"command":"x"}}}"#).unwrap_err();
        assert!(error.contains("invalid MCP server name"));
    }
}
