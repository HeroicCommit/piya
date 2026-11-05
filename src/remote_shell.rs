//! Sticky remote SSH target for chat so “next command” stays on the server.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

const STATE_FILE: &str = "shell-remote.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteShellState {
    /// SSH host / alias (e.g. `aapanel`). Empty = local Mac.
    #[serde(default)]
    pub host: String,
    /// Optional remote working directory sticky hint (informational for prompts).
    #[serde(default)]
    pub remote_cwd: String,
}

static MEMORY: Mutex<Option<RemoteShellState>> = Mutex::new(None);

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STATE_FILE)
}

pub fn load(data_dir: &Path) -> RemoteShellState {
    if let Ok(guard) = MEMORY.lock() {
        if let Some(cached) = guard.as_ref() {
            return cached.clone();
        }
    }
    let path = state_path(data_dir);
    let state: RemoteShellState = std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default();
    if let Ok(mut guard) = MEMORY.lock() {
        *guard = Some(state.clone());
    }
    state
}

pub fn save(data_dir: &Path, state: &RemoteShellState) {
    let _ = std::fs::create_dir_all(data_dir);
    if let Ok(raw) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(state_path(data_dir), raw);
    }
    if let Ok(mut guard) = MEMORY.lock() {
        *guard = Some(state.clone());
    }
}

pub fn clear(data_dir: &Path) {
    save(
        data_dir,
        &RemoteShellState {
            host: String::new(),
            remote_cwd: String::new(),
        },
    );
}

pub fn set_host(data_dir: &Path, host: &str) {
    let host = host.trim();
    if host.is_empty() || host.eq_ignore_ascii_case("local") {
        clear(data_dir);
        return;
    }
    let mut state = load(data_dir);
    state.host = host.to_string();
    save(data_dir, &state);
}

/// If `command` is already an ssh/scp invocation, learn the host and return as-is.
/// If sticky remote host is set and command is a bare remote shell line, wrap with ssh.
pub fn maybe_wrap_remote(
    data_dir: &Path,
    command: &str,
    host_arg: Option<&str>,
) -> (String, Option<String>) {
    let command = command.trim();
    let lower = command.to_lowercase();

    // Explicit local escape hatch.
    if host_arg.is_some_and(|h| h.eq_ignore_ascii_case("local") || h.is_empty())
        || lower.starts_with("local:")
    {
        let stripped = command
            .strip_prefix("local:")
            .or_else(|| command.strip_prefix("LOCAL:"))
            .unwrap_or(command)
            .trim();
        return (
            stripped.to_string(),
            Some("running locally (remote sticky bypassed)".into()),
        );
    }

    // Explicit host for this call.
    if let Some(host) = host_arg.map(str::trim).filter(|h| !h.is_empty()) {
        set_host(data_dir, host);
        if is_remote_transport_command(&lower) {
            note_host_from_ssh_command(data_dir, command);
            return (command.to_string(), None);
        }
        let wrapped = wrap_ssh(host, command);
        return (
            wrapped,
            Some(format!("wrapped for sticky/explicit host `{host}`")),
        );
    }

    // Learn sticky host from an ssh command the model already wrote.
    if is_remote_transport_command(&lower) {
        note_host_from_ssh_command(data_dir, command);
        return (command.to_string(), None);
    }

    let sticky = load(data_dir);
    if sticky.host.is_empty() {
        return (command.to_string(), None);
    }

    // Already targeted somehow.
    if lower.contains(&format!("ssh {}", sticky.host.to_lowercase())) {
        return (command.to_string(), None);
    }

    let wrapped = wrap_ssh(&sticky.host, command);
    (
        wrapped,
        Some(format!(
            "auto-wrapped with sticky SSH host `{}` (pass host=\"local\" to run on this Mac)",
            sticky.host
        )),
    )
}

fn is_remote_transport_command(lower: &str) -> bool {
    lower.starts_with("ssh ")
        || lower.starts_with("scp ")
        || lower.starts_with("sftp ")
        || lower == "ssh"
}

fn note_host_from_ssh_command(data_dir: &Path, command: &str) {
    if let Some(host) = extract_ssh_host(command) {
        set_host(data_dir, &host);
    }
}

fn wrap_ssh(host: &str, remote_cmd: &str) -> String {
    let escaped = remote_cmd.replace('\'', "'\"'\"'");
    format!("ssh {host} '{escaped}'")
}

/// Best-effort parse of `ssh [opts] destination [command…]`.
pub fn extract_ssh_host(command: &str) -> Option<String> {
    let mut tokens = tokenize(command);
    if tokens.first()?.to_lowercase() != "ssh" {
        return None;
    }
    tokens.remove(0);
    let takes_value = [
        "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o", "-p",
        "-Q", "-R", "-S", "-W", "-w",
    ];
    let mut i = 0usize;
    while i < tokens.len() {
        let t = tokens[i].as_str();
        if t == "--" {
            return tokens.get(i + 1).cloned();
        }
        if t.starts_with('-') {
            if takes_value.contains(&t) || (t.len() == 2 && "bcDeEFIiJLlmOopQRSWw".contains(&t[1..]))
            {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        return Some(t.to_string());
    }
    None
}

fn tokenize(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in cmd.chars() {
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

pub fn prompt_hint(data_dir: &Path) -> Option<String> {
    let state = load(data_dir);
    if state.host.is_empty() {
        return None;
    }
    Some(format!(
        "Sticky remote shell is ON — host `{}`. \
         Bare `piya_run_command` commands are auto-wrapped as `ssh {} '…'`. \
         To run on this Mac instead, pass host=\"local\" (or prefix the command with local:). \
         To change host, pass host=\"other\" or run ssh other '…'.",
        state.host, state.host
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir() -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("piya-remote-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn wraps_when_sticky_host_set() {
        let dir = tmp_dir();
        set_host(&dir, "aapanel");
        let (cmd, note) = maybe_wrap_remote(&dir, "ls /www", None);
        assert!(cmd.starts_with("ssh aapanel "), "{cmd}");
        assert!(cmd.contains("ls /www"), "{cmd}");
        assert!(note.is_some());
    }

    #[test]
    fn learns_host_from_ssh_command() {
        let dir = tmp_dir();
        let (cmd, _) = maybe_wrap_remote(&dir, "ssh aapanel 'uname -a'", None);
        assert_eq!(cmd, "ssh aapanel 'uname -a'");
        assert_eq!(load(&dir).host, "aapanel");
    }

    #[test]
    fn local_bypass() {
        let dir = tmp_dir();
        set_host(&dir, "aapanel");
        let (cmd, _) = maybe_wrap_remote(&dir, "pwd", Some("local"));
        assert_eq!(cmd, "pwd");
        let (cmd2, _) = maybe_wrap_remote(&dir, "local:pwd", None);
        assert_eq!(cmd2, "pwd");
    }

    #[test]
    fn extract_host() {
        assert_eq!(
            extract_ssh_host("ssh -o BatchMode=yes aapanel 'ls'").as_deref(),
            Some("aapanel")
        );
        assert_eq!(
            extract_ssh_host("ssh user@example.com uptime").as_deref(),
            Some("user@example.com")
        );
    }
}
