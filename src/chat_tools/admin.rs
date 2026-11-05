//! MCP config tools (Chat built-ins).

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};
use serde_json::{json, Value};

use crate::mcp_config::{McpConfig, McpServerEntry};

pub fn openai_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            "piya_mcp_list",
            "List MCP servers configured in Piya (name, command, args, env keys, enabled).",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        tool(
            "piya_mcp_upsert",
            "Add or update an MCP server in Piya config. Saves to mcp.json. \
             For stdio servers: name, command, args, env. \
             For HTTP servers (e.g. Nextcloud): name, type=\"http\", url, headers. \
             External tools from new servers are available on the user's next message.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "type": {
                        "type": "string",
                        "description": "Transport: omit or \"stdio\" for subprocess, \"http\" for remote URL"
                    },
                    "command": { "type": "string" },
                    "url": { "type": "string" },
                    "args": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "env": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    },
                    "headers": {
                        "type": "object",
                        "additionalProperties": { "type": "string" }
                    },
                    "enabled": { "type": "boolean" }
                },
                "required": ["name"]
            }),
        ),
        tool(
            "piya_mcp_remove",
            "Remove an MCP server from Piya config by name.",
            json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" }
                },
                "required": ["name"]
            }),
        ),
    ]
}

pub fn dispatch(data_dir: &Path, name: &str, args_json: &str) -> Result<String> {
    let args: Value = if args_json.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(args_json).context("parse tool arguments")?
    };

    match name {
        "piya_mcp_list" => list(data_dir),
        "piya_mcp_upsert" => upsert(data_dir, &args),
        "piya_mcp_remove" => remove(data_dir, &args),
        _ => Err(anyhow!("unknown MCP tool `{name}`")),
    }
}

fn list(data_dir: &Path) -> Result<String> {
    let cfg = McpConfig::load(data_dir)?;
    if cfg.servers.is_empty() {
        return Ok("No MCP servers configured.".into());
    }

    let mut rows = Vec::new();
    for (name, entry) in &cfg.servers {
        rows.push(json!({
            "name": name,
            "type": entry.transport,
            "command": entry.command,
            "url": entry.url,
            "args": entry.args,
            "env": entry.env,
            "headers": entry.headers,
            "enabled": entry.enabled,
        }));
    }
    Ok(serde_json::to_string_pretty(&rows)?)
}

fn upsert(data_dir: &Path, args: &Value) -> Result<String> {
    let name = required_str(args, "name")?;
    let transport = args
        .get("type")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_lowercase());
    let url = args.get("url").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);
    let entry_args = args
        .get("args")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let env = parse_env(args.get("env"))?;
    let headers = parse_env(args.get("headers"))?;
    let enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);

    let is_http = transport.as_deref() == Some("http")
        || url.is_some()
        || command.as_deref().is_some_and(|c| c.starts_with("http://") || c.starts_with("https://"));

    if is_http {
        let url = url
            .map(String::from)
            .or_else(|| command.clone())
            .context("HTTP MCP server requires `url` (or `command` set to the URL)")?;
        let mut cfg = McpConfig::load(data_dir)?;
        cfg.servers.insert(
            name.clone(),
            McpServerEntry {
                command: String::new(),
                args: vec![],
                env: HashMap::new(),
                enabled,
                transport: Some("http".into()),
                url: Some(url.clone()),
                headers,
            },
        );
        cfg.save(data_dir)?;
        crate::chat_engine::invalidate_mcp();
        return Ok(format!(
            "Saved HTTP MCP server `{name}` (url: {url}, enabled: {enabled}). \
             Its tools will connect on the user's next message."
        ));
    }

    let command = command.context("`command` is required for stdio MCP servers")?;

    let mut cfg = McpConfig::load(data_dir)?;
    cfg.servers.insert(
        name.clone(),
        McpServerEntry {
            command: command.clone(),
            args: entry_args.clone(),
            env: env.clone(),
            enabled,
            transport: None,
            url: None,
            headers: HashMap::new(),
        },
    );
    cfg.save(data_dir)?;
    crate::chat_engine::invalidate_mcp();

    Ok(format!(
        "Saved MCP server `{name}` (command: {command}, {} args, {} env vars, enabled: {enabled}). \
         Its tools will connect on the user's next message.",
        entry_args.len(),
        env.len()
    ))
}

fn remove(data_dir: &Path, args: &Value) -> Result<String> {
    let name = required_str(args, "name")?;
    let mut cfg = McpConfig::load(data_dir)?;
    if cfg.servers.remove(&name).is_none() {
        return Ok(format!("No MCP server named `{name}` — nothing to remove."));
    }
    cfg.save(data_dir)?;
    crate::chat_engine::invalidate_mcp();
    Ok(format!("Removed MCP server `{name}`."))
}

fn required_str(args: &Value, key: &str) -> Result<String> {
    let s = args
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`{key}` is required"))?
        .trim()
        .to_string();
    if s.is_empty() {
        return Err(anyhow!("`{key}` cannot be empty"));
    }
    Ok(s)
}

fn parse_env(value: Option<&Value>) -> Result<HashMap<String, String>> {
    let Some(value) = value else {
        return Ok(HashMap::new());
    };
    let Some(map) = value.as_object() else {
        return Err(anyhow!("`env` must be an object"));
    };
    let mut out = HashMap::new();
    for (k, v) in map {
        let Some(s) = v.as_str() else {
            return Err(anyhow!("env `{k}` must be a string"));
        };
        out.insert(k.clone(), s.to_string());
    }
    Ok(out)
}

fn tool(name: &str, description: &str, parameters: Value) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: name.to_string(),
            description: Some(description.to_string()),
            parameters: Some(parameters),
            strict: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn upsert_and_list_roundtrip() {
        let dir = std::env::temp_dir().join(format!("piya-mcp-admin-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();

        let args = json!({
            "name": "demo",
            "command": "node",
            "args": ["server.js"],
            "env": { "FOO": "bar" }
        });
        upsert(&dir, &args).unwrap();
        let listed = list(&dir).unwrap();
        assert!(listed.contains("demo"));

        let http_args = json!({
            "name": "remote",
            "type": "http",
            "url": "http://127.0.0.1:8000/mcp"
        });
        upsert(&dir, &http_args).unwrap();
        let cfg = McpConfig::load(&dir).unwrap();
        assert_eq!(cfg.servers["remote"].url.as_deref(), Some("http://127.0.0.1:8000/mcp"));

        remove(&dir, &json!({ "name": "demo" })).unwrap();
        let _ = fs::remove_dir_all(dir);
    }
}
