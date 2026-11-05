use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use uuid::Uuid;

use serde::Serialize;

/// A task whose worker has not heartbeated for this long is handed back to the queue.
/// Workers heartbeat every [`HEARTBEAT_INTERVAL_SECS`].
pub const HEARTBEAT_TIMEOUT_SECS: i64 = 90;
pub const HEARTBEAT_INTERVAL_SECS: u64 = 10;

/// Stable id for this process, used as the task owner (`pid-xxxxxxxx`).
pub fn default_worker_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let short = &Uuid::new_v4().to_string()[..8];
        format!("{}-{short}", std::process::id())
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Done,
    Failed,
    Blocked,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(s: &str) -> Result<Self> {
        Ok(match s {
            "pending" => Self::Pending,
            "in_progress" => Self::InProgress,
            "done" => Self::Done,
            "failed" => Self::Failed,
            "blocked" => Self::Blocked,
            other => anyhow::bail!("unknown task status: {other}"),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub priority: i32,
    pub attempts: i32,
    pub best_accuracy: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    pub tags: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
}

#[derive(Clone)]
pub struct TaskQueue {
    db_path: PathBuf,
}

impl TaskQueue {
    pub fn new(db_path: impl AsRef<Path>) -> Result<Self> {
        let queue = Self {
            db_path: db_path.as_ref().to_path_buf(),
        };
        queue.init_db()?;
        Ok(queue)
    }

    fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&self.db_path).context("open task database")?;
        // Several worker processes/threads share this file: wait instead of failing.
        conn.busy_timeout(std::time::Duration::from_secs(10))
            .context("set busy_timeout")?;
        Ok(conn)
    }

    fn init_db(&self) -> Result<()> {
        if let Some(parent) = self.db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = self.connect()?;
        // WAL lets readers (UI) and writers (workers) proceed concurrently.
        let _mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                description TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                priority INTEGER NOT NULL DEFAULT 0,
                attempts INTEGER NOT NULL DEFAULT 0,
                best_accuracy REAL NOT NULL DEFAULT 0,
                result TEXT,
                tags TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS cycles (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                phase TEXT NOT NULL,
                input TEXT,
                output TEXT,
                accuracy REAL,
                created_at TEXT NOT NULL,
                FOREIGN KEY (task_id) REFERENCES tasks(id)
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_status ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_cycles_task ON cycles(task_id);
            ",
        )?;
        Self::migrate_schema(&conn)?;
        Ok(())
    }

    fn migrate_schema(conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(tasks)")?;
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get(1))?
            .filter_map(|r| r.ok())
            .collect();
        if !cols.iter().any(|c| c == "run_at") {
            conn.execute("ALTER TABLE tasks ADD COLUMN run_at TEXT", [])?;
        }
        if !cols.iter().any(|c| c == "recurrence") {
            conn.execute("ALTER TABLE tasks ADD COLUMN recurrence TEXT", [])?;
        }
        // Worker ownership: which worker runs the task and when it last proved it is alive.
        for col in ["worker_id", "claimed_at", "heartbeat_at"] {
            if !cols.iter().any(|c| c == col) {
                conn.execute(&format!("ALTER TABLE tasks ADD COLUMN {col} TEXT"), [])?;
            }
        }
        Ok(())
    }

    fn now() -> String {
        Utc::now().to_rfc3339()
    }

    pub fn add(
        &self,
        title: &str,
        description: &str,
        priority: i32,
        tags: &str,
    ) -> Result<Task> {
        let (run_at, recurrence) = crate::schedule::encode_for_task(description);
        self.add_with_schedule(
            title,
            description,
            priority,
            tags,
            run_at.as_deref(),
            recurrence.as_deref(),
        )
    }

    pub fn add_with_run_at(
        &self,
        title: &str,
        description: &str,
        priority: i32,
        tags: &str,
        run_at: Option<&str>,
    ) -> Result<Task> {
        self.add_with_schedule(title, description, priority, tags, run_at, None)
    }

    pub fn add_with_schedule(
        &self,
        title: &str,
        description: &str,
        priority: i32,
        tags: &str,
        run_at: Option<&str>,
        recurrence: Option<&str>,
    ) -> Result<Task> {
        let id = Uuid::new_v4().to_string();
        let now = Self::now();
        let conn = self.connect()?;
        conn.execute(
            "
            INSERT INTO tasks (id, title, description, status, priority, attempts,
                               best_accuracy, tags, created_at, updated_at, run_at, recurrence)
            VALUES (?1, ?2, ?3, 'pending', ?4, 0, 0, ?5, ?6, ?6, ?7, ?8)
            ",
            params![id, title, description, priority, tags, now, run_at, recurrence],
        )?;
        self.get(&id)
    }

    /// After a recurring task finishes, queue the next occurrence.
    pub fn reschedule_if_recurring(&self, id: &str) -> Result<Option<Task>> {
        let task = self.get(id)?;
        let Some(recurrence_json) = task.recurrence else {
            return Ok(None);
        };
        let rule: crate::schedule::Recurrence = serde_json::from_str(&recurrence_json)
            .context("parse recurrence rule")?;
        let next = crate::schedule::next_occurrence(&rule, Utc::now());
        let now = Self::now();
        let conn = self.connect()?;
        conn.execute(
            "UPDATE tasks SET status = 'pending', run_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![next.to_rfc3339(), now, id],
        )?;
        Ok(Some(self.get(id)?))
    }

    pub fn is_recurring(task: &Task) -> bool {
        task.recurrence.is_some()
    }

    pub fn get(&self, id: &str) -> Result<Task> {
        if let Ok(task) = self.get_exact(id) {
            return Ok(task);
        }
        self.find_by_prefix(id)?
            .ok_or_else(|| anyhow::anyhow!("task not found: {id}"))
    }

    fn get_exact(&self, id: &str) -> Result<Task> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare("SELECT * FROM tasks WHERE id = ?1")?;
        stmt.query_row(params![id], row_to_task)
            .with_context(|| format!("task not found: {id}"))
    }

    pub fn find_by_prefix(&self, prefix: &str) -> Result<Option<Task>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "SELECT * FROM tasks WHERE id LIKE ?1 ORDER BY updated_at DESC LIMIT 1",
        )?;
        let pattern = format!("{prefix}%");
        let mut rows = stmt.query(params![pattern])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_task(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn list(&self, status: Option<TaskStatus>, limit: i32) -> Result<Vec<Task>> {
        let conn = self.connect()?;
        let mut tasks = Vec::new();

        if let Some(status) = status {
            let mut stmt = conn.prepare(
                "
                SELECT * FROM tasks WHERE status = ?1
                ORDER BY priority DESC, created_at ASC LIMIT ?2
                ",
            )?;
            let rows = stmt.query_map(params![status.as_str(), limit], row_to_task)?;
            for row in rows {
                tasks.push(row?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT * FROM tasks ORDER BY priority DESC, created_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit], row_to_task)?;
            for row in rows {
                tasks.push(row?);
            }
        }

        Ok(tasks)
    }

    /// Most recently updated finished tasks (for chat status reports).
    pub fn list_recent_finished(&self, limit: i32) -> Result<Vec<Task>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "
            SELECT * FROM tasks
            WHERE status IN ('done', 'failed', 'blocked')
            ORDER BY updated_at DESC
            LIMIT ?1
            ",
        )?;
        let rows = stmt.query_map(params![limit], row_to_task)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("list recent finished tasks")
    }

    pub fn count(&self, status: Option<TaskStatus>) -> Result<usize> {
        let conn = self.connect()?;
        let count: i64 = match status {
            Some(s) => conn.query_row(
                "SELECT COUNT(*) FROM tasks WHERE status = ?1",
                params![s.as_str()],
                |row| row.get(0),
            )?,
            None => conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?,
        };
        Ok(count as usize)
    }

    /// Reset tasks stuck in `in_progress` (e.g. after a crashed worker).
    pub fn reset_stale_in_progress(&self, max_age_secs: i64) -> Result<usize> {
        self.reclaim_stale(max_age_secs)
    }

    pub fn count_due_pending(&self) -> Result<usize> {
        let now = Self::now();
        let conn = self.connect()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'pending'
             AND (run_at IS NULL OR run_at <= ?1)",
            params![now],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    pub fn count_scheduled(&self) -> Result<usize> {
        let now = Self::now();
        let conn = self.connect()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE status = 'pending' AND run_at > ?1",
            params![now],
            |row| row.get(0),
        )?;
        Ok(count as usize)
    }

    /// Seconds until the soonest future `run_at`, if any scheduled pending task exists.
    pub fn seconds_until_next_scheduled(&self) -> Result<Option<u64>> {
        let now = Utc::now();
        let conn = self.connect()?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT run_at FROM tasks
                 WHERE status = 'pending' AND run_at IS NOT NULL AND run_at > ?1
                 ORDER BY run_at ASC LIMIT 1",
                params![now.to_rfc3339()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(raw) = raw else {
            return Ok(None);
        };
        let Ok(dt) = DateTime::parse_from_rfc3339(&raw) else {
            return Ok(None);
        };
        let secs = (dt.with_timezone(&Utc) - now).num_seconds().max(0) as u64;
        Ok(Some(secs))
    }

    /// Atomically claim the next due pending task for `worker_id`.
    ///
    /// A single `UPDATE … RETURNING` statement picks and marks the row, so two
    /// workers can never claim the same task.
    pub fn claim_next(&self, worker_id: &str) -> Result<Option<Task>> {
        self.reclaim_stale(HEARTBEAT_TIMEOUT_SECS)?;
        let now = Self::now();
        let conn = self.connect()?;
        let task = conn
            .query_row(
                "
                UPDATE tasks
                   SET status = 'in_progress', worker_id = ?1, claimed_at = ?2,
                       heartbeat_at = ?2, updated_at = ?2
                 WHERE id = (
                         SELECT id FROM tasks
                          WHERE status = 'pending'
                            AND (run_at IS NULL OR run_at <= ?2)
                          ORDER BY priority DESC, created_at ASC
                          LIMIT 1)
                   AND status = 'pending'
                RETURNING *
                ",
                params![worker_id, now],
                row_to_task,
            )
            .optional()?;
        Ok(task)
    }

    /// Record that `worker_id` is still working on `id`. Returns `false` when the
    /// task is no longer owned by this worker (cancelled, reclaimed, finished).
    pub fn heartbeat(&self, id: &str, worker_id: &str) -> Result<bool> {
        let now = Self::now();
        let conn = self.connect()?;
        let n = conn.execute(
            "UPDATE tasks SET heartbeat_at = ?1
              WHERE id = ?2 AND worker_id = ?3 AND status = 'in_progress'",
            params![now, id, worker_id],
        )?;
        Ok(n > 0)
    }

    /// Return tasks whose worker stopped heartbeating (crashed or killed) to `pending`.
    /// Workers started before heartbeats existed fall back to `updated_at`.
    pub fn reclaim_stale(&self, timeout_secs: i64) -> Result<usize> {
        let cutoff = (Utc::now() - chrono::Duration::seconds(timeout_secs)).to_rfc3339();
        let conn = self.connect()?;
        let n = conn.execute(
            "UPDATE tasks
                SET status = 'pending', worker_id = NULL, claimed_at = NULL,
                    heartbeat_at = NULL, updated_at = ?1
              WHERE status = 'in_progress'
                AND COALESCE(heartbeat_at, updated_at) < ?2",
            params![Self::now(), cutoff],
        )?;
        Ok(n)
    }

    pub fn next_pending(&self) -> Result<Option<Task>> {
        self.reclaim_stale(HEARTBEAT_TIMEOUT_SECS)?;
        let now = Self::now();
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "
            SELECT * FROM tasks
            WHERE status = 'pending'
              AND (run_at IS NULL OR run_at <= ?1)
            ORDER BY priority DESC, created_at ASC
            LIMIT 1
            ",
        )?;
        let mut rows = stmt.query(params![now])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row_to_task(row)?))
        } else {
            Ok(None)
        }
    }

    pub fn update_status(
        &self,
        id: &str,
        status: TaskStatus,
        result: Option<&str>,
        accuracy: Option<f64>,
    ) -> Result<Task> {
        let now = Self::now();
        let conn = self.connect()?;

        if let Some(accuracy) = accuracy {
            conn.execute(
                "
                UPDATE tasks SET status = ?1, result = COALESCE(?2, result),
                       best_accuracy = MAX(best_accuracy, ?3), updated_at = ?4,
                       worker_id = CASE WHEN ?1 = 'in_progress' THEN worker_id END,
                       heartbeat_at = CASE WHEN ?1 = 'in_progress' THEN heartbeat_at END
                WHERE id = ?5
                ",
                params![status.as_str(), result, accuracy, now, id],
            )?;
        } else {
            conn.execute(
                "
                UPDATE tasks SET status = ?1, result = COALESCE(?2, result), updated_at = ?3,
                       worker_id = CASE WHEN ?1 = 'in_progress' THEN worker_id END,
                       heartbeat_at = CASE WHEN ?1 = 'in_progress' THEN heartbeat_at END
                WHERE id = ?4
                ",
                params![status.as_str(), result, now, id],
            )?;
        }

        self.get(id)
    }

    pub fn increment_attempts(&self, id: &str) -> Result<i32> {
        let now = Self::now();
        let conn = self.connect()?;
        conn.execute(
            "UPDATE tasks SET attempts = attempts + 1, updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;
        let attempts: i32 = conn.query_row(
            "SELECT attempts FROM tasks WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )?;
        Ok(attempts)
    }

    /// Pending + in-progress tasks for the management UI.
    pub fn list_active(&self, limit: i32) -> Result<Vec<Task>> {
        let conn = self.connect()?;
        let mut stmt = conn.prepare(
            "
            SELECT * FROM tasks
            WHERE status IN ('pending', 'in_progress')
            ORDER BY
              CASE status WHEN 'in_progress' THEN 0 ELSE 1 END,
              CASE WHEN run_at IS NULL THEN 1 ELSE 0 END,
              run_at ASC,
              priority DESC,
              created_at ASC
            LIMIT ?1
            ",
        )?;
        let rows = stmt.query_map(params![limit], row_to_task)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .context("list active tasks")
    }

    pub fn cancel(&self, id: &str) -> Result<Task> {
        let task = self.get(id)?;
        match task.status {
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Blocked => {
                anyhow::bail!("task `{}` is already finished", id);
            }
            TaskStatus::InProgress | TaskStatus::Pending => self.update_status(
                id,
                TaskStatus::Blocked,
                Some("cancelled by user"),
                None,
            ),
        }
    }

    pub fn update_details(
        &self,
        id: &str,
        title: Option<&str>,
        description: Option<&str>,
    ) -> Result<Task> {
        let task = self.get(id)?;
        match task.status {
            TaskStatus::Done | TaskStatus::Failed | TaskStatus::Blocked => {
                anyhow::bail!("cannot edit a finished task");
            }
            TaskStatus::InProgress | TaskStatus::Pending => {}
        }

        let new_title = title.unwrap_or(&task.title);
        let new_description = description.unwrap_or(&task.description);
        let now = Self::now();
        let conn = self.connect()?;
        conn.execute(
            "UPDATE tasks SET title = ?1, description = ?2, updated_at = ?3 WHERE id = ?4",
            params![new_title, new_description, now, id],
        )?;
        self.get(id)
    }

    pub fn log_cycle(
        &self,
        task_id: &str,
        phase: &str,
        input: Option<&str>,
        output: Option<&str>,
        accuracy: Option<f64>,
    ) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        let conn = self.connect()?;
        conn.execute(
            "
            INSERT INTO cycles (id, task_id, phase, input, output, accuracy, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![id, task_id, phase, input, output, accuracy, Self::now()],
        )?;
        Ok(id)
    }
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get("id")?,
        title: row.get("title")?,
        description: row.get("description")?,
        status: TaskStatus::parse(row.get::<_, String>("status")?.as_str()).unwrap_or(TaskStatus::Pending),
        priority: row.get("priority")?,
        attempts: row.get("attempts")?,
        best_accuracy: row.get("best_accuracy")?,
        result: row.get("result")?,
        tags: row.get("tags")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        run_at: row.get("run_at")?,
        recurrence: row.get("recurrence")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_and_update_active_task() {
        let dir = std::env::temp_dir().join(format!("piya-task-mgmt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let queue = TaskQueue::new(dir.join("piya.db")).unwrap();
        let task = queue.add("demo", "write report", 0, "").unwrap();
        queue
            .update_details(&task.id, Some("updated title"), Some("updated body"))
            .unwrap();
        let updated = queue.get(&task.id).unwrap();
        assert_eq!(updated.title, "updated title");
        queue.cancel(&task.id).unwrap();
        assert_eq!(queue.get(&task.id).unwrap().status, TaskStatus::Blocked);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn temp_queue(tag: &str) -> (TaskQueue, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("piya-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (TaskQueue::new(dir.join("piya.db")).unwrap(), dir)
    }

    #[test]
    fn concurrent_claims_never_double_run() {
        let (queue, dir) = temp_queue("claim");
        let total = 40;
        for i in 0..total {
            queue.add(&format!("t{i}"), "work", 0, "").unwrap();
        }
        let db = dir.join("piya.db");
        let handles: Vec<_> = (0..6)
            .map(|w| {
                let db = db.clone();
                std::thread::spawn(move || {
                    let q = TaskQueue::new(db).unwrap();
                    let mut mine = Vec::new();
                    while let Some(t) = q.claim_next(&format!("w{w}")).unwrap() {
                        mine.push(t.id);
                    }
                    mine
                })
            })
            .collect();
        let mut all: Vec<String> = handles.into_iter().flat_map(|h| h.join().unwrap()).collect();
        assert_eq!(all.len(), total, "every task claimed");
        all.sort();
        all.dedup();
        assert_eq!(all.len(), total, "no task claimed twice");
        assert_eq!(queue.count(Some(TaskStatus::InProgress)).unwrap(), total);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_claims_are_reclaimed_but_heartbeating_ones_are_not() {
        let (queue, dir) = temp_queue("stale");
        let a = queue.add("a", "work", 0, "").unwrap();
        let claimed = queue.claim_next("w1").unwrap().unwrap();
        assert_eq!(claimed.id, a.id);
        assert!(queue.heartbeat(&a.id, "w1").unwrap());
        assert!(!queue.heartbeat(&a.id, "other").unwrap(), "wrong owner");

        // Fresh heartbeat: not reclaimed.
        assert_eq!(queue.reclaim_stale(HEARTBEAT_TIMEOUT_SECS).unwrap(), 0);

        // Age the heartbeat: reclaimed, claimable again by another worker.
        let old = (Utc::now() - chrono::Duration::seconds(600)).to_rfc3339();
        queue
            .connect()
            .unwrap()
            .execute("UPDATE tasks SET heartbeat_at = ?1", params![old])
            .unwrap();
        assert_eq!(queue.reclaim_stale(HEARTBEAT_TIMEOUT_SECS).unwrap(), 1);
        assert!(!queue.heartbeat(&a.id, "w1").unwrap(), "old owner lost the task");
        let again = queue.claim_next("w2").unwrap().unwrap();
        assert_eq!(again.id, a.id);

        // Finishing releases ownership.
        queue
            .update_status(&a.id, TaskStatus::Done, Some("ok"), Some(1.0))
            .unwrap();
        assert!(!queue.heartbeat(&a.id, "w2").unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
