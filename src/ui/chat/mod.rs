mod conversation;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod glass;
#[cfg(target_os = "macos")]
mod panel;
#[cfg(target_os = "macos")]
#[allow(deprecated)]
mod spotlight;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tao::event::{ElementState, WindowEvent};
use tao::event_loop::EventLoopWindowTarget;
use tao::keyboard::Key;
use tao::window::{Window, WindowBuilder};
use wry::WebViewBuilder;

use crate::chat_session::{ChatSession, SessionMessage};
use crate::config::Settings;
use crate::tasks::TaskQueue;
use crate::ui::assign::{converse_task, spawn_if_needed, AssignOutcome};

/// Background LLM/task work — run off the UI thread.
pub struct SendRequest {
    pub settings: Settings,
    pub mode: ChatMode,
    pub history: Vec<ChatMessage>,
    pub text: String,
    pub data_dir: PathBuf,
}

pub struct SendComplete {
    pub outcome: AssignOutcome,
    pub text: String,
    pub mode: ChatMode,
    pub settings: Settings,
}

pub enum IpcDispatch {
    Done,
    ShouldClose,
    SpawnSend(SendRequest),
}

impl SendRequest {
    pub fn run(self) -> SendComplete {
        let queue = TaskQueue::new(self.data_dir.join("piya.db"))
            .expect("open task queue for send");
        let outcome = match self.mode {
            ChatMode::Chat => conversation::reply(&self.settings, &self.history, &self.text),
            ChatMode::Task => converse_task(&queue, &self.settings, &self.text),
        };
        SendComplete {
            outcome,
            text: self.text,
            mode: self.mode,
            settings: self.settings,
        }
    }
}

const PANEL_HTML: &str = include_str!("panel.html");

const WELCOME_CHAT: &str = "Hey! I'm Piya 👋\nAsk me anything — I'm here for conversation.\nSwitch to **Task** mode when you want me to queue work, schedules, or save a memory.";
const WELCOME_TASK: &str = "Task mode — tell me what to work on.\nResearch, reports, schedules, **remember that** notes.\nSwitch to **Chat** mode for normal Q&A.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatMode {
    #[default]
    Chat,
    Task,
}

impl ChatMode {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "task" => Self::Task,
            _ => Self::Chat,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Task => "task",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
}

pub struct ChatWindow {
    window: Option<Window>,
    webview: Option<wry::WebView>,
    session_path: Option<PathBuf>,
    mode: ChatMode,
    messages: Vec<ChatMessage>,
    open_settings: bool,
}

impl Default for ChatWindow {
    fn default() -> Self {
        Self {
            window: None,
            webview: None,
            session_path: None,
            mode: ChatMode::Chat,
            messages: Vec::new(),
            open_settings: false,
        }
    }
}

impl ChatWindow {
    pub fn new(data_dir: &PathBuf, open_settings: bool) -> Self {
        let path = ChatSession::legacy_path(data_dir);
        let session = ChatSession::load(data_dir);
        let mode = ChatMode::from_str(if session.mode.is_empty() {
            "chat"
        } else {
            &session.mode
        });
        let messages = session
            .messages
            .into_iter()
            .map(|m| ChatMessage {
                role: m.role,
                text: m.text,
            })
            .collect();
        Self {
            window: None,
            webview: None,
            session_path: Some(path),
            mode,
            messages,
            open_settings,
        }
    }

    pub fn owns_window(&self, id: tao::window::WindowId) -> bool {
        self.window.as_ref().is_some_and(|w| w.id() == id)
    }

    pub fn is_open(&self) -> bool {
        self.window.is_some()
    }

    pub fn keep_on_top(&self) {
        if let Some(window) = &self.window {
            #[cfg(target_os = "macos")]
            spotlight::raise(window);
        }
    }

    pub fn toggle<T>(
        &mut self,
        target: &EventLoopWindowTarget<T>,
        settings: &Settings,
        queue: &TaskQueue,
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<bool> {
        if self.is_open() {
            self.close();
            return Ok(false);
        }
        self.open(target, settings, queue, on_ipc)?;
        Ok(true)
    }

    pub fn open<T>(
        &mut self,
        target: &EventLoopWindowTarget<T>,
        _settings: &Settings,
        _queue: &TaskQueue,
        on_ipc: impl Fn(String) + 'static,
    ) -> Result<()> {
        if let Some(window) = &self.window {
            #[cfg(target_os = "macos")]
            spotlight::focus(window);
            #[cfg(not(target_os = "macos"))]
            window.set_focus();
            self.sync_ui()?;
            return Ok(());
        }

        if self.messages.is_empty() {
            self.messages.push(ChatMessage {
                role: "piya".into(),
                text: welcome_for(self.mode).into(),
            });
            let _ = self.save_session();
        }

        let boot = serde_json::json!({
            "mode": self.mode.as_str(),
            "messages": self.messages,
            "openSettings": self.open_settings,
        });
        let boot_json = serde_json::to_string(&boot)?;
        let init_script = format!("window.__PIYA_BOOT__ = {boot_json};");

        let window = WindowBuilder::new()
            .with_title("Piya")
            .with_inner_size(tao::dpi::LogicalSize::new(680.0, 540.0))
            .with_min_inner_size(tao::dpi::LogicalSize::new(560.0, 420.0))
            .with_max_inner_size(tao::dpi::LogicalSize::new(760.0, 680.0))
            .with_decorations(false)
            .with_always_on_top(true)
            .with_transparent(true)
            .with_resizable(true)
            .build(target)
            .context("create chat window")?;

        #[cfg(target_os = "macos")]
        glass::prepare(&window);

        let webview = WebViewBuilder::new()
            .with_html(PANEL_HTML)
            .with_transparent(true)
            .with_initialization_script(&init_script)
            .with_ipc_handler(move |req| {
                on_ipc(req.body().clone());
            })
            .build(&window)
            .context("create chat webview")?;

        #[cfg(target_os = "macos")]
        spotlight::apply(&window);

        self.window = Some(window);
        self.webview = Some(webview);
        Ok(())
    }

    /// Apply native glass after the event loop has started (safe on Tahoe).
    #[cfg(target_os = "macos")]
    pub fn apply_glass(&self) -> bool {
        if let (Some(window), Some(webview)) = (&self.window, &self.webview) {
            glass::apply(window, webview)
        } else {
            false
        }
    }

    pub fn handle_ipc(
        &mut self,
        raw: &str,
        settings: &Settings,
        _queue: &TaskQueue,
    ) -> Result<IpcDispatch> {
        let payload: serde_json::Value = serde_json::from_str(raw).unwrap_or_default();
        let msg_type = payload.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "close" => {
                self.close();
                return Ok(IpcDispatch::ShouldClose);
            }
            "set_mode" => {
                let Some(mode_str) = payload.get("mode").and_then(|v| v.as_str()) else {
                    return Ok(IpcDispatch::Done);
                };
                let new_mode = ChatMode::from_str(mode_str);
                if new_mode == self.mode {
                    self.sync_ui()?;
                    return Ok(IpcDispatch::Done);
                }
                self.mode = new_mode;
                let notice = match self.mode {
                    ChatMode::Chat => {
                        "Switched to **Chat** mode — ask me anything.".to_string()
                    }
                    ChatMode::Task => {
                        "Switched to **Task** mode — tell me what to work on.".to_string()
                    }
                };
                self.messages.push(ChatMessage {
                    role: "piya".into(),
                    text: notice.clone(),
                });
                self.save_session()?;
                self.sync_ui()?;
            }
            "send" => {
                let Some(text) = payload.get("text").and_then(|v| v.as_str()) else {
                    return Ok(IpcDispatch::Done);
                };
                if let Some(mode_str) = payload.get("mode").and_then(|v| v.as_str()) {
                    self.mode = ChatMode::from_str(mode_str);
                }

                let history = self.messages.clone();
                return Ok(IpcDispatch::SpawnSend(SendRequest {
                    settings: settings.clone(),
                    mode: self.mode,
                    history,
                    text: text.to_string(),
                    data_dir: settings.data_dir.clone(),
                }));
            }
            "get_mcp" => {
                let cfg = crate::mcp_config::McpConfig::load(&settings.data_dir)?;
                self.push_mcp_config(&cfg)?;
            }
            "save_mcp" => {
                let Some(servers_val) = payload.get("servers") else {
                    return Ok(IpcDispatch::Done);
                };
                let servers: std::collections::HashMap<String, crate::mcp_config::McpServerEntry> =
                    serde_json::from_value(servers_val.clone())?;
                let cfg = crate::mcp_config::McpConfig { servers };
                cfg.save(&settings.data_dir)?;
                self.push_mcp_saved()?;
            }
            _ => {}
        }
        Ok(IpcDispatch::Done)
    }

    pub fn complete_send(&mut self, result: SendComplete) -> Result<()> {
        self.messages.push(ChatMessage {
            role: "user".into(),
            text: result.text,
        });
        self.messages.push(ChatMessage {
            role: "piya".into(),
            text: result.outcome.reply.clone(),
        });
        self.save_session()?;
        self.push_reply(&result.outcome.reply)?;
        if result.mode == ChatMode::Task {
            spawn_if_needed(&result.settings, &result.outcome);
        }
        Ok(())
    }

    pub fn handle_window_event(&mut self, event: &WindowEvent) -> bool {
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if key.state == ElementState::Pressed
                && matches!(key.logical_key, Key::Escape)
            {
                self.close();
                return true;
            }
        }
        if matches!(event, WindowEvent::CloseRequested) {
            self.close();
            return true;
        }
        false
    }

    pub fn close(&mut self) {
        self.webview = None;
        self.window = None;
    }

    /// Open MCP settings in an already-visible panel (menu bar → MCP Settings).
    pub fn drain_commands(&mut self, data_dir: &PathBuf, settings: &Settings) -> Result<()> {
        let path = data_dir.join("piya-chat-cmd.json");
        if !path.is_file() {
            return Ok(());
        }
        let raw = std::fs::read_to_string(&path)?;
        let _ = std::fs::remove_file(&path);
        let payload: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
        match payload.get("type").and_then(|v| v.as_str()) {
            Some("open_settings") => self.show_settings(settings)?,
            _ => {}
        }
        Ok(())
    }

    pub fn show_settings(&self, settings: &Settings) -> Result<()> {
        if let Some(webview) = &self.webview {
            let _ = webview.evaluate_script("window.openSettingsPanel && window.openSettingsPanel()");
            let cfg = crate::mcp_config::McpConfig::load(&settings.data_dir)?;
            let json = serde_json::to_string(&cfg)?;
            let _ = webview.evaluate_script(&format!("window.loadMcpConfig({json})"));
        }
        self.keep_on_top();
        Ok(())
    }

    fn sync_ui(&self) -> Result<()> {
        if let Some(webview) = &self.webview {
            let boot = serde_json::json!({
                "mode": self.mode.as_str(),
                "messages": self.messages,
            });
            let script = format!("window.boot({})", serde_json::to_string(&boot)?);
            let _ = webview.evaluate_script(&script);
        }
        Ok(())
    }

    fn push_reply(&self, text: &str) -> Result<()> {
        if let Some(webview) = &self.webview {
            let encoded = serde_json::to_string(text)?;
            let _ = webview.evaluate_script(&format!("window.onReply({encoded})"));
        }
        Ok(())
    }

    fn push_mcp_config(&self, cfg: &crate::mcp_config::McpConfig) -> Result<()> {
        if let Some(webview) = &self.webview {
            let json = serde_json::to_string(cfg)?;
            let _ = webview.evaluate_script(&format!("window.loadMcpConfig({json})"));
        }
        Ok(())
    }

    fn push_mcp_saved(&self) -> Result<()> {
        if let Some(webview) = &self.webview {
            let _ = webview.evaluate_script("window.onMcpSaved()");
        }
        Ok(())
    }

    fn save_session(&self) -> Result<()> {
        if let Some(path) = &self.session_path {
            let session = ChatSession {
                mode: self.mode.as_str().to_string(),
                messages: self
                    .messages
                    .iter()
                    .map(|m| SessionMessage {
                        role: m.role.clone(),
                        text: m.text.clone(),
                    })
                    .collect(),
            };
            session.save(path.parent().unwrap_or(path))
        } else {
            Ok(())
        }
    }
}

fn welcome_for(mode: ChatMode) -> &'static str {
    match mode {
        ChatMode::Chat => WELCOME_CHAT,
        ChatMode::Task => WELCOME_TASK,
    }
}

pub type SharedChat = Rc<RefCell<ChatWindow>>;

#[cfg(target_os = "macos")]
pub use panel::{run_panel, spawn_or_open_settings, spawn_or_toggle};

pub fn shared(data_dir: &PathBuf, open_settings: bool) -> SharedChat {
    Rc::new(RefCell::new(ChatWindow::new(data_dir, open_settings)))
}
