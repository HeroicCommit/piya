//! Sandboxed shell commands for Chat and the background agent.
//!
//! Safe commands run immediately. Commands outside the allow-list prompt the user
//! (macOS dialog / CLI confirm) instead of being hard-blocked.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

const CHAT_TIMEOUT: Duration = Duration::from_secs(60);
const AGENT_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_STDOUT: usize = 32_000;
const MAX_STDERR: usize = 6_000;

/// Programs that may run without a prompt (matched on the exact command word).
const ALLOWED_PROGRAMS: &[&str] = &[
    "python", "python3", "pip", "pip3", "node", "npm", "npx", "cat", "ls", "grep", "rg",
    "ripgrep", "find", "mdfind", "mdls", "head", "tail", "wc", "file", "stat", "du", "df",
    "echo", "pwd", "which", "date", "sleep", "osascript", "afplay", "mkdir", "touch", "cargo",
    "rustc", "rustfmt", "clippy",
];

/// `git` subcommands that are read-only.
const ALLOWED_GIT_SUBCOMMANDS: &[&str] = &["status", "diff", "log", "show"];

/// `find` flags that delete files or execute other commands.
const BLOCKED_FIND_FLAGS: &[&str] = &[
    "-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprint0", "-fprintf", "-fls",
];

/// Redirections that are harmless and allowed outside quotes.
const ALLOWED_REDIRECTS: &[&str] = &["2>&1", "2>/dev/null", ">/dev/null", "</dev/null"];

const MAX_NESTING: usize = 3;

const SAFE_OPEN_APPS: &[&str] = &["clock", "calculator", "calendar", "notes", "reminders"];

const BLOCKED_FRAGMENTS: &[&str] = &[
    "rm -",
    "rm ",
    "sudo ",
    "sudo\t",
    "chmod ",
    "chown ",
    "kill ",
    "pkill",
    "killall",
    "mkfs",
    "dd ",
    "| bash",
    "curl ",
    "wget ",
    "> /dev/",
    ">> /dev/",
    "launchctl",
    "defaults write",
    "diskutil",
    "do shell script",
    "display notification",
];

fn contains_blocked_fragment(lowered: &str) -> bool {
    if BLOCKED_FRAGMENTS.iter().any(|b| lowered.contains(b)) {
        return true;
    }
    // Bare `sudo` / `chmod` / `chown` as a command word (avoid matching inside
    // `BatchMode`, hostnames, etc.).
    lowered.split_whitespace().any(|tok| {
        matches!(
            tok.trim_matches(|c: char| matches!(c, '\'' | '"' | ';')),
            "sudo" | "chmod" | "chown"
        )
    })
}

static ACTIVITY: Mutex<Option<String>> = Mutex::new(None);
static CURRENT_PID: AtomicU32 = AtomicU32::new(0);

/// Kill the shell command belonging to the in-flight chat turn.
pub fn interrupt_current() {
    let pid = CURRENT_PID.swap(0, Ordering::SeqCst);
    if pid != 0 {
        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
    }
    set_activity(None);
}

/// Current shell/tool activity for UI status (e.g. `$ ls ~/Documents`).
pub fn activity_status() -> Option<String> {
    ACTIVITY.lock().ok().and_then(|g| g.clone())
}

fn set_activity(msg: Option<String>) {
    if let Ok(mut guard) = ACTIVITY.lock() {
        *guard = msg;
    }
}

struct ActivityGuard;

impl ActivityGuard {
    fn running(cmd: &str) -> Self {
        set_activity(Some(format!("$ {cmd}")));
        Self
    }
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        set_activity(None);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandOutput {
    pub cmd: String,
    pub cwd: String,
    pub returncode: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub skipped: bool,
    pub reason: Option<String>,
}

impl CommandOutput {
    pub fn skipped(cmd: &str, cwd: &Path, reason: &str) -> Self {
        Self {
            cmd: cmd.to_string(),
            cwd: cwd.display().to_string(),
            returncode: None,
            stdout: String::new(),
            stderr: String::new(),
            skipped: true,
            reason: Some(reason.to_string()),
        }
    }

    pub fn to_llm_text(&self) -> String {
        if self.skipped {
            return format!(
                "Command not run: {}\nReason: {}",
                self.cmd,
                self.reason.as_deref().unwrap_or("policy")
            );
        }
        let mut parts = vec![
            format!("$ {}", self.cmd),
            format!("cwd: {}", self.cwd),
            format!("exit: {}", self.returncode.unwrap_or(-1)),
        ];
        if !self.stdout.is_empty() {
            parts.push(format!("stdout:\n{}", self.stdout));
        }
        if !self.stderr.is_empty() {
            parts.push(format!("stderr:\n{}", self.stderr));
        }
        parts.join("\n")
    }
}

pub fn is_safe_command(cmd: &str) -> bool {
    is_safe_command_depth(cmd, 0)
}

fn is_safe_command_depth(cmd: &str, depth: usize) -> bool {
    let trimmed = cmd.trim();
    if trimmed.is_empty() || depth > MAX_NESTING {
        return false;
    }
    let lowered = trimmed.to_lowercase();

    if contains_blocked_fragment(&lowered) {
        return false;
    }

    // Any unquoted redirect, command substitution or backtick means the command
    // can do more than it appears to: ask the user.
    let Some(segments) = split_segments(trimmed) else {
        return false;
    };
    if segments.is_empty() {
        return false;
    }

    // Remote ssh/scp/sftp: one simple remote hop only. Dangerous remote payloads
    // (sudo/rm/curl/…) fail the fragment check above and prompt the user.
    if is_remote_transport(&lowered) {
        return segments.len() == 1 && !interactive_ssh_without_remote_command(trimmed);
    }

    segments.iter().all(|seg| segment_is_safe(seg, depth))
}

/// Split a command on unquoted `;`, `|`, `&` and newlines. Returns `None` when the
/// command uses syntax we refuse to auto-approve: unquoted redirects (other than
/// harmless `/dev/null` ones), backticks or `$(...)` outside single quotes.
fn split_segments(cmd: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = cmd.chars().collect();
    let mut segments = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some('\'') => {
                if c == '\'' {
                    quote = None;
                }
                cur.push(c);
            }
            Some(_) => {
                // Inside double quotes the shell still expands `$(...)` and backticks.
                if c == '`' || (c == '$' && chars.get(i + 1) == Some(&'(')) {
                    return None;
                }
                if c == '\\' {
                    cur.push(c);
                    i += 1;
                    if let Some(n) = chars.get(i) {
                        cur.push(*n);
                    }
                } else {
                    if c == '"' {
                        quote = None;
                    }
                    cur.push(c);
                }
            }
            None => {
                if let Some(r) = ALLOWED_REDIRECTS.iter().find(|r| {
                    r.chars()
                        .enumerate()
                        .all(|(k, rc)| chars.get(i + k) == Some(&rc))
                }) {
                    i += r.chars().count();
                    continue;
                }
                match c {
                    '\'' | '"' => {
                        quote = Some(c);
                        cur.push(c);
                    }
                    '\\' => {
                        cur.push(c);
                        i += 1;
                        if let Some(n) = chars.get(i) {
                            cur.push(*n);
                        }
                    }
                    '`' | '<' | '>' => return None,
                    '$' if chars.get(i + 1) == Some(&'(') => return None,
                    ';' | '|' | '&' | '\n' => {
                        if !cur.trim().is_empty() {
                            segments.push(std::mem::take(&mut cur));
                        }
                        cur.clear();
                    }
                    _ => cur.push(c),
                }
            }
        }
        i += 1;
    }
    if quote.is_some() {
        return None; // unbalanced quotes
    }
    if !cur.trim().is_empty() {
        segments.push(cur);
    }
    Some(segments)
}

fn segment_is_safe(seg: &str, depth: usize) -> bool {
    let tokens = shell_tokens(seg.trim());
    tokens_are_safe(&tokens, seg, depth)
}

fn tokens_are_safe(tokens: &[String], raw: &str, depth: usize) -> bool {
    let Some(first) = tokens.first().map(String::as_str) else {
        return false;
    };
    match first {
        "nohup" => {
            let rest = &tokens[1..];
            match rest {
                [sh, flag, inner] if sh == "sh" && flag == "-c" => {
                    is_safe_command_depth(inner, depth + 1)
                }
                [] => false,
                _ => tokens_are_safe(rest, raw, depth + 1),
            }
        }
        "open" => is_safe_open(raw.trim()),
        "git" => tokens
            .get(1)
            .is_some_and(|sub| ALLOWED_GIT_SUBCOMMANDS.contains(&sub.as_str())),
        "find" => !tokens.iter().any(|t| BLOCKED_FIND_FLAGS.contains(&t.as_str())),
        prog => ALLOWED_PROGRAMS.contains(&prog),
    }
}

fn is_remote_transport(lowered: &str) -> bool {
    lowered.starts_with("ssh ")
        || lowered.starts_with("scp ")
        || lowered.starts_with("sftp ")
        || lowered == "ssh"
        || lowered == "scp"
        || lowered == "sftp"
}

/// `ssh user@host` with no remote command needs a TTY — Piya cannot drive that.
fn interactive_ssh_without_remote_command(cmd: &str) -> bool {
    let tokens = shell_tokens(cmd);
    if tokens.first().map(String::as_str) != Some("ssh") {
        return false;
    }
    let mut i = 1usize;
    while i < tokens.len() {
        let t = tokens[i].as_str();
        if t == "--" {
            i += 1;
            return i >= tokens.len();
        }
        if t.starts_with('-') {
            // Options that take a value: -o/-i/-F/-l/-p/-J/-w/-b/-c/-D/-e/-L/-R/-S/-W
            const TAKES_VALUE: &[&str] = &[
                "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o",
                "-p", "-Q", "-R", "-S", "-W", "-w",
            ];
            let takes_value = TAKES_VALUE.contains(&t)
                || (t.len() == 2 && "bcDeEFIiJLlmOopQRSWw".contains(&t[1..]));
            i += if takes_value { 2 } else { 1 };
            continue;
        }
        // Destination (user@host)
        i += 1;
        // Anything after destination is the remote command.
        return i >= tokens.len();
    }
    true
}

fn shell_tokens(cmd: &str) -> Vec<String> {
    // Lightweight split that respects simple quotes (enough for ssh option parsing).
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = cmd.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Ensure ssh is non-interactive and time-bounded so Chat does not hang on password/TTY.
fn prepare_command(cmd: &str) -> Result<String, String> {
    let trimmed = cmd.trim();
    let lowered = trimmed.to_lowercase();
    if !lowered.starts_with("ssh ") && lowered != "ssh" {
        return Ok(trimmed.to_string());
    }
    if interactive_ssh_without_remote_command(trimmed) {
        return Err(
            "Interactive SSH needs a terminal. Run a remote command in one shot, e.g.\n\
             ssh -o BatchMode=yes user@host 'cd /app && ls && systemctl status nginx'\n\
             Chain remote steps with && / ; inside the quoted remote command — \
             do not open a bare ssh session and then run local commands."
                .into(),
        );
    }
    if lowered.contains("batchmode=") || lowered.contains(" -n ") || lowered.contains("\t-n ") {
        return Ok(ensure_ssh_connect_timeout(trimmed));
    }
    // Force non-interactive auth (key-based). Fails fast instead of hanging on password.
    Ok(ensure_ssh_connect_timeout(&format!(
        "ssh -o BatchMode=yes -o StrictHostKeyChecking=accept-new {}",
        trim_ssh_prefix(trimmed)
    )))
}

fn trim_ssh_prefix(cmd: &str) -> &str {
    let t = cmd.trim();
    if let Some(rest) = t.strip_prefix("ssh ") {
        rest.trim_start()
    } else if t == "ssh" {
        ""
    } else {
        t
    }
}

fn ensure_ssh_connect_timeout(cmd: &str) -> String {
    let lower = cmd.to_lowercase();
    if lower.contains("connecttimeout=") {
        cmd.to_string()
    } else if let Some(rest) = cmd.trim().strip_prefix("ssh ") {
        format!("ssh -o ConnectTimeout=20 {rest}")
    } else {
        cmd.to_string()
    }
}

fn is_safe_open(cmd: &str) -> bool {
    let lowered = cmd.to_lowercase();
    if !lowered.starts_with("open ") {
        return false;
    }
    if !lowered.contains("-a ") {
        return false;
    }
    SAFE_OPEN_APPS.iter().any(|app| {
        lowered.contains(&format!("-a {app}"))
            || lowered.contains(&format!("-a \"{app}\""))
            || lowered.contains(&format!("-a '{app}'"))
    })
}

pub fn is_background_command(cmd: &str) -> bool {
    let trimmed = cmd.trim();
    let lowered = trimmed.to_lowercase();
    lowered.starts_with("nohup ") || trimmed.ends_with('&')
}

pub fn resolve_cwd(requested: Option<&str>, workspace: &Path) -> Result<PathBuf> {
    let home = home_dir().context("HOME is not set")?;
    let workspace = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());

    let path = match requested.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => home.clone(),
    };

    let absolute = if path.is_absolute() {
        path
    } else {
        home.join(path)
    };

    let canonical = absolute
        .canonicalize()
        .with_context(|| format!("cwd not found: {}", absolute.display()))?;

    if canonical.starts_with(&home) || canonical.starts_with(&workspace) {
        Ok(canonical)
    } else {
        Err(anyhow!(
            "cwd must be under your home directory or Piya workspace ({})",
            workspace.display()
        ))
    }
}

pub fn run_chat_command(cmd: &str, cwd: Option<&str>, workspace: &Path) -> Result<CommandOutput> {
    let cwd = cwd.or_else(|| infer_workspace_cwd(cmd, workspace));
    run_command(cmd, cwd, workspace, CHAT_TIMEOUT)
}

/// When reading a relative file path, assume Piya workspace unless cwd was set explicitly.
fn infer_workspace_cwd<'a>(cmd: &str, workspace: &'a Path) -> Option<&'a str> {
    let trimmed = cmd.trim();
    let lowered = trimmed.to_lowercase();
    let read_prefixes = ["cat ", "head ", "tail ", "grep ", "rg ", "less "];
    if !read_prefixes.iter().any(|p| lowered.starts_with(p)) {
        return None;
    }
    let arg = trimmed.split_whitespace().nth(1)?;
    if arg.starts_with('/') || arg.starts_with('~') || arg.contains("://") {
        return None;
    }
    workspace.to_str()
}

pub fn run_agent_command(cmd: &str, cwd: &Path) -> Result<CommandOutput> {
    run_command(cmd, Some(cwd.to_str().unwrap_or(".")), cwd, AGENT_TIMEOUT)
}

fn run_command(cmd: &str, cwd: Option<&str>, workspace: &Path, timeout: Duration) -> Result<CommandOutput> {
    let cmd = cmd.trim();
    let cwd_path = resolve_cwd(cwd, workspace)?;
    let _activity = ActivityGuard::running(cmd);

    if cmd.to_lowercase().contains("display notification") {
        return Ok(CommandOutput::skipped(
            cmd,
            &cwd_path,
            "use the piya_notify tool for Notification Center banners (osascript has no Piya app icon)",
        ));
    }

    let prepared = match prepare_command(cmd) {
        Ok(c) => c,
        Err(reason) => {
            return Ok(CommandOutput::skipped(cmd, &cwd_path, &reason));
        }
    };

    if !is_safe_command(&prepared) && !is_safe_command(cmd) {
        match request_run_permission(cmd, &cwd_path) {
            PermissionOutcome::Allow => {
                // Fall through and run.
            }
            PermissionOutcome::Deny { reason } => {
                return Ok(CommandOutput::skipped(cmd, &cwd_path, &reason));
            }
        }
    }

    eprintln!(
        "piya: running command: $ {prepared}  (cwd: {})",
        cwd_path.display()
    );

    if is_background_command(&prepared) {
        return run_detached(&prepared, &cwd_path);
    }

    run_with_timeout(&prepared, &cwd_path, timeout)
}

fn run_with_timeout(cmd: &str, cwd_path: &Path, timeout: Duration) -> Result<CommandOutput> {
    use std::io::Read;
    use std::sync::mpsc;
    use std::thread;

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd_path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run command: {cmd}"))?;

    let pid = child.id();
    CURRENT_PID.store(pid, Ordering::SeqCst);
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let (tx_out, rx_out) = mpsc::channel();
    thread::spawn(move || {
        let mut stdout = String::new();
        let mut stderr = String::new();
        if let Some(mut out) = stdout_pipe {
            let _ = out.read_to_string(&mut stdout);
        }
        if let Some(mut err) = stderr_pipe {
            let _ = err.read_to_string(&mut stderr);
        }
        let _ = tx_out.send((stdout, stderr));
    });

    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                CURRENT_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst).ok();
                let (stdout, stderr) = rx_out.recv().unwrap_or_default();
                return Ok(CommandOutput {
                    cmd: cmd.to_string(),
                    cwd: cwd_path.display().to_string(),
                    returncode: status.code(),
                    stdout: truncate_head(&stdout, MAX_STDOUT),
                    stderr: truncate_head(&stderr, MAX_STDERR),
                    skipped: false,
                    reason: None,
                });
            }
            Ok(None)
                if started.elapsed() < timeout && !crate::chat_cancel::active_cancelled() =>
            {
                thread::sleep(Duration::from_millis(50));
            }
            Ok(None) if crate::chat_cancel::active_cancelled() => {
                let _ = child.kill();
                let _ = child.wait();
                CURRENT_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst).ok();
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                return Ok(CommandOutput {
                    cmd: cmd.to_string(),
                    cwd: cwd_path.display().to_string(),
                    returncode: None,
                    stdout: String::new(),
                    stderr: "Stopped.".into(),
                    skipped: true,
                    reason: Some("cancelled".into()),
                });
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                CURRENT_PID.compare_exchange(pid, 0, Ordering::SeqCst, Ordering::SeqCst).ok();
                // Also kill process-group children when possible (ssh may spawn).
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                return Ok(CommandOutput {
                    cmd: cmd.to_string(),
                    cwd: cwd_path.display().to_string(),
                    returncode: None,
                    stdout: String::new(),
                    stderr: format!(
                        "Command timed out after {}s. For SSH, use one non-interactive remote command \
                         and keep remote steps inside a single quoted string.",
                        timeout.as_secs()
                    ),
                    skipped: true,
                    reason: Some(format!("timed out after {}s", timeout.as_secs())),
                });
            }
            Err(err) => {
                return Err(anyhow!("wait for command: {err}"));
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PermissionOutcome {
    Allow,
    Deny { reason: String },
}

fn shell_policy_from_env() -> Option<PermissionOutcome> {
    match std::env::var("PIYA_SHELL_POLICY")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "allow" | "auto" | "1" | "true" => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| {
                eprintln!(
                    "piya: WARNING: PIYA_SHELL_POLICY=allow auto-approves every shell command \
                     (no prompts). Use only in a sandbox."
                );
            });
            Some(PermissionOutcome::Allow)
        }
        "deny" | "block" | "0" | "false" => Some(PermissionOutcome::Deny {
            reason: "blocked by PIYA_SHELL_POLICY=deny".into(),
        }),
        "ask" | "" => None,
        _ => None,
    }
}

fn request_run_permission(cmd: &str, cwd: &Path) -> PermissionOutcome {
    if let Some(policy) = shell_policy_from_env() {
        return policy;
    }

    let preview = truncate_for_prompt(cmd, 400);
    let message = format!(
        "Piya wants to run a command that is outside the auto-allow list:\n\n$ {preview}\n\ncwd: {}\n\nAllow this command?",
        cwd.display()
    );

    #[cfg(target_os = "macos")]
    {
        crate::ui::macos::activate_app();
        set_activity(Some(format!("Allow? $ {preview}")));
        let allowed = crate::ui::macos::choose_yes_no("Allow command?", &message);
        return if allowed {
            PermissionOutcome::Allow
        } else {
            PermissionOutcome::Deny {
                reason: "user declined to allow this command".into(),
            }
        };
    }

    #[cfg(not(target_os = "macos"))]
    {
        use std::io::IsTerminal;
        if std::io::stdin().is_terminal() {
            set_activity(Some(format!("Allow? $ {preview}")));
            let allowed = inquire::Confirm::new(&message)
                .with_default(false)
                .prompt()
                .unwrap_or(false);
            return if allowed {
                PermissionOutcome::Allow
            } else {
                PermissionOutcome::Deny {
                    reason: "user declined to allow this command".into(),
                }
            };
        }
        PermissionOutcome::Deny {
            reason: "command needs your approval but no prompt was available (set PIYA_SHELL_POLICY=allow to auto-approve in automation)".into(),
        }
    }
}

fn run_detached(cmd: &str, cwd_path: &Path) -> Result<CommandOutput> {
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start background command: {cmd}"))?;

    let mut stderr = String::new();
    if let Some(mut err) = child.stderr {
        let _ = std::io::Read::read_to_string(&mut err, &mut stderr);
    }

    Ok(CommandOutput {
        cmd: cmd.to_string(),
        cwd: cwd_path.display().to_string(),
        returncode: Some(0),
        stdout: "Started in background.".into(),
        stderr: truncate_head(&stderr, MAX_STDERR),
        skipped: false,
        reason: None,
    })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

fn truncate_head(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut out = s[..max].to_string();
        out.push_str("\n… (output truncated)");
        out
    }
}

fn truncate_for_prompt(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn allows_ssh_with_remote_command() {
        assert!(is_safe_command("ssh user@host 'ls -la /var/www'"));
        assert!(is_safe_command(
            "ssh -o BatchMode=yes user@host \"cd /tmp && pwd\""
        ));
    }

    #[test]
    fn rejects_interactive_ssh() {
        assert!(interactive_ssh_without_remote_command("ssh user@host"));
        assert!(interactive_ssh_without_remote_command("ssh -i ~/.ssh/id_ed25519 user@host"));
        assert!(!interactive_ssh_without_remote_command(
            "ssh user@host 'uptime'"
        ));
        assert!(!is_safe_command("ssh user@host"));
    }

    #[test]
    fn prepare_ssh_injects_batchmode() {
        let prepared = prepare_command("ssh user@host 'echo hi'").unwrap();
        assert!(prepared.contains("BatchMode=yes"), "{prepared}");
        assert!(prepared.contains("user@host"), "{prepared}");
        let err = prepare_command("ssh user@host").unwrap_err();
        assert!(err.to_lowercase().contains("interactive"));
    }

    #[test]
    fn ssh_with_sudo_remote_still_prompts() {
        assert!(!is_safe_command("ssh user@host 'sudo systemctl restart nginx'"));
    }

    #[test]
    fn allows_timer_nohup() {
        assert!(is_safe_command(
            "nohup sh -c 'sleep 60 && afplay /System/Library/Sounds/Glass.aiff' </dev/null >/dev/null 2>&1 &"
        ));
    }

    #[test]
    fn allows_open_clock() {
        assert!(is_safe_command("open -a Clock"));
    }

    #[test]
    fn blocks_open_unsafe() {
        assert!(!is_safe_command("open -a Safari https://evil.com"));
    }

    #[test]
    fn blocks_rm() {
        assert!(!is_safe_command("rm -rf /"));
    }

    #[test]
    fn blocks_sudo() {
        assert!(!is_safe_command("sudo ls"));
        assert!(!is_safe_command("sudo -u root ls"));
    }

    #[test]
    fn blocks_shell_composition_escapes() {
        assert!(!is_safe_command("echo x > ~/.zshrc"));
        assert!(!is_safe_command("ls; /bin/rm -rf ~/x"));
        assert!(!is_safe_command("cat f | sh"));
        assert!(!is_safe_command("ls && python3 -c 'print(1)' | tee /etc/x"));
        assert!(!is_safe_command("echo $(whoami)"));
        assert!(!is_safe_command("echo \"$(whoami)\""));
        assert!(!is_safe_command("echo `whoami`"));
        assert!(!is_safe_command("find . -delete"));
        assert!(!is_safe_command("find . -exec ls {} \\;"));
        assert!(!is_safe_command("git push origin main"));
        assert!(!is_safe_command("git status; git push"));
        assert!(!is_safe_command("catalog"));
        assert!(!is_safe_command("ls 'unbalanced"));
        assert!(!is_safe_command("ssh user@host 'ls' ; ls"));
    }

    #[test]
    fn allows_simple_pipelines_and_quoted_metachars() {
        assert!(is_safe_command("ls -la | head -5"));
        assert!(is_safe_command("grep -rn 'a|b;c > d' src"));
        assert!(is_safe_command("echo '$(not run)'"));
        assert!(is_safe_command("git status"));
        assert!(is_safe_command("ls 2>/dev/null"));
        assert!(is_safe_command("cargo test 2>&1 | tail -20"));
    }

    #[test]
    fn resolve_home_cwd() {
        let home = home_dir().unwrap();
        let cwd = resolve_cwd(None, Path::new("/tmp/workspace")).unwrap();
        assert_eq!(cwd, home.canonicalize().unwrap_or(home));
    }

    #[test]
    fn deny_policy_skips_unsafe_without_prompt() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("PIYA_SHELL_POLICY", "deny");
        let out = run_chat_command("sudo ls", None, Path::new("/tmp")).unwrap();
        std::env::remove_var("PIYA_SHELL_POLICY");
        assert!(out.skipped);
        assert!(out.cmd.contains("sudo"));
        assert!(out
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("PIYA_SHELL_POLICY"));
        assert!(out.to_llm_text().contains("Command not run"));
    }

    #[test]
    fn allow_policy_runs_otherwise_blocked_echo() {
        let _guard = ENV_LOCK.lock().unwrap();
        // `curl` is blocked by fragment list; with allow policy it should run.
        std::env::set_var("PIYA_SHELL_POLICY", "allow");
        let out = run_chat_command("curl --version", None, Path::new("/tmp")).unwrap();
        std::env::remove_var("PIYA_SHELL_POLICY");
        assert!(!out.skipped, "{out:?}");
        assert_eq!(out.cmd, "curl --version");
        assert!(out.to_llm_text().starts_with("$ curl --version"));
    }

    #[test]
    fn activity_clears_after_command() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("PIYA_SHELL_POLICY", "deny");
        let _ = run_chat_command("sudo ls", None, Path::new("/tmp"));
        std::env::remove_var("PIYA_SHELL_POLICY");
        assert!(activity_status().is_none());
    }
}
