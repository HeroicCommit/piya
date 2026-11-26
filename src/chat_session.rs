//! Persisted chat sessions in SQLite (`chat_sessions` / `chat_messages` in `piya.db`).
//!
//! Older installs kept chats as JSON files under `data/chat-sessions/` (and before
//! that a single `chat-session.json`). They are imported once on first open; the old
//! folder is renamed to `chat-sessions.migrated` and kept as a backup.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::attachments::ChatAttachment;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<ChatAttachment>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
pub struct ChatSession {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
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

/// Shape of the legacy `chat-sessions/manifest.json`.
#[derive(Deserialize, Default)]
struct LegacyManifest {
    #[serde(default)]
    active_id: String,
    #[serde(default)]
    sessions: Vec<SessionMeta>,
}

const ACTIVE_KEY: &str = "chat_active_session";
const JSON_IMPORTED_KEY: &str = "chat_json_imported";

pub struct ChatSessionStore {
    conn: Connection,
    sessions: Vec<SessionMeta>,
    active_id: String,
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

    pub fn open(data_dir: &Path) -> Result<Self> {
        let conn = crate::db::open(data_dir)?;
        import_legacy_json(&conn, data_dir)?;
        let mut store = Self {
            conn,
            sessions: Vec::new(),
            active_id: String::new(),
        };
        store.reload()?;
        if store.sessions.is_empty() {
            store.create("chat")?;
        } else if !store.sessions.iter().any(|s| s.id == store.active_id) {
            let first = store.sessions[0].id.clone();
            store.set_active(&first)?;
        }
        Ok(store)
    }

    fn reload(&mut self) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT id, title, created_at, updated_at, mode FROM chat_sessions
             ORDER BY created_at DESC, id DESC",
        )?;
        self.sessions = stmt
            .query_map([], |r| {
                Ok(SessionMeta {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    created_at: r.get(2)?,
                    updated_at: r.get(3)?,
                    mode: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        self.active_id = crate::db::get_state(&self.conn, ACTIVE_KEY)?.unwrap_or_default();
        Ok(())
    }

    pub fn list(&self) -> &[SessionMeta] {
        &self.sessions
    }

    pub fn active_id(&self) -> &str {
        &self.active_id
    }

    pub fn load_active(&self) -> Result<ChatSession> {
        self.load(&self.active_id)
    }

    pub fn load(&self, id: &str) -> Result<ChatSession> {
        let mode = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.mode.clone())
            .unwrap_or_default();
        Ok(ChatSession {
            mode,
            messages: load_messages(&self.conn, id)?,
        })
    }

    pub fn save_active(&mut self, session: &ChatSession) -> Result<()> {
        let id = self.active_id.clone();
        self.save(&id, session)
    }

    /// Replace a session's messages. Unchanged messages keep their ids (and search
    /// index rows); only differing positions are rewritten.
    pub fn save(&mut self, id: &str, session: &ChatSession) -> Result<()> {
        let Some(meta) = self.sessions.iter().find(|s| s.id == id).cloned() else {
            anyhow::bail!("unknown session {id}");
        };
        let tx = self.conn.transaction()?;
        let existing = load_messages_with_ids(&tx, id)?;
        let now = now_unix();
        let mut changed = existing.len() != session.messages.len();
        for (seq, msg) in session.messages.iter().enumerate() {
            match existing.get(seq) {
                Some((msg_id, old))
                    if old.role == msg.role
                        && old.text == msg.text
                        && old.attachments == msg.attachments =>
                {
                    let _ = msg_id;
                }
                Some((msg_id, _)) => {
                    changed = true;
                    tx.execute(
                        "UPDATE chat_messages SET role = ?1, text = ?2, created_at = ?3
                         WHERE session_id = ?4 AND seq = ?5",
                        params![msg.role, msg.text, now, id, seq as i64],
                    )?;
                    sync_message_attachments(&tx, msg_id, &msg.attachments, now)?;
                }
                None => {
                    changed = true;
                    let msg_id = Uuid::new_v4().to_string();
                    tx.execute(
                        "INSERT INTO chat_messages(id, session_id, seq, role, text, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params![msg_id, id, seq as i64, msg.role, msg.text, now],
                    )?;
                    sync_message_attachments(&tx, &msg_id, &msg.attachments, now)?;
                }
            }
        }
        if existing.len() > session.messages.len() {
            tx.execute(
                "DELETE FROM chat_messages WHERE session_id = ?1 AND seq >= ?2",
                params![id, session.messages.len() as i64],
            )?;
        }
        let mode = if session.mode.is_empty() { "chat".to_string() } else { session.mode.clone() };
        let mut title = meta.title.clone();
        if meta.title == "New chat" || changed {
            let next = title_for_session(session);
            if !next.is_empty() {
                title = next;
            }
        }
        // Only bump "last updated" when the conversation actually changed.
        let updated_at = if changed { now } else { meta.updated_at };
        tx.execute(
            "UPDATE chat_sessions SET title = ?1, mode = ?2, updated_at = ?3 WHERE id = ?4",
            params![title, mode, updated_at, id],
        )?;
        tx.commit()?;
        self.reload()
    }

    pub fn set_active(&mut self, id: &str) -> Result<()> {
        if !self.sessions.iter().any(|s| s.id == id) {
            anyhow::bail!("unknown session {id}");
        }
        crate::db::set_state(&self.conn, ACTIVE_KEY, id)?;
        self.active_id = id.to_string();
        Ok(())
    }

    pub fn create(&mut self, mode: &str) -> Result<SessionMeta> {
        let id = Uuid::new_v4().to_string();
        let mode = if mode.is_empty() { "chat" } else { mode }.to_string();
        // Timestamps have 1s resolution: keep a new session strictly newer than every
        // existing one so list order is deterministic even for rapid "new chat" clicks.
        let newest = self.sessions.iter().map(|m| m.created_at).max().unwrap_or(0);
        let now = now_unix().max(newest + 1);
        self.conn.execute(
            "INSERT INTO chat_sessions(id, title, mode, created_at, updated_at) VALUES (?1, 'New chat', ?2, ?3, ?3)",
            params![id, mode, now],
        )?;
        crate::db::set_state(&self.conn, ACTIVE_KEY, &id)?;
        self.reload()?;
        Ok(SessionMeta {
            id,
            title: "New chat".into(),
            created_at: now,
            updated_at: now,
            mode,
        })
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        if self.sessions.len() <= 1 {
            anyhow::bail!("cannot delete the last session");
        }
        self.conn.execute("DELETE FROM chat_messages WHERE session_id = ?1", params![id])?;
        self.conn.execute("DELETE FROM chat_sessions WHERE id = ?1", params![id])?;
        self.reload()?;
        if self.active_id == id || !self.sessions.iter().any(|s| s.id == self.active_id) {
            let first = self.sessions[0].id.clone();
            self.set_active(&first)?;
        }
        Ok(())
    }
}

fn load_messages(conn: &Connection, session_id: &str) -> Result<Vec<SessionMessage>> {
    Ok(load_messages_with_ids(conn, session_id)?
        .into_iter()
        .map(|(_, m)| m)
        .collect())
}

fn load_messages_with_ids(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<(String, SessionMessage)>> {
    let mut att_stmt = conn.prepare(
        "SELECT a.message_id, a.id, a.name, a.path, a.mime, a.kind, a.text
         FROM attachments a
         JOIN chat_messages m ON m.id = a.message_id
         WHERE m.session_id = ?1
         ORDER BY a.rowid",
    )?;
    let mut by_msg: std::collections::HashMap<String, Vec<ChatAttachment>> =
        std::collections::HashMap::new();
    let att_rows = att_stmt.query_map(params![session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            ChatAttachment {
                id: r.get(1)?,
                name: r.get(2)?,
                path: r.get(3)?,
                mime: r.get(4)?,
                kind: r.get(5)?,
                text: r.get(6)?,
            },
        ))
    })?;
    for row in att_rows {
        let (mid, att) = row?;
        by_msg.entry(mid).or_default().push(att);
    }

    let mut stmt = conn.prepare(
        "SELECT id, role, text FROM chat_messages WHERE session_id = ?1 ORDER BY seq",
    )?;
    let rows = stmt
        .query_map(params![session_id], |r| {
            let id: String = r.get(0)?;
            let attachments = by_msg.remove(&id).unwrap_or_default();
            Ok((
                id,
                SessionMessage {
                    role: r.get(1)?,
                    text: r.get(2)?,
                    attachments,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn sync_message_attachments(
    conn: &Connection,
    message_id: &str,
    attachments: &[ChatAttachment],
    now: i64,
) -> Result<()> {
    conn.execute("DELETE FROM attachments WHERE message_id = ?1", params![message_id])?;
    for att in attachments {
        let id = if att.id.trim().is_empty() {
            Uuid::new_v4().to_string()
        } else {
            att.id.clone()
        };
        conn.execute(
            "INSERT OR REPLACE INTO attachments(id, message_id, name, path, mime, kind, text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![id, message_id, att.name, att.path, att.mime, att.kind, att.text, now],
        )?;
    }
    Ok(())
}

/// One-time import of JSON chats (multi-session folder, or the older single file).
fn import_legacy_json(conn: &Connection, data_dir: &Path) -> Result<()> {
    if crate::db::get_state(conn, JSON_IMPORTED_KEY)?.is_some() {
        return Ok(());
    }
    let dir = ChatSessionStore::sessions_dir(data_dir);
    let manifest_path = dir.join("manifest.json");
    let legacy_path = ChatSession::legacy_path(data_dir);

    let mut imported: Vec<(SessionMeta, ChatSession)> = Vec::new();
    let mut active = String::new();
    if manifest_path.is_file() {
        let raw = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest: LegacyManifest = serde_json::from_str(&raw)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        active = manifest.active_id;
        for mut meta in manifest.sessions {
            if meta.created_at <= 0 {
                meta.created_at = meta.updated_at;
            }
            let path = dir.join(format!("{}.json", meta.id));
            let session = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<ChatSession>(&raw).ok())
                .unwrap_or_default();
            imported.push((meta, session));
        }
    } else if legacy_path.is_file() {
        let raw = std::fs::read_to_string(&legacy_path).unwrap_or_default();
        let session = serde_json::from_str::<ChatSession>(&raw)
            .ok()
            .or_else(|| {
                serde_json::from_str::<Vec<SessionMessage>>(&raw)
                    .ok()
                    .map(|messages| ChatSession { mode: "chat".into(), messages })
            });
        if let Some(session) = session {
            let now = now_unix();
            let meta = SessionMeta {
                id: Uuid::new_v4().to_string(),
                title: title_for_session(&session),
                created_at: now,
                updated_at: now,
                mode: if session.mode.is_empty() { "chat".into() } else { session.mode.clone() },
            };
            active = meta.id.clone();
            imported.push((meta, session));
        }
    }

    let tx = conn.unchecked_transaction()?;
    for (meta, session) in &imported {
        tx.execute(
            "INSERT OR IGNORE INTO chat_sessions(id, title, mode, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![meta.id, meta.title, if meta.mode.is_empty() { "chat" } else { &meta.mode }, meta.created_at, meta.updated_at],
        )?;
        for (seq, msg) in session.messages.iter().enumerate() {
            // Exact send times were never stored; the session's last update is the best estimate.
            tx.execute(
                "INSERT OR IGNORE INTO chat_messages(id, session_id, seq, role, text, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![Uuid::new_v4().to_string(), meta.id, seq as i64, msg.role, msg.text, meta.updated_at],
            )?;
        }
    }
    if !active.is_empty() {
        crate::db::set_state(&tx, ACTIVE_KEY, &active)?;
    }
    crate::db::set_state(&tx, JSON_IMPORTED_KEY, &now_unix().to_string())?;
    tx.commit()?;

    // Keep the JSON files as a backup under a new name.
    if dir.is_dir() {
        let backup = data_dir.join("chat-sessions.migrated");
        if !backup.exists() {
            let _ = std::fs::rename(&dir, &backup);
        }
    }
    if legacy_path.is_file() {
        let _ = std::fs::rename(&legacy_path, data_dir.join("chat-session.json.migrated"));
    }
    if !imported.is_empty() {
        let messages: usize = imported.iter().map(|(_, s)| s.messages.len()).sum();
        eprintln!("piya: imported {} chats ({messages} messages) into piya.db", imported.len());
    }
    Ok(())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn title_for_session(session: &ChatSession) -> String {
    session
        .messages
        .iter()
        .find(|m| m.role == "user")
        .map(|m| {
            if !m.text.trim().is_empty() {
                truncate_title(&m.text)
            } else if let Some(att) = m.attachments.first() {
                if let Some(t) = att.text.as_deref().filter(|s| !s.trim().is_empty()) {
                    truncate_title(t)
                } else {
                    truncate_title(&format!("📎 {}", att.name))
                }
            } else {
                "New chat".into()
            }
        })
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

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("piya-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn msg(role: &str, text: &str) -> SessionMessage {
        SessionMessage {
            role: role.into(),
            text: text.into(),
            attachments: Vec::new(),
        }
    }

    #[test]
    fn attachments_persist_and_index_in_fts() {
        let dir = temp_dir("session-att");
        let mut store = ChatSessionStore::open(&dir).unwrap();
        let id = store.active_id().to_string();
        let session = ChatSession {
            mode: "chat".into(),
            messages: vec![SessionMessage {
                role: "user".into(),
                text: String::new(),
                attachments: vec![ChatAttachment {
                    id: "att-1".into(),
                    name: "invoice.pdf".into(),
                    path: "attachments/s/invoice.pdf".into(),
                    mime: "application/pdf".into(),
                    kind: "pdf".into(),
                    text: Some("Vendor: Acme Cloud · Amount due: $420".into()),
                }],
            }],
        };
        store.save(&id, &session).unwrap();
        assert_eq!(store.list()[0].title, "Vendor: Acme Cloud · Amount due: $420");
        let back = store.load(&id).unwrap();
        assert_eq!(back.messages[0].attachments.len(), 1);
        assert_eq!(back.messages[0].attachments[0].name, "invoice.pdf");
        let fts_hits: i64 = store
            .conn
            .query_row(
                "SELECT count(*) FROM attachments_fts WHERE attachments_fts MATCH 'acme'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(fts_hits, 1);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn roundtrip() {
        let dir = temp_dir("session");
        let mut store = ChatSessionStore::open(&dir).unwrap();
        let id = store.active_id().to_string();
        let session = ChatSession { mode: "chat".into(), messages: vec![msg("user", "hello world")] };
        store.save(&id, &session).unwrap();
        let back = store.load(&id).unwrap();
        assert_eq!(back.messages.len(), 1);
        assert_eq!(store.list()[0].title, "hello world");
        // reopen: persisted
        let store = ChatSessionStore::open(&dir).unwrap();
        assert_eq!(store.load_active().unwrap().messages[0].text, "hello world");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sorts_by_created_not_updated_and_select_does_not_bump() {
        let dir = temp_dir("session-order");
        let mut store = ChatSessionStore::open(&dir).unwrap();
        let first = store.active_id().to_string();
        let second = store.create("chat").unwrap().id;
        assert_eq!(store.list()[0].id, second);
        assert_eq!(store.list()[1].id, first);

        let session = ChatSession { mode: "chat".into(), messages: vec![msg("user", "hello")] };
        store.save(&first, &session).unwrap();
        assert_eq!(store.list()[0].id, second);
        let updated = store.list().iter().find(|s| s.id == first).unwrap().updated_at;
        store.save(&first, &session).unwrap();
        let again = store.list().iter().find(|s| s.id == first).unwrap().updated_at;
        assert_eq!(updated, again);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn save_keeps_unchanged_rows_and_truncates() {
        let dir = temp_dir("session-diff");
        let mut store = ChatSessionStore::open(&dir).unwrap();
        let id = store.active_id().to_string();
        let s1 = ChatSession { mode: "chat".into(), messages: vec![msg("user", "a"), msg("piya", "b"), msg("user", "c")] };
        store.save(&id, &s1).unwrap();
        let first_id: String = store
            .conn
            .query_row("SELECT id FROM chat_messages WHERE session_id = ?1 AND seq = 0", params![id], |r| r.get(0))
            .unwrap();
        let s2 = ChatSession { mode: "chat".into(), messages: vec![msg("user", "a"), msg("piya", "B!")] };
        store.save(&id, &s2).unwrap();
        let same_id: String = store
            .conn
            .query_row("SELECT id FROM chat_messages WHERE session_id = ?1 AND seq = 0", params![id], |r| r.get(0))
            .unwrap();
        assert_eq!(first_id, same_id, "unchanged message keeps its id");
        let back = store.load(&id).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].text, "B!");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn imports_json_sessions_once_and_keeps_backup() {
        let dir = temp_dir("session-import");
        let chats = dir.join("chat-sessions");
        std::fs::create_dir_all(&chats).unwrap();
        let manifest = serde_json::json!({
            "active_id": "b",
            "sessions": [
                {"id": "a", "title": "first", "created_at": 10, "updated_at": 20, "mode": "chat"},
                {"id": "b", "title": "second", "created_at": 30, "updated_at": 40, "mode": "chat"}
            ]
        });
        std::fs::write(chats.join("manifest.json"), manifest.to_string()).unwrap();
        for (id, text) in [("a", "remember my server"), ("b", "hi again")] {
            let s = ChatSession { mode: "chat".into(), messages: vec![msg("user", text), msg("piya", "ok")] };
            std::fs::write(chats.join(format!("{id}.json")), serde_json::to_string(&s).unwrap()).unwrap();
        }

        let store = ChatSessionStore::open(&dir).unwrap();
        assert_eq!(store.list().len(), 2);
        assert_eq!(store.active_id(), "b");
        assert_eq!(store.list()[0].id, "b", "newest created first");
        assert_eq!(store.load("a").unwrap().messages[0].text, "remember my server");
        assert!(dir.join("chat-sessions.migrated").is_dir(), "backup kept");
        assert!(!chats.exists());
        drop(store);
        let store = ChatSessionStore::open(&dir).unwrap();
        assert_eq!(store.list().len(), 2, "no double import");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn legacy_single_file_import() {
        let dir = temp_dir("legacy");
        let legacy = ChatSession { mode: "task".into(), messages: vec![msg("user", "queue a report")] };
        std::fs::write(ChatSession::legacy_path(&dir), serde_json::to_string_pretty(&legacy).unwrap()).unwrap();
        let store = ChatSessionStore::open(&dir).unwrap();
        assert_eq!(store.list().len(), 1);
        assert_eq!(store.list()[0].title, "queue a report");
        assert_eq!(store.load_active().unwrap().mode, "task");
        let _ = std::fs::remove_dir_all(dir);
    }
}
