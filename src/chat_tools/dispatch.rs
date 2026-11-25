//! Dispatch for all Chat built-in tools.

use anyhow::{anyhow, Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::tasks::{NewTask, TaskKind, TaskQueue};

use super::admin;
use super::schedule as schedule_tools;
use super::shell as shell_tools;

pub fn is_builtin_tool(name: &str) -> bool {
    name.starts_with("piya_mcp_")
        || name.starts_with("piya_task_")
        || name.starts_with("piya_memory_")
        || name.starts_with("piya_run_")
        || name.starts_with("piya_schedule_")
        || name == "piya_notify"
}

pub fn openai_tools() -> Vec<ChatCompletionTool> {
    let mut tools = admin::openai_tools();
    tools.extend(shell_tools::openai_tools());
    tools.extend(notify_tools());
    tools.extend(schedule_tools::openai_tools());
    tools.extend(task_tools());
    tools.extend(memory_tools());
    tools
}

/// Tools for a scheduled `action` run: same as chat, minus queueing new background work.
pub fn background_tools() -> Vec<ChatCompletionTool> {
    openai_tools()
        .into_iter()
        .filter(|t| t.function.name != "piya_task_queue")
        .collect()
}

pub fn system_hint() -> &'static str {
    "How to route requests (critical):\n\
     • Answer questions, facts, explanations, opinions and small talk DIRECTLY in chat. \
       Do not create files or tasks for them.\n\
     • Reminders, timers, alarms, 'ping/notify me at…', and anything recurring ('every day at 6…') → \
       `piya_schedule_create` (kind=reminder for a message, kind=action when work must run at that time). \
       NEVER write scripts, cron jobs, `sleep` commands or 'reminder systems' for these.\n\
     • 'Send me a (test) notification' right now → `piya_notify`. If they want it on screen / major / \
       impossible to miss, or it is urgent, set major=true (also on piya_schedule_create).\n\
     • Remove/cancel/stop/delete a reminder or task → `piya_schedule_list` to find the id, then `piya_task_cancel`. \
       Change a time → `piya_schedule_update`. If it isn't listed, say it doesn't exist.\n\
     • Quick Mac or server actions (find a file, check disk, read a log) → `piya_run_command` yourself, now.\n\
     • Only substantial multi-step work (research reports, writing documents, multi-file changes) → \
       `piya_task_queue` with a clear `done_when`. ONE task per request: research + writing + checking for the \
       same goal go in a single task's brief; never split one request into several tasks.\n\
     • Questions about tasks, results or why something happened → `piya_task_status` / `piya_schedule_list`. \
       Never queue a task to answer a question.\n\
     Verification: only say something is scheduled/cancelled/done when a tool result says so \
     (e.g. 'Saved ✔', 'Cancelled ✔'), and quote the time it reports. If a tool fails, say what failed. \
     When you run a shell command, tell the user the exact command. If the user declines a permission prompt, \
     acknowledge that and suggest an alternative. Never invent results or use [Insert …] placeholders. \
     Confirm actions in one short sentence."
}

pub fn dispatch_builtin_tool(settings: &Settings, name: &str, args_json: &str) -> Result<String> {
    if name.starts_with("piya_mcp_") {
        return admin::dispatch(&settings.data_dir, name, args_json);
    }
    if name.starts_with("piya_run_") {
        return shell_tools::dispatch(settings, args_json);
    }

    let args: Value = if args_json.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(args_json).context("parse tool arguments")?
    };

    if schedule_tools::is_schedule_tool(name) {
        return schedule_tools::dispatch(settings, name, &args);
    }

    match name {
        "piya_task_queue" => queue_task(settings, &args),
        "piya_task_status" => task_status(settings),
        "piya_notify" => send_notify(&args),
        "piya_memory_remember" => remember(settings, &args),
        "piya_memory_recall" => recall(settings, &args),
        _ => Err(anyhow!("unknown built-in tool `{name}`")),
    }
}

fn notify_tools() -> Vec<ChatCompletionTool> {
    vec![tool(
        "piya_notify",
        "Send a macOS Notification Center banner from the Piya app (with Piya branding). \
         Use for test notifications, alerts, and reminders the user should see immediately. \
         Do NOT use osascript or piya_run_command for this.",
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short title (default: Piya)"
                },
                "body": {
                    "type": "string",
                    "description": "Notification message body"
                },
                "major": {
                    "type": "boolean",
                    "description": "true = major alert: a panel in the middle of the screen that stays until the user dismisses it. \
                                    Use when the user asks for an on-screen/major/big/can't-miss alert, or for something urgent. Default false (normal banner)."
                }
            },
            "required": ["body"]
        }),
    )]
}

fn send_notify(args: &Value) -> Result<String> {
    let title = args
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or("Piya")
        .trim();
    let body = args
        .get("body")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`body` is required"))?
        .trim();
    if body.is_empty() {
        return Err(anyhow!("`body` cannot be empty"));
    }
    if args.get("major").and_then(|v| v.as_bool()).unwrap_or(false) {
        crate::notify::major_alert(title, body);
        return Ok(format!("Major alert shown on screen ✔ {title} — {body} (stays until dismissed)"));
    }
    crate::notify::user_alert(title, body);
    Ok(format!("Notification sent ✔ {title} — {body}"))
}

fn task_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            "piya_task_queue",
            "Start a background task for SUBSTANTIAL multi-step work only: research reports, \
             writing a document, multi-file changes. Not for questions, quick lookups, \
             notifications, reminders or timers (use piya_schedule_create / piya_notify / answer directly). \
             Call it ONCE per user request: put every step of the same goal (research, write, verify) \
             in one brief. Starts immediately.",
            json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short title" },
                    "brief": {
                        "type": "string",
                        "description": "Complete description of the work, with all details the user gave"
                    },
                    "done_when": {
                        "type": "string",
                        "description": "Checkable completion condition, e.g. 'workspace/okr_overview.md exists with sections X, Y, Z and cited sources'"
                    }
                },
                "required": ["title", "brief", "done_when"]
            }),
        ),
        tool(
            "piya_task_status",
            "List Piya tasks: running, queued, and recently completed/failed with full descriptions \
             and result summaries. Call this when the user asks about tasks, results, \
             or why something did/didn't happen — never queue a task to answer.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
    ]
}

fn memory_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            "piya_memory_remember",
            "Save a durable fact to long-term memory. For infra/servers, store ONE complete note \
             with every field the user gave (name, Instance ID, instance type, volume id, storage size, \
             upgrade history). Never fragment into short incomplete snippets; never drop IDs.",
            json!({
                "type": "object",
                "properties": {
                    "text": { "type": "string" }
                },
                "required": ["text"]
            }),
        ),
        tool(
            "piya_memory_recall",
            "Search long-term memory for saved facts. Use a specific query with proper nouns \
             (server name, Instance ID, project) when answering 'what are my … details' questions.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
        ),
    ]
}

fn queue_task(settings: &Settings, args: &Value) -> Result<String> {
    let get = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let brief = get("brief").ok_or_else(|| anyhow!("`brief` is required"))?;
    let title: String = get("title")
        .unwrap_or(brief)
        .chars()
        .take(80)
        .collect();
    let done_when = get("done_when");

    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    let mut new = NewTask::new(&title, brief, TaskKind::Task);
    new.done_when = done_when;
    let task = queue.insert(&new)?;
    if let Err(err) = crate::worker::spawn_worker(settings, &["run", "--continuous"]) {
        return Ok(format!(
            "Queued task `{}` \"{}\", but the worker could not start: {err:#}",
            task.short_id(),
            task.title
        ));
    }
    Ok(format!(
        "Queued ✔ task `{}` \"{}\" — running in the background. Done when: {}",
        task.short_id(),
        task.title,
        task.done_when.as_deref().unwrap_or("(not set)")
    ))
}

fn task_status(settings: &Settings) -> Result<String> {
    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    Ok(crate::ui::assign::format_task_chat_report(
        &queue,
        Some(&settings.workspace),
    ))
}

fn remember(settings: &Settings, args: &Value) -> Result<String> {
    let text = args
        .get("text")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`text` is required"))?
        .trim();
    if text.is_empty() {
        return Err(anyhow!("`text` cannot be empty"));
    }
    crate::ui::assign::store_memory(settings, text)?;
    Ok(format!("Saved to memory ✔ {text}"))
}

fn recall(settings: &Settings, args: &Value) -> Result<String> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`query` is required"))?
        .trim();
    if query.is_empty() {
        return Err(anyhow!("`query` cannot be empty"));
    }
    crate::ui::assign::search_memory(settings, query)
}

pub(crate) fn tool(name: &str, description: &str, parameters: Value) -> ChatCompletionTool {
    ChatCompletionTool {
        r#type: ChatCompletionToolType::Function,
        function: FunctionObject {
            name: name.to_string(),
            description: Some(description.to_string()),
            parameters: Some(parameters),
            strict: None,
        },
    }
}
