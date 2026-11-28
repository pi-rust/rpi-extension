//! Resolve the native adapter's implicit configuration against the tool call's cwd.
use serde_json::{json, Value};
use std::{
    borrow::Cow,
    path::{Path, PathBuf},
};

pub(crate) fn with_config_path<'a>(
    params: &'a Value,
    cwd: &Path,
) -> Result<Cow<'a, Value>, String> {
    if !params.is_object() {
        return Err("MCP parameters must be an object".into());
    }
    if params.get("configPath").is_some() || params.get("url").is_some() {
        return Ok(Cow::Borrowed(params));
    }
    let agent_dir = std::env::var_os("RPI_CODING_AGENT_DIR")
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".rpi/agent")));
    let cwd = std::path::absolute(cwd).map_err(|e| format!("resolve MCP workspace: {e}"))?;
    let path = discover(
        &cwd,
        agent_dir.as_deref(),
        std::env::var_os("RPI_MCP_CONFIG").as_deref().map(Path::new),
    )?;
    let mut params = params.clone();
    let path = std::path::absolute(path).map_err(|e| format!("resolve MCP config: {e}"))?;
    params["configPath"] = json!(path.to_str().ok_or("MCP config path must be UTF-8")?);
    Ok(Cow::Owned(params))
}

fn discover(
    cwd: &Path,
    agent_dir: Option<&Path>,
    override_path: Option<&Path>,
) -> Result<PathBuf, String> {
    if let Some(path) = override_path {
        if path.as_os_str().is_empty() {
            return Err("RPI_MCP_CONFIG must name a configuration file".into());
        }
        return Ok(cwd.join(path));
    }
    let project = cwd.join(".rpi");
    let config_dirs = std::iter::once(project.as_path()).chain(agent_dir);
    for directory in config_dirs.clone() {
        let settings = directory.join("settings.json");
        match settings.metadata() {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(format!("MCP settings {} is not a file", settings.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("read MCP settings {}: {e}", settings.display())),
        }
        let text = std::fs::read_to_string(&settings)
            .map_err(|e| format!("read MCP settings {}: {e}", settings.display()))?;
        let value = parse_settings(&text)
            .map_err(|e| format!("invalid MCP settings {}: {e}", settings.display()))?;
        if let Some(path) = value["mcp"].get("configPath") {
            let path = path
                .as_str()
                .filter(|path| !path.trim().is_empty())
                .ok_or_else(|| {
                    format!(
                        "mcp.configPath in {} must name a configuration file",
                        settings.display()
                    )
                })?;
            return Ok(directory.join(path));
        }
    }
    let mut attempted = Vec::new();
    for directory in config_dirs {
        let path = directory.join("mcp.json");
        match path.metadata() {
            Ok(metadata) if metadata.is_file() => return Ok(path),
            Ok(_) => return Err(format!("MCP config {} is not a file", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                attempted.push(path.display().to_string())
            }
            Err(e) => return Err(format!("read MCP config {}: {e}", path.display())),
        }
    }
    Err(format!(
        "MCP config not found; tried {}",
        attempted.join(", ")
    ))
}

fn parse_settings(text: &str) -> serde_json::Result<Value> {
    serde_json::from_str(text).or_else(|original| {
        // The native settings reader accepts // comments outside JSON strings.
        let mut stripped = String::with_capacity(text.len());
        for line in text.lines() {
            let mut in_string = false;
            let mut chars = line.char_indices();
            let mut end = line.len();
            while let Some((i, ch)) = chars.next() {
                match ch {
                    '\\' if in_string => {
                        chars.next();
                    }
                    '"' => in_string = !in_string,
                    '/' if !in_string && line.as_bytes().get(i + 1) == Some(&b'/') => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            stripped.push_str(&line[..end]);
            stripped.push('\n');
        }
        serde_json::from_str(&stripped).map_err(|_| original)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_search_prefers_overrides_then_project_then_agent() {
        let root = std::env::temp_dir().join(format!("mcp-config-search-{}", std::process::id()));
        let cwd = root.join("workspace");
        let project = cwd.join(".rpi");
        let agent = root.join("agent");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&agent).unwrap();

        let error = discover(&cwd, Some(&agent), None).unwrap_err();
        for path in [project.join("mcp.json"), agent.join("mcp.json")] {
            assert!(error.contains(&path.display().to_string()), "{error}");
        }
        std::fs::write(agent.join("mcp.json"), "{}").unwrap();
        assert_eq!(
            discover(&cwd, Some(&agent), None).unwrap(),
            agent.join("mcp.json")
        );
        std::fs::write(project.join("mcp.json"), "{}").unwrap();
        assert_eq!(
            discover(&cwd, Some(&agent), None).unwrap(),
            project.join("mcp.json")
        );
        std::fs::write(
            agent.join("settings.json"),
            r#"{
                // Agent override; both comments and double slash inside a string are preserved.
                "mcp":{"configPath":"sub//other.json"} // override
            }"#,
        )
        .unwrap();
        assert_eq!(
            discover(&cwd, Some(&agent), None).unwrap(),
            agent.join("sub//other.json")
        );
        let forced = Some(Path::new("override.json"));
        assert_eq!(
            discover(&cwd, Some(&agent), forced).unwrap(),
            cwd.join("override.json")
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
