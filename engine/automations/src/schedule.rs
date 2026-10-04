//! Schedule parsing: cron expressions plus a small set of friendly phrases,
//! all normalized to a cron expression that `croner` evaluates.

use chrono::{DateTime, Local, TimeZone, Timelike, Utc};
use croner::Cron;
use odex_protocol::ScheduleValidateResponse;

/// A parsed, validated schedule. Internally always a cron expression
/// (5 fields, or 6 with a leading seconds field).
#[derive(Debug, Clone)]
pub struct Schedule {
    source: String,
    cron: String,
    parsed: Cron,
}

impl Schedule {
    /// The original text the schedule was parsed from (trimmed).
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The normalized cron expression.
    pub fn cron(&self) -> &str {
        &self.cron
    }

    /// Next occurrence strictly after `t`, in local time.
    pub fn next_after(&self, t: DateTime<Local>) -> Option<DateTime<Local>> {
        self.next_after_in(t)
    }

    /// Next occurrence strictly after `t` in any time zone.
    ///
    /// Guarantees the result is later than `t` even around DST transitions
    /// (when a wall-clock time is ambiguous the evaluator may pick the earlier
    /// instant; such candidates are skipped).
    pub fn next_after_in<Tz: TimeZone>(&self, t: DateTime<Tz>) -> Option<DateTime<Tz>> {
        // Truncate to whole seconds on the absolute timeline (`with_nanosecond`
        // fails for ambiguous wall-clock times).
        let mut cursor = t.clone() - chrono::Duration::nanoseconds(i64::from(t.nanosecond()));
        let tz = t.timezone();
        for _ in 0..1_000 {
            let candidate = self.parsed.find_next_occurrence(&cursor, false).ok()?;
            if candidate > t {
                return Some(candidate);
            }
            // During a "fall back" hour the evaluator resolves an ambiguous
            // wall-clock time to its earlier instant; try the later one.
            if let Some(alt) = tz.from_local_datetime(&candidate.naive_local()).latest() {
                if alt > t {
                    return Some(alt);
                }
            }
            let delta = (candidate.naive_local() - cursor.naive_local()).max(chrono::Duration::seconds(1));
            cursor += delta;
        }
        None
    }

    /// The next `n` occurrences after `t`.
    pub fn upcoming(&self, t: DateTime<Local>, n: usize) -> Vec<DateTime<Local>> {
        let mut out = Vec::with_capacity(n);
        let mut cursor = t;
        while out.len() < n {
            match self.next_after(cursor) {
                Some(next) => {
                    out.push(next);
                    cursor = next;
                }
                None => break,
            }
        }
        out
    }

    /// Human-readable description, e.g. "Every 15 minutes", "Weekdays at 09:00".
    pub fn describe(&self) -> String {
        describe_cron(&self.cron)
    }
}

/// Parse a schedule: a 5-field cron (`*/15 * * * *`), a 6-field cron with
/// seconds, an `@` shorthand (`@hourly`, `@daily`, `@weekly`, `@monthly`,
/// `@yearly`), or a friendly phrase (`every 15m`, `every 2h`,
/// `every day at 09:00`, `daily 09:00`, `weekdays 09:00`, `weekly mon 09:00`,
/// `hourly`, `monthly 1 09:00`, `every monday at 9am`, ...).
pub fn parse_schedule(s: &str) -> Result<Schedule, String> {
    let source = s.trim();
    if source.is_empty() {
        return Err("schedule is empty".to_string());
    }
    let cron = normalize(source)?;
    let parsed = Cron::new(&cron)
        .with_seconds_optional()
        .parse()
        .map_err(|e| format!("invalid cron expression `{cron}`: {e}"))?;
    // Reject expressions that can never fire (e.g. February 30th).
    if parsed.find_next_occurrence(&Utc::now(), false).is_err() {
        return Err(format!("schedule `{source}` never runs"));
    }
    Ok(Schedule { source: source.to_string(), cron, parsed })
}

/// Validate a schedule for the UI: description plus the next five runs (epoch ms).
pub fn validate(s: &str, now: DateTime<Local>) -> ScheduleValidateResponse {
    match parse_schedule(s) {
        Ok(schedule) => ScheduleValidateResponse {
            valid: true,
            error: None,
            description: Some(schedule.describe()),
            next_runs: schedule.upcoming(now, 5).into_iter().map(|t| t.timestamp_millis()).collect(),
        },
        Err(e) => ScheduleValidateResponse { valid: false, error: Some(e), description: None, next_runs: Vec::new() },
    }
}

// ---------------------------------------------------------------------------
// Normalization
// ---------------------------------------------------------------------------

const HELP: &str = "use cron (e.g. `*/15 * * * *`) or a phrase like `every 15m`, `every 2h`, `daily 09:00`, \
                    `weekdays 09:00`, `weekly mon 09:00`, `monthly 1 09:00`, `hourly`";

fn normalize(source: &str) -> Result<String, String> {
    let lower = source.to_ascii_lowercase();
    if let Some(nick) = lower.strip_prefix('@') {
        return match nick.trim() {
            "hourly" => Ok("0 * * * *".into()),
            "daily" | "midnight" => Ok("0 0 * * *".into()),
            "weekly" => Ok("0 0 * * 0".into()),
            "monthly" => Ok("0 0 1 * *".into()),
            "yearly" | "annually" => Ok("0 0 1 1 *".into()),
            other => {
                Err(format!("unknown shorthand `@{other}`; supported: @hourly, @daily, @weekly, @monthly, @yearly"))
            }
        };
    }
    let fields: Vec<&str> = source.split_whitespace().collect();
    if (fields.len() == 5 || fields.len() == 6) && fields.iter().all(|f| is_cron_field(f)) {
        return Ok(fields.join(" "));
    }
    friendly(&lower).ok_or_else(|| format!("unrecognized schedule `{source}`; {HELP}"))?
}

fn is_cron_field(field: &str) -> bool {
    field.split([',', '-', '/', '#']).all(|part| {
        let upper = part.to_ascii_uppercase();
        !part.is_empty()
            && (part == "*"
                || part == "?"
                || part.chars().all(|c| c.is_ascii_digit())
                || upper == "L"
                || upper == "LW"
                || ((upper.ends_with('L') || upper.ends_with('W'))
                    && upper[..upper.len() - 1].chars().all(|c| c.is_ascii_digit()))
                || (part.len() == 3 && weekday_number(&upper.to_ascii_lowercase()).is_some())
                || month_name(&upper).is_some())
    })
}

fn month_name(upper: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = ["JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC"];
    MONTHS.iter().position(|m| *m == upper).map(|i| i as u32 + 1)
}

/// Returns `Some(Ok(cron))` for a recognized phrase, `Some(Err)` for a
/// recognized but invalid phrase, `None` when nothing matched.
fn friendly(lower: &str) -> Option<Result<String, String>> {
    let toks = tokenize(lower);
    let t: Vec<&str> = toks.iter().map(String::as_str).collect();
    let r = match t.as_slice() {
        ["hourly"] | ["every", "hour"] => Ok("0 * * * *".to_string()),
        ["hourly", m] | ["every", "hour", m] => {
            minute_of_hour(m).map(|m| Ok(format!("{m} * * * *"))).unwrap_or_else(|| Err(bad_time(m)))
        }
        ["daily"] | ["every", "day"] | ["everyday"] | ["nightly"] => Ok("0 0 * * *".to_string()),
        ["daily", time] | ["every", "day", time] | ["everyday", time] | ["nightly", time] => {
            at_time(time, "*", "*", "*")
        }
        ["weekdays"] | ["weekday"] | ["every", "weekday"] => Ok("0 0 * * 1-5".to_string()),
        ["weekdays", time] | ["weekday", time] | ["every", "weekday", time] => at_time(time, "*", "*", "1-5"),
        ["weekends"] | ["weekend"] | ["every", "weekend"] => Ok("0 0 * * 0,6".to_string()),
        ["weekends", time] | ["weekend", time] | ["every", "weekend", time] => at_time(time, "*", "*", "0,6"),
        ["weekly"] | ["every", "week"] => Ok("0 0 * * 0".to_string()),
        ["weekly", rest @ ..] | ["every", "week", rest @ ..] => days_and_time(rest)?,
        ["monthly"] | ["every", "month"] => Ok("0 0 1 * *".to_string()),
        ["monthly", rest @ ..] | ["every", "month", rest @ ..] => month_day_and_time(rest)?,
        ["yearly"] | ["annually"] | ["every", "year"] => Ok("0 0 1 1 *".to_string()),
        ["every", rest @ ..] => interval(rest).or_else(|| days_and_time(rest))?,
        [first, ..] if first.split('-').next().and_then(weekday_number).is_some() => days_and_time(&t)?,
        _ => return None,
    };
    Some(r)
}

fn bad_time(t: &str) -> String {
    format!("invalid time `{t}`; use HH:MM (24h) or e.g. 9am / 5:30pm")
}

fn at_time(time: &str, dom: &str, month: &str, dow: &str) -> Result<String, String> {
    let (h, m) = parse_time(time).ok_or_else(|| bad_time(time))?;
    Ok(format!("{m} {h} {dom} {month} {dow}"))
}

/// Split into words, splitting commas, dropping filler words and merging a
/// trailing `am`/`pm` into the preceding time.
fn tokenize(lower: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for raw in lower.split(|c: char| c.is_whitespace() || c == ',') {
        if raw.is_empty() || matches!(raw, "at" | "on" | "the" | "and" | "of" | "&") {
            continue;
        }
        if matches!(raw, "am" | "pm" | "a.m." | "p.m.") {
            if let Some(prev) = out.last_mut() {
                if prev.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    prev.push_str(&raw.replace('.', ""));
                    continue;
                }
            }
        }
        out.push(raw.to_string());
    }
    out
}

/// `every N unit`, `every Nunit`, `every unit`.
fn interval(rest: &[&str]) -> Option<Result<String, String>> {
    let (n, unit) = match rest {
        [unit] => match split_compact(unit) {
            Some((n, u)) => (n, u),
            None => (1, unit.to_string()),
        },
        [n, unit] => (n.parse::<u32>().ok()?, unit.to_string()),
        _ => return None,
    };
    let unit = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 's',
        "m" | "min" | "mins" | "minute" | "minutes" => 'm',
        "h" | "hr" | "hrs" | "hour" | "hours" => 'h',
        "d" | "day" | "days" => 'd',
        _ => return None,
    };
    Some(interval_cron(n, unit))
}

fn split_compact(tok: &str) -> Option<(u32, String)> {
    let digits: String = tok.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n = digits.parse().ok()?;
    Some((n, tok[digits.len()..].to_string()))
}

/// `n % d == 0`, written without the modulo idiom so it works on older toolchains.
fn divisible(n: u32, d: u32) -> bool {
    n.checked_rem(d) == Some(0)
}

fn interval_cron(n: u32, unit: char) -> Result<String, String> {
    if n == 0 {
        return Err("interval must be at least 1".to_string());
    }
    match unit {
        's' if n == 1 => Ok("* * * * * *".to_string()),
        's' if n < 60 => Ok(format!("*/{n} * * * * *")),
        's' if divisible(n, 60) => interval_cron(n / 60, 'm'),
        's' => Err("second intervals must be below 60 or a multiple of 60".to_string()),
        'm' if n == 1 => Ok("* * * * *".to_string()),
        'm' if n < 60 => Ok(format!("*/{n} * * * *")),
        'm' if divisible(n, 60) => interval_cron(n / 60, 'h'),
        'm' => Err("minute intervals must be below 60 or a multiple of 60".to_string()),
        'h' if n == 1 => Ok("0 * * * *".to_string()),
        'h' if n < 24 => Ok(format!("0 */{n} * * *")),
        'h' if divisible(n, 24) => interval_cron(n / 24, 'd'),
        'h' => Err("hour intervals must be below 24 or a multiple of 24".to_string()),
        'd' if n == 1 => Ok("0 0 * * *".to_string()),
        'd' if n <= 31 => Ok(format!("0 0 */{n} * *")),
        _ => Err("day intervals must be between 1 and 31".to_string()),
    }
}

/// `mon 09:00`, `mon wed fri at 9am`, `mon-fri 17:00`, `mondays`.
fn days_and_time(toks: &[&str]) -> Option<Result<String, String>> {
    let mut days: Vec<u32> = Vec::new();
    let mut idx = 0;
    while idx < toks.len() {
        let tok = toks[idx];
        if let Some((a, b)) = tok.split_once('-') {
            match (weekday_number(a), weekday_number(b)) {
                (Some(a), Some(b)) => {
                    let mut d = a;
                    loop {
                        days.push(d);
                        if d == b {
                            break;
                        }
                        d = (d + 1) % 7;
                    }
                }
                _ => break,
            }
        } else if let Some(d) = weekday_number(tok) {
            days.push(d);
        } else {
            break;
        }
        idx += 1;
    }
    if days.is_empty() {
        return None;
    }
    let rest = &toks[idx..];
    let (h, m) = match rest {
        [] => (0, 0),
        [time] => match parse_time(time) {
            Some(hm) => hm,
            None => return Some(Err(bad_time(time))),
        },
        _ => return None,
    };
    days.sort_unstable();
    days.dedup();
    Some(Ok(format!("{m} {h} * * {}", dow_field(&days))))
}

fn dow_field(days: &[u32]) -> String {
    match days {
        [0, 1, 2, 3, 4, 5, 6] => "*".to_string(),
        [1, 2, 3, 4, 5] => "1-5".to_string(),
        _ => days.iter().map(u32::to_string).collect::<Vec<_>>().join(","),
    }
}

/// `1 09:00`, `1st at 9am`, `15th`.
fn month_day_and_time(toks: &[&str]) -> Option<Result<String, String>> {
    let (day_tok, time) = match toks {
        [d] => (*d, None),
        [d, t] => (*d, Some(*t)),
        _ => return None,
    };
    let digits: String = day_tok.chars().take_while(|c| c.is_ascii_digit()).collect();
    let suffix = &day_tok[digits.len()..];
    if digits.is_empty() || !matches!(suffix, "" | "st" | "nd" | "rd" | "th") {
        return None;
    }
    let day: u32 = digits.parse().ok()?;
    if !(1..=31).contains(&day) {
        return Some(Err(format!("day of month must be 1-31, got {day}")));
    }
    let (h, m) = match time {
        None => (0, 0),
        Some(t) => match parse_time(t) {
            Some(hm) => hm,
            None => return Some(Err(bad_time(t))),
        },
    };
    Some(Ok(format!("{m} {h} {day} * *")))
}

fn weekday_number(tok: &str) -> Option<u32> {
    let tok = tok.trim_end_matches('s');
    Some(match tok {
        "sun" | "sunday" => 0,
        "mon" | "monday" => 1,
        "tue" | "tues" | "tuesday" => 2,
        "wed" | "wednesday" => 3,
        "thu" | "thur" | "thurs" | "thursday" => 4,
        "fri" | "friday" => 5,
        "sat" | "saturday" => 6,
        _ => return None,
    })
}

/// Minute within the hour: `:15`, `15`, `xx:15`.
fn minute_of_hour(tok: &str) -> Option<u32> {
    let t = tok.trim_start_matches("xx").trim_start_matches(':');
    let m: u32 = t.parse().ok()?;
    (m < 60).then_some(m)
}

/// `09:00`, `9:30`, `9`, `9am`, `5:30pm`, `17.45`, `noon`, `midnight`.
fn parse_time(tok: &str) -> Option<(u32, u32)> {
    match tok {
        "noon" | "midday" => return Some((12, 0)),
        "midnight" => return Some((0, 0)),
        _ => {}
    }
    let (body, meridiem) = if let Some(b) = tok.strip_suffix("am") {
        (b, Some(false))
    } else if let Some(b) = tok.strip_suffix("pm") {
        (b, Some(true))
    } else {
        (tok, None)
    };
    let (h, m) = match body.split_once([':', '.']) {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok().filter(|_| m.len() == 2)?),
        None => (body.parse::<u32>().ok()?, 0),
    };
    if m > 59 {
        return None;
    }
    let h = match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            match (h, pm) {
                (12, false) => 0,
                (12, true) => 12,
                (h, true) => h + 12,
                (h, false) => h,
            }
        }
        None if h < 24 => h,
        None => return None,
    };
    Some((h, m))
}

// ---------------------------------------------------------------------------
// Description
// ---------------------------------------------------------------------------

fn describe_cron(cron: &str) -> String {
    let fields: Vec<&str> = cron.split_whitespace().collect();
    let fallback = || format!("Cron schedule `{cron}`");
    let rest: &[&str] = match fields.len() {
        6 => {
            let sec = fields[0];
            let tail = &fields[1..];
            if sec != "0" {
                if tail.iter().all(|f| *f == "*") {
                    if sec == "*" {
                        return "Every second".to_string();
                    }
                    if let Some(n) = step(sec) {
                        return plural(n, "second");
                    }
                }
                return fallback();
            }
            tail
        }
        5 => &fields,
        _ => return fallback(),
    };
    let [min, hour, dom, month, dow] = [rest[0], rest[1], rest[2], rest[3], rest[4]];
    let num = |s: &str| s.parse::<u32>().ok();
    match (min, hour, dom, month, dow) {
        ("*", "*", "*", "*", "*") => "Every minute".to_string(),
        (m, "*", "*", "*", "*") if step(m).is_some() => plural(step(m).unwrap_or(1), "minute"),
        (m, "*", "*", "*", "*") if num(m).is_some() => match num(m) {
            Some(0) => "Every hour".to_string(),
            Some(m) => format!("Every hour at :{m:02}"),
            None => fallback(),
        },
        (m, h, "*", "*", "*") if num(m).is_some() && step(h).is_some() => {
            let base = plural(step(h).unwrap_or(1), "hour");
            match num(m) {
                Some(0) | None => base,
                Some(m) => format!("{base} at :{m:02}"),
            }
        }
        (m, h, dom, "*", dow) if num(m).is_some() && num(h).is_some() => {
            let at = format!("{:02}:{:02}", num(h).unwrap_or(0), num(m).unwrap_or(0));
            match (dom, dow) {
                ("*", "*") => format!("Daily at {at}"),
                ("*", dow) => match parse_dow_list(dow) {
                    Some(days) if days == [1, 2, 3, 4, 5] => format!("Weekdays at {at}"),
                    Some(days) if days == [0, 6] => format!("Weekends at {at}"),
                    Some(days) if days.len() == 7 => format!("Daily at {at}"),
                    Some(days) if days.len() == 1 => format!("Weekly on {} at {at}", DAY_NAMES[days[0] as usize]),
                    Some(days) => {
                        let names: Vec<&str> = days.iter().map(|d| DAY_NAMES[*d as usize]).collect();
                        format!("Every {} at {at}", join_and(&names))
                    }
                    None => fallback(),
                },
                (dom, "*") => {
                    if let Some(d) = num(dom) {
                        format!("Monthly on the {} at {at}", ordinal(d))
                    } else if let Some(n) = step(dom) {
                        format!("{} at {at}", plural(n, "day"))
                    } else if dom.eq_ignore_ascii_case("L") {
                        format!("Monthly on the last day at {at}")
                    } else {
                        fallback()
                    }
                }
                _ => fallback(),
            }
        }
        _ => {
            // Yearly: "M H D Mo *"
            if let (Some(m), Some(h), Some(d), Some(mo), "*") = (num(min), num(hour), num(dom), num(month), dow) {
                if (1..=12).contains(&mo) {
                    return format!("Yearly on {} {d} at {h:02}:{m:02}", MONTH_NAMES[mo as usize - 1]);
                }
            }
            fallback()
        }
    }
}

const DAY_NAMES: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTH_NAMES: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

fn step(field: &str) -> Option<u32> {
    field.strip_prefix("*/").and_then(|n| n.parse().ok()).filter(|n| *n > 0)
}

fn plural(n: u32, unit: &str) -> String {
    if n == 1 {
        format!("Every {unit}")
    } else {
        format!("Every {n} {unit}s")
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn join_and(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => one.to_string(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Parse a day-of-week field made of numbers/names, lists and ranges.
fn parse_dow_list(field: &str) -> Option<Vec<u32>> {
    let mut days = Vec::new();
    for part in field.split(',') {
        let one = |s: &str| -> Option<u32> {
            if let Ok(n) = s.parse::<u32>() {
                return (n <= 7).then_some(n % 7);
            }
            weekday_number(&s.to_ascii_lowercase()).filter(|_| s.len() == 3)
        };
        if let Some((a, b)) = part.split_once('-') {
            let (a, b) = (one(a)?, one(b)?);
            // `5-7` style ranges that wrap through Sunday are written with 7.
            let b_raw = part.split_once('-').map(|(_, b)| b == "7").unwrap_or(false);
            let end = if b_raw { 7 } else { b };
            if a > end {
                return None;
            }
            for d in a..=end {
                days.push(d % 7);
            }
        } else {
            days.push(one(part)?);
        }
    }
    days.sort_unstable();
    days.dedup();
    Some(days)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, FixedOffset, NaiveDate};

    fn local(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).earliest().expect("valid local time")
    }

    fn cron_of(s: &str) -> String {
        parse_schedule(s).unwrap_or_else(|e| panic!("{s}: {e}")).cron().to_string()
    }

    #[test]
    fn cron_expressions_pass_through() {
        assert_eq!(cron_of("*/15 * * * *"), "*/15 * * * *");
        assert_eq!(cron_of("  0   9 * * 1-5 "), "0 9 * * 1-5");
        assert_eq!(cron_of("30 0 9 * * MON-FRI"), "30 0 9 * * MON-FRI");
        assert_eq!(cron_of("0 0 L * *"), "0 0 L * *");
        assert!(parse_schedule("61 * * * *").is_err());
        assert!(parse_schedule("* * *").is_err());
        assert!(parse_schedule("").is_err());
        assert!(parse_schedule("0 0 30 2 *").is_err(), "Feb 30 never runs");
    }

    #[test]
    fn shorthands_and_friendly_phrases() {
        let cases = [
            ("@hourly", "0 * * * *"),
            ("@daily", "0 0 * * *"),
            ("@weekly", "0 0 * * 0"),
            ("@monthly", "0 0 1 * *"),
            ("hourly", "0 * * * *"),
            ("Hourly at :30", "30 * * * *"),
            ("every 15m", "*/15 * * * *"),
            ("every 15 minutes", "*/15 * * * *"),
            ("every minute", "* * * * *"),
            ("every 2h", "0 */2 * * *"),
            ("every 2 hours", "0 */2 * * *"),
            ("every hour", "0 * * * *"),
            ("every 120 min", "0 */2 * * *"),
            ("every 30s", "*/30 * * * * *"),
            ("every day at 09:00", "0 9 * * *"),
            ("Every day at 9:30pm", "30 21 * * *"),
            ("daily 09:00", "0 9 * * *"),
            ("daily at noon", "0 12 * * *"),
            ("weekdays 09:00", "0 9 * * 1-5"),
            ("every weekday at 8 am", "0 8 * * 1-5"),
            ("weekends 10:15", "15 10 * * 0,6"),
            ("weekly mon 09:00", "0 9 * * 1"),
            ("weekly on Friday at 17:30", "30 17 * * 5"),
            ("every monday at 9am", "0 9 * * 1"),
            ("mon, wed and fri at 07:05", "5 7 * * 1,3,5"),
            ("mon-fri 18:00", "0 18 * * 1-5"),
            ("mondays", "0 0 * * 1"),
            ("monthly", "0 0 1 * *"),
            ("monthly 15 09:00", "0 9 15 * *"),
            ("monthly on the 1st at 8am", "0 8 1 * *"),
            ("every 3 days", "0 0 */3 * *"),
        ];
        for (input, expected) in cases {
            assert_eq!(cron_of(input), expected, "input: {input}");
        }
        // 90 minutes is not representable as one cron step; it must be rejected.
        assert!(parse_schedule("every 90m").is_err());
        assert!(parse_schedule("every 7 parsecs").is_err());
        assert!(parse_schedule("daily 25:00").is_err());
        assert!(parse_schedule("weekly mon 9:7").is_err());
        assert!(parse_schedule("every 0m").is_err());
        assert!(parse_schedule("whenever").is_err());
        assert!(parse_schedule("@fortnightly").is_err());
    }

    #[test]
    fn describes_common_shapes() {
        let cases = [
            ("*/15 * * * *", "Every 15 minutes"),
            ("* * * * *", "Every minute"),
            ("@hourly", "Every hour"),
            ("15 * * * *", "Every hour at :15"),
            ("every 2h", "Every 2 hours"),
            ("weekdays 09:00", "Weekdays at 09:00"),
            ("daily 7:05", "Daily at 07:05"),
            ("weekends 10:00", "Weekends at 10:00"),
            ("weekly mon 09:00", "Weekly on Monday at 09:00"),
            ("mon wed fri 9am", "Every Monday, Wednesday and Friday at 09:00"),
            ("0 9 * * MON-FRI", "Weekdays at 09:00"),
            ("@monthly", "Monthly on the 1st at 00:00"),
            ("0 8 22 * *", "Monthly on the 22nd at 08:00"),
            ("@yearly", "Yearly on Jan 1 at 00:00"),
            ("every 30s", "Every 30 seconds"),
            ("0 30 9 * * *", "Daily at 09:30"),
            ("5,10 * * * *", "Cron schedule `5,10 * * * *`"),
        ];
        for (input, expected) in cases {
            assert_eq!(parse_schedule(input).unwrap().describe(), expected, "input: {input}");
        }
    }

    #[test]
    fn next_times_from_fixed_now() {
        // Thursday 2026-01-15 08:07 local.
        let now = local(2026, 1, 15, 8, 7);
        let s = parse_schedule("every 15m").unwrap();
        assert_eq!(s.next_after(now).unwrap(), local(2026, 1, 15, 8, 15));

        let s = parse_schedule("weekdays 09:00").unwrap();
        let runs = s.upcoming(now, 3);
        assert_eq!(runs, vec![local(2026, 1, 15, 9, 0), local(2026, 1, 16, 9, 0), local(2026, 1, 19, 9, 0)]);

        let s = parse_schedule("weekly mon 09:00").unwrap();
        assert_eq!(s.next_after(now).unwrap(), local(2026, 1, 19, 9, 0));

        // Exactly on a boundary -> strictly after.
        let s = parse_schedule("daily 09:00").unwrap();
        assert_eq!(s.next_after(local(2026, 1, 15, 9, 0)).unwrap(), local(2026, 1, 16, 9, 0));

        // Sub-second precision does not cause a duplicate fire.
        let almost = local(2026, 1, 15, 9, 0) + Duration::milliseconds(250);
        assert_eq!(s.next_after(almost).unwrap(), local(2026, 1, 16, 9, 0));

        let s = parse_schedule("monthly 31 12:00").unwrap();
        assert_eq!(s.next_after(local(2026, 2, 1, 0, 0)).unwrap(), local(2026, 3, 31, 12, 0));
    }

    #[test]
    fn fixed_offset_zones_work() {
        let tz = FixedOffset::east_opt(5 * 3600 + 1800).unwrap();
        let now =
            tz.from_local_datetime(&NaiveDate::from_ymd_opt(2026, 6, 1).unwrap().and_hms_opt(23, 59, 30).unwrap());
        let s = parse_schedule("@daily").unwrap();
        let next = s.next_after_in(now.single().unwrap()).unwrap();
        assert_eq!(next.naive_local(), NaiveDate::from_ymd_opt(2026, 6, 2).unwrap().and_hms_opt(0, 0, 0).unwrap());
    }

    #[test]
    fn iteration_is_monotonic_across_a_year() {
        // Walks through whatever DST transitions the local zone has.
        for expr in ["*/30 * * * *", "30 2 * * *", "0 * * * *"] {
            let s = parse_schedule(expr).unwrap();
            let mut t = local(2026, 1, 1, 0, 0);
            let end = local(2027, 1, 1, 0, 0);
            let mut count = 0;
            while t < end {
                let next = s.next_after(t).unwrap_or_else(|| panic!("{expr}: no next run after {t}"));
                assert!(next > t, "{expr}: {next} <= {t}");
                assert!(next - t <= Duration::hours(26), "{expr}: gap too large after {t}");
                t = next;
                count += 1;
            }
            assert!(count > 300);
        }
    }

    #[test]
    fn validate_reports_next_five_runs() {
        let now = local(2026, 1, 15, 8, 7);
        let ok = validate("every 15m", now);
        assert!(ok.valid);
        assert_eq!(ok.description.as_deref(), Some("Every 15 minutes"));
        assert_eq!(ok.next_runs.len(), 5);
        assert_eq!(ok.next_runs[0], local(2026, 1, 15, 8, 15).timestamp_millis());
        assert_eq!(ok.next_runs[1] - ok.next_runs[0], 15 * 60 * 1000);

        let bad = validate("every blue moon", now);
        assert!(!bad.valid);
        assert!(bad.error.unwrap().contains("unrecognized"));
        assert!(bad.next_runs.is_empty());
    }
}
