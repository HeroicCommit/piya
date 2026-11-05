use anyhow::Result;

use crate::agent::expertise;
use crate::config::Settings;
use crate::memory::MemoryKind;
use crate::schedule::{self, Schedule};
use crate::tasks::{Task, TaskQueue, TaskStatus};

const HELP: &str = "You're in **Task** mode — messages become work, not chat.

**Tasks**:
• Research Rust async → workspace/rust.md
• Send daily stats every day at 6:30 am
• Remind me at 9pm to review the queue

**Memory**:
• Remember that the user prefers markdown reports
• Note: API docs are at docs.example.com

**Commands**:
• queue — pending tasks
• recall <topic> — search memory
• help — this message

Switch to **Chat** mode for normal conversation.";

#[derive(Debug, Clone)]
pub struct AssignOutcome {
    pub reply: String,
    pub task: Option<Task>,
    pub run_worker: bool,
}

pub fn converse_task(queue: &TaskQueue, settings: &Settings, text: &str) -> AssignOutcome {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return AssignOutcome {
            reply: "What should I work on?".into(),
            task: None,
            run_worker: false,
        };
    }

    let lower = trimmed.to_lowercase();
    if is_help(&lower) {
        return AssignOutcome {
            reply: HELP.into(),
            task: None,
            run_worker: false,
        };
    }
    if is_queue_request(&lower) {
        return AssignOutcome {
            reply: format_queue_summary(queue),
            task: None,
            run_worker: false,
        };
    }
    if is_greeting(&lower) {
        return AssignOutcome {
            reply: "Hey! What can I help you with today?".into(),
            task: None,
            run_worker: false,
        };
    }
    if is_thanks(&lower) {
        return AssignOutcome {
            reply: "Anytime — assign another task or tell me something to remember.".into(),
            task: None,
            run_worker: false,
        };
    }
    if is_recall_request(&lower) {
        return recall_from_chat(settings, trimmed);
    }
    if is_memory_request(trimmed) && !should_queue_as_task(trimmed) {
        return remember_from_chat(settings, trimmed);
    }

    match queue_from_brief(queue, trimmed) {
        Ok(outcome) => outcome,
        Err(err) => AssignOutcome {
            reply: format!("Couldn't queue that: {err}"),
            task: None,
            run_worker: false,
        },
    }
}

pub fn remember_from_chat(settings: &Settings, text: &str) -> AssignOutcome {
    let content = extract_memory_content(text);
    if content.is_empty() {
        return AssignOutcome {
            reply: "What should I remember? Try: \"Remember that …\" or \"Note: …\"".into(),
            task: None,
            run_worker: false,
        };
    }

    match store_memory(settings, &content) {
        Ok(_) => AssignOutcome {
            reply: format!("Got it — I'll remember:\n**{content}**"),
            task: None,
            run_worker: false,
        },
        Err(err) => AssignOutcome {
            reply: format!("Couldn't save that to memory: {err}"),
            task: None,
            run_worker: false,
        },
    }
}

pub fn recall_from_chat(settings: &Settings, text: &str) -> AssignOutcome {
    let query = extract_recall_query(text);
    if query.is_empty() {
        return AssignOutcome {
            reply: "What should I look up? Try: \"recall API keys\" or \"what do you remember about deploys?\"".into(),
            task: None,
            run_worker: false,
        };
    }

    match search_memory(settings, &query) {
        Ok(lines) => AssignOutcome {
            reply: lines,
            task: None,
            run_worker: false,
        },
        Err(err) => AssignOutcome {
            reply: format!("Couldn't search memory: {err}"),
            task: None,
            run_worker: false,
        },
    }
}

fn store_memory(settings: &Settings, text: &str) -> Result<()> {
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

fn search_memory(settings: &Settings, query: &str) -> Result<String> {
    let settings = settings.clone();
    let query = query.to_string();
    crate::runtime::block_on(async move {
        let (_llm, memory) = crate::chat_engine::llm_and_memory(&settings)?;
        let hits = memory
            .recall(&query, crate::memory::RecallOpts::search(8))
            .await?;
        if hits.is_empty() {
            return Ok(format!("I don't have anything remembered about **{query}** yet."));
        }
        let mut lines = vec![format!("Here's what I remember about **{query}**:")];
        for (i, hit) in hits.iter().enumerate() {
            lines.push(format!("{}. {}", i + 1, hit.text));
        }
        Ok(lines.join("\n"))
    })
}

pub fn queue_from_brief(queue: &TaskQueue, brief: &str) -> Result<AssignOutcome> {
    let title: String = brief.chars().take(80).collect();
    let tags = merge_tags("", &title, brief);
    let (run_at_str, recurrence_str) = schedule::encode_for_task(brief);

    let task = queue.add_with_schedule(
        &title,
        brief,
        1,
        &tags,
        run_at_str.as_deref(),
        recurrence_str.as_deref(),
    )?;

    let schedule_desc =
        schedule::describe_task_schedule(task.run_at.as_deref(), task.recurrence.as_deref());

    let scheduled_future = task
        .run_at
        .as_deref()
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .is_some_and(|dt| dt.with_timezone(&chrono::Utc) > chrono::Utc::now());

    let reply = if task.recurrence.is_some() {
        format!(
            "Set up a recurring task: **{}**.\n{}",
            task.title,
            schedule_desc.unwrap_or_else(|| "I'll run it on schedule.".into())
        )
    } else if scheduled_future {
        let kind = if crate::reminder::is_reminder_text(&brief.to_lowercase()) {
            "reminder"
        } else {
            "task"
        };
        format!(
            "Scheduled {kind}: **{}**.\n{}",
            task.title,
            schedule_desc.unwrap_or_default()
        )
    } else {
        format!("Got it — queued **{}**. I'll start working on it now.", task.title)
    };

    let run_worker = !scheduled_future && task.recurrence.is_none();

    Ok(AssignOutcome {
        reply,
        task: Some(task),
        run_worker,
    })
}

pub fn spawn_if_needed(settings: &Settings, outcome: &AssignOutcome) {
    if !outcome.run_worker {
        return;
    }
    if let Err(err) = crate::worker::spawn_worker(settings, &["run", "--continuous"]) {
        log_spawn_error(settings, &err);
    }
}

fn log_spawn_error(settings: &Settings, err: &anyhow::Error) {
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

/// User wants to store a fact/preference — not run a task.
pub fn is_memory_request(text: &str) -> bool {
    let lower = text.trim().to_lowercase();
    lower.starts_with("remember that ")
        || lower.starts_with("remember: ")
        || lower.starts_with("note that ")
        || lower.starts_with("note: ")
        || lower.starts_with("keep in mind")
        || lower.starts_with("don't forget that ")
        || lower.starts_with("dont forget that ")
        || lower.starts_with("save this:")
        || lower.starts_with("save that:")
        || (lower.starts_with("remember ")
            && !lower.starts_with("remember to ")
            && !lower.starts_with("remember me "))
}

/// "Remember" phrasing that means a reminder/scheduled task, not a stored memory.
pub fn should_queue_as_task(text: &str) -> bool {
    let lower = text.to_lowercase();
    if lower.starts_with("remember to ")
        || lower.starts_with("remember me ")
        || lower.starts_with("remind me ")
    {
        return true;
    }
    if lower.contains("workspace/") {
        return true;
    }
    !matches!(schedule::parse_schedule(text), Schedule::Immediate)
}

fn extract_memory_content(text: &str) -> String {
    let patterns = [
        r"(?i)^remember that\s+",
        r"(?i)^remember:\s*",
        r"(?i)^note that\s+",
        r"(?i)^note:\s*",
        r"(?i)^keep in mind that\s+",
        r"(?i)^keep in mind:\s*",
        r"(?i)^keep in mind\s+",
        r"(?i)^don't forget that\s+",
        r"(?i)^dont forget that\s+",
        r"(?i)^save this:\s*",
        r"(?i)^save that:\s*",
        r"(?i)^remember\s+",
    ];
    let mut s = text.trim().to_string();
    for pat in patterns {
        if let Ok(re) = regex::Regex::new(pat) {
            if let Some(m) = re.find(&s) {
                if m.start() == 0 {
                    s = s[m.end()..].trim().to_string();
                    break;
                }
            }
        }
    }
    s
}

pub fn is_recall_request(lower: &str) -> bool {
    lower.starts_with("recall ")
        || lower.contains("what do you remember")
        || lower.contains("what you remember")
        || lower.starts_with("do you remember")
        || lower.contains("what do you know about")
        || is_saved_facts_question(lower)
}

/// Clear "look up what you saved" questions (not operational server talk).
fn is_saved_facts_question(lower: &str) -> bool {
    let asking = lower.contains("what ")
        || lower.contains("what's ")
        || lower.contains("whats ")
        || lower.contains("give me")
        || lower.contains("tell me")
        || lower.contains("show me")
        || lower.starts_with("do you know")
        || lower.contains("saved ");
    if !asking {
        return false;
    }
    lower.contains("server detail")
        || lower.contains("server spec")
        || lower.contains("my server")
        || lower.contains("instance id")
        || lower.contains("instance detail")
        || lower.contains("remember about")
        || lower.contains("you remember")
}

fn extract_recall_query(text: &str) -> String {
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("recall ") {
        return text[text.len() - rest.len()..].trim().to_string();
    }
    if let Some(idx) = lower.find("about ") {
        return text[idx + 6..].trim().trim_end_matches('?').to_string();
    }
    if let Some(rest) = lower.strip_prefix("do you remember ") {
        return text[text.len() - rest.len()..].trim().trim_end_matches('?').to_string();
    }
    // Fact questions like "give me the server details" — use a focused query.
    if lower.contains("server") {
        return "server instance id specs storage volume".into();
    }
    text.trim().trim_end_matches('?').to_string()
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
    let mut lines = vec![format!("• **{}** ({})", task.title, task.status.as_str())];
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
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

fn merge_tags(user_tags: &str, title: &str, description: &str) -> String {
    let mut tags: Vec<String> = user_tags
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    for d in expertise::detect_domains(title, description) {
        let t = d.as_str().to_string();
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    tags.join(",")
}

fn is_help(lower: &str) -> bool {
    matches!(
        lower,
        "help" | "?" | "commands" | "what can you do" | "what can you do?"
    ) || lower.starts_with("help ")
}

fn is_queue_request(lower: &str) -> bool {
    lower.contains("what's in my queue")
        || lower.contains("whats in my queue")
        || lower.contains("show queue")
        || lower.contains("view queue")
        || lower == "queue"
        || lower == "list"
        || lower == "list tasks"
        || lower == "pending"
        || lower == "what's pending"
        || lower == "whats pending"
}

fn is_greeting(lower: &str) -> bool {
    matches!(
        lower,
        "hi" | "hello" | "hey" | "yo" | "hiya" | "good morning" | "good evening"
    )
}

fn is_thanks(lower: &str) -> bool {
    lower.starts_with("thank") || lower == "thx" || lower == "ty"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_that_is_memory_not_task() {
        assert!(is_memory_request("Remember that the user prefers markdown"));
        assert!(!should_queue_as_task("Remember that the user prefers markdown"));
    }

    #[test]
    fn remember_to_is_task() {
        assert!(!is_memory_request("Remember to send the report"));
        assert!(should_queue_as_task("Remember to send the report"));
    }

    #[test]
    fn remind_me_is_task() {
        assert!(should_queue_as_task("Remind me around 6:30 am for daily stats"));
    }

    #[test]
    fn extracts_memory_content() {
        assert_eq!(
            extract_memory_content("Remember that the API is at /v2"),
            "the API is at /v2"
        );
    }

    #[test]
    fn server_details_question_is_recall() {
        assert!(is_recall_request("give me the server details"));
        assert!(is_recall_request("what are my server specs?"));
        assert!(!is_recall_request("ssh to the instance id and check disk"));
    }
}
