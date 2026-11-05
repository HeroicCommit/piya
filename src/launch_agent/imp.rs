use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

pub const APP_LABEL: &str = "com.piya.app";
const LEGACY_LABEL: &str = "com.piya.badge";

pub fn register_plist(plist_path: &Path) -> Result<()> {
    let domain = user_domain();

    bootout_quiet(&domain, APP_LABEL);
    bootout_quiet(&domain, LEGACY_LABEL);
    bootout_plist_quiet(&domain, plist_path);

    if let Some(parent) = plist_path.parent() {
        let legacy = parent.join("com.piya.badge.plist");
        if legacy.exists() {
            bootout_plist_quiet(&domain, &legacy);
            let _ = std::fs::remove_file(legacy);
        }
    }

    let status = Command::new("launchctl")
        .args(["bootstrap", &domain, plist_path.to_str().unwrap_or("")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("launchctl bootstrap")?;

    if status.success() {
        let _ = Command::new("launchctl")
            .args(["enable", &format!("{domain}/{APP_LABEL}")])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        return Ok(());
    }

    Command::new("launchctl")
        .args(["load", "-w"])
        .arg(plist_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("launchctl load")?;
    Ok(())
}

pub fn unregister_label(label: &str) -> Result<()> {
    let domain = user_domain();
    bootout_quiet(&domain, label);

    if let Ok(plist) = home_launch_agents().map(|d| d.join(format!("{label}.plist"))) {
        if plist.exists() {
            bootout_plist_quiet(&domain, &plist);
            std::fs::remove_file(&plist).ok();
        }
    }
    Ok(())
}

fn user_domain() -> String {
    format!("gui/{}", user_id())
}

fn user_id() -> u32 {
    std::env::var("UID")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            Command::new("id")
                .arg("-u")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(501)
        })
}

fn bootout_quiet(domain: &str, label: &str) {
    let _ = Command::new("launchctl")
        .args(["bootout", &format!("{domain}/{label}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if let Ok(dir) = home_launch_agents() {
        let plist = dir.join(format!("{label}.plist"));
        if plist.exists() {
            let _ = Command::new("launchctl")
                .args(["unload", "-w"])
                .arg(&plist)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn bootout_plist_quiet(domain: &str, plist: &Path) {
    let _ = Command::new("launchctl")
        .args(["bootout", domain])
        .arg(plist)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn home_launch_agents() -> Result<PathBuf> {
    Ok(std::env::var("HOME")
        .map(PathBuf::from)
        .context("HOME not set")?
        .join("Library/LaunchAgents"))
}
