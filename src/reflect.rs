//! Daily reflection and note maintenance (`src/reflect.rs`).
//!
//! Runs automatically once per local calendar day in the background worker (and on
//! demand via `piya reflect` or Settings → Memory → "Reflect now"):
//! 1. **Repair** (no LLM): archives expired notes (`expires_at < today`), archives
//!    60-day-old unrepeated `tentative` guesses, and merges normalized duplicates.
//! 2. **Reflect** (quote-verified): reads new user messages & voice transcripts since
//!    the watermark in batches of 40, extracts tentative facts/preferences/lessons/opinions,
//!    and promotes repeated tentative notes to `learned`.

use anyhow::Result;
use chrono::{Duration, Local, TimeZone, Utc};
use rusqlite::{params, Connection};

use crate::config::Settings;
use crate::db;
use crate::llm::LlmClient;
use crate::notes::{self, normalize_ws_lower, UserUtterance};
use crate::prefs::UserPrefs;

const WATERMARK_ROWID_KEY: &str = "reflection_last_msg_rowid";
const LAST_RUN_DATE_KEY: &str = "reflection_last_run_date";
const BATCH_SIZE: usize = 40;
const MAX_BATCHES_PER_RUN: usize = 5;

#[derive(Clone, Debug, Default)]
pub struct ReflectReport {
    pub ran_reflection: bool,
    pub messages_scanned: usize,
    pub notes_saved: usize,
    pub notes_promoted: usize,
    pub notes_replaced: usize,
    pub expired_archived: usize,
    pub stale_tentative_archived: usize,
    pub duplicates_merged: usize,
}

/// Repair pass — pure SQLite, runs without calling any model.
pub fn repair(conn: &Connection) -> Result<(usize, usize, usize)> {
    let now = Utc::now();
    let now_s = now.to_rfc3339();
    let today = Local::now().format("%Y-%m-%d").to_string();
    let stale_cutoff = (now - Duration::days(60)).to_rfc3339();

    let expired = conn.execute(
        "UPDATE notes SET status = 'archived', updated_at = ?1
         WHERE status != 'archived' AND expires_at IS NOT NULL AND expires_at != '' AND expires_at < ?2",
        params![now_s, today],
    )?;

    let stale = conn.execute(
        "UPDATE notes SET status = 'archived', updated_at = ?1
         WHERE status = 'tentative' AND origin = 'inferred' AND evidence_count <= 1 AND updated_at < ?2",
        params![now_s, stale_cutoff],
    )?;

    // Merge exact normalized-text duplicates in the same area (strongest status stays first).
    let active = notes::list_active(conn)?;
    let mut seen: std::collections::HashMap<(String, String), String> = std::collections::HashMap::new();
    let mut merged = 0usize;
    for n in active {
        let key = (n.area.clone(), normalize_ws_lower(&n.text));
        if let Some(keeper_id) = seen.get(&key) {
            conn.execute(
                "UPDATE notes SET evidence_count = evidence_count + ?1, updated_at = ?2 WHERE id = ?3",
                params![n.evidence_count.max(1), now_s, keeper_id],
            )?;
            conn.execute(
                "UPDATE notes SET status = 'archived', replaced_by = ?1, updated_at = ?2 WHERE id = ?3",
                params![keeper_id, now_s, n.id],
            )?;
            merged += 1;
        } else {
            seen.insert(key, n.id);
        }
    }

    Ok((expired, stale, merged))
}

/// Run repair + daily reflection if not yet run today (or unconditionally when `force` is true).
pub async fn reflect_if_due(settings: &Settings, force: bool) -> Result<ReflectReport> {
    let today = Local::now().format("%Y-%m-%d").to_string();
    let prefs = UserPrefs::load(&settings.data_dir).unwrap_or_default();
    let _ = crate::activity::synthesize_work_journal_if_due(settings, false).await;

    let (mut report, mut watermark, should_reflect) = {
        let conn = db::open(&settings.data_dir)?;
        let _ = notes::migrate_user_memories_once(&conn);
        let _ = crate::activity::prune_old_events(&conn, prefs.activity_retention_days);
        let (expired_archived, stale_tentative_archived, duplicates_merged) = repair(&conn)?;
        let report = ReflectReport {
            expired_archived,
            stale_tentative_archived,
            duplicates_merged,
            ..Default::default()
        };

        if !force && !prefs.learn_from_conversations {
            return Ok(report);
        }
        if !force {
            if let Some(last_date) = db::get_state(&conn, LAST_RUN_DATE_KEY)? {
                if last_date == today {
                    return Ok(report);
                }
            }
        }
        if settings.require_model().is_err() {
            return Ok(report);
        }

        let mut watermark: i64 = db::get_state(&conn, WATERMARK_ROWID_KEY)?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        // On very first automatic run with a large historical DB, start from the most recent
        // 120 user messages so we don't burn dozens of calls on ancient logs at once.
        if watermark == 0 {
            let floor: i64 = conn
                .query_row(
                    "SELECT COALESCE(MIN(r), 0) FROM (
                        SELECT rowid AS r FROM chat_messages WHERE role = 'user' ORDER BY rowid DESC LIMIT 120
                    )",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            watermark = floor.saturating_sub(1);
        }
        (report, watermark, true)
    };

    if !should_reflect {
        return Ok(report);
    }

    let llm = LlmClient::new(settings)?;

    for _ in 0..MAX_BATCHES_PER_RUN {
        let batch = {
            let conn = db::open(&settings.data_dir)?;
            fetch_user_batch(&conn, watermark, BATCH_SIZE)?
        };
        if batch.is_empty() {
            break;
        }
        let next_watermark = batch.last().map(|(rowid, _)| *rowid).unwrap_or(watermark);
        let utterances: Vec<UserUtterance> = batch.into_iter().map(|(_, u)| u).collect();
        report.messages_scanned += utterances.len();
        report.ran_reflection = true;

        let outcome = notes::learn_from_utterances(
            &llm,
            &settings.data_dir,
            &utterances,
            false,
            prefs.remember_sensitive,
        )
        .await?;
        report.notes_saved += outcome.saved.len();
        report.notes_promoted += outcome.promoted.len();
        report.notes_replaced += outcome.replaced.len();

        watermark = next_watermark;
        let conn = db::open(&settings.data_dir)?;
        db::set_state(&conn, WATERMARK_ROWID_KEY, &watermark.to_string())?;
    }

    let conn = db::open(&settings.data_dir)?;
    db::set_state(&conn, LAST_RUN_DATE_KEY, &today)?;
    Ok(report)
}

fn fetch_user_batch(
    conn: &Connection,
    after_rowid: i64,
    limit: usize,
) -> Result<Vec<(i64, UserUtterance)>> {
    let mut stmt = conn.prepare(
        "SELECT m.rowid, m.id, m.text, m.created_at
         FROM chat_messages m
         WHERE m.rowid > ?1 AND m.role = 'user'
         ORDER BY m.rowid ASC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(params![after_rowid, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut out = Vec::with_capacity(rows.len());
    for (rowid, id, mut text, ts) in rows {
        // Include voice/audio attachment transcripts spoken by the user in this message!
        let mut att_stmt = conn.prepare(
            "SELECT text FROM attachments WHERE message_id = ?1 AND kind = 'audio' AND text IS NOT NULL",
        )?;
        let transcripts: Vec<String> = att_stmt
            .query_map(params![id], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter(|t| !t.trim().is_empty())
            .collect();
        if !transcripts.is_empty() {
            if !text.trim().is_empty() {
                text.push('\n');
            }
            text.push_str(&transcripts.join("\n"));
        }
        if text.trim().is_empty() || text.chars().count() > 4_000 {
            out.push((
                rowid,
                UserUtterance {
                    id,
                    created_at: String::new(),
                    text: String::new(),
                },
            ));
            continue;
        }
        let created_at = Utc
            .timestamp_opt(ts, 0)
            .single()
            .map(|dt| dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        out.push((rowid, UserUtterance { id, created_at, text }));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn repair_archives_expired_and_merges_duplicates() {
        let dir = std::env::temp_dir().join(format!("piya-reflect-{}", Uuid::new_v4()));
        let conn = db::open(&dir).unwrap();
        let recent = Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO notes(id, area, slug, text, status, origin, evidence_count, created_at, updated_at, expires_at)
             VALUES ('n1', 'about', 'old-plan', 'On vacation', 'confirmed', 'stated', 1, '2025-01-01', '2025-01-01', '2025-01-10'),
                    ('n2', 'preferences', 'fmt-1', 'Prefers bullet points', 'confirmed', 'stated', 1, ?1, ?1, NULL),
                    ('n3', 'preferences', 'fmt-2', 'prefers   bullet points', 'tentative', 'inferred', 1, ?1, ?1, NULL),
                    ('n4', 'about', 'stale-guess', 'Maybe likes tea', 'tentative', 'inferred', 1, '2025-01-02', '2025-01-02', NULL)",
            params![recent],
        )
        .unwrap();

        let (expired, stale, merged) = repair(&conn).unwrap();
        assert_eq!(expired, 1);
        assert_eq!(stale, 1);
        assert_eq!(merged, 1);
        let active = notes::list_active(&conn).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, "n2");
        assert_eq!(active[0].evidence_count, 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
