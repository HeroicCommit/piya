use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::config::Settings;
use crate::process_util::is_alive;

pub fn worker_lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join("piya-worker.lock")
}

pub fn worker_running(data_dir: &Path) -> bool {
    let lock = worker_lock_path(data_dir);
    let Ok(pid_str) = std::fs::read_to_string(&lock) else {
        return false;
    };
    let Ok(pid) = pid_str.trim().parse::<u32>() else {
        let _ = std::fs::remove_file(&lock);
        return false;
    };
    if is_alive(pid) {
        return true;
    }
    let _ = std::fs::remove_file(&lock);
    false
}

pub fn spawn_worker(settings: &Settings, args: &[&str]) -> Result<()> {
    if settings.openai_api_key.is_empty() {
        anyhow::bail!("OPENAI_API_KEY is missing — add it to ~/Library/Application Support/Piya/.env");
    }
    if worker_running(&settings.data_dir) {
        return Ok(());
    }

    let exe = resolve_executable()?;
    let log_path = settings.data_dir.join("piya-worker.log");
    let lock_path = worker_lock_path(&settings.data_dir);

    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let log_err = log.try_clone().context("clone worker log")?;

    let mut cmd = Command::new(&exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .env("PIYA_DATA_DIR", &settings.data_dir)
        .env("PIYA_WORKSPACE", &settings.workspace)
        .env("PIYA_NOTIFY", "all")
        .env("OPENAI_API_KEY", &settings.openai_api_key)
        .env("OPENAI_BASE_URL", &settings.openai_base_url)
        .env("PIYA_MODEL", &settings.model)
        .env("PIYA_EMBEDDING_MODEL", &settings.embedding_model)
        .env("PIYA_BIN", &exe);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd.spawn().context("spawn worker")?;
    std::fs::write(&lock_path, child.id().to_string())
        .with_context(|| format!("write {}", lock_path.display()))?;

    std::thread::Builder::new()
        .name("piya-worker-wait".into())
        .spawn(move || {
            let _ = child.wait();
        })
        .ok();

    Ok(())
}

pub fn maybe_auto_run(settings: &Settings, queue: &crate::tasks::TaskQueue) -> Result<()> {
    if !auto_run_enabled() {
        return Ok(());
    }
    let pending = queue.count_due_pending()?;
    if pending == 0 {
        return Ok(());
    }
    if worker_running(&settings.data_dir) {
        return Ok(());
    }
    queue.reset_stale_in_progress(120)?;
    spawn_worker(settings, &["run", "--continuous"])?;
    Ok(())
}

fn auto_run_enabled() -> bool {
    match std::env::var("PIYA_AUTO_RUN")
        .unwrap_or_default()
        .trim()
        .to_lowercase()
        .as_str()
    {
        "0" | "false" | "off" | "no" => false,
        _ => true,
    }
}

/// Path to the `piya` CLI used for background workers (not the Swift menu-bar app).
pub fn resolve_executable() -> Result<PathBuf> {
    if let Ok(bin) = std::env::var("PIYA_BIN") {
        let path = PathBuf::from(bin.trim());
        if path.is_file() {
            return Ok(path);
        }
        anyhow::bail!("PIYA_BIN points to missing file: {}", path.display());
    }

    if let Ok(exe) = std::env::current_exe() {
        if is_piya_cli(&exe) {
            return Ok(exe);
        }
        if let Some(cli) = bundled_cli_next_to(&exe) {
            return Ok(cli);
        }
    }

    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/release/piya");
    if dev.is_file() {
        return Ok(dev);
    }

    let legacy = PathBuf::from("/Applications/Piya.app/Contents/MacOS/piya");
    if legacy.is_file() {
        return Ok(legacy);
    }

    anyhow::bail!(
        "piya CLI not found — set PIYA_BIN in .env or rebuild with ./scripts/build-swift-app.sh"
    );
}

fn is_piya_cli(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| name == "piya")
}

/// When loaded from PiyaSwift.app, `current_exe` is the GUI binary — use bundled CLI instead.
fn bundled_cli_next_to(gui_exe: &Path) -> Option<PathBuf> {
    let contents = gui_exe.parent()?.parent()?;
    for candidate in [
        contents.join("Helpers/piya"),
        contents.join("MacOS/piya"),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
