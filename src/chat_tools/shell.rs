//! Run sandboxed shell commands from Chat.

use anyhow::{anyhow, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::remote_shell;
use crate::shell;

pub fn openai_tools() -> Vec<ChatCompletionTool> {
    vec![tool(
        "piya_run_command",
        "Run a shell command on the user's Mac — or on a sticky SSH host.\n\
         File search: mdfind -name 'report.pdf', find ~/Documents -name '*.csv' -maxdepth 4, ls -la ~/Downloads\n\
         Read files: cat, head, grep, rg\n\
         Timers: open -a Clock, OR nohup sleep (notifications → piya_notify, not osascript)\n\
         Remote SSH:\n\
         • Prefer `host` arg: host=\"aapanel\", command=\"ls /www/...\" (auto-wraps as ssh).\n\
         • Or pass a full `ssh aapanel '…'` command — that sets sticky host for later turns.\n\
         • While sticky host is set, bare commands like `ls /path` or `cat file` run ON THE REMOTE \
           host automatically (not locally). Pass host=\"local\" to force this Mac.\n\
         • NEVER open interactive `ssh host` alone. Key-based auth (BatchMode) required.\n\
         • For logs: ls the dir first, then head/grep/wc — only cite tool stdout.\n\
         Background (nohup … &) returns immediately. Outside the allow-list → Allow/Deny prompt.",
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "Shell command to run (remote commands may omit ssh when sticky host is set)"
                },
                "host": {
                    "type": "string",
                    "description": "SSH host/alias (e.g. aapanel). Sets sticky remote target. Use \"local\" to run on this Mac."
                },
                "cwd": {
                    "type": "string",
                    "description": "Local working directory on this Mac (ignored for remote-wrapped commands)"
                }
            },
            "required": ["command"]
        }),
    )]
}

pub fn dispatch(settings: &Settings, args_json: &str) -> Result<String> {
    let args: Value = serde_json::from_str(args_json).map_err(|err| anyhow!("parse tool arguments: {err}"))?;
    let command = args
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("`command` is required"))?
        .trim();
    if command.is_empty() {
        return Err(anyhow!("`command` cannot be empty"));
    }
    let host = args.get("host").and_then(|v| v.as_str());
    let cwd = args.get("cwd").and_then(|v| v.as_str());

    let (command, wrap_note) = remote_shell::maybe_wrap_remote(&settings.data_dir, command, host);
    let output = shell::run_chat_command(&command, cwd, &settings.workspace)?;
    let mut text = output.to_llm_text();
    if let Some(note) = wrap_note {
        text = format!("[{note}]\n{text}");
    }
    if let Some(hint) = remote_shell::prompt_hint(&settings.data_dir) {
        // Remind the model sticky state persists for the next tool call.
        if !text.contains("Sticky remote shell") {
            text = format!("{text}\n\n({hint})");
        }
    }
    Ok(text)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn runs_pwd() {
        let settings = Settings {
            openai_api_key: String::new(),
            openai_base_url: String::new(),
            model: String::new(),
            embedding_model: String::new(),
            workspace: PathBuf::from("/tmp/piya-shell-test"),
            data_dir: PathBuf::from("/tmp/piya-shell-test"),
            poll_interval_sec: 30,
            max_cycles: 0,
            min_accuracy: 0.7,
            max_retries: 3,
            workers: 1,
        };
        let args = json!({ "command": "pwd", "host": "local" });
        let out = dispatch(&settings, &args.to_string()).unwrap();
        assert!(out.contains("cwd:"));
    }
}
