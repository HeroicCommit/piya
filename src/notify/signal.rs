use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};

static BADGE_HINT: OnceLock<()> = OnceLock::new();

#[derive(serde::Serialize)]
struct BadgeSignal {
    event: String,
    title: String,
    detail: String,
    at: String,
}

pub fn task_finished(title: &str, status: &str, accuracy: f64) {
    let detail = if status == "done" {
        format!("Completed · {:.0}% accurate", accuracy * 100.0)
    } else {
        format!("Failed · {:.0}% accurate", accuracy * 100.0)
    };
    signal("finished".into(), title.to_string(), detail);
}

pub fn task_started(title: &str) {
    signal("started".into(), title.to_string(), String::new());
}

/// Major alert: a panel that stays on screen (above full-screen apps) until dismissed.
/// For important reminders or when the user explicitly asks for an on-screen alert.
/// Shown even in quiet (badge) mode; only `PIYA_NOTIFY=off` suppresses it.
pub fn major_alert(title: &str, body: &str) {
    let title = if title.trim().is_empty() { "Piya" } else { title.trim() };
    signal("major".into(), title.to_string(), body.trim().to_string());
}

/// User-facing alert (test notification, reminders from chat, etc.).
pub fn user_alert(title: &str, body: &str) {
    let title = if title.trim().is_empty() {
        "Piya"
    } else {
        title.trim()
    };
    signal("notify".into(), title.to_string(), body.trim().to_string());
}

fn signal(event: String, title: String, detail: String) {
    // Evals record notifications to a file instead of showing banners.
    if let Some(log) = std::env::var_os("PIYA_NOTIFY_LOG") {
        let line = serde_json::json!({ "event": event, "title": title, "detail": detail });
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "{line}")
            });
        return;
    }
    if notify_disabled() {
        return;
    }
    let path = badge_signal_path();
    let payload = BadgeSignal {
        event: event.clone(),
        title: title.clone(),
        detail: detail.clone(),
        at: Utc::now().to_rfc3339(),
    };
    if let Ok(json) = serde_json::to_string(&payload) {
        let _ = std::fs::write(&path, json);
    }

    // Fallback popup from Rust (may lack app icon). Swift menu-bar app mirrors
    // badge-signal.json via UserNotifications for a branded banner.
    if event == "finished" || event == "notify" {
        crate::notify::send_popup("Piya", &title, &detail);
    }
    // No menu-bar app to draw the panel: fall back to a system alert that stays until clicked.
    if event == "major" && !gui_running() {
        crate::notify::send_popup("Piya", &title, &detail);
        system_alert(&title, &detail);
    }

    hint_badge_once();
}

pub fn signal_age_secs(raw: &str) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let at = value.get("at")?.as_str()?;
    let dt = DateTime::parse_from_rfc3339(at).ok()?.with_timezone(&Utc);
    Some((Utc::now() - dt).num_seconds())
}

pub fn last_notified_path() -> PathBuf {
    std::env::var("PIYA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data"))
        .join("last-notified.json")
}

pub fn load_last_notified() -> Option<String> {
    std::fs::read_to_string(last_notified_path()).ok()
}

pub fn save_last_notified(raw: &str) {
    let _ = std::fs::write(last_notified_path(), raw);
}

/// `pgrep -f` pattern for the SwiftUI menu-bar app (installed, staged or dev build).
const GUI_PROCESS_PATTERN: &str = "/Contents/MacOS/Piya";

/// The SwiftUI menu-bar app (or its dev build) is running and will draw major alerts.
///
/// macOS `pgrep` skips its own ancestors unless given `-a`. The app (clicking "Test major
/// alert") and workers it spawned are descendants of the app, so without `-a` it was never
/// found and every major alert also opened the fallback system alert.
fn gui_running() -> bool {
    if std::env::current_exe().is_ok_and(|p| p.to_string_lossy().contains(GUI_PROCESS_PATTERN)) {
        return true;
    }
    std::process::Command::new("/usr/bin/pgrep")
        .args(["-a", "-f", GUI_PROCESS_PATTERN])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Blocking-until-clicked system alert, spawned so the caller does not wait.
fn system_alert(title: &str, body: &str) {
    let script = format!(
        "display alert {} message {} as critical buttons {{\"Done\"}} default button \"Done\"",
        applescript_string(title),
        applescript_string(if body.is_empty() { "Reminder" } else { body })
    );
    let _ = std::process::Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn notify_disabled() -> bool {
    matches!(
        std::env::var("PIYA_NOTIFY")
            .unwrap_or_default()
            .trim()
            .to_lowercase()
            .as_str(),
        "0" | "false" | "off" | "none"
    )
}

fn badge_signal_path() -> PathBuf {
    std::env::var("PIYA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data"))
        .join("badge-signal.json")
}

fn hint_badge_once() {
    BADGE_HINT.get_or_init(|| {
        if !badge_daemon_running() {
            eprintln!(
                "Tip: run Piya from Applications for menu bar + Notification Center alerts.\n\
                 Or: piya app --install"
            );
        }
    });
}

fn badge_daemon_running() -> bool {
    let lock = std::env::var("PIYA_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("./data"))
        .join("piya-badge.lock");
    if let Ok(pid_str) = std::fs::read_to_string(&lock) {
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            return crate::process_util::is_alive(pid);
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::GUI_PROCESS_PATTERN;

    #[test]
    fn gui_pattern_is_plain_and_matches_app_paths() {
        assert!(
            !GUI_PROCESS_PATTERN.contains('\\'),
            "no regex escapes: pgrep would look for a literal backslash"
        );
        for path in [
            "/Applications/Piya.app/Contents/MacOS/Piya",
            "/Users/x/Projects/Piya/dist/build.noindex/PiyaDev.app/Contents/MacOS/Piya",
        ] {
            assert!(path.contains(GUI_PROCESS_PATTERN), "{path}");
        }
        assert!(!"/Applications/Piya.app/Contents/Helpers/piya run --continuous"
            .contains(GUI_PROCESS_PATTERN));
    }
}
