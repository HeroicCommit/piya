//! Persisted chat sessions under `data/chat-sessions/`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ChatSession {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionMeta {
    pub id: String,
    pub title: String,
    /// When the session was created — used for sidebar ordering.
    #[serde(default)]
    pub created_at: i64,
    /// Last time messages were actually changed (not mere selection).
    pub updated_at: i64,
    pub mode: String,
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct SessionManifest {
    #[serde(default)]
    active_id: String,
    #[serde(default)]
    sessions: Vec<SessionMeta>,
}

pub struct ChatSessionStore {
    data_dir: PathBuf,
    manifest: SessionManifest,
}

impl ChatSession {
    /// Legacy single-file path (pre multi-session).
    pub fn legacy_path(data_dir: &Path) -> PathBuf {
        data_dir.join("chat-session.json")
    }

    pub fn load(data_dir: &Path) -> Self {
        ChatSessionStore::open(data_dir)
            .and_then(|store| store.load_active())
            .unwrap_or_default()
    }

    pub fn save(&self, data_dir: &Path) -> Result<()> {
        let mut store = ChatSessionStore::open(data_dir)?;
        store.save_active(self)
    }
}

impl ChatSessionStore {
    pub fn sessions_dir(data_dir: &Path) -> PathBuf {
        data_dir.join("chat-sessions")
    }

    fn manifest_path(data_dir: &Path) -> PathBuf {
        Self::sessions_dir(data_dir).join("manifest.json")
    }

    fn session_path(data_dir: &Path, id: &str) -> PathBuf {
        Self::sessions_dir(data_dir).join(format!("{id}.json"))
    }

    pub fn open(data_dir: &Path) -> Result<Self> {
        Self::migrate_legacy(data_dir)?;
        std::fs::create_dir_all(Self::sessions_dir(data_dir))?;
        let path = Self::manifest_path(data_dir);
        let manifest = if path.is_file() {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("read {}", path.display()))?;
            serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?
        } else {
            SessionManifest::default()
        };

        let mut store = Self {
            data_dir: data_dir.to_path_buf(),
            manifest,
        };
        store.normalize_created_at();
        if store.manifest.sessions.is_empty() {
            store.create("chat")?;
        } else if store.manifest.active_id.is_empty()
            || !store.manifest.sessions.iter().any(|s| s.id == store.manifest.active_id)
        {
            store.manifest.active_id = store.manifest.sessions[0].id.clone();
            store.sort_sessions();
            store.save_manifest()?;
        } else {
            // Keep list ordered by creation even if an older manifest was unsorted.
            let before = store.manifest.sessions.clone();
            store.sort_sessions();
            if store.manifest.sessions != before {
                store.save_manifest()?;
            }
        }
        Ok(store)
    }

    /// Backfill `created_at` for sessions saved before that field existed.
    fn normalize_created_at(&mut self) {
        for meta in &mut self.manifest.sessions {
            if meta.created_at <= 0 {
                meta.created_at = meta.updated_at;
            }
        }
    }

    fn migrate_legacy(data_dir: &Path) -> Result<()> {
        let manifest_path = Self::manifest_path(data_dir);
        if manifest_path.is_file() {
            return Ok(());
        }

        let legacy_path = ChatSession::legacy_path(data_dir);
        if !legacy_path.is_file() {
            return Ok(());
        }

        let Ok(raw) = std::fs::read_to_string(&legacy_path) else {
            return Ok(());
        };

        let session = if let Ok(session) = serde_json::from_str::<ChatSession>(&raw) {
            session
        } else if let Ok(messages) = serde_json::from_str::<Vec<SessionMessage>>(&raw) {
            ChatSession {
                mode: "chat".into(),
                messages,
            }
        } else {
            return Ok(());
        };

        std::fs::create_dir_all(Self::sessions_dir(data_dir))?;
        let id = Uuid::new_v4().to_string();
        let mode = if session.mode.is_empty() {
            "chat".to_string()
        } else {
            session.mode.clone()
        };
        let title = title_for_session(&session);
        let now = now_unix();
        let meta = SessionMeta {
            id: id.clone(),
            title,
            created_at: now,
            updated_at: now,
            mode: mode.clone(),
        };

        let session_path = Self::session_path(data_dir, &id);
        let json = serde_json::to_string_pretty(&session)?;
        std::fs::write(&session_path, json)
            .with_context(|| format!("write {}", session_path.display()))?;

        let manifest = SessionManifest {
            active_id: id,
            sessions: vec![meta],
        };
        let manifest_json = serde_json::to_string_pretty(&manifest)?;
        std::fs::write(&manifest_path, manifest_json)
            .with_context(|| format!("write {}", manifest_path.display()))?;

        Ok(())
    }

    pub fn list(&self) -> &[SessionMeta] {
        &self.manifest.sessions
    }

    pub fn active_id(&self) -> &str {
        &self.manifest.active_id
    }

    pub fn load_active(&self) -> Result<ChatSession> {
        self.load(&self.manifest.active_id)
    }

    pub fn load(&self, id: &str) -> Result<ChatSession> {
        let path = Self::session_path(&self.data_dir, id);
        if !path.is_file() {
            return Ok(ChatSession::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save_active(&mut self, session: &ChatSession) -> Result<()> {
        let id = self.manifest.active_id.clone();
        self.save(&id, session)
    }

    pub fn save(&mut self, id: &str, session: &ChatSession) -> Result<()> {
        let path = Self::session_path(&self.data_dir, id);
        let previous = self.load(id).unwrap_or_default();
        let content_changed = !sessions_equal(&previous, session);

        let json = serde_json::to_string_pretty(session)?;
        std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;

        if let Some(meta) = self.manifest.sessions.iter_mut().find(|s| s.id == id) {
            // Only bump "last updated" when the user actually changed the conversation.
            if content_changed {
                meta.updated_at = now_unix();
            }
            meta.mode = if session.mode.is_empty() {
                "chat".into()
            } else {
                session.mode.clone()
            };
            if meta.title == "New chat" || content_changed {
                let next_title = title_for_session(session);
                if !next_title.is_empty() {
                    meta.title = next_title;
                }
            }
        }
        // Order is by creation date — selecting / saving must not reshuffle the list.
        self.save_manifest()
    }

    pub fn set_active(&mut self, id: &str) -> Result<()> {
        if !self.manifest.sessions.iter().any(|s| s.id == id) {
            anyhow::bail!("unknown session {id}");
        }
        self.manifest.active_id = id.to_string();
        self.save_manifest()
    }

    pub fn create(&mut self, mode: &str) -> Result<SessionMeta> {
        let id = Uuid::new_v4().to_string();
        let mode = if mode.is_empty() { "chat" } else { mode }.to_string();
        // Timestamps have 1s resolution: keep a new session strictly newer than every
        // existing one so list order is deterministic even for rapid "new chat" clicks.
        let newest = self
            .manifest
            .sessions
            .iter()
            .map(|m| m.created_at)
            .max()
            .unwrap_or(0);
        let now = now_unix().max(newest + 1);
        let meta = SessionMeta {
            id: id.clone(),
            title: "New chat".into(),
            created_at: now,
            updated_at: now,
            mode: mode.clone(),
        };
        let session = ChatSession {
            mode,
            messages: Vec::new(),
        };
        let path = Self::session_path(&self.data_dir, &id);
        let json = serde_json::to_string_pretty(&session)?;
        std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;

        self.manifest.sessions.push(meta.clone());
        self.manifest.active_id = id;
        self.sort_sessions();
        self.save_manifest()?;
        Ok(meta)
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        if self.manifest.sessions.len() <= 1 {
            anyhow::bail!("cannot delete the last session");
        }
        let path = Self::session_path(&self.data_dir, id);
        if path.is_file() {
            std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        }
        self.manifest.sessions.retain(|s| s.id != id);
        if self.manifest.active_id == id {
            self.manifest.active_id = self.manifest.sessions[0].id.clone();
        }
        self.save_manifest()
    }

    fn sort_sessions(&mut self) {
        // Newest created first; stable by id if timestamps match.
        self.manifest.sessions.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.id.cmp(&a.id))
        });
    }

    fn save_manifest(&self) -> Result<()> {
        let path = Self::manifest_path(&self.data_dir);
        let json = serde_json::to_string_pretty(&self.manifest)?;
        std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sessions_equal(a: &ChatSession, b: &ChatSession) -> bool {
    a.mode == b.mode
        && a.messages.len() == b.messages.len()
        && a.messages
            .iter()
            .zip(b.messages.iter())
            .all(|(x, y)| x.role == y.role && x.text == y.text)
}

fn title_for_session(session: &ChatSession) -> String {
    session
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| truncate_title(&m.text))
        .unwrap_or_else(|| "New chat".into())
}

fn truncate_title(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let max = 48;
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        let trimmed: String = collapsed.chars().take(max).collect();
        format!("{trimmed}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("piya-session-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = ChatSessionStore::open(&dir).unwrap();
        let id = store.active_id().to_string();
        let session = ChatSession {
            mode: "chat".into(),
            messages: vec![SessionMessage {
                role: "user".into(),
                text: "hello world".into(),
            }],
        };
        store.save(&id, &session).unwrap();
        let back = store.load(&id).unwrap();
        assert_eq!(back.messages.len(), 1);
        assert_eq!(store.list()[0].title, "hello world");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sorts_by_created_not_updated_and_select_does_not_bump() {
        let dir = std::env::temp_dir().join(format!("piya-session-order-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut store = ChatSessionStore::open(&dir).unwrap();
        let first = store.active_id().to_string();

        // Create a second (newer) session — should appear first.
        let second = store.create("chat").unwrap().id;
        assert_eq!(store.list()[0].id, second);
        assert_eq!(store.list()[1].id, first);

        // Touch the older session with a real message change.
        let session = ChatSession {
            mode: "chat".into(),
            messages: vec![SessionMessage {
                role: "user".into(),
                text: "hello".into(),
            }],
        };
        store.save(&first, &session).unwrap();
        // Order must stay creation-based (second still first).
        assert_eq!(store.list()[0].id, second);
        assert_eq!(store.list()[1].id, first);
        let updated_after_save = store.list().iter().find(|s| s.id == first).unwrap().updated_at;

        // Re-save identical content (like selecting away) must not bump updated_at.
        store.save(&first, &session).unwrap();
        let updated_again = store.list().iter().find(|s| s.id == first).unwrap().updated_at;
        assert_eq!(updated_after_save, updated_again);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_migration() {
        let dir = std::env::temp_dir().join(format!("piya-legacy-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = ChatSession {
            mode: "task".into(),
            messages: vec![SessionMessage {
                role: "user".into(),
                text: "queue a report".into(),
            }],
        };
        let json = serde_json::to_string_pretty(&legacy).unwrap();
        std::fs::write(ChatSession::legacy_path(&dir), json).unwrap();

        let store = ChatSessionStore::open(&dir).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].title, "queue a report");
        assert_eq!(store.load_active().unwrap().mode, "task");
        let _ = std::fs::remove_dir_all(dir);
    }
}
