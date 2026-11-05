use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Recurrence {
    Daily { hour: u32, minute: u32 },
    Weekdays { hour: u32, minute: u32 },
    Weekly { weekday: String, hour: u32, minute: u32 },
    EveryHours { interval: u32 },
    EveryMinutes { interval: u32 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Schedule {
    Immediate,
    Once(DateTime<Utc>),
    Recurring(Recurrence),
}

impl Schedule {
    pub fn initial_run_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Immediate => None,
            Self::Once(dt) => Some(*dt),
            Self::Recurring(rule) => Some(next_occurrence(rule, Utc::now())),
        }
    }

    pub fn recurrence(&self) -> Option<&Recurrence> {
        match self {
            Self::Recurring(r) => Some(r),
            _ => None,
        }
    }
}

/// Parse schedule phrases from a task brief.
pub fn parse_schedule(text: &str) -> Schedule {
    if let Some(rule) = parse_recurrence(text) {
        return Schedule::Recurring(rule);
    }
    if let Some(dt) = parse_one_off(text) {
        return Schedule::Once(dt);
    }
    Schedule::Immediate
}

pub fn next_occurrence(rule: &Recurrence, after: DateTime<Utc>) -> DateTime<Utc> {
    match rule {
        Recurrence::Daily { hour, minute } => next_daily(*hour, *minute, after, |_| true),
        Recurrence::Weekdays { hour, minute } => {
            next_daily(*hour, *minute, after, |d| !matches!(d, Weekday::Sat | Weekday::Sun))
        }
        Recurrence::Weekly { weekday, hour, minute } => {
            let target = parse_weekday(weekday).unwrap_or(Weekday::Mon);
            next_weekly(target, *hour, *minute, after)
        }
        Recurrence::EveryHours { interval } => after + Duration::hours(*interval as i64),
        Recurrence::EveryMinutes { interval } => after + Duration::minutes(*interval as i64),
    }
}

pub fn format_local(dt: DateTime<Utc>) -> String {
    dt.with_timezone(&Local)
        .format("%a %b %-d, %-I:%M %p")
        .to_string()
}

pub fn encode_for_task(text: &str) -> (Option<String>, Option<String>) {
    let schedule = parse_schedule(text);
    let run_at = schedule.initial_run_at().map(|dt| dt.to_rfc3339());
    let recurrence = schedule
        .recurrence()
        .and_then(|r| serde_json::to_string(r).ok());
    (run_at, recurrence)
}

pub fn describe_task_schedule(run_at: Option<&str>, recurrence: Option<&str>) -> Option<String> {
    if let Some(recurrence_json) = recurrence {
        if let Ok(rule) = serde_json::from_str::<Recurrence>(recurrence_json) {
            let mut desc = describe_recurrence(&rule);
            if let Some(run_at) = run_at {
                if let Ok(dt) = DateTime::parse_from_rfc3339(run_at) {
                    desc.push_str(&format!(" · next {}", format_local(dt.with_timezone(&Utc))));
                }
            }
            return Some(desc);
        }
    }
    run_at.and_then(|raw| {
        DateTime::parse_from_rfc3339(raw)
            .ok()
            .map(|dt| format!("Runs {}", format_local(dt.with_timezone(&Utc))))
    })
}

pub fn describe_recurrence(rule: &Recurrence) -> String {
    match rule {
        Recurrence::Daily { hour, minute } => {
            format!("Every day at {}", format_time(*hour, *minute))
        }
        Recurrence::Weekdays { hour, minute } => {
            format!("Every weekday at {}", format_time(*hour, *minute))
        }
        Recurrence::Weekly { weekday, hour, minute } => {
            format!(
                "Every {} at {}",
                weekday,
                format_time(*hour, *minute)
            )
        }
        Recurrence::EveryHours { interval } => format!("Every {interval} hours"),
        Recurrence::EveryMinutes { interval } => format!("Every {interval} minutes"),
    }
}

fn parse_one_off(text: &str) -> Option<DateTime<Utc>> {
    let lower = text.to_lowercase();

    if let Some(m) = regex::Regex::new(r"in\s+(\d+)\s*(minute|minutes|min|mins)")
        .ok()?
        .captures(&lower)
    {
        let n: i64 = m[1].parse().ok()?;
        return Some(Utc::now() + Duration::minutes(n));
    }

    if let Some(m) = regex::Regex::new(r"in\s+(\d+)\s*(hour|hours|hr|hrs)")
        .ok()?
        .captures(&lower)
    {
        let n: i64 = m[1].parse().ok()?;
        return Some(Utc::now() + Duration::hours(n));
    }

    if lower.contains("tomorrow") {
        if let Some(m) = regex::Regex::new(r"tomorrow\s+at\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?")
            .ok()?
            .captures(&lower)
        {
            let hour: u32 = m[1].parse().ok()?;
            let minute: u32 = m.get(2).and_then(|c| c.as_str().parse().ok()).unwrap_or(0);
            let hour24 = to_hour24(hour, m.get(3).map(|c| c.as_str()))?;
            let time = NaiveTime::from_hms_opt(hour24, minute, 0)?;
            let tomorrow = (Local::now() + Duration::days(1)).date_naive().and_time(time);
            return Local
                .from_local_datetime(&tomorrow)
                .single()
                .map(|l| l.with_timezone(&Utc));
        }
    }

    // One-off clock time without recurring keywords.
    if !is_recurring_context(&lower) {
        if let Some((hour, minute, _)) = parse_clock(&lower) {
            return Some(next_daily(hour, minute, Utc::now(), |_| true));
        }
    }

    None
}

fn parse_recurrence(text: &str) -> Option<Recurrence> {
    let lower = text.to_lowercase();

    if let Some(m) = regex::Regex::new(
        r"(?:every|each)\s+(\d+)\s*(minute|minutes|min|mins)\b",
    )
    .ok()?
    .captures(&lower)
    {
        let n: u32 = m[1].parse().ok()?;
        return Some(Recurrence::EveryMinutes { interval: n.max(1) });
    }

    if let Some(m) = regex::Regex::new(r"(?:every|each)\s+(\d+)\s*(hour|hours|hr|hrs)\b")
        .ok()?
        .captures(&lower)
    {
        let n: u32 = m[1].parse().ok()?;
        return Some(Recurrence::EveryHours { interval: n.max(1) });
    }

    if let Some((hour, minute, _)) = parse_clock(&lower) {
        if regex::Regex::new(r"(?:every|each)\s+day\b|daily\s+(?:at|around)\b")
            .ok()?
            .is_match(&lower)
        {
            return Some(Recurrence::Daily { hour, minute });
        }

        if regex::Regex::new(r"(?:every|each)\s+weekday\b")
            .ok()?
            .is_match(&lower)
        {
            return Some(Recurrence::Weekdays { hour, minute });
        }

        if let Some(m) = regex::Regex::new(
            r"(?:every|each|weekly\s+on)\s+(monday|tuesday|wednesday|thursday|friday|saturday|sunday|mon|tue|wed|thu|fri|sat|sun)\b",
        )
        .ok()?
        .captures(&lower)
        {
            let day = m[1].to_string();
            return Some(Recurrence::Weekly {
                weekday: normalize_weekday(&day),
                hour,
                minute,
            });
        }
    }

    None
}

fn is_recurring_context(lower: &str) -> bool {
    regex::Regex::new(
        r"(?:every|each)\s+(?:day|\d+|weekday|monday|tuesday|wednesday|thursday|friday|saturday|sunday|mon|tue|wed|thu|fri|sat|sun)\b|daily\s+(?:at|around)\b|weekly\s+on\b",
    )
    .ok()
    .is_some_and(|re| re.is_match(lower))
}

fn parse_clock(lower: &str) -> Option<(u32, u32, Option<&str>)> {
    let m = regex::Regex::new(r"(?:at|around)\s+(\d{1,2})(?::(\d{2}))?\s*(am|pm)?\b")
        .ok()?
        .captures(lower)?;
    let hour: u32 = m[1].parse().ok()?;
    let minute: u32 = m.get(2).and_then(|c| c.as_str().parse().ok()).unwrap_or(0);
    let ampm = m.get(3).map(|c| c.as_str());
    let hour24 = to_hour24(hour, ampm)?;
    Some((hour24, minute, ampm))
}

fn next_daily<F>(hour: u32, minute: u32, after: DateTime<Utc>, valid: F) -> DateTime<Utc>
where
    F: Fn(Weekday) -> bool,
{
    let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or(NaiveTime::MIN);
    let mut cursor = after.with_timezone(&Local);
    for _ in 0..370 {
        let date = cursor.date_naive();
        if valid(date.weekday()) {
            if let Some(local_dt) = Local.from_local_datetime(&date.and_time(time)).single() {
                let utc = local_dt.with_timezone(&Utc);
                if utc > after {
                    return utc;
                }
            }
        }
        cursor = cursor + Duration::days(1);
    }
    after + Duration::days(1)
}

fn next_weekly(weekday: Weekday, hour: u32, minute: u32, after: DateTime<Utc>) -> DateTime<Utc> {
    let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap_or(NaiveTime::MIN);
    let mut cursor = after.with_timezone(&Local);
    for _ in 0..370 {
        let date = cursor.date_naive();
        if date.weekday() == weekday {
            if let Some(local_dt) = Local.from_local_datetime(&date.and_time(time)).single() {
                let utc = local_dt.with_timezone(&Utc);
                if utc > after {
                    return utc;
                }
            }
        }
        cursor = cursor + Duration::days(1);
    }
    after + Duration::weeks(1)
}

fn format_time(hour: u32, minute: u32) -> String {
    let h12 = if hour == 0 || hour == 12 {
        12
    } else {
        hour % 12
    };
    let ampm = if hour < 12 { "AM" } else { "PM" };
    if minute == 0 {
        format!("{h12} {ampm}")
    } else {
        format!("{h12}:{minute:02} {ampm}")
    }
}

fn normalize_weekday(s: &str) -> String {
    match s {
        "mon" => "Monday".into(),
        "tue" => "Tuesday".into(),
        "wed" => "Wednesday".into(),
        "thu" => "Thursday".into(),
        "fri" => "Friday".into(),
        "sat" => "Saturday".into(),
        "sun" => "Sunday".into(),
        other => {
            let mut c = other.chars();
            match c.next() {
                None => other.to_string(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        }
    }
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s.to_lowercase().as_str() {
        "monday" | "mon" => Some(Weekday::Mon),
        "tuesday" | "tue" => Some(Weekday::Tue),
        "wednesday" | "wed" => Some(Weekday::Wed),
        "thursday" | "thu" => Some(Weekday::Thu),
        "friday" | "fri" => Some(Weekday::Fri),
        "saturday" | "sat" => Some(Weekday::Sat),
        "sunday" | "sun" => Some(Weekday::Sun),
        _ => None,
    }
}

fn to_hour24(hour: u32, ampm: Option<&str>) -> Option<u32> {
    match ampm {
        Some("pm") if hour < 12 => Some(hour + 12),
        Some("pm") => Some(hour),
        Some("am") if hour == 12 => Some(0),
        Some("am") => Some(hour),
        None if hour < 24 => Some(hour),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    #[test]
    fn parses_in_minutes() {
        let s = parse_schedule("check logs in 30 minutes");
        assert!(matches!(s, Schedule::Once(_)));
    }

    #[test]
    fn parses_around_time_one_off() {
        let s = parse_schedule("Remind me around 6:30 am for daily stats");
        let Schedule::Once(dt) = s else {
            panic!("expected one-off");
        };
        let local = dt.with_timezone(&Local);
        assert_eq!(local.hour(), 6);
        assert_eq!(local.minute(), 30);
    }

    #[test]
    fn parses_daily_recurring() {
        let s = parse_schedule("Send daily stats every day at 6:30 am");
        let Schedule::Recurring(Recurrence::Daily { hour, minute }) = s else {
            panic!("expected daily recurrence");
        };
        assert_eq!(hour, 6);
        assert_eq!(minute, 30);
    }

    #[test]
    fn daily_stats_is_not_recurrence() {
        let s = parse_schedule("daily stats report around 6:30 am");
        assert!(!matches!(s, Schedule::Recurring(_)));
    }

    #[test]
    fn computes_next_daily() {
        let rule = Recurrence::Daily { hour: 6, minute: 30 };
        let next = next_occurrence(&rule, Utc::now());
        let local = next.with_timezone(&Local);
        assert_eq!(local.hour(), 6);
        assert_eq!(local.minute(), 30);
    }
}
