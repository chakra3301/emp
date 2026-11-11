//! Five-field cron expressions (minute hour day-of-month month day-of-week) in the Mac's local
//! time, as crontab reads them: names (`mon`, `jan`), ranges, steps and lists, `7` for Sunday,
//! and the `@daily`-style shorthands. When both day fields are restricted, either may match.
//! A time that a daylight-saving jump skips doesn't run that day; one that happens twice runs
//! the first time.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    months: Vec<u32>,
    /// 0 = Sunday.
    weekdays: Vec<u32>,
    any_day: bool,
    any_weekday: bool,
}

const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
/// How far ahead `next_after` looks before deciding the expression never matches (Feb 30th).
const SEARCH_DAYS: i64 = 366 * 5;

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let expanded = match expr.trim().to_lowercase().as_str() {
            "@hourly" => "0 * * * *".to_string(),
            "@daily" | "@midnight" => "0 0 * * *".to_string(),
            "@weekly" => "0 0 * * 0".to_string(),
            "@monthly" => "0 0 1 * *".to_string(),
            "@yearly" | "@annually" => "0 0 1 1 *".to_string(),
            other => other.to_string(),
        };
        let fields: Vec<&str> = expanded.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "a cron expression has 5 fields (minute hour day month weekday); \"{}\" has {}",
                expr.trim(),
                fields.len()
            ));
        }
        let mut weekdays = field(fields[4], 0, 7, &DAYS, "weekday")?;
        for d in &mut weekdays {
            *d %= 7;
        }
        weekdays.sort_unstable();
        weekdays.dedup();
        Ok(Cron {
            minutes: field(fields[0], 0, 59, &[], "minute")?,
            hours: field(fields[1], 0, 23, &[], "hour")?,
            days: field(fields[2], 1, 31, &[], "day")?,
            months: field(fields[3], 1, 12, &MONTHS, "month")?,
            weekdays,
            any_day: fields[2] == "*" || fields[2] == "?",
            any_weekday: fields[4] == "*" || fields[4] == "?",
        })
    }

    fn day_matches(&self, d: NaiveDate) -> bool {
        if !self.months.contains(&d.month()) {
            return false;
        }
        let dom = self.days.contains(&d.day());
        let dow = self.weekdays.contains(&d.weekday().num_days_from_sunday());
        match (self.any_day, self.any_weekday) {
            (true, true) => true,
            (true, false) => dow,
            (false, true) => dom,
            (false, false) => dom || dow,
        }
    }

    /// The first matching minute strictly after `t`.
    pub fn next_after(&self, t: DateTime<Local>) -> Option<DateTime<Local>> {
        let start = t.naive_local();
        let start_date = start.date();
        for offset in 0..SEARCH_DAYS {
            let date = start_date + Duration::days(offset);
            if !self.day_matches(date) {
                continue;
            }
            for &h in &self.hours {
                for &m in &self.minutes {
                    let Some(naive) = date.and_hms_opt(h, m, 0) else { continue };
                    if offset == 0 && naive <= start {
                        continue;
                    }
                    if let Some(local) = Local.from_local_datetime(&naive).earliest()
                        && local > t
                    {
                        return Some(local);
                    }
                }
            }
        }
        None
    }

    /// The timetable in words, for the common shapes; `None` for the rest.
    pub fn describe(&self) -> Option<String> {
        let all = |v: &[u32], lo: u32, hi: u32| v.len() as u32 == hi - lo + 1;
        let every_month = all(&self.months, 1, 12);
        if !every_month {
            return None;
        }
        // Sub-daily: every minute, every N minutes, hourly, every N hours.
        if self.any_day && self.any_weekday {
            if all(&self.hours, 0, 23) {
                if all(&self.minutes, 0, 59) {
                    return Some("Every minute".into());
                }
                if let Some(step) = step_of(&self.minutes, 60) {
                    return Some(format!("Every {step} minutes"));
                }
                if let [m] = self.minutes[..] {
                    return Some(if m == 0 { "Every hour".into() } else { format!("Every hour at :{m:02}") });
                }
                return None;
            }
            if let (Some(step), [m]) = (step_of(&self.hours, 24), &self.minutes[..]) {
                return Some(if *m == 0 { format!("Every {step} hours") } else { format!("Every {step} hours at :{m:02}") });
            }
        }
        let times = self.times()?;
        let days = match (self.any_day, self.any_weekday) {
            (true, true) => "Daily".to_string(),
            (true, false) => match &self.weekdays[..] {
                [1, 2, 3, 4, 5] => "Weekdays".into(),
                [0, 6] => "Weekends".into(),
                [d] => format!("{}s", day_name(*d)),
                ds => ds.iter().map(|d| format!("{}s", day_name(*d))).collect::<Vec<_>>().join(", "),
            },
            (false, true) => match &self.days[..] {
                [d] => format!("Monthly on the {}", ordinal(*d)),
                _ => return None,
            },
            (false, false) => return None,
        };
        Some(format!("{days} at {times}"))
    }

    /// "9:00 AM", "9:00 AM and 5:30 PM"; `None` for more than a few.
    fn times(&self) -> Option<String> {
        if self.hours.len() * self.minutes.len() > 4 {
            return None;
        }
        let mut out = Vec::new();
        for &h in &self.hours {
            for &m in &self.minutes {
                out.push(clock(h, m));
            }
        }
        Some(match out.len() {
            1 => out.remove(0),
            _ => {
                let last = out.pop()?;
                format!("{} and {last}", out.join(", "))
            }
        })
    }
}

/// One cron field as the sorted values it allows.
fn field(raw: &str, lo: u32, hi: u32, names: &[&str], what: &str) -> Result<Vec<u32>, String> {
    let value = |s: &str| -> Result<u32, String> {
        if let Some(i) = names.iter().position(|n| *n == s) {
            // Month names count from 1, day names from 0.
            return Ok(if names.len() == 12 { i as u32 + 1 } else { i as u32 });
        }
        let n: u32 = s.parse().map_err(|_| format!("\"{s}\" isn't a valid {what}"))?;
        if n < lo || n > hi {
            return Err(format!("{what} {n} is out of range ({lo}–{hi})"));
        }
        Ok(n)
    };
    let mut out = Vec::new();
    for part in raw.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => {
                let step: u32 = s.parse().map_err(|_| format!("\"{s}\" isn't a valid step"))?;
                if step == 0 {
                    return Err("a step of 0 never runs".into());
                }
                (r, step)
            }
            None => (part, 1),
        };
        let (from, to) = match range {
            "*" | "?" => (lo, hi),
            r => match r.split_once('-') {
                Some((a, b)) => (value(a)?, value(b)?),
                // `5/15` means from 5 to the end, every 15.
                None if step > 1 => (value(r)?, hi),
                None => {
                    let v = value(r)?;
                    (v, v)
                }
            },
        };
        if from > to {
            return Err(format!("{what} range {range} runs backwards"));
        }
        out.extend((from..=to).step_by(step as usize));
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// `n` when `v` is 0, n, 2n… across the whole field and n divides it evenly.
fn step_of(v: &[u32], span: u32) -> Option<u32> {
    let step = *v.get(1)? - v[0];
    (v[0] == 0 && step > 1 && span.is_multiple_of(step) && v.len() as u32 == span / step && v.windows(2).all(|w| w[1] - w[0] == step))
        .then_some(step)
}

fn day_name(d: u32) -> &'static str {
    ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"][d as usize % 7]
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

/// "9:00 AM", "12:30 PM".
pub fn clock(h: u32, m: u32) -> String {
    let (h12, half) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{h12}:{m:02} {half}")
}

/// "Tue Oct 21 at 9:00 AM" (the year too when it isn't this one).
pub fn moment(t: DateTime<Local>) -> String {
    let date = if t.year() == Local::now().year() { t.format("%a %b %-d").to_string() } else { t.format("%a %b %-d %Y").to_string() };
    format!("{date} at {}", clock(t.hour(), t.minute()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().unwrap()
    }

    #[test]
    fn finds_the_next_run() {
        let weekdays = Cron::parse("0 9 * * 1-5").unwrap();
        // Friday 2026-10-23 10:00 → Monday 9:00.
        assert_eq!(weekdays.next_after(at(2026, 10, 23, 10, 0)), Some(at(2026, 10, 26, 9, 0)));
        // Exactly on time → the next one, not the same minute.
        assert_eq!(weekdays.next_after(at(2026, 10, 26, 9, 0)), Some(at(2026, 10, 27, 9, 0)));
        let quarter = Cron::parse("*/15 * * * *").unwrap();
        assert_eq!(quarter.next_after(at(2026, 10, 20, 23, 50)), Some(at(2026, 10, 21, 0, 0)));
        assert_eq!(Cron::parse("@monthly").unwrap().next_after(at(2026, 10, 20, 0, 0)), Some(at(2026, 11, 1, 0, 0)));
        // Day of month OR weekday when both are given.
        let either = Cron::parse("0 8 1 * mon").unwrap();
        assert_eq!(either.next_after(at(2026, 10, 20, 9, 0)), Some(at(2026, 10, 26, 8, 0)));
        assert_eq!(Cron::parse("0 0 30 2 *").unwrap().next_after(at(2026, 1, 1, 0, 0)), None);
        assert_eq!(Cron::parse("0 0 * * 7").unwrap(), Cron::parse("0 0 * * sun").unwrap());
    }

    #[test]
    fn rejects_nonsense() {
        assert!(Cron::parse("0 9 * *").unwrap_err().contains("5 fields"));
        assert!(Cron::parse("61 * * * *").unwrap_err().contains("out of range"));
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert!(Cron::parse("0 9 * * fri-mon").unwrap_err().contains("backwards"));
        assert!(Cron::parse("0 nine * * *").is_err());
    }

    #[test]
    fn says_it_in_words() {
        let d = |e: &str| Cron::parse(e).unwrap().describe();
        assert_eq!(d("0 9 * * 1-5").as_deref(), Some("Weekdays at 9:00 AM"));
        assert_eq!(d("30 17 * * *").as_deref(), Some("Daily at 5:30 PM"));
        assert_eq!(d("0 0 * * 6,0").as_deref(), Some("Weekends at 12:00 AM"));
        assert_eq!(d("0 9 * * mon").as_deref(), Some("Mondays at 9:00 AM"));
        assert_eq!(d("0 9 * * 1,4").as_deref(), Some("Mondays, Thursdays at 9:00 AM"));
        assert_eq!(d("0 9,17 * * *").as_deref(), Some("Daily at 9:00 AM and 5:00 PM"));
        assert_eq!(d("0 9 1 * *").as_deref(), Some("Monthly on the 1st at 9:00 AM"));
        assert_eq!(d("*/10 * * * *").as_deref(), Some("Every 10 minutes"));
        assert_eq!(d("0 * * * *").as_deref(), Some("Every hour"));
        assert_eq!(d("15 */6 * * *").as_deref(), Some("Every 6 hours at :15"));
        assert_eq!(d("0 9 * 1 *"), None);
    }
}
