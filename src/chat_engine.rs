//! Process-long LLM, vector memory, and MCP sessions for Chat.
//!
//! Connect once; reconnect only when settings or `mcp.json` change.

use std::path::Path;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use anyhow::{anyhow, Result};
use async_openai::types::ChatCompletionTool;

use crate::config::Settings;
use crate::llm::LlmClient;
use crate::mcp::McpToolHub;
use crate::mcp_config::McpConfig;
use crate::memory::VectorMemory;

struct CachedLlm {
    fingerprint: String,
    llm: LlmClient,
    memory: VectorMemory,
}

struct CachedMcp {
    fingerprint: String,
    hub: McpToolHub,
}

static LLM: Mutex<Option<CachedLlm>> = Mutex::new(None);
static MCP: Mutex<Option<CachedMcp>> = Mutex::new(None);

fn llm_fingerprint(settings: &Settings) -> String {
    format!(
        "{}|{}|{}|{}|{}",
        settings.openai_api_key,
        settings.openai_base_url,
        settings.model,
        settings.embedding_model,
        settings.data_dir.display()
    )
}

fn mcp_fingerprint(data_dir: &Path) -> String {
    let path = McpConfig::path(data_dir);
    match std::fs::metadata(&path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            format!("{}:{}:{}", path.display(), meta.len(), mtime)
        }
        Err(_) => format!("{}:missing", path.display()),
    }
}

/// Cheap clones of the process-long LLM client and LanceDB handle.
pub fn llm_and_memory(settings: &Settings) -> Result<(LlmClient, VectorMemory)> {
    let fp = llm_fingerprint(settings);
    let mut guard = LLM
        .lock()
        .map_err(|err| anyhow!("llm cache poisoned: {err}"))?;
    if let Some(cached) = guard.as_ref() {
        if cached.fingerprint == fp {
            return Ok((cached.llm.clone(), cached.memory.clone()));
        }
    }
    let llm = LlmClient::new(settings)?;
    let memory = VectorMemory::new(&settings.data_dir, llm.clone())?;
    *guard = Some(CachedLlm {
        fingerprint: fp,
        llm: llm.clone(),
        memory: memory.clone(),
    });
    Ok((llm, memory))
}

pub struct McpSnapshot {
    pub tools: Vec<ChatCompletionTool>,
    pub summary: String,
    pub is_empty: bool,
    pub has_notes: bool,
}

/// Spawn MCP servers on first use; reuse while `mcp.json` is unchanged.
pub fn mcp_snapshot(data_dir: &Path, max_tools: usize) -> Result<McpSnapshot> {
    let fp = mcp_fingerprint(data_dir);
    let mut guard = MCP
        .lock()
        .map_err(|err| anyhow!("mcp cache poisoned: {err}"))?;
    let needs_connect = match guard.as_ref() {
        Some(cached) => cached.fingerprint != fp,
        None => true,
    };
    if needs_connect {
        let cfg = McpConfig::load(data_dir)?;
        let mut hub = McpToolHub::connect(&cfg);
        hub.select_for_llm(max_tools);
        *guard = Some(CachedMcp {
            fingerprint: fp,
            hub,
        });
    }
    let hub = &guard.as_ref().expect("mcp hub").hub;
    Ok(McpSnapshot {
        tools: hub.openai_tools(),
        summary: hub.system_summary(),
        is_empty: hub.is_empty(),
        has_notes: !hub.connect_errors().is_empty(),
    })
}

pub fn mcp_call(qualified_name: &str, arguments_json: &str) -> Result<String> {
    let mut guard = MCP
        .lock()
        .map_err(|err| anyhow!("mcp cache poisoned: {err}"))?;
    let cached = guard
        .as_mut()
        .ok_or_else(|| anyhow!("MCP hub is not connected"))?;
    cached.hub.call(qualified_name, arguments_json)
}

/// Force reconnect on the next snapshot (config changed this process).
pub fn invalidate_mcp() {
    if let Ok(mut guard) = MCP.lock() {
        if let Some(cached) = guard.as_mut() {
            cached.fingerprint.clear();
        }
    }
}

pub fn invalidate_llm() {
    if let Ok(mut guard) = LLM.lock() {
        *guard = None;
    }
}

/// Test helper: fingerprint is stable for a missing file.
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn missing_mcp_json_fingerprint() {
        let dir = PathBuf::from("/tmp/piya-no-such-mcp-dir");
        let a = mcp_fingerprint(&dir);
        let b = mcp_fingerprint(&dir);
        assert_eq!(a, b);
        assert!(a.ends_with(":missing"));
    }
}
