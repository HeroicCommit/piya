//! UniFFI surface for the SwiftUI macOS app.

use std::collections::HashMap;

use crate::chat_service::{self, ChatTurn};
use crate::chat_session::{ChatSession, SessionMessage};
use crate::config::{self, Settings};
use crate::mcp_config::{McpConfig, McpServerEntry};
use crate::tasks::{TaskQueue, TaskStatus};
use crate::ui::assign::{converse_task, spawn_if_needed};

#[derive(uniffi::Enum, Clone, Debug)]
pub enum FfiChatMode {
    Chat,
    Task,
}

#[derive(uniffi::Error, Debug)]
pub enum PiyaError {
    Config { message: String },
    Chat { message: String },
    Mcp { message: String },
    Queue { message: String },
}

impl std::fmt::Display for PiyaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config { message } => write!(f, "config: {message}"),
            Self::Chat { message } => write!(f, "chat: {message}"),
            Self::Mcp { message } => write!(f, "mcp: {message}"),
            Self::Queue { message } => write!(f, "queue: {message}"),
        }
    }
}

impl std::error::Error for PiyaError {}

fn map_err(err: anyhow::Error) -> PiyaError {
    PiyaError::Config {
        message: err.to_string(),
    }
}

fn load_settings() -> Result<Settings, PiyaError> {
    Settings::load().map_err(map_err)
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiChatTurn {
    pub role: String,
    pub text: String,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiMcpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub enabled: bool,
    /// `stdio` or `http`
    pub transport: String,
    pub url: String,
    pub headers: HashMap<String, String>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiQueueSummary {
    pub pending: u32,
    pub running: u32,
    pub scheduled: u32,
    pub lines: Vec<String>,
}

#[uniffi::export]
pub fn piya_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[uniffi::export]
pub fn piya_data_dir() -> Result<String, PiyaError> {
    let settings = load_settings()?;
    Ok(settings.data_dir.display().to_string())
}

#[uniffi::export]
pub fn piya_workspace_dir() -> Result<String, PiyaError> {
    let settings = load_settings()?;
    Ok(settings.workspace.display().to_string())
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiChatSession {
    pub mode: String,
    pub messages: Vec<FfiChatTurn>,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiChatSessionMeta {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub mode: String,
}

fn meta_to_ffi(m: crate::chat_session::SessionMeta) -> FfiChatSessionMeta {
    FfiChatSessionMeta {
        id: m.id,
        title: m.title,
        updated_at: m.updated_at,
        mode: m.mode,
    }
}

fn session_to_ffi(session: ChatSession) -> FfiChatSession {
    FfiChatSession {
        mode: session.mode,
        messages: session
            .messages
            .into_iter()
            .map(|m| FfiChatTurn {
                role: m.role,
                text: m.text,
            })
            .collect(),
    }
}

fn turns_to_session(mode: String, messages: Vec<FfiChatTurn>) -> ChatSession {
    ChatSession {
        mode,
        messages: messages
            .into_iter()
            .map(|m| SessionMessage {
                role: m.role,
                text: m.text,
            })
            .collect(),
    }
}

#[uniffi::export]
pub fn piya_list_chat_sessions() -> Result<Vec<FfiChatSessionMeta>, PiyaError> {
    let settings = load_settings()?;
    let store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(|err| {
        PiyaError::Chat {
            message: err.to_string(),
        }
    })?;
    Ok(store.list().iter().cloned().map(meta_to_ffi).collect())
}

#[uniffi::export]
pub fn piya_active_chat_session_id() -> Result<String, PiyaError> {
    let settings = load_settings()?;
    let store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(|err| {
        PiyaError::Chat {
            message: err.to_string(),
        }
    })?;
    Ok(store.active_id().to_string())
}

#[uniffi::export]
pub fn piya_select_chat_session(id: String) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let mut store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(
        |err| PiyaError::Chat {
            message: err.to_string(),
        },
    )?;
    store.set_active(&id).map_err(|err| PiyaError::Chat {
        message: err.to_string(),
    })
}

#[uniffi::export]
pub fn piya_create_chat_session(mode: String) -> Result<FfiChatSessionMeta, PiyaError> {
    let settings = load_settings()?;
    let mut store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(
        |err| PiyaError::Chat {
            message: err.to_string(),
        },
    )?;
    store.create(&mode).map(meta_to_ffi).map_err(|err| PiyaError::Chat {
        message: err.to_string(),
    })
}

#[uniffi::export]
pub fn piya_delete_chat_session(id: String) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let mut store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(
        |err| PiyaError::Chat {
            message: err.to_string(),
        },
    )?;
    store.delete(&id).map_err(|err| PiyaError::Chat {
        message: err.to_string(),
    })
}

#[uniffi::export]
pub fn piya_load_chat_session() -> Result<FfiChatSession, PiyaError> {
    let settings = load_settings()?;
    let store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(|err| {
        PiyaError::Chat {
            message: err.to_string(),
        }
    })?;
    let session = store.load_active().map_err(|err| PiyaError::Chat {
        message: err.to_string(),
    })?;
    Ok(session_to_ffi(session))
}

#[uniffi::export]
pub fn piya_save_chat_session(mode: String, messages: Vec<FfiChatTurn>) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let mut store = crate::chat_session::ChatSessionStore::open(&settings.data_dir).map_err(
        |err| PiyaError::Chat {
            message: err.to_string(),
        },
    )?;
    let session = turns_to_session(mode, messages);
    store.save_active(&session).map_err(|err| PiyaError::Chat {
        message: err.to_string(),
    })
}

#[uniffi::export]
pub fn piya_chat(message: String, history: Vec<FfiChatTurn>) -> Result<String, PiyaError> {
    piya_send(message, FfiChatMode::Chat, history)
}

#[uniffi::export]
pub fn piya_send(
    message: String,
    mode: FfiChatMode,
    history: Vec<FfiChatTurn>,
) -> Result<String, PiyaError> {
    let settings = load_settings()?;
    match mode {
        FfiChatMode::Chat => {
            let turns: Vec<ChatTurn> = history
                .into_iter()
                .map(|t| ChatTurn {
                    role: t.role,
                    text: t.text,
                })
                .collect();
            chat_service::chat_reply(&settings, &turns, &message).map_err(|err| PiyaError::Chat {
                message: format!("{err:#}"),
            })
        }
        FfiChatMode::Task => {
            let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(|err| {
                PiyaError::Chat {
                    message: format!("{err:#}"),
                }
            })?;
            let outcome = converse_task(&queue, &settings, &message);
            spawn_if_needed(&settings, &outcome);
            Ok(outcome.reply)
        }
    }
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiMcpToolInfo {
    pub name: String,
    pub description: String,
}

/// Result of Settings → "Test connection" (never throws; errors are in `error`).
#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiMcpProbe {
    pub ok: bool,
    pub tools: Vec<FfiMcpToolInfo>,
    pub error: String,
}

fn ffi_to_entry(s: FfiMcpServer) -> (String, McpServerEntry) {
    let is_http = s.transport.eq_ignore_ascii_case("http") || !s.url.trim().is_empty();
    (
        s.name,
        McpServerEntry {
            command: s.command,
            args: s.args,
            env: s.env,
            enabled: s.enabled,
            transport: if is_http {
                Some("http".into())
            } else {
                Some("stdio".into())
            },
            url: if is_http && !s.url.trim().is_empty() {
                Some(s.url)
            } else {
                None
            },
            headers: s.headers,
        },
    )
}

/// Reject lists that would silently lose data when stored by name.
fn validate_mcp_servers(servers: &[FfiMcpServer]) -> Result<(), String> {
    let mut seen = std::collections::HashSet::new();
    for s in servers {
        let name = s.name.trim();
        if name.is_empty() {
            return Err("every MCP server needs a name".into());
        }
        if !seen.insert(name.to_lowercase()) {
            return Err(format!("duplicate MCP server name `{name}`"));
        }
    }
    Ok(())
}

#[uniffi::export]
pub fn piya_list_mcp_servers() -> Result<Vec<FfiMcpServer>, PiyaError> {
    let settings = load_settings()?;
    let cfg = McpConfig::load(&settings.data_dir).map_err(map_err)?;
    let mut servers: Vec<FfiMcpServer> = cfg
        .servers
        .into_iter()
        .map(|(name, entry)| {
            let transport = entry
                .transport
                .clone()
                .unwrap_or_else(|| {
                    if entry.url.is_some() {
                        "http".into()
                    } else {
                        "stdio".into()
                    }
                });
            FfiMcpServer {
                name,
                command: entry.command,
                args: entry.args,
                env: entry.env,
                enabled: entry.enabled,
                transport,
                url: entry.url.unwrap_or_default(),
                headers: entry.headers,
            }
        })
        .collect();
    // HashMap order is random; keep the list stable between loads.
    servers.sort_by_key(|s| s.name.to_lowercase());
    Ok(servers)
}

#[uniffi::export]
pub fn piya_save_mcp_servers(servers: Vec<FfiMcpServer>) -> Result<(), PiyaError> {
    validate_mcp_servers(&servers).map_err(|message| PiyaError::Mcp { message })?;
    let settings = load_settings()?;
    let mut map = HashMap::new();
    for s in servers {
        let (name, entry) = ffi_to_entry(s);
        map.insert(name.trim().to_string(), entry);
    }
    McpConfig { servers: map }
        .save(&settings.data_dir)
        .map_err(|err| PiyaError::Mcp {
            message: err.to_string(),
        })?;
    crate::chat_engine::invalidate_mcp();
    Ok(())
}

/// Start the server, run the MCP handshake and list its tools (15s limit).
#[uniffi::export]
pub fn piya_test_mcp_server(server: FfiMcpServer) -> FfiMcpProbe {
    let (name, entry) = ffi_to_entry(server);
    match crate::mcp::client::probe_server(&name, &entry, std::time::Duration::from_secs(15)) {
        Ok(tools) => FfiMcpProbe {
            ok: true,
            tools: tools
                .into_iter()
                .map(|(name, description)| FfiMcpToolInfo { name, description })
                .collect(),
            error: String::new(),
        },
        Err(err) => FfiMcpProbe {
            ok: false,
            tools: Vec::new(),
            error: format!("{err:#}"),
        },
    }
}

#[uniffi::export]
pub fn piya_queue_summary() -> Result<FfiQueueSummary, PiyaError> {
    let settings = load_settings()?;
    let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;

    let pending = queue.count_due_pending().map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })? as u32;
    let running = queue
        .count(Some(TaskStatus::InProgress))
        .map_err(|err| PiyaError::Queue {
            message: err.to_string(),
        })? as u32;
    let scheduled = queue.count_scheduled().map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })? as u32;

    let mut lines = vec![format!("{pending} pending")];
    for t in queue
        .list(Some(TaskStatus::Pending), 8)
        .map_err(|err| PiyaError::Queue {
            message: err.to_string(),
        })?
    {
        lines.push(format!("• {}", t.title));
    }

    Ok(FfiQueueSummary {
        pending,
        running,
        scheduled,
        lines,
    })
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiTaskItem {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: String,
    pub category: String,
    pub schedule_label: String,
    pub run_at: Option<String>,
    pub is_recurring: bool,
}

fn task_to_ffi(task: &crate::tasks::Task) -> FfiTaskItem {
    use chrono::Utc;
    use crate::schedule;
    use crate::tasks::TaskStatus;

    let now = Utc::now();
    let category = match task.status {
        TaskStatus::InProgress => "running",
        TaskStatus::Pending => {
            if let Some(run_at) = &task.run_at {
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(run_at) {
                    if dt.with_timezone(&Utc) > now {
                        "scheduled"
                    } else {
                        "queued"
                    }
                } else {
                    "queued"
                }
            } else {
                "queued"
            }
        }
        _ => "other",
    };

    let schedule_label = schedule::describe_task_schedule(
        task.run_at.as_deref(),
        task.recurrence.as_deref(),
    )
    .unwrap_or_else(|| "Run now".into());

    FfiTaskItem {
        id: task.id.clone(),
        title: task.title.clone(),
        description: task.description.clone(),
        status: task.status.as_str().into(),
        category: category.into(),
        schedule_label,
        run_at: task.run_at.clone(),
        is_recurring: task.recurrence.is_some(),
    }
}

#[uniffi::export]
pub fn piya_list_tasks() -> Result<Vec<FfiTaskItem>, PiyaError> {
    let settings = load_settings()?;
    let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;
    let tasks = queue.list_active(100).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;
    Ok(tasks.iter().map(task_to_ffi).collect())
}

#[uniffi::export]
pub fn piya_cancel_task(id: String) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;
    queue.cancel(&id).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;
    Ok(())
}

#[uniffi::export]
pub fn piya_update_task(id: String, title: String, description: String) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;
    let title = if title.trim().is_empty() {
        None
    } else {
        Some(title.trim())
    };
    let description = if description.trim().is_empty() {
        None
    } else {
        Some(description.trim())
    };
    queue
        .update_details(&id, title, description)
        .map_err(|err| PiyaError::Queue {
            message: err.to_string(),
        })?;
    Ok(())
}

/// Spawn the CLI worker if there are due pending tasks (safe to call repeatedly).
#[uniffi::export]
pub fn piya_run_pending_tasks() -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })?;
    crate::worker::maybe_auto_run(&settings, &queue).map_err(|err| PiyaError::Queue {
        message: err.to_string(),
    })
}

/// Queue a Notification Center banner (Swift app delivers it with Piya branding).
#[uniffi::export]
pub fn piya_send_notification(title: String, body: String) -> Result<(), PiyaError> {
    let title = if title.trim().is_empty() {
        "Piya".into()
    } else {
        title
    };
    if body.trim().is_empty() {
        return Err(PiyaError::Queue {
            message: "notification body cannot be empty".into(),
        });
    }
    crate::notify::user_alert(&title, body.trim());
    Ok(())
}

/// Ensure Application Support config exists when launched from SwiftUI.
#[uniffi::export]
pub fn piya_bootstrap_app_support() -> Result<String, PiyaError> {
    let settings = load_settings()?;
    if let Some(dir) = config::app_support_dir() {
        if !dir.join(".env").is_file() {
            config::write_app_support_config(&settings).map_err(map_err)?;
        }
    }
    Ok(settings.data_dir.display().to_string())
}

/// Live activity line while a chat turn is in progress (e.g. `$ mdfind …`).
/// Empty string when idle.
#[uniffi::export]
pub fn piya_activity_status() -> String {
    crate::shell::activity_status().unwrap_or_default()
}

/// Stop the in-flight chat turn (LLM + current shell command).
#[uniffi::export]
pub fn piya_cancel_chat() {
    crate::chat_cancel::cancel();
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiUserPrefs {
    pub profile: String,
    pub auto_capture_prefs: bool,
    pub digest_enabled: bool,
    pub digest_hour: u32,
}

#[uniffi::export]
pub fn piya_get_user_prefs() -> Result<FfiUserPrefs, PiyaError> {
    let settings = load_settings()?;
    let prefs = crate::prefs::UserPrefs::load(&settings.data_dir).map_err(map_err)?;
    Ok(FfiUserPrefs {
        profile: prefs.profile,
        auto_capture_prefs: prefs.auto_capture_prefs,
        digest_enabled: prefs.digest_enabled,
        digest_hour: prefs.digest_hour,
    })
}

#[uniffi::export]
pub fn piya_save_user_prefs(prefs: FfiUserPrefs) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let mut stored = crate::prefs::UserPrefs::load(&settings.data_dir).unwrap_or_default();
    stored.profile = prefs.profile;
    stored.auto_capture_prefs = prefs.auto_capture_prefs;
    stored.digest_enabled = prefs.digest_enabled;
    stored.digest_hour = prefs.digest_hour.min(23);
    stored.save(&settings.data_dir).map_err(map_err)
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiAppEnvSettings {
    pub openai_api_key: String,
    pub openai_base_url: String,
    pub model: String,
    pub notify: String,
    pub shell_policy: String,
}

#[uniffi::export]
pub fn piya_get_app_env_settings() -> Result<FfiAppEnvSettings, PiyaError> {
    let _ = load_settings()?;
    let env = config::AppEnvSettings::load_from_env();
    Ok(FfiAppEnvSettings {
        openai_api_key: env.openai_api_key,
        openai_base_url: env.openai_base_url,
        model: env.model,
        notify: env.notify,
        shell_policy: env.shell_policy,
    })
}

#[uniffi::export]
pub fn piya_save_app_env_settings(env: FfiAppEnvSettings) -> Result<(), PiyaError> {
    let payload = config::AppEnvSettings {
        openai_api_key: env.openai_api_key,
        openai_base_url: env.openai_base_url,
        model: env.model,
        notify: env.notify,
        shell_policy: env.shell_policy,
    };
    payload.save_to_app_support().map_err(map_err)?;
    crate::chat_engine::invalidate_llm();
    Ok(())
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiMemoryEntry {
    pub id: String,
    pub text: String,
    pub kind: String,
    pub tags: String,
    pub created_at: String,
    pub source: String,
}

#[uniffi::export]
pub fn piya_list_memories(limit: u32) -> Result<Vec<FfiMemoryEntry>, PiyaError> {
    let settings = load_settings()?;
    let (_llm, memory) = crate::chat_engine::llm_and_memory(&settings).map_err(map_err)?;
    let limit = if limit == 0 { 100 } else { limit as usize };
    let entries = crate::runtime::block_on(async move { memory.list_recent(limit).await })
        .map_err(map_err)?;
    Ok(entries
        .into_iter()
        .map(|e| FfiMemoryEntry {
            id: e.id,
            text: e.text,
            kind: e.kind,
            tags: e.tags,
            created_at: e.created_at,
            source: e.source,
        })
        .collect())
}

#[uniffi::export]
pub fn piya_delete_memory(id: String) -> Result<(), PiyaError> {
    let settings = load_settings()?;
    let (_llm, memory) = crate::chat_engine::llm_and_memory(&settings).map_err(map_err)?;
    crate::runtime::block_on(async move { memory.delete_by_id(&id).await }).map_err(map_err)
}
