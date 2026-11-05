//! Dispatch for all Chat built-in tools.

use anyhow::{anyhow, Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::tasks::TaskQueue;
use crate::ui::assign::{queue_from_brief, recall_from_chat, remember_from_chat, spawn_if_needed};

use super::admin;
use super::shell as shell_tools;

pub fn is_builtin_tool(name: &str) -> bool {
    name.starts_with("piya_mcp_")
        || name.starts_with("piya_task_")
        || name.starts_with("piya_memory_")
        || name.starts_with("piya_run_")
        || name == "piya_notify"
}

pub fn openai_tools() -> Vec<ChatCompletionTool> {
    let mut tools = admin::openai_tools();
    tools.extend(shell_tools::openai_tools());
    tools.extend(notify_tools());
    tools.extend(task_tools());
    tools.extend(memory_tools());
    tools
}

pub fn system_hint() -> &'static str {
    "Built-in tools (use these — do NOT tell the user to switch modes):\n\
     • `piya_notify` — send a macOS Notification Center banner from the Piya app \
       (use this for test notifications and alerts — NEVER use osascript display notification)\n\
     • `piya_run_command` — Mac or sticky SSH host. Use host=\"aapanel\" (or ssh aapanel '…'). \
       After remote is sticky, follow-up commands stay remote unless host=\"local\". \
       Never interactive bare ssh. Do NOT use osascript for notifications.\n\
     • `piya_task_queue` — queue NEW research, reports, scheduled/recurring work (starts worker when due now). \
       Do NOT use this for questions, status checks, or “why didn’t X happen” — use `piya_task_status` instead\n\
     • `piya_task_status` — list running, queued, and recent tasks with descriptions and results \
       (always use this when the user asks about tasks, queue, recent work, or why something failed — do NOT say you lack access; do NOT re-queue)\n\
     • `piya_memory_remember` — save a durable fact (one complete note; keep Instance IDs / specs intact)\n\
     • `piya_memory_recall` — search vector memory (use for 'what are my server details' etc.)\n\
     • `piya_mcp_list`, `piya_mcp_upsert`, `piya_mcp_remove` — manage MCP servers\n\
     For local Mac tasks, always try `piya_run_command` first. \
     For notifications/alerts, always use `piya_notify`. \
     Use MCP tools for Nextcloud, wiki, and other connected services. \
     When the user asks you to do NEW work, call `piya_task_queue` with a clear brief. \
     When they ask why a task/notification failed or what happened, call `piya_task_status` and explain — never create another identical task. \
     When you run a shell command, briefly tell the user the exact command (e.g. ran `mdfind …`). \
     If the user declines a permission prompt, acknowledge that and suggest an alternative. \
     Never invent results or use [Insert …] placeholders — only cite tool output. \
     Confirm actions briefly."
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
    crate::notify::user_alert(title, body);
    Ok(format!("Notification sent: {title} — {body}"))
}

fn task_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            "piya_task_queue",
            "Queue a NEW task for Piya's background agent (research, write files to workspace/, \
             one-off or scheduled/recurring jobs). Use ONLY when the user asks you to do new work. \
             Do NOT use for questions about past tasks, status, results, or missing notifications — \
             use piya_task_status instead.",
            json!({
                "type": "object",
                "properties": {
                    "brief": {
                        "type": "string",
                        "description": "Full task description including schedule if any (e.g. 'every day at 6am send stats')"
                    }
                },
                "required": ["brief"]
            }),
        ),
        tool(
            "piya_task_status",
            "List Piya tasks: running, queued, and recently completed/failed with full descriptions \
             and result summaries. Always call this when the user asks about tasks, the queue, \
             recent work, task outcomes, notifications, or why something did/didn't happen — \
             never say you lack workspace access, and never queue a duplicate task to answer.",
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
    let brief = args
        .get("brief")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`brief` is required"))?
        .trim();
    if brief.is_empty() {
        return Err(anyhow!("`brief` cannot be empty"));
    }

    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    let outcome = queue_from_brief(&queue, brief)?;
    spawn_if_needed(settings, &outcome);
    Ok(outcome.reply)
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
    Ok(remember_from_chat(settings, &format!("remember that {text}")).reply)
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
    Ok(recall_from_chat(settings, &format!("recall {query}")).reply)
}

fn tool(name: &str, description: &str, parameters: Value) -> ChatCompletionTool {
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
