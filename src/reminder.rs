//! Lightweight reminder / notify-only tasks — skip the OPPAR agent loop.

use std::sync::LazyLock;

use regex::Regex;

use crate::tasks::Task;

static REMINDER_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\s*(remind(?:\s+me)?|notify(?:\s+me)?|alert(?:\s+me)?|ping(?:\s+me)?|nudge(?:\s+me)?)\b",
    )
    .expect("reminder start")
});

static REMINDER_PHRASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(send(?:\s+me)?(?:\s+a)?\s+notification|give(?:\s+me)?(?:\s+a)?\s+notification|notification(?:\s+at|\s+on|\s+in))\b",
    )
    .expect("reminder phrase")
});

static SCHEDULE_CHUNK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:at|around|on)\s+\d{1,2}(?::\d{2})?\s*(?:am|pm)?\b|\bin\s+\d+\s*(?:minutes?|mins?|hours?|hrs?|days?)\b|\btomorrow\b|\btonight\b|\bevery\s+(?:day|weekday|morning|evening|\w+day)\b",
    )
    .expect("schedule chunk")
});

static LEAD_IN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:for|to|about|regarding)\s+").expect("lead-in")
});

static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("ws"));

/// True when the task should fire a notification and complete — no plan/research/act.
pub fn is_reminder_task(task: &Task) -> bool {
    let text = format!("{}\n{}", task.title, task.description).to_lowercase();
    is_reminder_text(&text)
}

pub fn is_reminder_text(lower: &str) -> bool {
    REMINDER_START.is_match(lower) || REMINDER_PHRASE.is_match(lower)
}

/// User-facing banner body extracted from the brief.
pub fn reminder_body(task: &Task) -> String {
    let raw = if task.description.trim().len() >= task.title.trim().len() {
        task.description.trim()
    } else {
        task.title.trim()
    };
    let cleaned = strip_reminder_phrasing(raw);
    if cleaned.is_empty() {
        "Reminder".into()
    } else {
        cleaned.chars().take(280).collect()
    }
}

fn strip_reminder_phrasing(text: &str) -> String {
    let mut s = REMINDER_START.replace(text.trim(), "").to_string();
    s = REMINDER_PHRASE.replace_all(&s, "").to_string();
    s = SCHEDULE_CHUNK.replace_all(&s, "").to_string();
    s = LEAD_IN.replace(&s, "").to_string();
    s = WHITESPACE.replace_all(&s, " ").trim().to_string();
    s.trim_matches(|c: char| matches!(c, '-' | ':' | ',' | '.' | '!'))
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{Task, TaskStatus};

    fn task(desc: &str) -> Task {
        Task {
            id: "t1".into(),
            title: desc.chars().take(40).collect(),
            description: desc.into(),
            status: TaskStatus::Pending,
            priority: 1,
            attempts: 0,
            best_accuracy: 0.0,
            result: None,
            tags: String::new(),
            created_at: String::new(),
            updated_at: String::new(),
            run_at: None,
            recurrence: None,
        }
    }

    #[test]
    fn detects_remind_and_notify() {
        assert!(is_reminder_task(&task(
            "Remind me around 6:30 am for daily stats"
        )));
        assert!(is_reminder_task(&task("notify me at 6:30")));
        assert!(is_reminder_task(&task(
            "Send me a notification at 9pm to stretch"
        )));
        assert!(!is_reminder_task(&task(
            "Research competitor pricing and write a summary"
        )));
    }

    #[test]
    fn extracts_body() {
        let body = reminder_body(&task("Remind me around 6:30 am for daily stats"));
        assert!(
            body.to_lowercase().contains("daily stats"),
            "body was: {body}"
        );
        assert_eq!(reminder_body(&task("notify me at 6:30")), "Reminder");
    }
}
