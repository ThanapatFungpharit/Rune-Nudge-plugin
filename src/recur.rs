//! Repeating reminders.
//!
//! Calendar rules (days, weeks, months, years) are kept in wall-clock time
//! in the user's zone: "every day at 09:00" stays at 09:00 across
//! daylight-saving changes. Minute/hour intervals are exact durations.
//!
//! The next occurrence is always computed *after a given instant*, so
//! occurrences missed while rune was closed are skipped over, never
//! replayed one by one.

use chrono::{Datelike, NaiveDate, NaiveTime, TimeDelta, Weekday};
use serde::{Deserialize, Serialize};

use crate::fmt::{fmt_clock, weekday_short};
use crate::zone::Zone;

/// Weekdays as a bit set, Monday = bit 0.
pub type Days = u8;
pub const WEEKDAYS: Days = 0b0001_1111;
pub const WEEKEND: Days = 0b0110_0000;

pub fn day_bit(w: Weekday) -> Days {
    1 << w.num_days_from_monday()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "unit", rename_all = "snake_case")]
pub enum Rule {
    /// Every `every` minutes, measured from the previous occurrence.
    Minutes,
    Days,
    Weeks { days: Days },
    /// On `day` (clamped to the month's last day: the 31st is the 30th in April).
    Months { day: u8 },
    /// On `month`/`day` (29 February is the 28th in other years).
    Years { month: u8, day: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recur {
    #[serde(flatten)]
    pub rule: Rule,
    /// Every this many units (1 = every unit).
    pub every: u32,
    /// Local time of day, minutes after midnight (calendar rules).
    #[serde(default)]
    pub at: u16,
    /// First local date the rule counts from (calendar rules), `YYYY-MM-DD`.
    #[serde(default)]
    pub start: String,
}

/// The largest `every` per unit (keeps every search short).
pub fn max_every(rule: &Rule) -> u32 {
    match rule {
        Rule::Minutes => 60 * 24 * 365,
        Rule::Days => 3650,
        Rule::Weeks { .. } => 520,
        Rule::Months { .. } => 120,
        Rule::Years { .. } => 10,
    }
}

fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    NaiveDate::from_ymd_opt(ny, nm, 1)
        .and_then(|d| d.pred_opt())
        .map_or(28, |d| d.day())
}

fn monday(d: NaiveDate) -> NaiveDate {
    d - TimeDelta::days(d.weekday().num_days_from_monday().into())
}

impl Recur {
    pub fn start_date(&self) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(&self.start, "%Y-%m-%d").ok()
    }

    pub fn time(&self) -> NaiveTime {
        NaiveTime::from_hms_opt(u32::from(self.at / 60), u32::from(self.at % 60), 0).unwrap_or_default()
    }

    /// Whether the stored rule makes sense (stored data is checked on load).
    pub fn validate(&self) -> Result<(), String> {
        if self.every == 0 || self.every > max_every(&self.rule) {
            return Err(format!("bad interval {}", self.every));
        }
        if self.rule == Rule::Minutes {
            return Ok(());
        }
        if self.at >= 24 * 60 {
            return Err("bad time of day".into());
        }
        if self.start_date().is_none() {
            return Err("bad start date".into());
        }
        match self.rule {
            Rule::Weeks { days } if days == 0 || days > 0x7f => Err("no weekdays".into()),
            Rule::Months { day } if !(1..=31).contains(&day) => Err("bad day of month".into()),
            Rule::Years { month, day }
                if !(1..=12).contains(&month)
                    || day == 0
                    || u32::from(day) > days_in_month(2000, month.into()) =>
            {
                Err("bad date".into())
            }
            _ => Ok(()),
        }
    }

    /// Whether the rule has an occurrence on local date `d` (the plain
    /// definition; tests check [`Recur::next_after`] against it).
    #[cfg(test)]
    fn on(&self, d: NaiveDate, start: NaiveDate) -> bool {
        if d < start {
            return false;
        }
        let every = i64::from(self.every);
        match self.rule {
            Rule::Minutes => false,
            Rule::Days => (d - start).num_days() % every == 0,
            Rule::Weeks { days } => {
                days & day_bit(d.weekday()) != 0
                    && ((monday(d) - monday(start)).num_days() / 7) % every == 0
            }
            Rule::Months { day } => {
                let months = (d.year() - start.year()) as i64 * 12 + i64::from(d.month())
                    - i64::from(start.month());
                months % every == 0 && d.day() == u32::from(day).min(days_in_month(d.year(), d.month()))
            }
            Rule::Years { month, day } => {
                i64::from(d.year() - start.year()) % every == 0
                    && d.month() == u32::from(month)
                    && d.day() == u32::from(day).min(days_in_month(d.year(), d.month()))
            }
        }
    }

    /// The first occurrence strictly after `after`. `prev` is the previous
    /// occurrence (intervals count from it). `None` only for a broken rule.
    ///
    /// Calendar rules jump straight to the first aligned day, week, month or
    /// year (a handful of steps, whatever the gap): plugins run on a fuel
    /// budget, and rune may have been closed for years.
    pub fn next_after(&self, zone: &Zone, prev: i64, after: i64) -> Option<i64> {
        if self.rule == Rule::Minutes {
            let step = i64::from(self.every) * 60_000;
            if prev > after {
                return Some(prev);
            }
            let k = (after - prev) / step + 1;
            return Some(prev + k * step);
        }
        let start = self.start_date()?;
        let time = self.time();
        // A day early: around offset changes, a local date can lag.
        let from = (zone.local(after).date() - TimeDelta::days(1)).max(start);
        let ok = |d: NaiveDate| {
            (d >= from)
                .then(|| zone.instant(d.and_time(time)))
                .filter(|t| *t > after)
        };
        let n = i64::from(self.every);
        // The first multiple of `n` at or above `x` (x ≥ 0).
        let align = |x: i64| x.div_euclid(n) * n + if x.rem_euclid(n) == 0 { 0 } else { n };
        match self.rule {
            Rule::Minutes => None,
            Rule::Days => {
                let mut k = align((from - start).num_days());
                for _ in 0..4 {
                    if let Some(t) = ok(start + TimeDelta::days(k)) {
                        return Some(t);
                    }
                    k += n;
                }
                None
            }
            Rule::Weeks { days } => {
                let w0 = monday(start);
                let mut k = align((monday(from) - w0).num_days() / 7);
                for _ in 0..4 {
                    let week = w0 + TimeDelta::weeks(k);
                    for i in 0..7 {
                        let d = week + TimeDelta::days(i);
                        if d >= start
                            && days & day_bit(d.weekday()) != 0
                            && let Some(t) = ok(d)
                        {
                            return Some(t);
                        }
                    }
                    k += n;
                }
                None
            }
            Rule::Months { day } => {
                let index = |d: NaiveDate| i64::from(d.year()) * 12 + i64::from(d.month0());
                let s = index(start);
                let mut k = align(index(from) - s);
                for _ in 0..4 {
                    let m = s + k;
                    let (y, mo) = (m.div_euclid(12) as i32, (m.rem_euclid(12) + 1) as u32);
                    let d = NaiveDate::from_ymd_opt(y, mo, u32::from(day).min(days_in_month(y, mo)))?;
                    if let Some(t) = ok(d) {
                        return Some(t);
                    }
                    k += n;
                }
                None
            }
            Rule::Years { month, day } => {
                let mut k = align(i64::from(from.year() - start.year()));
                for _ in 0..4 {
                    let y = start.year() + k as i32;
                    let mo = u32::from(month);
                    let d = NaiveDate::from_ymd_opt(y, mo, u32::from(day).min(days_in_month(y, mo)))?;
                    if let Some(t) = ok(d) {
                        return Some(t);
                    }
                    k += n;
                }
                None
            }
        }
    }

    /// How many occurrences fall in `(from, to]` (capped), for "missed 3".
    pub fn count_between(&self, zone: &Zone, from: i64, to: i64, cap: u32) -> u32 {
        if self.rule == Rule::Minutes {
            let step = i64::from(self.every) * 60_000;
            return ((to - from).max(0) / step).min(i64::from(cap)) as u32;
        }
        let mut n = 0;
        let mut t = from;
        while n < cap {
            match self.next_after(zone, t, t) {
                Some(next) if next <= to => {
                    n += 1;
                    t = next;
                }
                _ => break,
            }
        }
        n
    }

    /// "every weekday at 09:00", "every 2 weeks on Mon, Fri at 17:30", …
    /// The words are ones the parser reads back (used to pre-fill forms).
    pub fn describe(&self, h12: bool) -> String {
        let at = format!("at {}", fmt_clock(self.time(), h12));
        let n = self.every;
        let unit = |one: &str, many: &str| {
            if n == 1 {
                format!("every {one}")
            } else {
                format!("every {n} {many}")
            }
        };
        match self.rule {
            Rule::Minutes if n.is_multiple_of(24 * 60) => {
                let d = n / (24 * 60);
                if d == 1 { "every 24 hours".into() } else { format!("every {} hours", d * 24) }
            }
            Rule::Minutes if n.is_multiple_of(60) => {
                let h = n / 60;
                if h == 1 { "every hour".into() } else { format!("every {h} hours") }
            }
            Rule::Minutes => {
                if n == 1 { "every minute".into() } else { format!("every {n} minutes") }
            }
            Rule::Days => format!("{} {at}", unit("day", "days")),
            Rule::Weeks { days } => {
                let names = days_label(days);
                if n == 1 {
                    format!("every {names} {at}")
                } else {
                    format!("every {n} weeks on {names} {at}")
                }
            }
            Rule::Months { day } => format!("{} on the {} {at}", unit("month", "months"), ordinal(day.into())),
            Rule::Years { month, day } => {
                let m = chrono::Month::try_from(month).map_or("?", |m| &m.name()[..3]);
                format!("{} on {m} {day} {at}", unit("year", "years"))
            }
        }
    }
}

/// `weekday`, `weekend`, `mon`, `mon,wed,fri`.
pub fn days_label(days: Days) -> String {
    match days {
        WEEKDAYS => "weekday".into(),
        WEEKEND => "weekend".into(),
        0x7f => "day".into(),
        _ => (0..7u8)
            .filter(|i| days & (1 << i) != 0)
            .map(|i| weekday_short(Weekday::try_from(i).unwrap_or(Weekday::Mon)).to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(","),
    }
}

pub fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (1, x) if x != 11 => "st",
        (2, x) if x != 12 => "nd",
        (3, x) if x != 13 => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn z(name: &str) -> Zone {
        Zone::parse(name).unwrap()
    }

    fn at(zone: &Zone, s: &str) -> i64 {
        zone.instant(NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap())
    }

    fn local(zone: &Zone, ms: i64) -> String {
        zone.local(ms).format("%Y-%m-%d %H:%M %a").to_string()
    }

    fn rule(rule: Rule, every: u32, hm: (u16, u16), start: &str) -> Recur {
        Recur {
            rule,
            every,
            at: hm.0 * 60 + hm.1,
            start: start.into(),
        }
    }

    #[test]
    fn daily_keeps_wall_clock_time_across_dst() {
        let zone = z("Europe/Berlin");
        let r = rule(Rule::Days, 1, (9, 0), "2026-03-27");
        let mut t = at(&zone, "2026-03-27 08:00");
        let mut seen = vec![];
        for _ in 0..4 {
            t = r.next_after(&zone, t, t).unwrap();
            seen.push(local(&zone, t));
        }
        assert_eq!(
            seen,
            [
                "2026-03-27 09:00 Fri",
                "2026-03-28 09:00 Sat",
                "2026-03-29 09:00 Sun",
                "2026-03-30 09:00 Mon"
            ]
        );
        // …while the gap in UTC between the 28th and 29th is 23 hours.
        let a = at(&zone, "2026-03-28 09:00");
        let b = at(&zone, "2026-03-29 09:00");
        assert_eq!(b - a, 23 * 3_600_000);
    }

    #[test]
    fn a_time_in_the_dst_gap_fires_once_shifted_and_the_overlap_once() {
        let zone = z("Europe/Berlin");
        let r = rule(Rule::Days, 1, (2, 30), "2026-03-01");
        let t = r.next_after(&zone, 0, at(&zone, "2026-03-28 12:00")).unwrap();
        assert_eq!(local(&zone, t), "2026-03-29 03:30 Sun");
        let t2 = r.next_after(&zone, t, t).unwrap();
        assert_eq!(local(&zone, t2), "2026-03-30 02:30 Mon");
        // Fall back: 02:30 happens twice on 2026-10-25; one occurrence.
        let t = r.next_after(&zone, 0, at(&zone, "2026-10-24 12:00")).unwrap();
        assert_eq!(local(&zone, t), "2026-10-25 02:30 Sun");
        let next = r.next_after(&zone, t, t).unwrap();
        assert_eq!(local(&zone, next), "2026-10-26 02:30 Mon");
    }

    #[test]
    fn weekdays_and_every_other_week() {
        let zone = z("UTC");
        let r = rule(Rule::Weeks { days: WEEKDAYS }, 1, (9, 0), "2026-11-13");
        // Friday 10:00 → Monday 09:00.
        let t = r.next_after(&zone, 0, at(&zone, "2026-11-13 10:00")).unwrap();
        assert_eq!(local(&zone, t), "2026-11-16 09:00 Mon");
        let r = rule(Rule::Weeks { days: day_bit(Weekday::Fri) }, 2, (17, 0), "2026-11-16");
        let a = r.next_after(&zone, 0, at(&zone, "2026-11-16 00:00")).unwrap();
        let b = r.next_after(&zone, a, a).unwrap();
        assert_eq!(local(&zone, a), "2026-11-20 17:00 Fri");
        assert_eq!(local(&zone, b), "2026-12-04 17:00 Fri");
    }

    #[test]
    fn monthly_clamps_to_short_months_and_yearly_leap_days() {
        let zone = z("UTC");
        let r = rule(Rule::Months { day: 31 }, 1, (8, 0), "2026-01-31");
        let mut t = at(&zone, "2026-01-31 09:00");
        let mut seen = vec![];
        for _ in 0..3 {
            t = r.next_after(&zone, t, t).unwrap();
            seen.push(local(&zone, t));
        }
        assert_eq!(seen, ["2026-02-28 08:00 Sat", "2026-03-31 08:00 Tue", "2026-04-30 08:00 Thu"]);
        let r = rule(Rule::Years { month: 2, day: 29 }, 1, (9, 0), "2028-02-29");
        let a = r.next_after(&zone, 0, at(&zone, "2028-03-01 00:00")).unwrap();
        assert_eq!(local(&zone, a), "2029-02-28 09:00 Wed");
        let r = rule(Rule::Months { day: 15 }, 3, (9, 0), "2026-01-15");
        let a = r.next_after(&zone, 0, at(&zone, "2026-01-16 00:00")).unwrap();
        assert_eq!(local(&zone, a), "2026-04-15 09:00 Wed");
    }

    #[test]
    fn intervals_skip_missed_occurrences() {
        let zone = z("UTC");
        let r = Recur {
            rule: Rule::Minutes,
            every: 30,
            at: 0,
            start: String::new(),
        };
        let prev = 0;
        assert_eq!(r.next_after(&zone, prev, 0), Some(30 * 60_000));
        // Rune was closed for 2h05m: the next one is at 2h30m, not 30m.
        assert_eq!(r.next_after(&zone, prev, 125 * 60_000), Some(150 * 60_000));
        assert_eq!(r.count_between(&zone, prev, 125 * 60_000, 100), 4);
    }

    /// The jumping search agrees with walking day by day.
    #[test]
    fn next_after_matches_a_day_by_day_scan() {
        let zones = [z("UTC"), z("Europe/Berlin"), z("America/New_York"), z("Asia/Bangkok")];
        let rules = [
            (Rule::Days, 1), (Rule::Days, 3), (Rule::Days, 10),
            (Rule::Weeks { days: WEEKDAYS }, 1), (Rule::Weeks { days: 0b100_0001 }, 2),
            (Rule::Weeks { days: day_bit(Weekday::Wed) }, 3),
            (Rule::Months { day: 31 }, 1), (Rule::Months { day: 15 }, 4), (Rule::Months { day: 29 }, 12),
            (Rule::Years { month: 2, day: 29 }, 1), (Rule::Years { month: 12, day: 31 }, 3),
        ];
        let mut seed: u64 = 7;
        let mut rnd = |m: u64| {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) % m
        };
        for zone in &zones {
            for (rule, every) in &rules {
                for _ in 0..12 {
                    let hm = ((rnd(24)) as u16, (rnd(60)) as u16);
                    let r = rule_at(rule.clone(), *every, hm, &format!("2025-{:02}-{:02}", rnd(12) + 1, rnd(28) + 1));
                    let start = r.start_date().unwrap();
                    let after = at(zone, "2025-01-01 00:00") + (rnd(4000) as i64) * 86_400_000 + (rnd(1440) as i64) * 60_000;
                    // Brute force.
                    let mut d = (zone.local(after).date() - TimeDelta::days(1)).max(start);
                    let want = loop {
                        if r.on(d, start) {
                            let t = zone.instant(d.and_time(r.time()));
                            if t > after {
                                break t;
                            }
                        }
                        d = d.succ_opt().unwrap();
                    };
                    assert_eq!(r.next_after(zone, 0, after), Some(want), "{r:?} after {}", local(zone, after));
                }
            }
        }
    }

    fn rule_at(rule: Rule, every: u32, hm: (u16, u16), start: &str) -> Recur {
        Recur { rule, every, at: hm.0 * 60 + hm.1, start: start.into() }
    }

    #[test]
    fn descriptions() {
        let d = |r: Recur| r.describe(false);
        assert_eq!(d(rule(Rule::Days, 1, (9, 0), "2026-01-01")), "every day at 09:00");
        assert_eq!(
            d(rule(Rule::Weeks { days: WEEKDAYS }, 1, (9, 5), "2026-01-01")),
            "every weekday at 09:05"
        );
        assert_eq!(
            d(rule(Rule::Weeks { days: 0b10001 }, 2, (17, 0), "2026-01-01")),
            "every 2 weeks on mon,fri at 17:00"
        );
        assert_eq!(
            d(rule(Rule::Months { day: 1 }, 1, (9, 0), "2026-01-01")),
            "every month on the 1st at 09:00"
        );
        assert_eq!(
            rule(Rule::Years { month: 3, day: 2 }, 1, (13, 0), "2026-01-01").describe(true),
            "every year on Mar 2 at 1:00 pm"
        );
        let m = |n| Recur { rule: Rule::Minutes, every: n, at: 0, start: String::new() }.describe(false);
        assert_eq!(m(90), "every 90 minutes");
        assert_eq!(m(120), "every 2 hours");
        assert_eq!(m(60), "every hour");
    }

    #[test]
    fn validation() {
        assert!(rule(Rule::Days, 1, (9, 0), "2026-01-01").validate().is_ok());
        assert!(rule(Rule::Days, 0, (9, 0), "2026-01-01").validate().is_err());
        assert!(rule(Rule::Days, 1, (24, 0), "2026-01-01").validate().is_err());
        assert!(rule(Rule::Days, 1, (9, 0), "nope").validate().is_err());
        assert!(rule(Rule::Weeks { days: 0 }, 1, (9, 0), "2026-01-01").validate().is_err());
        assert!(rule(Rule::Months { day: 32 }, 1, (9, 0), "2026-01-01").validate().is_err());
        assert!(rule(Rule::Years { month: 2, day: 30 }, 1, (9, 0), "2026-01-01").validate().is_err());
        assert!(rule(Rule::Years { month: 2, day: 29 }, 1, (9, 0), "2026-01-01").validate().is_ok());
    }
}
