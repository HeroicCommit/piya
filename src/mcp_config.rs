use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default, rename = "mcpServers")]
    pub servers: HashMap<String, McpServerEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServerEntry {
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// `stdio` (default) or `http`
    #[serde(default, rename = "type")]
    pub transport: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

fn default_enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Http {
        url: String,
        headers: HashMap<String, String>,
    },
}

impl McpServerEntry {
    pub fn resolve_transport(&self) -> Result<McpTransport> {
        if self.transport.as_deref() == Some("http") || self.url.is_some() {
            let url = self
                .url
                .clone()
                .or_else(|| infer_http_url(self))
                .context("HTTP MCP server requires a `url`")?;
            return Ok(McpTransport::Http {
                url,
                headers: self.headers.clone(),
            });
        }

        if infer_http_url(self).is_some() {
            let url = infer_http_url(self).unwrap();
            return Ok(McpTransport::Http {
                url,
                headers: self.headers.clone(),
            });
        }

        if self.command.is_empty() {
            anyhow::bail!("MCP server requires `command` (stdio) or `url` (http)");
        }

        Ok(McpTransport::Stdio {
            command: self.command.clone(),
            args: self.args.clone(),
            env: self.env.clone(),
        })
    }
}

fn infer_http_url(entry: &McpServerEntry) -> Option<String> {
    if entry.command.starts_with("http://") || entry.command.starts_with("https://") {
        return Some(entry.command.clone());
    }
    if entry.command == "http" {
        return entry.args.first().cloned();
    }
    None
}

impl McpConfig {
    pub fn path(data_dir: &Path) -> PathBuf {
        data_dir.join("mcp.json")
    }

    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = Self::path(data_dir);
        if !path.is_file() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        let mut cfg: Self =
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
        cfg.normalize_entries();
        Ok(cfg)
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let path = Self::path(data_dir);
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json + "\n").with_context(|| format!("write {}", path.display()))
    }

    pub fn enabled_servers(&self) -> impl Iterator<Item = (&String, &McpServerEntry)> {
        self.servers.iter().filter(|(_, e)| e.enabled)
    }

    fn normalize_entries(&mut self) {
        for entry in self.servers.values_mut() {
            if entry.transport.as_deref() == Some("http") {
                continue;
            }
            if let Some(url) = infer_http_url(entry) {
                entry.transport = Some("http".into());
                entry.url = Some(url);
                entry.command.clear();
                entry.args.clear();
            }
        }
    }

    /// Cursor-compatible export (drops Piya-only `enabled` flag).
    pub fn to_cursor_format(&self) -> serde_json::Value {
        let mut servers = serde_json::Map::new();
        for (name, entry) in &self.servers {
            if !entry.enabled {
                continue;
            }
            match entry.resolve_transport() {
                Ok(McpTransport::Http { url, headers }) => {
                    servers.insert(
                        name.clone(),
                        serde_json::json!({
                            "type": "http",
                            "url": url,
                            "headers": headers,
                        }),
                    );
                }
                Ok(McpTransport::Stdio { command, args, env }) => {
                    servers.insert(
                        name.clone(),
                        serde_json::json!({
                            "command": command,
                            "args": args,
                            "env": env,
                        }),
                    );
                }
                Err(_) => {}
            }
        }
        serde_json::json!({ "mcpServers": servers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_cursor_shape() {
        let mut cfg = McpConfig::default();
        cfg.servers.insert(
            "demo".into(),
            McpServerEntry {
                command: "node".into(),
                args: vec!["server.js".into()],
                env: HashMap::from([("FOO".into(), "bar".into())]),
                enabled: true,
                transport: None,
                url: None,
                headers: HashMap::new(),
            },
        );
        let json = serde_json::to_string(&cfg).unwrap();
        let back: McpConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.servers["demo"].command, "node");
    }

    #[test]
    fn infers_http_from_command_url() {
        let entry = McpServerEntry {
            command: "http://127.0.0.1:8000/mcp".into(),
            args: vec![],
            env: HashMap::new(),
            enabled: true,
            transport: None,
            url: None,
            headers: HashMap::new(),
        };
        assert!(matches!(
            entry.resolve_transport().unwrap(),
            McpTransport::Http { .. }
        ));
    }
}
