//! First-class reminders, scheduled actions and cancellation for Chat.
//!
//! Every write is read back from the database and the reply reports the stored
//! state (id, next run), so the model can only claim what actually happened.

use anyhow::{anyhow, bail, Context, Result};
use async_openai::types::ChatCompletionTool;
use chrono::{DateTime, Duration, Local, NaiveDateTime, TimeZone, Utc};
use serde_json::{json, Value};

use crate::config::Settings;
use crate::schedule::{self, Recurrence};
use crate::tasks::{NewTask, Task, TaskKind, TaskQueue, TaskStatus};

use super::dispatch::tool;

pub const CREATE: &str = "piya_schedule_create";
pub const LIST: &str = "piya_schedule_list";
pub const UPDATE: &str = "piya_schedule_update";
pub const CANCEL: &str = "piya_task_cancel";
/// Tag on a reminder/action row that should fire as a major on-screen alert.
pub const MAJOR_TAG: &str = "major";

pub fn is_major(t: &Task) -> bool {
    t.tags.split(',').any(|x| x.trim() == MAJOR_TAG)
}

pub fn is_schedule_tool(name: &str) -> bool {
    matches!(name, CREATE | LIST | UPDATE | CANCEL)
}

fn repeat_schema() -> Value {
    json!({
        "type": "object",
        "description": "Repeat rule. Omit for a one-time schedule. Times are the user's local time.",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["daily", "weekdays", "weekly", "every_hours", "every_minutes"]
            },
            "hour": { "type": "integer", "description": "0-23 (daily/weekdays/weekly)" },
            "minute": { "type": "integer", "description": "0-59 (daily/weekdays/weekly)" },
            "weekday": { "type": "string", "description": "Monday..Sunday (weekly)" },
            "interval": { "type": "integer", "description": ">= 1 (every_hours/every_minutes)" }
        },
        "required": ["kind"]
    })
}

pub fn openai_tools() -> Vec<ChatCompletionTool> {
    vec![
        tool(
            CREATE,
            "Create a reminder or scheduled action. This is the ONLY correct way to handle \
             'remind me…', 'set a timer for…', 'ping me at…', 'every day at 6 send…'. \
             Never write scripts, cron jobs or sleep commands for these. \
             kind=reminder shows `message` as a notification at the time (no AI run). \
             kind=action runs `message` as an instruction for you at the time (e.g. check \
             something, summarize, then notify). Give exactly one of run_at / in_minutes, \
             and/or repeat. The result contains the stored id and next run — report those.",
            json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": ["reminder", "action"] },
                    "title": { "type": "string", "description": "Short label, e.g. 'Take a break'" },
                    "message": {
                        "type": "string",
                        "description": "reminder: the notification text. action: a complete \
                                        instruction for the future run (what to check/do and what to send)."
                    },
                    "run_at": {
                        "type": "string",
                        "description": "First run, local time 'YYYY-MM-DDTHH:MM' or RFC3339 with offset. Must be in the future."
                    },
                    "in_minutes": { "type": "number", "description": "Relative first run (timers): minutes from now." },
                    "repeat": repeat_schema(),
                    "major": {
                        "type": "boolean",
                        "description": "true = show as a major on-screen alert that stays until dismissed (user asked for an \
                                        on-screen/major/can't-miss alert, or it is urgent). Default false: normal banner."
                    }
                },
                "required": ["kind", "title", "message"]
            }),
        ),
        tool(
            LIST,
            "List pending reminders, scheduled actions and queued tasks with their ids and next run. \
             Call this before saying something is or isn't scheduled, and to find the id to \
             change or cancel.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            UPDATE,
            "Change a pending reminder/action: its time, repeat rule or message. Use the id from \
             piya_schedule_list. Set repeat to null to make it one-time.",
            json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Id or first 8 characters" },
                    "run_at": { "type": "string" },
                    "in_minutes": { "type": "number" },
                    "repeat": { "anyOf": [repeat_schema(), { "type": "null" }] },
                    "title": { "type": "string" },
                    "message": { "type": "string" },
                    "major": { "type": "boolean", "description": "Switch between major on-screen alert and normal banner" }
                },
                "required": ["id"]
            }),
        ),
        tool(
            CANCEL,
            "Cancel (remove/delete/stop) a pending reminder, scheduled action or queued/running task. \
             Use the id from piya_schedule_list or piya_task_status. If the item is not in the list, \
             tell the user it doesn't exist — never queue a task to remove something.",
            json!({
                "type": "object",
                "properties": { "id": { "type": "string", "description": "Id or first 8 characters" } },
                "required": ["id"]
            }),
        ),
    ]
}

pub fn dispatch(settings: &Settings, name: &str, args: &Value) -> Result<String> {
    let queue = TaskQueue::new(settings.data_dir.join("piya.db"))?;
    match name {
        CREATE => {
            let out = create(&queue, args, Utc::now())?;
            ensure_worker(settings);
            Ok(out)
        }
        LIST => Ok(list(&queue)?),
        UPDATE => {
            let out = update(&queue, args, Utc::now())?;
            ensure_worker(settings);
            Ok(out)
        }
        CANCEL => cancel(&queue, args),
        other => Err(anyhow!("unknown schedule tool `{other}`")),
    }
}

/// Scheduled rows only fire while a worker is polling the queue.
fn ensure_worker(settings: &Settings) {
    if let Err(err) = crate::worker::spawn_worker(settings, &["run", "--continuous"]) {
        eprintln!("piya: could not start worker for schedules: {err:#}");
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

pub fn create(queue: &TaskQueue, args: &Value, now: DateTime<Utc>) -> Result<String> {
    let kind = match str_arg(args, "kind").unwrap_or("reminder") {
        "action" => TaskKind::Action,
        "reminder" => TaskKind::Reminder,
        other => bail!("`kind` must be reminder or action, got `{other}`"),
    };
    let title = str_arg(args, "title").ok_or_else(|| anyhow!("`title` is required"))?;
    let message = str_arg(args, "message").unwrap_or(title);

    let recurrence = parse_repeat(args.get("repeat"))?;
    let first = parse_first_run(args, now)?;
    let run_at = match (first, &recurrence) {
        (Some(dt), _) => dt,
        (None, Some(rule)) => schedule::next_occurrence(rule, now),
        (None, None) => bail!("give run_at, in_minutes or repeat — a schedule needs a time"),
    };

    let run_at_s = run_at.to_rfc3339();
    let rec_s = recurrence.as_ref().map(serde_json::to_string).transpose()?;
    let major = args.get("major").and_then(|v| v.as_bool()).unwrap_or(false);
    let mut new = NewTask::new(title, message, kind);
    new.priority = 2;
    new.tags = if major { MAJOR_TAG } else { "" };
    new.run_at = Some(&run_at_s);
    new.recurrence = rec_s.as_deref();
    let stored = queue.insert(&new)?;

    // Read back what is actually in the database.
    let check = queue.get(&stored.id)?;
    if check.status != TaskStatus::Pending || check.run_at.is_none() {
        bail!("schedule was not stored correctly (status {})", check.status.as_str());
    }
    Ok(format!("Saved ✔ {}", describe(&check)))
}

pub fn update(queue: &TaskQueue, args: &Value, now: DateTime<Utc>) -> Result<String> {
    let id = str_arg(args, "id").ok_or_else(|| anyhow!("`id` is required"))?;
    let current = queue.get(id)?;
    let recurrence: Option<Option<Recurrence>> = match args.get("repeat") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(v) => Some(parse_repeat(Some(v))?),
    };
    let first = parse_first_run(args, now)?;
    let run_at = match (first, &recurrence) {
        (Some(dt), _) => Some(dt),
        (None, Some(Some(rule))) => Some(schedule::next_occurrence(rule, now)),
        _ => None,
    };
    let run_at_s = run_at.map(|d| d.to_rfc3339());
    let rec_json = recurrence
        .as_ref()
        .map(|r| r.as_ref().map(serde_json::to_string).transpose())
        .transpose()?;
    let updated = queue.update_schedule(
        &current.id,
        run_at_s.as_deref(),
        rec_json.as_ref().map(|r| r.as_deref()),
        str_arg(args, "title"),
        str_arg(args, "message"),
    )?;
    let updated = match args.get("major").and_then(|v| v.as_bool()) {
        Some(major) => queue.set_tags(&updated.id, if major { MAJOR_TAG } else { "" })?,
        None => updated,
    };
    Ok(format!("Updated ✔ {}", describe(&updated)))
}

pub fn cancel(queue: &TaskQueue, args: &Value) -> Result<String> {
    let id = str_arg(args, "id").ok_or_else(|| anyhow!("`id` is required"))?;
    let task = queue
        .get(id)
        .with_context(|| format!("no reminder/task with id `{id}` — call piya_schedule_list to see what exists"))?;
    let cancelled = queue.cancel(&task.id)?;
    let check = queue.get(&cancelled.id)?;
    if check.status != TaskStatus::Blocked {
        bail!("cancel did not stick: `{}` is still {}", check.short_id(), check.status.as_str());
    }
    Ok(format!(
        "Cancelled ✔ {} `{}` — {} (it will not run again)",
        check.kind.as_str(),
        check.short_id(),
        check.title
    ))
}

pub fn list(queue: &TaskQueue) -> Result<String> {
    let items = queue.list_schedules(50)?;
    let running = queue.list(Some(TaskStatus::InProgress), 10)?;
    let queued: Vec<Task> = queue
        .list(Some(TaskStatus::Pending), 20)?
        .into_iter()
        .filter(|t| !t.is_schedule())
        .collect();
    if items.is_empty() && running.is_empty() && queued.is_empty() {
        return Ok("Nothing scheduled or queued.".into());
    }
    let mut lines = Vec::new();
    if !items.is_empty() {
        lines.push("Scheduled:".to_string());
        lines.extend(items.iter().map(|t| format!("• {}", describe(t))));
    }
    let busy: Vec<&Task> = running.iter().filter(|t| !t.is_schedule()).collect();
    if !busy.is_empty() {
        lines.push("Running tasks:".into());
        lines.extend(busy.iter().map(|t| format!("• `{}` {}", t.short_id(), t.title)));
    }
    if !queued.is_empty() {
        lines.push("Queued tasks:".into());
        lines.extend(queued.iter().map(|t| format!("• `{}` {}", t.short_id(), t.title)));
    }
    Ok(lines.join("\n"))
}

/// One line: kind, id, title, next run, repeat, message.
pub fn describe(t: &Task) -> String {
    let next = t
        .run_at
        .as_deref()
        .and_then(|r| DateTime::parse_from_rfc3339(r).ok())
        .map(|d| schedule::format_local(d.with_timezone(&Utc)))
        .unwrap_or_else(|| "as soon as possible".into());
    let repeat = t
        .recurrence
        .as_deref()
        .and_then(|r| serde_json::from_str::<Recurrence>(r).ok())
        .map(|r| format!(" · repeats: {}", schedule::describe_recurrence(&r)))
        .unwrap_or_default();
    let msg = if t.description != t.title && !t.description.is_empty() {
        let short: String = t.description.chars().take(120).collect();
        format!(" · message: \"{short}\"")
    } else {
        String::new()
    };
    format!(
        "{}{} `{}` \"{}\" · next: {}{}{} · status: {}",
        if is_major(t) { "major " } else { "" },
        t.kind.as_str(),
        t.short_id(),
        t.title,
        next,
        repeat,
        msg,
        t.status.as_str()
    )
}

fn parse_first_run(args: &Value, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
    if let Some(mins) = args.get("in_minutes").and_then(|v| v.as_f64()) {
        if !(mins > 0.0) {
            bail!("`in_minutes` must be greater than 0");
        }
        return Ok(Some(now + Duration::seconds((mins * 60.0).round() as i64)));
    }
    let Some(raw) = str_arg(args, "run_at") else {
        return Ok(None);
    };
    let dt = parse_datetime(raw)?;
    if dt < now - Duration::seconds(30) {
        bail!(
            "run_at {} is in the past (now is {}) — pick a future time",
            schedule::format_local(dt),
            schedule::format_local(now)
        );
    }
    Ok(Some(dt))
}

pub fn parse_datetime(raw: &str) -> Result<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
            return Local
                .from_local_datetime(&naive)
                .earliest()
                .map(|l| l.with_timezone(&Utc))
                .ok_or_else(|| anyhow!("`{raw}` does not exist in the local time zone"));
        }
    }
    bail!("could not read run_at `{raw}` — use 'YYYY-MM-DDTHH:MM' (local) or RFC3339")
}

fn parse_repeat(v: Option<&Value>) -> Result<Option<Recurrence>> {
    let Some(v) = v.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let rule: Recurrence = serde_json::from_value(v.clone())
        .with_context(|| format!("invalid repeat rule {v}"))?;
    match &rule {
        Recurrence::Daily { hour, minute } | Recurrence::Weekdays { hour, minute } => {
            check_clock(*hour, *minute)?
        }
        Recurrence::Weekly { weekday, hour, minute } => {
            check_clock(*hour, *minute)?;
            if schedule::parse_weekday(weekday).is_none() {
                bail!("unknown weekday `{weekday}`");
            }
        }
        Recurrence::EveryHours { interval } | Recurrence::EveryMinutes { interval } => {
            if *interval == 0 {
                bail!("repeat interval must be at least 1");
            }
        }
    }
    Ok(Some(rule))
}

fn check_clock(hour: u32, minute: u32) -> Result<()> {
    if hour > 23 || minute > 59 {
        bail!("invalid time {hour}:{minute:02}");
    }
    Ok(())
}

/// Compact ledger injected into every chat turn so follow-ups ("cancel that",
/// "move it to 7") resolve against real ids.
pub fn ledger(queue: &TaskQueue) -> String {
    let mut lines = Vec::new();
    if let Ok(items) = queue.list_active(25) {
        for t in &items {
            lines.push(format!("- {}", describe(t)));
        }
    }
    if let Ok(recent) = queue.list_recent_finished(5) {
        for t in &recent {
            let result: String = t.result.as_deref().unwrap_or("").chars().take(140).collect();
            lines.push(format!(
                "- (finished) {} `{}` \"{}\" · {} · {}",
                t.kind.as_str(),
                t.short_id(),
                t.title,
                t.status.as_str(),
                result
            ));
        }
    }
    if lines.is_empty() {
        "(empty)".into()
    } else {
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_queue() -> (TaskQueue, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("piya-sched-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (TaskQueue::new(dir.join("piya.db")).unwrap(), dir)
    }

    #[test]
    fn timer_creates_one_pending_reminder_at_the_right_time() {
        let (q, dir) = temp_queue();
        let now = Utc::now();
        let out = create(
            &q,
            &json!({"kind":"reminder","title":"Timer","message":"15 minutes are up","in_minutes":15}),
            now,
        )
        .unwrap();
        assert!(out.starts_with("Saved"), "{out}");
        let items = q.list_schedules(10).unwrap();
        assert_eq!(items.len(), 1);
        let t = &items[0];
        assert_eq!(t.kind, TaskKind::Reminder);
        let at = DateTime::parse_from_rfc3339(t.run_at.as_deref().unwrap()).unwrap();
        assert_eq!((at.with_timezone(&Utc) - now).num_minutes(), 15);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn daily_repeat_is_stored_and_reschedules() {
        let (q, dir) = temp_queue();
        create(
            &q,
            &json!({"kind":"action","title":"Stats","message":"Send the daily stats",
                    "repeat":{"kind":"daily","hour":6,"minute":30}}),
            Utc::now(),
        )
        .unwrap();
        let t = q.list_schedules(10).unwrap().remove(0);
        assert_eq!(t.kind, TaskKind::Action);
        assert!(t.recurrence.is_some());
        q.update_status(&t.id, TaskStatus::Done, Some("ok"), Some(1.0)).unwrap();
        let next = q.reschedule_if_recurring(&t.id).unwrap().unwrap();
        assert_eq!(next.status, TaskStatus::Pending);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_past_and_timeless_schedules() {
        let (q, dir) = temp_queue();
        assert!(create(&q, &json!({"kind":"reminder","title":"x","message":"x"}), Utc::now()).is_err());
        assert!(create(
            &q,
            &json!({"kind":"reminder","title":"x","message":"x","run_at":"2001-01-01T10:00"}),
            Utc::now()
        )
        .is_err());
        assert!(q.list_schedules(10).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cancel_removes_and_unknown_ids_fail_loudly() {
        let (q, dir) = temp_queue();
        create(&q, &json!({"kind":"reminder","title":"pool test","message":"hi","in_minutes":5}), Utc::now())
            .unwrap();
        let t = q.list_schedules(10).unwrap().remove(0);
        let out = cancel(&q, &json!({"id": t.short_id()})).unwrap();
        assert!(out.starts_with("Cancelled"), "{out}");
        assert!(q.list_schedules(10).unwrap().is_empty());
        assert!(cancel(&q, &json!({"id": t.short_id()})).is_err(), "double cancel");
        assert!(cancel(&q, &json!({"id": "deadbeef"})).is_err(), "unknown id");
        assert!(cancel(&q, &json!({"id": ""})).is_err(), "empty id");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn major_flag_is_stored_and_toggleable() {
        let (q, dir) = temp_queue();
        create(&q, &json!({"kind":"reminder","title":"Standup","message":"Standup now","in_minutes":5,"major":true}), Utc::now())
            .unwrap();
        let t = q.list_schedules(10).unwrap().remove(0);
        assert!(is_major(&t));
        assert!(describe(&t).starts_with("major reminder"));
        update(&q, &json!({"id": t.short_id(), "major": false}), Utc::now()).unwrap();
        assert!(!is_major(&q.get(&t.id).unwrap()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn update_moves_time() {
        let (q, dir) = temp_queue();
        create(&q, &json!({"kind":"reminder","title":"call","message":"call mom","in_minutes":5}), Utc::now())
            .unwrap();
        let t = q.list_schedules(10).unwrap().remove(0);
        update(&q, &json!({"id": t.short_id(), "in_minutes": 60}), Utc::now()).unwrap();
        let t2 = q.get(&t.id).unwrap();
        let at = DateTime::parse_from_rfc3339(t2.run_at.as_deref().unwrap()).unwrap();
        assert!((at.with_timezone(&Utc) - Utc::now()).num_minutes() >= 58);
        let _ = std::fs::remove_dir_all(dir);
    }
}
