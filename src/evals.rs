//! `piya eval` — basic-errand evals against the real model.
//!
//! Each case runs in a throwaway data dir + workspace, sends one chat message
//! through the production chat path, then asserts on **state** (task ledger,
//! notifications, workspace files) — never on the model's wording.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Duration, Local, Timelike, Utc};
use serde_json::json;

use crate::chat_service::{chat_reply, ChatTurn};
use crate::chat_tools::schedule as sched;
use crate::config::Settings;
use crate::schedule::Recurrence;
use crate::tasks::{Task, TaskKind, TaskQueue, TaskStatus};

pub struct EvalState {
    pub reply: String,
    pub rows: Vec<Task>,
    pub notifications: usize,
    /// `event` field of each recorded notification (`notify`, `major`, …).
    pub notification_events: Vec<String>,
    pub workspace_files: Vec<String>,
    pub settings: Settings,
}

impl EvalState {
    fn pending(&self) -> Vec<&Task> {
        self.rows
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .collect()
    }
    fn of_kind(&self, kind: TaskKind) -> Vec<&Task> {
        self.pending().into_iter().filter(|t| t.kind == kind).collect()
    }
    fn by_title(&self, needle: &str) -> Option<&Task> {
        let n = needle.to_lowercase();
        self.rows.iter().find(|t| t.title.to_lowercase().contains(&n))
    }
}

type Setup = fn(&TaskQueue);
type Check = fn(&EvalState) -> Result<(), String>;

pub struct Case {
    pub name: &'static str,
    pub prompt: &'static str,
    pub history: &'static [(&'static str, &'static str)],
    pub setup: Option<Setup>,
    pub check: Check,
}

fn run_at(t: &Task) -> Option<DateTime<Utc>> {
    t.run_at
        .as_deref()
        .and_then(|r| DateTime::parse_from_rfc3339(r).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn minutes_from_now(t: &Task) -> i64 {
    run_at(t).map(|d| (d - Utc::now()).num_minutes()).unwrap_or(i64::MIN)
}

fn no_side_effects(s: &EvalState) -> Result<(), String> {
    if !s.workspace_files.is_empty() {
        return Err(format!("created workspace files: {:?}", s.workspace_files));
    }
    let new_rows: Vec<_> = s.rows.iter().map(|t| t.title.clone()).collect();
    if !new_rows.is_empty() {
        return Err(format!("created ledger rows: {new_rows:?}"));
    }
    Ok(())
}

fn one_reminder(s: &EvalState) -> Result<&Task, String> {
    if !s.workspace_files.is_empty() {
        return Err(format!("wrote files instead of scheduling: {:?}", s.workspace_files));
    }
    if !s.of_kind(TaskKind::Task).is_empty() {
        return Err("queued a background task instead of a schedule".into());
    }
    let r = s.of_kind(TaskKind::Reminder);
    if r.len() != 1 {
        return Err(format!("expected 1 pending reminder, found {}", r.len()));
    }
    Ok(r[0])
}

fn add_reminder(q: &TaskQueue, title: &str, msg: &str, mins: i64) {
    sched::create(
        q,
        &json!({"kind": "reminder", "title": title, "message": msg, "in_minutes": mins}),
        Utc::now(),
    )
    .expect("setup reminder");
}

pub fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "remind-in-minutes",
            prompt: "Remind me in 10 minutes to stretch",
            history: &[],
            setup: None,
            check: |s| {
                let r = one_reminder(s)?;
                let m = minutes_from_now(r);
                (8..=11).contains(&m).then_some(()).ok_or(format!("fires in {m} min, expected ~10"))
            },
        },
        Case {
            name: "timer",
            prompt: "Set a timer for 15 minutes",
            history: &[],
            setup: None,
            check: |s| {
                let r = one_reminder(s)?;
                let m = minutes_from_now(r);
                (13..=16).contains(&m).then_some(()).ok_or(format!("fires in {m} min, expected ~15"))
            },
        },
        Case {
            name: "remind-tomorrow-9am",
            prompt: "Remind me tomorrow at 9am to call the bank",
            history: &[],
            setup: None,
            check: |s| {
                let r = one_reminder(s)?;
                let at = run_at(r).ok_or("no run_at")?.with_timezone(&Local);
                let tomorrow = (Local::now() + Duration::days(1)).date_naive();
                if at.date_naive() == tomorrow && at.hour() == 9 && at.minute() == 0 {
                    Ok(())
                } else {
                    Err(format!("fires {at}, expected tomorrow 09:00"))
                }
            },
        },
        Case {
            name: "recurring-weekdays",
            prompt: "Every weekday at 6:30 am remind me to send the daily stats report",
            history: &[],
            setup: None,
            check: |s| {
                let r = one_reminder(s)?;
                let rule: Recurrence = serde_json::from_str(r.recurrence.as_deref().ok_or("not recurring")?)
                    .map_err(|e| e.to_string())?;
                match rule {
                    Recurrence::Weekdays { hour: 6, minute: 30 } => Ok(()),
                    other => Err(format!("wrong rule {other:?}")),
                }
            },
        },
        Case {
            name: "recurring-action",
            prompt: "Every day at 8am check the weather in Pune and notify me",
            history: &[],
            setup: None,
            check: |s| {
                let a = s.of_kind(TaskKind::Action);
                if a.len() != 1 {
                    return Err(format!("expected 1 action schedule, found {} (rows: {:?})", a.len(),
                        s.rows.iter().map(|t| (t.kind, t.title.clone())).collect::<Vec<_>>()));
                }
                let at = run_at(a[0]).ok_or("no run_at")?.with_timezone(&Local);
                (at.hour() == 8 && at.minute() == 0 && a[0].recurrence.is_some())
                    .then_some(())
                    .ok_or(format!("next run {at}, recurrence {:?}", a[0].recurrence))
            },
        },
        Case {
            name: "notify-now",
            prompt: "Send me a test notification",
            history: &[],
            setup: None,
            check: |s| {
                no_side_effects(s)?;
                (s.notifications >= 1).then_some(()).ok_or("no notification was sent".into())
            },
        },
        Case {
            name: "major-now",
            prompt: "Put a big alert on my screen right now saying: stand up and stretch",
            history: &[],
            setup: None,
            check: |s| {
                no_side_effects(s)?;
                s.notification_events
                    .iter()
                    .any(|e| e == "major")
                    .then_some(())
                    .ok_or(format!("no major alert (events: {:?})", s.notification_events))
            },
        },
        Case {
            name: "major-reminder",
            prompt: "Remind me in 10 minutes to join the client call — make sure it pops up on my screen, I can't miss it",
            history: &[],
            setup: None,
            check: |s| {
                let r = one_reminder(s)?;
                crate::chat_tools::schedule::is_major(r)
                    .then_some(())
                    .ok_or("reminder is not flagged major".into())
            },
        },
        Case {
            name: "quick-fact",
            prompt: "Who is Spiderman?",
            history: &[],
            setup: None,
            check: |s| {
                no_side_effects(s)?;
                s.reply.to_lowercase().contains("parker").then_some(()).ok_or("answer missing".into())
            },
        },
        Case {
            name: "small-talk",
            prompt: "how are you?",
            history: &[],
            setup: None,
            check: no_side_effects,
        },
        Case {
            name: "cancel-existing",
            prompt: "Remove the reminder for pool test 4",
            history: &[],
            setup: Some(|q| {
                add_reminder(q, "pool test 3", "pool test 3", 30);
                add_reminder(q, "pool test 4", "pool test 4", 30);
            }),
            check: |s| {
                let t4 = s.by_title("pool test 4").ok_or("pool test 4 row missing")?;
                let t3 = s.by_title("pool test 3").ok_or("pool test 3 row missing")?;
                if t4.status != TaskStatus::Blocked {
                    return Err(format!("pool test 4 is {}", t4.status.as_str()));
                }
                if t3.status != TaskStatus::Pending {
                    return Err("cancelled the wrong reminder".into());
                }
                (s.rows.len() == 2).then_some(()).ok_or("created extra rows".into())
            },
        },
        Case {
            name: "cancel-missing",
            prompt: "Remove the reminder for pool test 99",
            history: &[],
            setup: Some(|q| add_reminder(q, "pool test 4", "pool test 4", 30)),
            check: |s| {
                let t4 = s.by_title("pool test 4").ok_or("row missing")?;
                if t4.status != TaskStatus::Pending {
                    return Err("cancelled an unrelated reminder".into());
                }
                (s.rows.len() == 1).then_some(()).ok_or("created a task to 'remove' it".into())
            },
        },
        Case {
            name: "follow-up-cancel",
            prompt: "actually cancel that",
            history: &[
                ("user", "Remind me in 20 minutes to stretch"),
                ("piya", "Done — I'll remind you to stretch in 20 minutes."),
            ],
            setup: Some(|q| add_reminder(q, "Stretch", "Time to stretch", 20)),
            check: |s| {
                let t = s.by_title("stretch").ok_or("row missing")?;
                (t.status == TaskStatus::Blocked && s.rows.len() == 1)
                    .then_some(())
                    .ok_or(format!("stretch is {}, rows {}", t.status.as_str(), s.rows.len()))
            },
        },
        Case {
            name: "reschedule",
            prompt: "Move my call mom reminder to 2 hours from now",
            history: &[],
            setup: Some(|q| add_reminder(q, "Call mom", "Call mom", 30)),
            check: |s| {
                let t = s.by_title("call mom").ok_or("row missing")?;
                let m = minutes_from_now(t);
                if s.rows.len() != 1 {
                    return Err(format!("expected the existing row to move, found {} rows", s.rows.len()));
                }
                (115..=121).contains(&m).then_some(()).ok_or(format!("fires in {m} min, expected ~120"))
            },
        },
        Case {
            name: "substantial-task",
            prompt: "Research the top 3 open-source vector databases and write a comparison report",
            history: &[],
            setup: None,
            check: |s| {
                let t = s.of_kind(TaskKind::Task);
                if t.len() != 1 {
                    return Err(format!("expected 1 background task, found {}", t.len()));
                }
                if t[0].done_when.as_deref().unwrap_or("").trim().is_empty() {
                    return Err("task has no done_when".into());
                }
                s.of_kind(TaskKind::Reminder).is_empty().then_some(()).ok_or("also created a reminder".into())
            },
        },
        Case {
            name: "remember-fact",
            prompt: "Remember that my staging server IP is 10.0.0.42",
            history: &[],
            setup: None,
            check: |s| {
                no_side_effects(s)?;
                let found = crate::ui::assign::search_memory(&s.settings, "staging server IP")
                    .map_err(|e| e.to_string())?;
                found.contains("10.0.0.42").then_some(()).ok_or(format!("not in memory: {found}"))
            },
        },
    ]
}

fn list_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(list_files(&p));
            } else {
                out.push(p.display().to_string());
            }
        }
    }
    out
}

fn run_case(base: &Settings, case: &Case, root: &Path) -> Result<Result<(), String>> {
    let dir = root.join(case.name);
    let mut settings = base.clone();
    settings.data_dir = dir.join("data");
    settings.workspace = dir.join("workspace");
    std::fs::create_dir_all(&settings.data_dir)?;
    std::fs::create_dir_all(&settings.workspace)?;
    let notify_log = dir.join("notifications.jsonl");
    std::env::set_var("PIYA_NOTIFY_LOG", &notify_log);

    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    if let Some(setup) = case.setup {
        setup(&queue);
    }
    let history: Vec<ChatTurn> = case
        .history
        .iter()
        .map(|(r, t)| ChatTurn {
            role: r.to_string(),
            text: t.to_string(),
            attachments: Vec::new(),
        })
        .collect();

    let reply = match chat_reply(&settings, &history, case.prompt) {
        Ok(r) => r,
        Err(err) => return Ok(Err(format!("chat failed: {err:#}"))),
    };
    let state = EvalState {
        reply,
        rows: queue.list(None, 100)?,
        notifications: std::fs::read_to_string(&notify_log)
            .map(|s| s.lines().count())
            .unwrap_or(0),
        notification_events: std::fs::read_to_string(&notify_log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v.get("event").and_then(|e| e.as_str()).map(str::to_string))
            .collect(),
        workspace_files: list_files(&settings.workspace),
        settings,
    };
    let verdict = (case.check)(&state);
    if verdict.is_err() {
        println!("    reply: {}", state.reply.replace('\n', " ").chars().take(300).collect::<String>());
    }
    Ok(verdict)
}

/// Run all (or filtered) cases; returns the number of failures.
pub fn run(base: &Settings, filter: Option<&str>, repeat: usize) -> Result<usize> {
    std::env::set_var("PIYA_NO_WORKER_SPAWN", "1");
    let root: PathBuf = std::env::temp_dir().join(format!(
        "piya-eval-{}",
        Local::now().format("%Y%m%d-%H%M%S")
    ));
    let cases: Vec<Case> = cases()
        .into_iter()
        .filter(|c| filter.is_none_or(|f| c.name.contains(f)))
        .collect();
    println!(
        "Running {} case(s) × {repeat} with model {} (scratch: {})",
        cases.len(),
        base.model,
        root.display()
    );
    let mut failures = 0;
    let mut total = 0;
    for case in &cases {
        for i in 0..repeat.max(1) {
            total += 1;
            let run_root = root.join(format!("run{i}"));
            let verdict = run_case(base, case, &run_root)?;
            match verdict {
                Ok(()) => println!("  ✅ {}", case.name),
                Err(why) => {
                    failures += 1;
                    println!("  ❌ {} — {why}", case.name);
                }
            }
        }
    }
    println!("\n{}/{} passed", total - failures, total);
    Ok(failures)
}
