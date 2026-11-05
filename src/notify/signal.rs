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
