//! UniFFI surface for the SwiftUI macOS app.

use std::collections::HashMap;

use crate::chat_service::{self, ChatTurn};
use crate::chat_session::{ChatSession, SessionMessage};
use crate::config::{self, Settings};
use crate::mcp_config::{McpConfig, McpServerEntry};
use crate::tasks::{TaskQueue, TaskStatus};

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
pub struct FfiAttachment {
    pub id: String,
    pub name: String,
    pub path: String,
    pub resolved_path: String,
    pub mime: String,
    pub kind: String,
    pub text: String,
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiChatTurn {
    pub role: String,
    pub text: String,
    pub attachments: Vec<FfiAttachment>,
}

fn att_to_ffi(a: crate::attachments::ChatAttachment, data_dir: &std::path::Path) -> FfiAttachment {
    let resolved_path = a.resolve_path(data_dir).display().to_string();
    FfiAttachment {
        id: a.id,
        name: a.name,
        path: a.path,
        resolved_path,
        mime: a.mime,
        kind: a.kind,
        text: a.text.unwrap_or_default(),
    }
}

fn ffi_to_att(a: FfiAttachment) -> crate::attachments::ChatAttachment {
    crate::attachments::ChatAttachment {
        id: a.id,
        name: a.name,
        path: a.path,
        mime: a.mime,
        kind: a.kind,
        text: if a.text.trim().is_empty() {
            None
        } else {
            Some(a.text)
        },
    }
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

fn session_to_ffi(session: ChatSession, data_dir: &std::path::Path) -> FfiChatSession {
    FfiChatSession {
        mode: session.mode,
        messages: session
            .messages
            .into_iter()
            .map(|m| FfiChatTurn {
                role: m.role,
                text: m.text,
                attachments: m
                    .attachments
                    .into_iter()
                    .map(|a| att_to_ffi(a, data_dir))
                    .collect(),
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
                attachments: m.attachments.into_iter().map(ffi_to_att).collect(),
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
    Ok(session_to_ffi(session, &settings.data_dir))
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
pub fn piya_stage_attachment(
    session_id: String,
    source_path: String,
    display_name: String,
    extracted_text: String,
) -> Result<FfiAttachment, PiyaError> {
    let settings = load_settings()?;
    let att = crate::attachments::stage_attachment(
        &settings,
        &session_id,
        std::path::Path::new(&source_path),
        (!display_name.trim().is_empty()).then_some(display_name.as_str()),
        (!extracted_text.trim().is_empty()).then_some(extracted_text.as_str()),
    )
    .map_err(|err| PiyaError::Chat {
        message: format!("{err:#}"),
    })?;
    Ok(att_to_ffi(att, &settings.data_dir))
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
    piya_send_with_attachments(message, Vec::new(), mode, history)
}

#[uniffi::export]
pub fn piya_send_with_attachments(
    message: String,
    attachments: Vec<FfiAttachment>,
    mode: FfiChatMode,
    history: Vec<FfiChatTurn>,
) -> Result<String, PiyaError> {
    let settings = load_settings()?;
    let turns: Vec<ChatTurn> = history
        .into_iter()
        .map(|t| ChatTurn {
            role: t.role,
            text: t.text,
            attachments: t.attachments.into_iter().map(ffi_to_att).collect(),
        })
        .collect();
    let user_atts: Vec<crate::attachments::ChatAttachment> =
        attachments.into_iter().map(ffi_to_att).collect();
    let intent = match mode {
        FfiChatMode::Chat => chat_service::ChatIntent::Chat,
        FfiChatMode::Task => chat_service::ChatIntent::Task,
    };
    chat_service::chat_reply_with_attachments(&settings, &turns, &message, &user_atts, intent)
        .map_err(|err| PiyaError::Chat {
            message: format!("{err:#}"),
        })
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
    pub notify: String,
    pub shell_policy: String,
}

#[uniffi::export]
pub fn piya_get_app_env_settings() -> Result<FfiAppEnvSettings, PiyaError> {
    let _ = load_settings()?;
    let env = config::AppEnvSettings::load_from_env();
    Ok(FfiAppEnvSettings {
        notify: env.notify,
        shell_policy: env.shell_policy,
    })
}

#[uniffi::export]
pub fn piya_save_app_env_settings(env: FfiAppEnvSettings) -> Result<(), PiyaError> {
    let payload = config::AppEnvSettings {
        notify: env.notify,
        shell_policy: env.shell_policy,
    };
    payload.save_to_app_support().map_err(map_err)?;
    Ok(())
}

// ----- Models ---------------------------------------------------------------

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiProvider {
    pub id: String,
    pub label: String,
    pub default_base_url: String,
    pub needs_key: bool,
    pub local: bool,
    pub key_url: String,
    pub supports_embeddings: bool,
    pub default_embedding_model: String,
    pub suggested_model: String,
    /// Key already saved for this provider (so switching back needs no re-paste).
    pub saved_key: String,
    pub saved_base_url: String,
}

#[uniffi::export]
pub fn piya_list_providers() -> Vec<FfiProvider> {
    let _ = load_settings();
    crate::providers::PROVIDERS
        .iter()
        .map(|p| FfiProvider {
            id: p.id.into(),
            label: p.label.into(),
            default_base_url: p.default_base_url.into(),
            needs_key: p.needs_key,
            local: p.local,
            key_url: p.key_url.into(),
            supports_embeddings: p.embeddings,
            default_embedding_model: p.default_embedding_model.into(),
            suggested_model: p.suggested_model.into(),
            saved_key: crate::providers::saved_key(p.id),
            saved_base_url: crate::providers::saved_base_url(p.id),
        })
        .collect()
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiModelSettings {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// `none` turns memory search off.
    pub embed_provider: String,
    pub embed_api_key: String,
    pub embed_base_url: String,
    pub embedding_model: String,
}

#[uniffi::export]
pub fn piya_get_model_settings() -> Result<FfiModelSettings, PiyaError> {
    let _ = load_settings()?;
    let m = config::ModelSettings::load_from_env();
    Ok(FfiModelSettings {
        provider: m.provider,
        api_key: m.api_key,
        base_url: m.base_url,
        model: m.model,
        embed_provider: m.embed_provider,
        embed_api_key: m.embed_api_key,
        embed_base_url: m.embed_base_url,
        embedding_model: m.embedding_model,
    })
}

#[uniffi::export]
pub fn piya_save_model_settings(settings: FfiModelSettings) -> Result<(), PiyaError> {
    let _ = load_settings()?;
    let m = config::ModelSettings {
        provider: settings.provider,
        api_key: settings.api_key,
        base_url: settings.base_url,
        model: settings.model,
        embed_provider: settings.embed_provider,
        embed_api_key: settings.embed_api_key,
        embed_base_url: settings.embed_base_url,
        embedding_model: settings.embedding_model,
    };
    m.save_to_app_support().map_err(map_err)?;
    crate::chat_engine::invalidate_llm();
    // A running worker still holds the old model; it restarts on the next task.
    restart_idle_worker();
    Ok(())
}

/// Stop a worker that has nothing in progress so the next task starts one with the new model.
fn restart_idle_worker() {
    let Ok(settings) = load_settings() else { return };
    let Ok(queue) = TaskQueue::new(settings.data_dir.join("piya.db")) else { return };
    if queue.count(Some(crate::tasks::TaskStatus::InProgress)).unwrap_or(1) > 0 {
        return;
    }
    let lock = crate::worker::worker_lock_path(&settings.data_dir);
    if let Ok(pid) = std::fs::read_to_string(&lock) {
        if let Ok(pid) = pid.trim().parse::<i32>() {
            let _ = std::process::Command::new("kill").args(["-9", &pid.to_string()]).status();
        }
        let _ = std::fs::remove_file(&lock);
    }
    if queue.count_scheduled().unwrap_or(0) > 0 || queue.count_due_pending().unwrap_or(0) > 0 {
        let _ = crate::worker::spawn_worker(&settings, &["run", "--continuous"]);
    }
}

/// Live model ids from the provider account. `embeddings` lists embedding models only.
#[uniffi::export]
pub fn piya_list_models(
    provider: String,
    api_key: String,
    base_url: String,
    embeddings: bool,
) -> Result<Vec<String>, PiyaError> {
    let conn = crate::providers::Connection { provider, api_key, base_url };
    crate::runtime::block_on(async move { crate::providers::list_models(&conn, embeddings).await })
        .map_err(map_err)
}

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiModelProbe {
    pub ok: bool,
    pub message: String,
}

/// Send one tiny request with the given connection + model (chat or embeddings).
#[uniffi::export]
pub fn piya_test_model(
    provider: String,
    api_key: String,
    base_url: String,
    model: String,
    embeddings: bool,
) -> FfiModelProbe {
    let result = (|| -> anyhow::Result<String> {
        let mut settings = load_settings().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if embeddings {
            settings.embed_provider = provider;
            settings.embed_api_key = api_key;
            settings.embed_base_url = base_url;
            settings.embedding_model = model;
        } else {
            settings.provider = provider;
            settings.api_key = api_key;
            settings.base_url = base_url;
            settings.model = model;
        }
        let llm = crate::llm::LlmClient::new(&settings)?;
        crate::runtime::block_on(async move {
            if embeddings {
                if llm.embedding_id().is_none() {
                    anyhow::bail!("choose a provider and embeddings model first");
                }
                let v = llm.embed(&["ping".to_string()]).await?;
                let n = v.first().map(|e| e.len()).unwrap_or(0);
                Ok(format!("Embeddings work ({n} dimensions)."))
            } else {
                let reply = llm.chat("Reply with exactly: OK", "ping", false).await?;
                let reply: String = reply.trim().chars().take(40).collect();
                Ok(format!("Connected to {} — replied \"{reply}\".", llm.describe()))
            }
        })
    })();
    match result {
        Ok(message) => FfiModelProbe { ok: true, message },
        Err(err) => FfiModelProbe { ok: false, message: format!("{err:#}") },
    }
}

/// Warning when a pasted key looks like another provider's (empty when fine/unknown).
#[uniffi::export]
pub fn piya_key_warning(provider: String, api_key: String) -> String {
    crate::providers::key_mismatch_warning(&provider, &api_key).unwrap_or_default()
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

#[derive(uniffi::Record, Clone, Debug)]
pub struct FfiUpdateCheck {
    pub current_version: String,
    pub latest_version: String,
    pub update_available: bool,
    pub release_url: String,
    pub release_notes: String,
    pub published_at: String,
    pub has_asset: bool,
}

#[uniffi::export]
pub fn piya_check_for_update() -> Result<FfiUpdateCheck, PiyaError> {
    let check = crate::updater::check_for_update().map_err(map_err)?;
    Ok(FfiUpdateCheck {
        current_version: check.current_version,
        latest_version: check.latest_version,
        update_available: check.update_available,
        release_url: check.release_url,
        release_notes: check.release_notes,
        published_at: check.published_at,
        has_asset: check.asset_url.is_some(),
    })
}

/// Downloads and stages the latest release in `/Applications/.Piya-update.noindex`,
/// verifies SHA-256 and code signature, and spawns a detached helper that atomically
/// swaps `/Applications/Piya.app` and relaunches once this GUI process exits.
#[uniffi::export]
pub fn piya_install_update_and_relaunch() -> Result<String, PiyaError> {
    crate::updater::prepare_gui_self_update().map_err(map_err)
}

// ----- Major alerts -----------------------------------------------------------

/// "Snooze" on a major alert: show it again (as a major alert) in `minutes`.
#[uniffi::export]
pub fn piya_snooze_alert(title: String, body: String, minutes: u32) -> Result<String, PiyaError> {
    let settings = load_settings()?;
    let queue = TaskQueue::new(settings.data_dir.join("piya.db")).map_err(map_err)?;
    let args = serde_json::json!({
        "kind": "reminder",
        "title": title,
        "message": if body.trim().is_empty() { title.clone() } else { body },
        "in_minutes": minutes.max(1),
        "major": true,
    });
    let out = crate::chat_tools::schedule::create(&queue, &args, chrono::Utc::now()).map_err(map_err)?;
    let _ = crate::worker::spawn_worker(&settings, &["run", "--continuous"]);
    Ok(out)
}

/// Settings → Notifications → "Test major alert".
#[uniffi::export]
pub fn piya_test_major_alert() {
    let _ = load_settings();
    crate::notify::major_alert("Test major alert", "This stays on screen until you dismiss it.");
}
