use anyhow::Result;

use crate::chat_service::{self, ChatIntent, ChatTurn};
use crate::config::Settings;
use crate::memory::MemoryKind;
use crate::schedule;
use crate::tasks::{Task, TaskQueue, TaskStatus};

#[derive(Debug, Clone)]
pub struct AssignOutcome {
    pub reply: String,
    pub task: Option<Task>,
    pub run_worker: bool,
}

/// Task mode: same model-driven routing as Chat, with a hint that the user wants
/// something done. The model picks schedule / notify / background task / answer —
/// no keyword routing.
pub fn converse_task(settings: &Settings, history: &[ChatTurn], text: &str) -> AssignOutcome {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return AssignOutcome {
            reply: "What should I work on?".into(),
            task: None,
            run_worker: false,
        };
    }
    let reply = match chat_service::chat_reply_with_intent(settings, history, trimmed, ChatIntent::Task) {
        Ok(text) => text,
        Err(err) => format!("Sorry, I couldn't handle that: {err:#}"),
    };
    AssignOutcome {
        reply,
        task: None,
        run_worker: false,
    }
}

pub fn store_memory(settings: &Settings, text: &str) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        anyhow::bail!("nothing to remember");
    }
    let settings = settings.clone();
    let text = text.to_string();
    crate::runtime::block_on(async move {
        let (_llm, memory) = crate::chat_engine::llm_and_memory(&settings)?;
        memory
            .remember_with_source(
                &text,
                MemoryKind::Learning,
                None,
                &["chat".into(), "user".into()],
                0.9,
                "user",
            )
            .await?;
        Ok(())
    })
}

pub fn search_memory(settings: &Settings, query: &str) -> Result<String> {
    let settings = settings.clone();
    let query = query.trim().to_string();
    crate::runtime::block_on(async move {
        let mut lines: Vec<String> = Vec::new();
        if let Ok(conn) = crate::db::open(&settings.data_dir) {
            if let Ok(note_hits) = crate::notes::search_keyword(&conn, &query, 10) {
                for n in note_hits {
                    lines.push(format!("- [{}/{} · {}] {}", n.area, n.slug, n.status, n.text));
                }
            }
        }
        if let Ok((_llm, memory)) = crate::chat_engine::llm_and_memory(&settings) {
            if let Ok(hits) = memory
                .recall(&query, crate::memory::RecallOpts::search(8))
                .await
            {
                for hit in hits {
                    let line = format!("- [vector:{}] {}", hit.source, hit.text);
                    if !lines.iter().any(|existing| existing.contains(&hit.text)) {
                        lines.push(line);
                    }
                }
            }
        }
        if lines.is_empty() {
            return Ok(format!("Nothing saved in memory about \"{query}\"."));
        }
        let mut out = vec![format!("Memory results for \"{query}\":")];
        out.extend(lines);
        Ok(out.join("\n"))
    })
}

pub fn spawn_if_needed(settings: &Settings, outcome: &AssignOutcome) {
    if !outcome.run_worker {
        return;
    }
    if let Err(err) = crate::worker::spawn_worker(settings, &["run", "--continuous"]) {
        eprintln!("piya: worker spawn failed: {err}");
        let log_path = settings.data_dir.join("piya-worker.log");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[spawn] {err}")
            });
    }
}

pub fn format_queue_summary(queue: &TaskQueue) -> String {
    format_task_chat_report(queue, None)
}

/// Rich task report for Chat (`piya_task_status`), optionally including workspace path.
pub fn format_task_chat_report(queue: &TaskQueue, workspace: Option<&std::path::Path>) -> String {
    let running = queue.list(Some(TaskStatus::InProgress), 5).unwrap_or_default();
    let pending = queue.list(Some(TaskStatus::Pending), 12).unwrap_or_default();
    let recent = queue.list_recent_finished(8).unwrap_or_default();

    if running.is_empty() && pending.is_empty() && recent.is_empty() {
        return "No tasks yet — tell me what to work on.".into();
    }

    let mut lines = Vec::new();

    if !running.is_empty() {
        lines.push("**Running**".into());
        for t in &running {
            lines.push(format_task_block(t, true));
        }
    }

    if !pending.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("**Queued / scheduled**".into());
        for t in &pending {
            lines.push(format_task_block(t, true));
        }
    }

    if !recent.is_empty() {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push("**Recent (completed / failed / cancelled)**".into());
        for t in &recent {
            lines.push(format_task_block(t, false));
        }
    }

    if let Some(ws) = workspace {
        lines.push(String::new());
        lines.push(format!(
            "Task output files live in workspace: `{}`. \
             To read them, call `piya_run_command` with `cwd` set to that path.",
            ws.display()
        ));
    }

    lines.join("\n")
}

fn format_task_block(task: &Task, include_schedule: bool) -> String {
    let mut lines = vec![format!(
        "• `{}` **{}** ({} · {})",
        task.short_id(),
        task.title,
        task.kind.as_str(),
        task.status.as_str()
    )];
    if !task.description.is_empty() && task.description != task.title {
        lines.push(format!("  Description: {}", truncate_chat(&task.description, 400)));
    }
    if include_schedule {
        if let Some(desc) =
            schedule::describe_task_schedule(task.run_at.as_deref(), task.recurrence.as_deref())
        {
            lines.push(format!("  Schedule: {desc}"));
        }
    }
    if let Some(result) = task.result.as_deref().filter(|s| !s.is_empty()) {
        lines.push(format!("  Result: {}", truncate_chat(result, 500)));
    }
    lines.push(format!("  Updated: {}", task.updated_at));
    lines.join("\n")
}

fn truncate_chat(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

