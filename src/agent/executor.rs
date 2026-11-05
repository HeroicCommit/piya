use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use async_openai::types::{ChatCompletionTool, ChatCompletionToolType, FunctionObject};
use serde_json::{json, Value};

use crate::agent::expertise::{self, Domain};
use crate::llm::LlmClient;
use crate::shell::{self, CommandOutput};

const MAX_TOOL_ROUNDS: usize = 8;

pub struct Executor {
    llm: LlmClient,
    workspace: PathBuf,
}

impl Executor {
    pub fn new(llm: LlmClient, workspace: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&workspace)?;
        Ok(Self { llm, workspace })
    }

    pub async fn execute(
        &self,
        task_title: &str,
        plan: &Value,
        research_context: &str,
        memory_context: &str,
        domains: &[Domain],
    ) -> Result<Value> {
        let user = format!(
            "{}\n\nTask: {task_title}\n\nPlan:\n{plan}\n\nResearch:\n{research_context}\n\n\
             {memory_context}\n\nWorkspace: {}\nExecute the plan now. Prefer running an existing \
             script over rewriting it. Use tools, read their output, then finish with JSON.",
            expertise::format_domain_context(domains),
            self.workspace.display(),
            plan = serde_json::to_string_pretty(plan)?
        );

        let mut artifacts: Vec<Value> = Vec::new();
        let mut command_outputs: Vec<Value> = Vec::new();

        let workspace = self.workspace.clone();
        let raw = self
            .llm
            .chat_with_tools(
                &expertise::executor_system(),
                &[],
                &user,
                &act_tools(),
                |name, args_json| {
                    dispatch_tool(
                        &workspace,
                        name,
                        args_json,
                        &mut artifacts,
                        &mut command_outputs,
                    )
                },
                0.2,
                MAX_TOOL_ROUNDS,
                None,
            )
            .await
            .unwrap_or_else(|err| {
                json!({
                    "summary": format!("Act stopped: {err:#}"),
                    "notes": "Tool loop ended before a final summary."
                })
                .to_string()
            });

        let mut result = parse_act_result(&raw);
        let extra_artifacts = result
            .get("artifacts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for artifact in extra_artifacts {
            if let Err(err) = write_artifact(&self.workspace, &artifact) {
                eprintln!("piya agent: artifact skipped: {err}");
            } else if !artifacts.iter().any(|a| a.get("path") == artifact.get("path")) {
                artifacts.push(artifact);
            }
        }
        if let Some(extra) = result.get("commands").cloned() {
            let extra = extra.as_array().cloned().unwrap_or_default();
            let remaining = 5usize.saturating_sub(command_outputs.len());
            for item in extra.iter().take(remaining) {
                let cmd = item.get("cmd").and_then(|v| v.as_str()).unwrap_or("").trim();
                if cmd.is_empty() {
                    continue;
                }
                command_outputs.push(command_output_json(&run_cmd(&self.workspace, cmd)));
            }
        }

        if let Some(obj) = result.as_object_mut() {
            obj.insert("artifacts".into(), json!(artifacts));
            obj.insert("command_outputs".into(), json!(command_outputs));
        }
        Ok(result)
    }
}

fn dispatch_tool(
    workspace: &Path,
    name: &str,
    args_json: &str,
    artifacts: &mut Vec<Value>,
    command_outputs: &mut Vec<Value>,
) -> Result<String> {
    let args: Value = if args_json.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(args_json).context("parse tool arguments")?
    };

    match name {
        "write_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("`path` is required"))?;
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let artifact = json!({ "path": path, "content": content });
            write_artifact(workspace, &artifact)?;
            artifacts.push(json!({ "path": path, "bytes": content.len() }));
            Ok(format!("Wrote {path} ({} bytes)", content.len()))
        }
        "read_file" => {
            let path = args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("`path` is required"))?;
            let target = safe_workspace_path(workspace, path)?;
            let text = std::fs::read_to_string(&target)
                .with_context(|| format!("read {}", target.display()))?;
            let clipped: String = text.chars().take(8_000).collect();
            Ok(clipped)
        }
        "run_command" => {
            let cmd = args
                .get("cmd")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("`cmd` is required"))?
                .trim();
            if cmd.is_empty() {
                anyhow::bail!("`cmd` cannot be empty");
            }
            let out = run_cmd(workspace, cmd);
            command_outputs.push(command_output_json(&out));
            Ok(format_command_for_model(&out))
        }
        other => Err(anyhow!("unknown tool `{other}`")),
    }
}

fn write_artifact(workspace: &Path, artifact: &Value) -> Result<()> {
    let Some(obj) = artifact.as_object() else {
        return Ok(());
    };
    let rel_path = obj.get("path").and_then(|v| v.as_str()).unwrap_or("").trim();
    let content = obj.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if rel_path.is_empty() {
        return Ok(());
    }
    let target = safe_workspace_path(workspace, rel_path)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, content)?;
    Ok(())
}

fn run_cmd(workspace: &Path, cmd: &str) -> CommandOutput {
    eprintln!("piya agent: running command: $ {cmd}");
    match shell::run_agent_command(cmd, workspace) {
        Ok(out) => {
            if out.skipped {
                eprintln!(
                    "piya agent: skipped `$ {}` — {}",
                    out.cmd,
                    out.reason.as_deref().unwrap_or("policy")
                );
            }
            out
        }
        Err(err) => CommandOutput {
            cmd: cmd.to_string(),
            cwd: workspace.display().to_string(),
            returncode: Some(-1),
            stdout: String::new(),
            stderr: err.to_string(),
            skipped: false,
            reason: None,
        },
    }
}

fn act_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            "write_file",
            "Write a file relative to the workspace. Use this for reports, scripts, and other artifacts.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Relative path from workspace root" },
                    "content": { "type": "string" }
                },
                "required": ["path", "content"]
            }),
        ),
        tool(
            "read_file",
            "Read an existing workspace file (e.g. a script from a previous run).",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        ),
        tool(
            "run_command",
            "Run a safe shell command in the workspace. Read stdout before the next step. \
             Prefer this to rewrite a job that already has a script.",
            json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string" }
                },
                "required": ["cmd"]
            }),
        ),
    ]
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

fn parse_act_result(raw: &str) -> Value {
    if let Some(value) = extract_json_object(raw) {
        return value;
    }
    json!({
        "summary": raw.chars().take(2000).collect::<String>(),
        "notes": ""
    })
}

fn extract_json_object(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if value.is_object() {
            return Some(value);
        }
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&trimmed[start..=end]).ok()
}

fn safe_workspace_path(workspace: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.trim();
    if rel.is_empty() || rel.contains("..") || Path::new(rel).is_absolute() {
        anyhow::bail!("invalid workspace path: {rel}");
    }
    let workspace = workspace.canonicalize().unwrap_or_else(|_| workspace.to_path_buf());
    let target = workspace.join(rel);
    if let Ok(canonical) = target.canonicalize() {
        if !canonical.starts_with(&workspace) {
            anyhow::bail!("path escapes workspace: {rel}");
        }
        return Ok(canonical);
    }
    if !target.starts_with(&workspace) {
        anyhow::bail!("path escapes workspace: {rel}");
    }
    Ok(target)
}

fn command_output_json(out: &CommandOutput) -> Value {
    json!({
        "cmd": out.cmd,
        "cwd": out.cwd,
        "returncode": out.returncode,
        "stdout": out.stdout,
        "stderr": out.stderr,
        "skipped": out.skipped,
        "reason": out.reason,
    })
}

fn format_command_for_model(out: &CommandOutput) -> String {
    if out.skipped {
        return format!(
            "Skipped `{}`: {}",
            out.cmd,
            out.reason.as_deref().unwrap_or("policy")
        );
    }
    let stdout: String = out.stdout.chars().take(6_000).collect();
    let stderr: String = out.stderr.chars().take(2_000).collect();
    let code = out
        .returncode
        .map(|c| c.to_string())
        .unwrap_or_else(|| "?".into());
    format!(
        "cmd: {}\ncwd: {}\nreturncode: {code}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        out.cmd, out.cwd
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_escape() {
        let dir = std::env::temp_dir().join("piya-exec-test");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(safe_workspace_path(&dir, "../secret").is_err());
        assert!(safe_workspace_path(&dir, "/etc/passwd").is_err());
        assert!(safe_workspace_path(&dir, "ok.md").is_ok());
    }

    #[test]
    fn parses_embedded_json() {
        let raw = "Done.\n{\"summary\":\"ran script\",\"notes\":\"ok\"}\n";
        let v = parse_act_result(raw);
        assert_eq!(v["summary"], "ran script");
    }
}
