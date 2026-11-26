//! The single Piya database (`data/piya.db`): tasks, chats, attachments, memory.
//!
//! Everything lives in one SQLite file (WAL mode, so the app and workers can use it
//! at the same time). Schema changes are applied in order via `PRAGMA user_version`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join("piya.db")
}

/// Open the database, creating/upgrading the schema as needed.
pub fn open(data_dir: &Path) -> Result<Connection> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("create data dir {}", data_dir.display()))?;
    let conn = Connection::open(path(data_dir)).context("open piya.db")?;
    conn.busy_timeout(std::time::Duration::from_secs(10))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;")?;
    migrate(&conn)?;
    Ok(conn)
}

const MIGRATIONS: &[&str] = &[
    // 1: chats, attachments, memory, app state
    r#"
    CREATE TABLE IF NOT EXISTS app_state (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    CREATE TABLE IF NOT EXISTS chat_sessions (
        id         TEXT PRIMARY KEY,
        title      TEXT NOT NULL,
        mode       TEXT NOT NULL DEFAULT 'chat',
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL
    );

    CREATE TABLE IF NOT EXISTS chat_messages (
        id         TEXT NOT NULL UNIQUE,
        session_id TEXT NOT NULL REFERENCES chat_sessions(id) ON DELETE CASCADE,
        seq        INTEGER NOT NULL,
        role       TEXT NOT NULL,
        text       TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        UNIQUE (session_id, seq)
    );
    CREATE INDEX IF NOT EXISTS idx_chat_messages_created ON chat_messages(created_at);

    CREATE VIRTUAL TABLE IF NOT EXISTS chat_messages_fts USING fts5(
        text, content='chat_messages', content_rowid='rowid', tokenize='porter unicode61'
    );
    CREATE TRIGGER IF NOT EXISTS chat_messages_ai AFTER INSERT ON chat_messages BEGIN
        INSERT INTO chat_messages_fts(rowid, text) VALUES (new.rowid, new.text);
    END;
    CREATE TRIGGER IF NOT EXISTS chat_messages_ad AFTER DELETE ON chat_messages BEGIN
        INSERT INTO chat_messages_fts(chat_messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
    END;
    CREATE TRIGGER IF NOT EXISTS chat_messages_au AFTER UPDATE OF text ON chat_messages BEGIN
        INSERT INTO chat_messages_fts(chat_messages_fts, rowid, text) VALUES ('delete', old.rowid, old.text);
        INSERT INTO chat_messages_fts(rowid, text) VALUES (new.rowid, new.text);
    END;

    CREATE TABLE IF NOT EXISTS attachments (
        id         TEXT PRIMARY KEY,
        message_id TEXT REFERENCES chat_messages(id) ON DELETE CASCADE,
        name       TEXT NOT NULL,
        path       TEXT NOT NULL,
        mime       TEXT NOT NULL DEFAULT '',
        kind       TEXT NOT NULL DEFAULT 'other',
        text       TEXT,
        created_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_id);

    CREATE TABLE IF NOT EXISTS memories (
        id              TEXT PRIMARY KEY,
        text            TEXT NOT NULL,
        kind            TEXT NOT NULL,
        task_id         TEXT NOT NULL DEFAULT '',
        tags            TEXT NOT NULL DEFAULT '[]',
        accuracy        REAL NOT NULL DEFAULT 0.5,
        source          TEXT NOT NULL DEFAULT '',
        created_at      TEXT NOT NULL,
        recall_count    INTEGER NOT NULL DEFAULT 0,
        embedding       BLOB,
        embedding_model TEXT NOT NULL DEFAULT ''
    );
    CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at);
    "#,
    // 2: full-text index on attachment names and extracted text / audio transcripts
    r#"
    CREATE VIRTUAL TABLE IF NOT EXISTS attachments_fts USING fts5(
        name, text, content='attachments', content_rowid='rowid', tokenize='porter unicode61'
    );
    INSERT INTO attachments_fts(rowid, name, text)
        SELECT rowid, name, COALESCE(text, '') FROM attachments;
    CREATE TRIGGER IF NOT EXISTS attachments_ai AFTER INSERT ON attachments BEGIN
        INSERT INTO attachments_fts(rowid, name, text) VALUES (new.rowid, new.name, COALESCE(new.text, ''));
    END;
    CREATE TRIGGER IF NOT EXISTS attachments_ad AFTER DELETE ON attachments BEGIN
        INSERT INTO attachments_fts(attachments_fts, rowid, name, text)
        VALUES ('delete', old.rowid, old.name, COALESCE(old.text, ''));
    END;
    CREATE TRIGGER IF NOT EXISTS attachments_au AFTER UPDATE OF name, text ON attachments BEGIN
        INSERT INTO attachments_fts(attachments_fts, rowid, name, text)
        VALUES ('delete', old.rowid, old.name, COALESCE(old.text, ''));
        INSERT INTO attachments_fts(rowid, name, text) VALUES (new.rowid, new.name, COALESCE(new.text, ''));
    END;
    "#,
];

fn migrate(conn: &Connection) -> Result<()> {
    let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let target = i as i64 + 1;
        if version >= target {
            continue;
        }
        conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version = {target}; COMMIT;"))
            .with_context(|| format!("apply database migration {target}"))?;
    }
    Ok(())
}

pub fn get_state(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT value FROM app_state WHERE key = ?1", params![key], |r| r.get(0))
        .optional()?)
}

pub fn set_state(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_state(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn embedding_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

pub fn blob_to_embedding(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_applies_once_and_fts_tracks_messages() {
        let dir = std::env::temp_dir().join(format!("piya-db-{}", uuid::Uuid::new_v4()));
        let conn = open(&dir).unwrap();
        drop(conn);
        let conn = open(&dir).unwrap(); // second open must not re-run migrations
        conn.execute(
            "INSERT INTO chat_sessions(id,title,mode,created_at,updated_at) VALUES ('s','t','chat',1,1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chat_messages(id,session_id,seq,role,text,created_at)
             VALUES ('m1','s',0,'user','The Allure server runs on t4g.large',1)",
            [],
        )
        .unwrap();
        let hits: i64 = conn
            .query_row("SELECT count(*) FROM chat_messages_fts WHERE chat_messages_fts MATCH 'servers'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 1, "porter stemming: 'servers' finds 'server'");
        conn.execute("UPDATE chat_messages SET text = 'nothing here' WHERE id = 'm1'", []).unwrap();
        let hits: i64 = conn
            .query_row("SELECT count(*) FROM chat_messages_fts WHERE chat_messages_fts MATCH 'allure'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 0, "FTS follows updates");
        assert_eq!(blob_to_embedding(&embedding_to_blob(&[1.5, -2.0])), vec![1.5, -2.0]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
