//! Model providers Piya can connect to, and how to reach each one.
//!
//! One connection is active at a time (chat, tasks, scheduled actions). Memory
//! embeddings use their own connection because many chat providers have no
//! embeddings API. Keys are stored per provider (`PIYA_KEY_<ID>`), so switching
//! back to a provider does not require pasting the key again.

use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::Value;

/// Which wire protocol a provider speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Api {
    /// OpenAI Chat Completions (`/chat/completions`, `/embeddings`, `/models`).
    OpenAiCompat,
    /// Anthropic Messages API (`/v1/messages`).
    Anthropic,
}

#[derive(Clone, Debug, Serialize)]
pub struct Provider {
    pub id: &'static str,
    pub label: &'static str,
    pub api: Api,
    pub default_base_url: &'static str,
    /// Local servers usually run without a key.
    pub needs_key: bool,
    pub local: bool,
    /// Where to create a key.
    pub key_url: &'static str,
    /// Has an OpenAI-compatible `/embeddings` endpoint Piya can use for memory.
    pub embeddings: bool,
    /// Suggested embeddings model when this provider is used for memory.
    pub default_embedding_model: &'static str,
    /// Suggested chat model; only used when the user has not picked one.
    pub suggested_model: &'static str,
}

pub const PROVIDERS: &[Provider] = &[
    Provider {
        id: "openai",
        label: "OpenAI",
        api: Api::OpenAiCompat,
        default_base_url: "https://api.openai.com/v1",
        needs_key: true,
        local: false,
        key_url: "https://platform.openai.com/api-keys",
        embeddings: true,
        default_embedding_model: "text-embedding-3-small",
        suggested_model: "gpt-4o-mini",
    },
    Provider {
        id: "anthropic",
        label: "Anthropic (Claude)",
        api: Api::Anthropic,
        default_base_url: "https://api.anthropic.com",
        needs_key: true,
        local: false,
        key_url: "https://console.anthropic.com/settings/keys",
        embeddings: false,
        default_embedding_model: "",
        suggested_model: "",
    },
    Provider {
        id: "gemini",
        label: "Google Gemini",
        api: Api::OpenAiCompat,
        default_base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        needs_key: true,
        local: false,
        key_url: "https://aistudio.google.com/apikey",
        embeddings: true,
        default_embedding_model: "gemini-embedding-001",
        suggested_model: "",
    },
    Provider {
        id: "openrouter",
        label: "OpenRouter",
        api: Api::OpenAiCompat,
        default_base_url: "https://openrouter.ai/api/v1",
        needs_key: true,
        local: false,
        key_url: "https://openrouter.ai/keys",
        embeddings: false,
        default_embedding_model: "",
        suggested_model: "",
    },
    Provider {
        id: "groq",
        label: "Groq",
        api: Api::OpenAiCompat,
        default_base_url: "https://api.groq.com/openai/v1",
        needs_key: true,
        local: false,
        key_url: "https://console.groq.com/keys",
        embeddings: false,
        default_embedding_model: "",
        suggested_model: "",
    },
    Provider {
        id: "mistral",
        label: "Mistral",
        api: Api::OpenAiCompat,
        default_base_url: "https://api.mistral.ai/v1",
        needs_key: true,
        local: false,
        key_url: "https://console.mistral.ai/api-keys",
        embeddings: true,
        default_embedding_model: "mistral-embed",
        suggested_model: "",
    },
    Provider {
        id: "deepseek",
        label: "DeepSeek",
        api: Api::OpenAiCompat,
        default_base_url: "https://api.deepseek.com/v1",
        needs_key: true,
        local: false,
        key_url: "https://platform.deepseek.com/api_keys",
        embeddings: false,
        default_embedding_model: "",
        suggested_model: "deepseek-chat",
    },
    Provider {
        id: "xai",
        label: "xAI (Grok)",
        api: Api::OpenAiCompat,
        default_base_url: "https://api.x.ai/v1",
        needs_key: true,
        local: false,
        key_url: "https://console.x.ai",
        embeddings: false,
        default_embedding_model: "",
        suggested_model: "",
    },
    Provider {
        id: "ollama",
        label: "Ollama (local)",
        api: Api::OpenAiCompat,
        default_base_url: "http://localhost:11434/v1",
        needs_key: false,
        local: true,
        key_url: "https://ollama.com/download",
        embeddings: true,
        default_embedding_model: "nomic-embed-text",
        suggested_model: "",
    },
    Provider {
        id: "lmstudio",
        label: "LM Studio (local)",
        api: Api::OpenAiCompat,
        default_base_url: "http://localhost:1234/v1",
        needs_key: false,
        local: true,
        key_url: "https://lmstudio.ai",
        embeddings: true,
        default_embedding_model: "",
        suggested_model: "",
    },
    Provider {
        id: "custom",
        label: "Other OpenAI-compatible server",
        api: Api::OpenAiCompat,
        default_base_url: "",
        needs_key: false,
        local: false,
        key_url: "",
        embeddings: true,
        default_embedding_model: "",
        suggested_model: "",
    },
];

pub fn provider(id: &str) -> Option<&'static Provider> {
    let id = id.trim().to_ascii_lowercase();
    PROVIDERS.iter().find(|p| p.id == id)
}

pub fn provider_or_err(id: &str) -> Result<&'static Provider> {
    provider(id).ok_or_else(|| {
        anyhow!(
            "unknown model provider `{id}` (known: {})",
            PROVIDERS.iter().map(|p| p.id).collect::<Vec<_>>().join(", ")
        )
    })
}

/// Env var holding the saved key for a provider, e.g. `PIYA_KEY_ANTHROPIC`.
pub fn key_env(id: &str) -> String {
    format!("PIYA_KEY_{}", id.to_ascii_uppercase())
}

/// Env var holding a base-URL override for a provider, e.g. `PIYA_BASE_URL_OLLAMA`.
pub fn base_url_env(id: &str) -> String {
    format!("PIYA_BASE_URL_{}", id.to_ascii_uppercase())
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Saved key for `id`. OpenAI also accepts the legacy `OPENAI_API_KEY`.
pub fn saved_key(id: &str) -> String {
    env_nonempty(&key_env(id))
        .or_else(|| (id == "openai").then(|| env_nonempty("OPENAI_API_KEY")).flatten())
        .unwrap_or_default()
}

/// Base URL for `id`: saved override, legacy `OPENAI_BASE_URL` for OpenAI, else the default.
pub fn saved_base_url(id: &str) -> String {
    env_nonempty(&base_url_env(id))
        .or_else(|| (id == "openai").then(|| env_nonempty("OPENAI_BASE_URL")).flatten())
        .or_else(|| provider(id).map(|p| p.default_base_url.to_string()))
        .unwrap_or_default()
}

/// A resolved way to reach one provider.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Connection {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
}

impl Connection {
    pub fn from_saved(id: &str) -> Self {
        Self {
            provider: id.to_string(),
            api_key: saved_key(id),
            base_url: saved_base_url(id),
        }
    }

    pub fn info(&self) -> Result<&'static Provider> {
        provider_or_err(&self.provider)
    }

    /// Fail early with an actionable message instead of a confusing HTTP error.
    pub fn validate(&self) -> Result<()> {
        let p = self.info()?;
        if self.base_url.trim().is_empty() {
            bail!("{} needs a server address (base URL)", p.label);
        }
        if p.needs_key && self.api_key.trim().is_empty() {
            bail!(
                "{} needs an API key — add it in Settings → Models{}",
                p.label,
                if p.key_url.is_empty() { String::new() } else { format!(" (get one at {})", p.key_url) }
            );
        }
        Ok(())
    }
}

/// Guess which provider issued a key from its format. `None` when unknown.
/// Used to warn when a key is pasted under the wrong provider.
pub fn detect_key_provider(key: &str) -> Option<&'static str> {
    let k = key.trim();
    if k.starts_with("sk-ant-") {
        Some("anthropic")
    } else if k.starts_with("sk-or-") {
        Some("openrouter")
    } else if k.starts_with("AIza") {
        Some("gemini")
    } else if k.starts_with("gsk_") {
        Some("groq")
    } else if k.starts_with("xai-") {
        Some("xai")
    } else if k.starts_with("sk-proj-") || k.starts_with("sk-svcacct-") {
        Some("openai")
    } else {
        None
    }
}

/// Warning text when `key` looks like it belongs to a different provider than `provider_id`.
pub fn key_mismatch_warning(provider_id: &str, key: &str) -> Option<String> {
    let guessed = detect_key_provider(key)?;
    if guessed == provider_id {
        return None;
    }
    let label = provider(guessed).map(|p| p.label).unwrap_or(guessed);
    Some(format!("This looks like a {label} key. Choose {label} or paste a key for the selected provider."))
}

pub(crate) fn http(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(timeout)
        .build()
        .unwrap_or_default()
}

fn join(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path.trim_start_matches('/'))
}

/// Model ids offered by the connection's account (live list, not a bundled catalog).
/// `embeddings = true` returns only models that look like embedding models.
pub async fn list_models(conn: &Connection, embeddings: bool) -> Result<Vec<String>> {
    let p = conn.info()?;
    if conn.base_url.trim().is_empty() {
        bail!("{} needs a server address (base URL)", p.label);
    }
    let client = http(Duration::from_secs(20));
    let req = match p.api {
        Api::Anthropic => client
            .get(join(&conn.base_url, "v1/models?limit=100"))
            .header("x-api-key", conn.api_key.trim())
            .header("anthropic-version", ANTHROPIC_VERSION),
        Api::OpenAiCompat => {
            let r = client.get(join(&conn.base_url, "models"));
            if conn.api_key.trim().is_empty() {
                r
            } else {
                r.bearer_auth(conn.api_key.trim())
            }
        }
    };
    let resp = req.send().await.map_err(|e| {
        if p.local && (e.is_connect() || e.is_timeout()) {
            anyhow!("could not reach {} at {} — is it running?", p.label, conn.base_url)
        } else {
            anyhow!("could not reach {} at {}: {e}", p.label, conn.base_url)
        }
    })?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("{} rejected the request ({status}): {}", p.label, api_error_message(&body));
    }
    let json: Value = serde_json::from_str(&body)
        .with_context(|| format!("{} returned an unexpected model list", p.label))?;
    Ok(parse_model_list(p.id, &json, embeddings))
}

fn parse_model_list(provider_id: &str, json: &Value, embeddings: bool) -> Vec<String> {
    let items = json
        .get("data")
        .or_else(|| json.get("models"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<String> = items
        .iter()
        .filter(|m| {
            // OpenRouter lists capabilities; keep chat models that support tools.
            if provider_id != "openrouter" || embeddings {
                return true;
            }
            match m.get("supported_parameters").and_then(|v| v.as_array()) {
                Some(params) => params.iter().any(|p| p.as_str() == Some("tools")),
                None => true,
            }
        })
        .filter_map(|m| m.get("id").or_else(|| m.get("name")).and_then(|v| v.as_str()))
        .map(|id| id.trim_start_matches("models/").to_string())
        .filter(|id| {
            let lower = id.to_ascii_lowercase();
            let is_embed = lower.contains("embed");
            if embeddings {
                is_embed
            } else {
                !is_embed
                    && !["whisper", "tts", "dall-e", "moderation", "transcribe", "image", "aqa", "imagen", "veo"]
                        .iter()
                        .any(|w| lower.contains(w))
            }
        })
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Best-effort human message from an API error body (OpenAI / Anthropic / Google shapes).
pub fn api_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let msg = v
            .pointer("/error/message")
            .or_else(|| v.pointer("/message"))
            .or_else(|| v.pointer("/0/error/message"))
            .and_then(|m| m.as_str());
        if let Some(m) = msg {
            return m.to_string();
        }
    }
    body.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_key_formats() {
        assert_eq!(detect_key_provider("sk-ant-api03-xyz"), Some("anthropic"));
        assert_eq!(detect_key_provider("sk-or-v1-abc"), Some("openrouter"));
        assert_eq!(detect_key_provider("AIzaSyD"), Some("gemini"));
        assert_eq!(detect_key_provider("gsk_123"), Some("groq"));
        assert_eq!(detect_key_provider("sk-proj-abc"), Some("openai"));
        assert_eq!(detect_key_provider("sk-abc"), None, "plain sk- is ambiguous (OpenAI, DeepSeek)");
        assert!(key_mismatch_warning("openai", "sk-ant-api03-x").unwrap().contains("Anthropic"));
        assert!(key_mismatch_warning("anthropic", "sk-ant-api03-x").is_none());
    }

    #[test]
    fn filters_model_lists() {
        let openai = json!({"data": [{"id":"gpt-4o-mini"},{"id":"text-embedding-3-small"},{"id":"whisper-1"},{"id":"o4-mini"}]});
        assert_eq!(parse_model_list("openai", &openai, false), vec!["gpt-4o-mini", "o4-mini"]);
        assert_eq!(parse_model_list("openai", &openai, true), vec!["text-embedding-3-small"]);
        let gemini = json!({"data": [{"id":"models/gemini-2.5-flash"},{"id":"models/gemini-embedding-001"}]});
        assert_eq!(parse_model_list("gemini", &gemini, false), vec!["gemini-2.5-flash"]);
        let router = json!({"data": [
            {"id":"a/tools","supported_parameters":["tools","temperature"]},
            {"id":"b/no-tools","supported_parameters":["temperature"]}]});
        assert_eq!(parse_model_list("openrouter", &router, false), vec!["a/tools"]);
    }

    #[test]
    fn validates_connections() {
        let c = Connection { provider: "anthropic".into(), api_key: "".into(), base_url: "https://api.anthropic.com".into() };
        assert!(c.validate().unwrap_err().to_string().contains("API key"));
        let local = Connection { provider: "ollama".into(), api_key: "".into(), base_url: "http://localhost:11434/v1".into() };
        assert!(local.validate().is_ok(), "local servers need no key");
        let custom = Connection { provider: "custom".into(), ..Default::default() };
        assert!(custom.validate().is_err(), "custom needs a base URL");
    }

    #[test]
    fn api_error_messages() {
        assert_eq!(api_error_message(r#"{"error":{"message":"bad key"}}"#), "bad key");
        assert_eq!(api_error_message(r#"{"type":"error","error":{"type":"x","message":"nope"}}"#), "nope");
    }
}
