//! Chat reply logic shared by wry UI and SwiftUI (via FFI).

use crate::chat_engine;
use crate::chat_tools::{dispatch_builtin_tool, is_builtin_tool, openai_tools, system_hint};
use crate::config::Settings;
use crate::prefs::{self, UserPrefs};
use crate::runtime;
use crate::ui::assign::{
    is_memory_request, is_recall_request, recall_from_chat, remember_from_chat, should_queue_as_task,
};

const CHAT_SYSTEM: &str = "You are Piya, a friendly AI assistant on the user's Mac. \
Have a natural, helpful conversation. Answer questions clearly and concisely. \
You know about software development, SEO, security, and project management. \
Use `piya_run_command` to act on the Mac — do not say you cannot do something if a command can handle it:\n\
• Find files → mdfind, find, ls, grep\n\
• Start a timer → open -a Clock, or nohup sh -c 'sleep N && …'\n\
• Check time → date\n\
• Remote server → prefer `piya_run_command` with host=\"aapanel\" and the remote command \
  (or `ssh aapanel '…'`). After the first successful remote call, sticky SSH stays on — \
  the NEXT command must stay remote automatically; do NOT run bare `ls`/`cat` locally. \
  Use host=\"local\" only when you intentionally want this Mac.\n\
• Send a notification → `piya_notify` (never osascript display notification)\n\
Use MCP tools for connected services (Nextcloud, wiki, etc.). \
Never tell the user you lack a capability when a tool or command can do it. \
When the user asks you to do NEW work (research, reports, files, schedules, reminders), \
use the built-in `piya_task_queue` tool — never tell them to switch to Task mode for that. \
When they ask about tasks, queue status, recent work, results, notifications, or WHY something \
happened (or did not happen), call `piya_task_status` and answer from that — \
do NOT queue a duplicate task. Questions are not new work. \
Never say you lack workspace or task access. \
Task mode in the UI is optional; Chat mode can queue work too. \
Use markdown sparingly (**bold** for emphasis, [labels](url) for links). Keep replies focused.\n\n\
Memory rules (critical):\n\
• When the user shares durable facts (server Instance ID, specs, IPs, volume IDs, storage upgrades, \
  credentials locations, project URLs), call `piya_memory_remember` with ONE dense note that includes \
  ALL fields they gave — never split into short incomplete snippets, never paraphrase away IDs.\n\
• When they ask what you remember / for saved details / server specs, call `piya_memory_recall` \
  with a specific query (e.g. \"Allure server instance id storage\") if injected memories look incomplete.\n\
• Answer memory questions ONLY from recalled/injected facts. Quote exact IDs and numbers. \
  If a field was never saved, say so — do NOT invent suggestions or fill gaps with advice.\n\n\
Grounding rules (critical):\n\
• NEVER invent file contents, metrics, or “insights” you did not see in a tool result.\n\
• NEVER output placeholder text like [Insert …], [TODO], or blank templates for the user to fill.\n\
• NEVER claim SSH/connect/fetch succeeded unless a `piya_run_command` result shows success.\n\
• NEVER ask the user to run a command you can run yourself with `piya_run_command`.\n\
• If a remote path is missing, run `ssh <host> 'ls -la <dir>'` (and similar) yourself, then fix the path.\n\
• When analyzing logs/reports, use concrete numbers and names from tool stdout. If the tool failed, say so with the exact error.";

const HISTORY_LIMIT: usize = 20;

#[derive(Clone, Debug)]
pub struct ChatTurn {
    pub role: String,
    pub text: String,
}

pub fn chat_reply(
    settings: &Settings,
    history: &[ChatTurn],
    user_text: &str,
) -> anyhow::Result<String> {
    let settings = settings.clone();
    let history = history.to_vec();
    let user_text = user_text.to_string();
    let epoch = crate::chat_cancel::begin();
    runtime::block_on(async move { reply_async(&settings, &history, &user_text, epoch).await })
}

async fn reply_async(
    settings: &Settings,
    history: &[ChatTurn],
    user_text: &str,
    epoch: u64,
) -> anyhow::Result<String> {
    crate::chat_cancel::check(epoch)?;
    // Deterministic memory paths — don't rely on the model calling tools.
    if is_memory_request(user_text) && !should_queue_as_task(user_text) {
        return Ok(remember_from_chat(settings, user_text).reply);
    }
    let lower = user_text.to_lowercase();
    if is_recall_request(&lower) {
        return Ok(recall_from_chat(settings, user_text).reply);
    }

    let (llm, memory) = chat_engine::llm_and_memory(settings)?;
    let prefs = UserPrefs::load(&settings.data_dir).unwrap_or_default();

    let recall_query = if looks_like_fact_lookup(&lower) {
        expand_fact_query(user_text)
    } else {
        user_text.to_string()
    };
    let hits = match memory
        .recall(&recall_query, crate::memory::RecallOpts::chat(6))
        .await
    {
        Ok(hits) => hits,
        Err(err) => {
            // Don't fail the whole chat if memory/embeddings hiccup (e.g. oversized paste).
            eprintln!("piya: memory recall skipped: {err}");
            Vec::new()
        }
    };
    let memory_context = crate::memory::VectorMemory::format_context(&hits);

    let mut system = CHAT_SYSTEM.to_string();
    system.push_str(&format!(
        "\n\nPiya workspace (task output files): `{}`\n\
         To read workspace files, call `piya_run_command` with `cwd` set to that path \
         (e.g. `cat report.md` with cwd = workspace).",
        settings.workspace.display()
    ));

    let profile = prefs.profile.trim();
    if !profile.is_empty() {
        system.push_str("\n\nAbout the user (always honor these preferences):\n");
        system.push_str(profile);
    }

    if prefs.auto_capture_prefs {
        system.push_str(
            "\n\nAuto-capture preferences is ON. When the user clearly states a lasting \
             preference (how they like things done, name, format defaults), call \
             `piya_memory_remember` with a concise fact. Do not save one-off tasks or reminders.",
        );
    }

    system.push_str("\n\n");
    system.push_str(system_hint());

    if !hits.is_empty() {
        system.push_str("\n\nRelevant things you remember from prior work:\n");
        system.push_str(&memory_context);
    }

    let mut tools = openai_tools();
    const OPENAI_TOOL_LIMIT: usize = 128;
    let mcp_budget = OPENAI_TOOL_LIMIT.saturating_sub(tools.len());
    let mcp = chat_engine::mcp_snapshot(&settings.data_dir, mcp_budget)?;
    tools.extend(mcp.tools);

    if !mcp.is_empty || mcp.has_notes {
        system.push_str("\n\n");
        system.push_str(&mcp.summary);
    }

    if let Some(hint) = crate::remote_shell::prompt_hint(&settings.data_dir) {
        system.push_str("\n\n");
        system.push_str(&hint);
    }

    let turn_history: Vec<(String, String)> = history
        .iter()
        .rev()
        .take(HISTORY_LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|m| (m.role.clone(), m.text.clone()))
        .collect();

    let settings = settings.clone();
    let mut raw = llm
        .chat_with_tools(
            &system,
            &turn_history,
            user_text,
            &tools,
            |name, args| {
                crate::chat_cancel::check(epoch)?;
                if is_builtin_tool(name) {
                    dispatch_builtin_tool(&settings, name, args)
                } else {
                    chat_engine::mcp_call(name, args)
                }
            },
            0.25,
            16,
            Some(epoch),
        )
        .await?;

    if looks_like_unfilled_template(&raw) {
        eprintln!("piya: placeholder/template reply detected — retrying with stricter grounding");
        let retry_user = format!(
            "{user_text}\n\n[Piya system: Your last draft was invalid — it used unfilled \
             placeholders like [Insert …] or asked the user for data you should fetch. \
             Call `piya_run_command` again if needed (e.g. ssh <host> 'ls …' then cat/grep the files). \
             Answer ONLY with concrete facts from tool stdout. If tools fail, report the exact error. \
             Do not output any [Insert …] brackets.]"
        );
        raw = llm
            .chat_with_tools(
                &system,
                &turn_history,
                &retry_user,
                &tools,
                |name, args| {
                    crate::chat_cancel::check(epoch)?;
                    if is_builtin_tool(name) {
                        dispatch_builtin_tool(&settings, name, args)
                    } else {
                        chat_engine::mcp_call(name, args)
                    }
                },
                0.1,
                16,
                Some(epoch),
            )
            .await?;
    }

    // Light heuristic stub: if auto-capture is on and the user message looks like a preference,
    // store it even when the model skipped the tool call.
    if prefs.auto_capture_prefs && prefs::looks_like_preference(user_text) {
        let _ = remember_from_chat(&settings, &format!("remember that {user_text}"));
    }

    Ok(raw.trim().to_string())
}

fn looks_like_fact_lookup(lower: &str) -> bool {
    lower.contains("server")
        || lower.contains("instance")
        || lower.contains("volume")
        || lower.contains("storage")
        || lower.contains("specs")
        || lower.contains("my aws")
        || lower.contains("credentials")
}

fn expand_fact_query(user_text: &str) -> String {
    let lower = user_text.to_lowercase();
    if lower.contains("server") || lower.contains("instance") || lower.contains("storage") {
        format!("{user_text} instance id volume id storage specs type")
    } else {
        user_text.to_string()
    }
}

/// Model sometimes emits fill-in-the-blank “insights” instead of using tool results.
fn looks_like_unfilled_template(s: &str) -> bool {
    let lower = s.to_lowercase();
    if lower.contains("[insert")
        || lower.contains("[your ")
        || lower.contains("[todo")
        || lower.contains("[number")
        || lower.contains("[name")
    {
        return true;
    }
    // Several bracket placeholders in one reply.
    let brackets = lower.matches('[').count();
    brackets >= 4 && (lower.contains("insert") || lower.contains("provide me with"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;

    #[test]
    fn detects_placeholder_templates() {
        assert!(looks_like_unfilled_template(
            "### Report\n- Total Plugins: [Insert total number of plugins from the log]\n- Theme: [Insert theme name]"
        ));
        assert!(!looks_like_unfilled_template(
            "Total plugins: 42. Theme: Twenty Twenty-Four. Active: 18."
        ));
    }

    #[test]
    #[ignore = "requires OPENAI_API_KEY and local MCP servers"]
    fn chat_reply_smoke() {
        let settings = Settings::load().expect("settings");
        let reply = chat_reply(&settings, &[], "Say hi in one short sentence.").expect("reply");
        assert!(!reply.is_empty(), "{reply}");
    }
}
