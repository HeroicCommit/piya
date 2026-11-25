//! `piya cleanup` — undo damage from the old keyword-routed task pipeline.
//!
//! * cancels queued "Remove reminder…/Remind me…" rows that were sent through the
//!   plan/act/review loop instead of becoming real schedules;
//! * deletes agent-written memories that teach the old behaviour ("implemented a
//!   reminder system", "removal documented"). Memories the user saved are never touched.

use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;

use crate::config::Settings;
use crate::tasks::{TaskKind, TaskQueue, TaskStatus};

static LEGACY_TASK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*((remove|delete|cancel)\s+(the\s+)?reminder|remind(\s+me)?\b|set\s+a\s+(timer|reminder)|send\s+(a\s+)?(sample|test)\s+notification)")
        .expect("legacy task regex")
});

static BAD_MEMORY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(reminder system|notification system|timer functionality|reminder functionality|sample notification|badge notification|reminder for pool test|pool test \d|removal process was documented|confirmed to not exist)")
        .expect("bad memory regex")
});

pub struct CleanupReport {
    pub cancelled: Vec<String>,
    pub memories_deleted: Vec<String>,
}

pub async fn run(settings: &Settings, dry_run: bool) -> Result<CleanupReport> {
    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    let mut cancelled = Vec::new();
    for t in queue.list_active(500)? {
        if t.kind == TaskKind::Task && LEGACY_TASK.is_match(&t.title) {
            if !dry_run && matches!(t.status, TaskStatus::Pending | TaskStatus::InProgress) {
                queue.cancel(&t.id)?;
            }
            cancelled.push(format!("{} {} ({})", t.short_id(), t.title, t.status.as_str()));
        }
    }

    let (_llm, memory) = crate::chat_engine::llm_and_memory(settings)?;
    let mut memories_deleted = Vec::new();
    for m in memory.list_recent(500).await? {
        if m.source == "user" {
            continue;
        }
        if BAD_MEMORY.is_match(&m.text) {
            if !dry_run {
                memory.delete_by_id(&m.id).await?;
            }
            memories_deleted.push(m.text.chars().take(120).collect());
        }
    }
    Ok(CleanupReport {
        cancelled,
        memories_deleted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_only_legacy_errands() {
        assert!(LEGACY_TASK.is_match("Remove reminder for pool test 8."));
        assert!(LEGACY_TASK.is_match("Remind me around 6:30 am for sending daily stats report"));
        assert!(LEGACY_TASK.is_match("Set a timer for 15 minutes."));
        assert!(!LEGACY_TASK.is_match("Research OpenEdx in detail"));
        assert!(BAD_MEMORY.is_match("Task 'x' — Successfully implemented a reminder system to notify at 05:08"));
        assert!(!BAD_MEMORY.is_match("Allure server instance id i-123, 100GB gp3"));
    }
}
