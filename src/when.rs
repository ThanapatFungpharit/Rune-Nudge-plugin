//! Reading "when" from what the user typed.
//!
//! ```text
//! in 2h · 2h30m · 45 min · in 3 days · an hour · half an hour · in 2 weeks
//! 15:30 · 3pm · 3:30 pm · at 9 · noon · midnight · tonight · this evening
//! tomorrow · tomorrow 9am · fri 17:00 · next mon · next week · weekend
//! 2026-12-01 · 2026-12-01 09:30 · 2026-12-01T09:30 · dec 5 · 5 dec 2027 3pm
//! every day at 9 · daily 9am · every weekday 9:00 · every mon,wed 10am
//! every 2 weeks on fri at 17:00 · every month on the 1st · yearly
//! every 30 minutes · every 2 hours · hourly
//! ```
//!
//! [`split`] finds the time at the start of `/remind` arguments (the longest
//! run of words that reads as one), or failing that at the end, starting
//! with `in`, `at`, `on`, `every`, `today`, `tomorrow`, `tonight`, `next`,
//! a weekday, a month or a number ("Call mom tomorrow 6pm", "Gym fri 7am").
//! The rest is the reminder's text.
//!
//! Times are resolved in the user's zone: a clock time that already passed
//! today means tomorrow (and says so), a weekday means the next one, a date
//! without a year the next one; an explicit date in the past is refused.

use chrono::{Datelike, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Timelike, Weekday};

use crate::fmt::fmt_clock;
use crate::recur::{Days, Recur, Rule, WEEKDAYS, WEEKEND, day_bit, max_every};
use crate::zone::Zone;

/// What the parser needs to know.
#[derive(Debug, Clone, Copy)]
pub struct Cx {
    pub now: i64,
    pub zone: Zone,
    /// Hour used for a day named without a time ("tomorrow").
    pub morning: u32,
    pub h12: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Spec {
    Once {
        due: i64,
        /// A word on how it was read, e.g. "15:30 has passed today, so tomorrow".
        note: Option<String>,
    },
    Repeat {
        recur: Recur,
        first: i64,
    },
}

impl Spec {
    pub fn first(&self) -> i64 {
        match self {
            Spec::Once { due, .. } => *due,
            Spec::Repeat { first, .. } => *first,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Fail {
    /// Not a time at all.
    NoMatch,
    /// Meant as a time, but wrong (with why).
    Bad(String),
}

type R<T> = Result<T, Fail>;

fn bad<T>(msg: impl Into<String>) -> R<T> {
    Err(Fail::Bad(msg.into()))
}

/// The furthest ahead a reminder may be.
const MAX_AHEAD_MS: i64 = 20 * 366 * 86_400_000;
/// Words that may start a time at the end of a reminder's text (besides
/// weekdays, months and anything starting with a digit).
const SUFFIX_STARTS: &[&str] = &[
    "in", "at", "on", "every", "today", "tomorrow", "tonight", "next", "this",
];
/// The most words a time takes.
const MAX_WORDS: usize = 9;

/// Read a whole string as a time ("" is no match).
pub fn parse(input: &str, cx: &Cx) -> R<Spec> {
    let toks = tokenize(input);
    if toks.is_empty() {
        return Err(Fail::NoMatch);
    }
    let mut p = P { t: toks, i: 0, cx };
    let spec = if is_repeat_start(&p.t[0]) {
        p.repeat()?
    } else {
        p.once()?
    };
    if let Spec::Once { due, .. } = spec
        && due - cx.now > MAX_AHEAD_MS
    {
        return bad("that is too far ahead (20 years at most)");
    }
    Ok(spec)
}

/// Split `/remind` arguments into a time (if any) and the text.
/// `Err` when something was clearly meant as a time but is wrong.
pub fn split(input: &str, cx: &Cx) -> Result<(Option<Spec>, String), String> {
    let words: Vec<(usize, &str)> = word_spans(input);
    let n = words.len();
    let mut first_bad: Option<String> = None;
    // Longest prefix first.
    for k in (1..=n.min(MAX_WORDS)).rev() {
        let end = words[k - 1].0 + words[k - 1].1.len();
        match parse(&input[..end], cx) {
            Ok(spec) => return Ok((Some(spec), tidy_text(&input[end..], true))),
            Err(Fail::Bad(m)) if first_bad.is_none() => first_bad = Some(m),
            Err(_) => {}
        }
    }
    // Then the longest suffix that starts with a time word.
    for k in (1..=n.saturating_sub(1).min(MAX_WORDS)).rev() {
        let (start, w) = words[n - k];
        if !suffix_start(w) {
            continue;
        }
        match parse(&input[start..], cx) {
            Ok(spec) => return Ok((Some(spec), tidy_text(&input[..start], false))),
            Err(Fail::Bad(m)) if first_bad.is_none() => first_bad = Some(m),
            Err(_) => {}
        }
    }
    match first_bad {
        Some(m) => Err(m),
        None => Ok((None, input.trim().to_owned())),
    }
}

/// Whether a time at the end of the text may start with `w`.
fn suffix_start(w: &str) -> bool {
    let w = w.to_lowercase();
    SUFFIX_STARTS.contains(&w.as_str())
        || weekday(&w).is_some()
        || month(&w).is_some()
        || w.starts_with(|c: char| c.is_ascii_digit())
}

fn word_spans(s: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in s.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(st)) => {
                out.push((st, &s[st..i]));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(st) = start {
        out.push((st, &s[st..]));
    }
    out
}

/// The text around a time: drop a joining "to", ":", "-" or ",".
fn tidy_text(s: &str, leading: bool) -> String {
    let mut t = s.trim();
    if leading {
        for sep in [":", "-", "—", ","] {
            t = t.strip_prefix(sep).unwrap_or(t).trim_start();
        }
        if t.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("to ")) {
            t = t[3..].trim_start();
        }
    } else {
        t = t.trim_end_matches([',', '-', '—', ':']).trim_end();
    }
    t.to_owned()
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in s.split(|c: char| c.is_whitespace() || c == ',') {
        let w = raw.trim_end_matches(['.', ':', ';', '!']).to_lowercase();
        if w.is_empty() {
            continue;
        }
        // 2026-12-01T09:30 → date, time
        if w.len() > 11 && w.as_bytes()[10] == b't' && w.as_bytes()[0].is_ascii_digit() {
            out.push(w[..10].to_owned());
            out.push(w[11..].to_owned());
            continue;
        }
        out.push(w);
    }
    out
}

fn is_repeat_start(w: &str) -> bool {
    matches!(
        w,
        "every" | "each" | "daily" | "weekly" | "monthly" | "yearly" | "annually" | "hourly" | "weekdays"
    )
}

pub fn weekday(w: &str) -> Option<Weekday> {
    Some(match w {
        "mon" | "monday" | "mondays" => Weekday::Mon,
        "tue" | "tues" | "tuesday" | "tuesdays" => Weekday::Tue,
        "wed" | "weds" | "wednesday" | "wednesdays" => Weekday::Wed,
        "thu" | "thur" | "thurs" | "thursday" | "thursdays" => Weekday::Thu,
        "fri" | "friday" | "fridays" => Weekday::Fri,
        "sat" | "saturday" | "saturdays" => Weekday::Sat,
        "sun" | "sunday" | "sundays" => Weekday::Sun,
        _ => return None,
    })
}

fn month(w: &str) -> Option<u32> {
    let m = match w {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    };
    Some(m)
}

/// `5`, `5th`, `1st`, `22nd`, `3rd` → 5, 5, 1, 22, 3 (1–31).
fn day_of_month(w: &str) -> Option<u32> {
    let digits = w
        .strip_suffix("st")
        .or_else(|| w.strip_suffix("nd"))
        .or_else(|| w.strip_suffix("rd"))
        .or_else(|| w.strip_suffix("th"))
        .unwrap_or(w);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|d| (1..=31).contains(d))
}

fn year(w: &str) -> Option<i32> {
    (w.len() == 4 && w.bytes().all(|b| b.is_ascii_digit()))
        .then(|| w.parse().ok())
        .flatten()
        .filter(|y| (2000..=2200).contains(y))
}

fn number(w: &str) -> Option<u32> {
    (!w.is_empty() && w.len() <= 6 && w.bytes().all(|b| b.is_ascii_digit()))
        .then(|| w.parse().ok())
        .flatten()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Unit {
    Secs(i64),
    Days(i64),
    Months(u32),
}

fn unit(w: &str) -> Option<Unit> {
    Some(match w {
        "s" | "sec" | "secs" | "second" | "seconds" => Unit::Secs(1),
        "m" | "min" | "mins" | "minute" | "minutes" => Unit::Secs(60),
        "h" | "hr" | "hrs" | "hour" | "hours" => Unit::Secs(3600),
        "d" | "day" | "days" => Unit::Days(1),
        "w" | "wk" | "wks" | "week" | "weeks" => Unit::Days(7),
        "mo" | "mos" | "month" | "months" => Unit::Months(1),
        "y" | "yr" | "yrs" | "year" | "years" => Unit::Months(12),
        _ => return None,
    })
}

/// `1h30m`, `90m`, `2d` → [(1, h), (30, m)] …
fn compound(w: &str) -> Option<Vec<(u32, Unit)>> {
    let mut out = Vec::new();
    let mut rest = w;
    while !rest.is_empty() {
        let nd = rest.find(|c: char| !c.is_ascii_digit())?;
        if nd == 0 {
            return None;
        }
        let n = number(&rest[..nd])?;
        let after = &rest[nd..];
        let nu = after.find(|c: char| c.is_ascii_digit()).unwrap_or(after.len());
        out.push((n, unit(&after[..nu])?));
        rest = &after[nu..];
    }
    (!out.is_empty()).then_some(out)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DateSpec {
    Today,
    Tomorrow,
    Tonight,
    Weekday { day: Weekday, next: bool },
    NextWeek,
    NextMonth,
    Weekend,
    Date { date: NaiveDate, year_given: bool },
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Clock {
    time: NaiveTime,
    /// am/pm was written (or the time is unambiguous, like 17:00).
    explicit: bool,
}

struct P<'a> {
    t: Vec<String>,
    i: usize,
    cx: &'a Cx,
}

impl P<'_> {
    fn peek(&self) -> Option<&str> {
        self.t.get(self.i).map(String::as_str)
    }
    fn peek_at(&self, k: usize) -> Option<&str> {
        self.t.get(self.i + k).map(String::as_str)
    }
    fn eat(&mut self, w: &str) -> bool {
        if self.peek() == Some(w) {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn done(&self) -> bool {
        self.i >= self.t.len()
    }
    fn now_local(&self) -> NaiveDateTime {
        self.cx.zone.local(self.cx.now)
    }
    fn instant(&self, d: NaiveDate, t: NaiveTime) -> i64 {
        self.cx.zone.instant(d.and_time(t))
    }
    fn morning(&self) -> NaiveTime {
        NaiveTime::from_hms_opt(self.cx.morning.min(23), 0, 0).unwrap_or_default()
    }

    // -- one-time ----------------------------------------------------------------

    fn once(&mut self) -> R<Spec> {
        let save = self.i;
        match self.duration() {
            Ok(Some(spec)) if self.done() => return Ok(spec),
            Err(Fail::Bad(m)) => return bad(m),
            _ => {}
        }
        self.i = save;
        self.date_time()
    }

    /// `in 2h`, `2h30m`, `in 2 hours and 30 minutes`, `an hour`, `half an hour`.
    fn duration(&mut self) -> R<Option<Spec>> {
        let had_in = self.eat("in");
        let (mut secs, mut days, mut months) = (0i64, 0i64, 0u32);
        let mut any = false;
        loop {
            if any && self.peek() == Some("and") {
                self.i += 1;
            }
            let Some(w) = self.peek().map(str::to_owned) else { break };
            let mut add = |n: u32, u: Unit| match u {
                Unit::Secs(s) => secs += i64::from(n) * s,
                Unit::Days(d) => days += i64::from(n) * d,
                Unit::Months(m) => months = months.saturating_add(n.saturating_mul(m)),
            };
            if let Some(parts) = compound(&w) {
                for (n, u) in parts {
                    add(n, u);
                }
                self.i += 1;
            } else if let (Some(n), Some(u)) = (number(&w), self.peek_at(1).and_then(unit)) {
                add(n, u);
                self.i += 2;
            } else if (w == "a" || w == "an") && let Some(u) = self.peek_at(1).and_then(unit) {
                add(1, u);
                self.i += 2;
            } else if w == "half" {
                // half an hour / half hour
                let k = if self.peek_at(1) == Some("an") { 2 } else { 1 };
                if self.peek_at(k).and_then(unit) != Some(Unit::Secs(3600)) {
                    break;
                }
                secs += 1800;
                self.i += k + 1;
            } else {
                break;
            }
            any = true;
        }
        if !any {
            return if had_in { Err(Fail::NoMatch) } else { Ok(None) };
        }
        if secs == 0 && days == 0 && months == 0 {
            return bad("a time from now has to be more than zero");
        }
        if months > 12 * 20 || days > 366 * 20 || secs > 86_400 * 366 * 20 {
            return bad("that is too far ahead (20 years at most)");
        }
        let cx = self.cx;
        let mut local = cx.zone.local(cx.now);
        if months > 0 {
            local = local.checked_add_months(Months::new(months)).ok_or(Fail::NoMatch)?;
        }
        local += TimeDelta::days(days);
        let base = if months > 0 || days > 0 {
            cx.zone.instant(local)
        } else {
            cx.now
        };
        Ok(Some(Spec::Once {
            due: base + secs * 1000,
            note: None,
        }))
    }

    fn date_time(&mut self) -> R<Spec> {
        let mut date: Option<DateSpec> = None;
        let mut clock: Option<Clock> = None;
        while !self.done() {
            let on = self.eat("on");
            if date.is_none()
                && let Some(d) = self.date()?
            {
                date = Some(d);
                continue;
            }
            if on {
                return Err(Fail::NoMatch);
            }
            if clock.is_none() {
                let at = self.eat("at");
                // "this evening", "in the morning"
                if !at && (self.peek() == Some("this") || self.peek() == Some("in")) {
                    let k = if self.peek_at(1) == Some("the") { 2 } else { 1 };
                    if self.peek_at(k).is_some_and(|w| part_of_day(w).is_some()) {
                        self.i += k;
                    }
                }
                let allow_bare = at || date.is_some();
                if let Some(c) = self.clock(allow_bare)? {
                    clock = Some(c);
                    continue;
                }
                if at {
                    return Err(Fail::NoMatch);
                }
            }
            return Err(Fail::NoMatch);
        }
        self.resolve(date, clock)
    }

    fn date(&mut self) -> R<Option<DateSpec>> {
        let Some(w) = self.peek().map(str::to_owned) else {
            return Ok(None);
        };
        let one = |p: &mut Self, d| {
            p.i += 1;
            Ok(Some(d))
        };
        match w.as_str() {
            "today" => return one(self, DateSpec::Today),
            "tomorrow" | "tmr" | "tmrw" | "tomorow" => return one(self, DateSpec::Tomorrow),
            "tonight" | "tonite" => return one(self, DateSpec::Tonight),
            "yesterday" => return bad("yesterday is in the past"),
            "weekend" => return one(self, DateSpec::Weekend),
            "next" | "this" => {
                let next = w == "next";
                let target = self.peek_at(1).map(str::to_owned);
                match target.as_deref() {
                    Some("week") if next => {
                        self.i += 2;
                        return Ok(Some(DateSpec::NextWeek));
                    }
                    Some("month") if next => {
                        self.i += 2;
                        return Ok(Some(DateSpec::NextMonth));
                    }
                    Some("weekend") => {
                        self.i += 2;
                        return Ok(Some(DateSpec::Weekend));
                    }
                    Some(t) => {
                        if let Some(day) = weekday(t) {
                            self.i += 2;
                            return Ok(Some(DateSpec::Weekday { day, next }));
                        }
                    }
                    None => {}
                }
                return Ok(None);
            }
            _ => {}
        }
        if let Some(day) = weekday(&w) {
            return one(self, DateSpec::Weekday { day, next: false });
        }
        if let Some(d) = self.iso_date(&w)? {
            return one(self, d);
        }
        // dec 5 [2027] / 5 dec [2027] / 5th of december
        let (m, d, used) = if let Some(m) = month(&w) {
            match self.peek_at(1).and_then(day_of_month) {
                Some(d) => (m, d, 2),
                None => return Ok(None),
            }
        } else if let Some(d) = day_of_month(&w) {
            let k = if self.peek_at(1) == Some("of") { 2 } else { 1 };
            match self.peek_at(k).and_then(month) {
                Some(m) => (m, d, k + 1),
                None => return Ok(None),
            }
        } else {
            return Ok(None);
        };
        let y = self.peek_at(used).and_then(year);
        let this_year = self.now_local().year();
        let Some(date) = NaiveDate::from_ymd_opt(y.unwrap_or(this_year), m, d)
            .or_else(|| {
                // 29 Feb without a year: the next leap year.
                (y.is_none() && m == 2 && d == 29)
                    .then(|| (1..=4).find_map(|k| NaiveDate::from_ymd_opt(this_year + k, 2, 29)))
                    .flatten()
            })
        else {
            return bad(format!("{w} {} is not a date", self.peek_at(1).unwrap_or("")));
        };
        self.i += used + usize::from(y.is_some());
        Ok(Some(DateSpec::Date {
            date,
            year_given: y.is_some(),
        }))
    }

    fn iso_date(&self, w: &str) -> R<Option<DateSpec>> {
        let parts: Vec<&str> = w.split(['-', '/']).collect();
        if parts.len() != 3
            || parts[0].len() != 4
            || !parts.iter().all(|p| !p.is_empty() && p.len() <= 4 && p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Ok(None);
        }
        let (y, m, d) = (
            parts[0].parse::<i32>().unwrap_or(0),
            parts[1].parse::<u32>().unwrap_or(0),
            parts[2].parse::<u32>().unwrap_or(0),
        );
        match NaiveDate::from_ymd_opt(y, m, d).filter(|_| (2000..=2200).contains(&y)) {
            Some(date) => Ok(Some(DateSpec::Date {
                date,
                year_given: true,
            })),
            None => bad(format!("{w} is not a date")),
        }
    }

    /// `15:30`, `3pm`, `3:30 pm`, `9` (only when `allow_bare`), `noon`, …
    fn clock(&mut self, allow_bare: bool) -> R<Option<Clock>> {
        let Some(w) = self.peek().map(str::to_owned) else {
            return Ok(None);
        };
        if let Some(t) = part_of_day(&w) {
            self.i += 1;
            let time = if w == "morning" { self.morning() } else { t };
            return Ok(Some(Clock { time, explicit: true }));
        }
        let (body, mut ampm) = match w
            .strip_suffix("am")
            .or_else(|| w.strip_suffix("a"))
            .filter(|b| !b.is_empty())
        {
            Some(b) => (b.to_owned(), Some(false)),
            None => match w.strip_suffix("pm").or_else(|| w.strip_suffix("p")).filter(|b| !b.is_empty()) {
                Some(b) => (b.to_owned(), Some(true)),
                None => (w.clone(), None),
            },
        };
        let (h, m, colon) = match body.split_once(':') {
            Some((h, m)) => (h, m, true),
            None => (body.as_str(), "0", false),
        };
        let digits = |s: &str, max: usize| !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(h, 2) || !digits(m, 2) || (colon && m.len() != 2) {
            return Ok(None);
        }
        let mut used = 1;
        if ampm.is_none() {
            match self.peek_at(1) {
                Some("am" | "a.m" | "a") => (ampm, used) = (Some(false), 2),
                Some("pm" | "p.m" | "p") => (ampm, used) = (Some(true), 2),
                _ => {}
            }
        }
        if !colon && ampm.is_none() && !allow_bare {
            return Ok(None);
        }
        let (h, m): (u32, u32) = (h.parse().unwrap_or(99), m.parse().unwrap_or(99));
        let hour = match ampm {
            Some(pm) => {
                if !(1..=12).contains(&h) || m > 59 {
                    return bad(format!("{w} is not a time"));
                }
                (h % 12) + if pm { 12 } else { 0 }
            }
            None => {
                if h > 23 || m > 59 {
                    return bad(format!("{w} is not a time (use 00:00–23:59)"));
                }
                h
            }
        };
        self.i += used;
        Ok(Some(Clock {
            time: NaiveTime::from_hms_opt(hour, m, 0).unwrap_or_default(),
            explicit: ampm.is_some() || hour == 0 || hour > 12,
        }))
    }

    fn resolve(&self, date: Option<DateSpec>, clock: Option<Clock>) -> R<Spec> {
        let now = self.cx.now;
        let today = self.now_local().date();
        let h12 = self.cx.h12;
        let once = |due, note: Option<String>| Ok(Spec::Once { due, note });
        let Some(date) = date else {
            let Some(c) = clock else { return Err(Fail::NoMatch) };
            let t = self.instant(today, c.time);
            if t > now {
                return once(t, None);
            }
            return once(
                self.instant(today + TimeDelta::days(1), c.time),
                Some(format!("{} has passed today, so tomorrow", fmt_clock(c.time, h12))),
            );
        };
        let mut time = clock.map_or_else(
            || {
                if date == DateSpec::Tonight {
                    NaiveTime::from_hms_opt(20, 0, 0).unwrap_or_default()
                } else {
                    self.morning()
                }
            },
            |c| c.time,
        );
        // "tonight at 9" is 21:00.
        if date == DateSpec::Tonight
            && let Some(c) = clock
            && !c.explicit
            && c.time.hour() < 12
        {
            time += TimeDelta::hours(12);
        }
        let at = |d: NaiveDate| self.instant(d, time);
        let clock_s = fmt_clock(time, h12);
        let wd = today.weekday().num_days_from_monday() as i64;
        let due = match date {
            DateSpec::Today => {
                if clock.is_none() {
                    return bad("today at what time? e.g. today 5pm");
                }
                if at(today) <= now {
                    return bad(format!("{clock_s} today has already passed"));
                }
                at(today)
            }
            DateSpec::Tonight => {
                if at(today) <= now {
                    return bad(format!("it is already past {clock_s} tonight"));
                }
                at(today)
            }
            DateSpec::Tomorrow => at(today + TimeDelta::days(1)),
            DateSpec::Weekday { day, next } => {
                let mut delta = (day.num_days_from_monday() as i64 - wd).rem_euclid(7);
                if delta == 0 && (next || at(today) <= now) {
                    delta = 7;
                }
                at(today + TimeDelta::days(delta))
            }
            DateSpec::NextWeek => at(today + TimeDelta::days(7 - wd)),
            DateSpec::NextMonth => {
                let first = today.with_day(1).and_then(|d| d.checked_add_months(Months::new(1)));
                at(first.ok_or(Fail::NoMatch)?)
            }
            DateSpec::Weekend => {
                let mut delta = (5 - wd).rem_euclid(7);
                if delta == 0 && at(today) <= now {
                    delta = 7;
                }
                at(today + TimeDelta::days(delta))
            }
            DateSpec::Date { date, year_given } => {
                let t = at(date);
                if t > now {
                    t
                } else if !year_given {
                    let next = date
                        .checked_add_months(Months::new(12))
                        .ok_or(Fail::NoMatch)?;
                    at(next)
                } else {
                    return bad(format!("{} {clock_s} is in the past", date.format("%Y-%m-%d")));
                }
            }
        };
        once(due, None)
    }

    // -- repeating ------------------------------------------------------------------

    fn repeat(&mut self) -> R<Spec> {
        let today = self.now_local().date();
        let w = self.peek().unwrap_or_default().to_owned();
        self.i += 1;
        let mut rule: Option<(Rule, u32)> = match w.as_str() {
            "daily" => Some((Rule::Days, 1)),
            "weekly" => Some((Rule::Weeks { days: day_bit(today.weekday()) }, 1)),
            "monthly" => Some((Rule::Months { day: today.day() as u8 }, 1)),
            "yearly" | "annually" => Some((
                Rule::Years {
                    month: today.month() as u8,
                    day: today.day() as u8,
                },
                1,
            )),
            "hourly" => Some((Rule::Minutes, 60)),
            "weekdays" => Some((Rule::Weeks { days: WEEKDAYS }, 1)),
            _ => None, // every / each
        };
        if rule.is_none() {
            rule = Some(self.every_what(today)?);
        }
        let (mut rule, every) = rule.ok_or(Fail::NoMatch)?;
        if every == 0 {
            return bad("every 0? use a number of 1 or more");
        }
        if every > max_every(&rule) {
            return bad("that repeats too rarely");
        }
        // on mon,fri / on the 15th / on mar 2
        if self.eat("on") {
            self.eat("the");
            match &mut rule {
                Rule::Weeks { days } => *days = self.day_list().ok_or(Fail::NoMatch)?,
                Rule::Months { day } => {
                    let d = self.peek().and_then(day_of_month).ok_or(Fail::NoMatch)?;
                    *day = d as u8;
                    self.i += 1;
                }
                Rule::Years { month: mo, day } => {
                    let a = self.peek().map(str::to_owned).unwrap_or_default();
                    let b = self.peek_at(1).map(str::to_owned).unwrap_or_default();
                    let (m, d) = match (month(&a), day_of_month(&b), day_of_month(&a), month(&b)) {
                        (Some(m), Some(d), _, _) | (_, _, Some(d), Some(m)) => (m, d),
                        _ => return Err(Fail::NoMatch),
                    };
                    if NaiveDate::from_ymd_opt(2000, m, d).is_none() {
                        return bad(format!("{a} {b} is not a date"));
                    }
                    (*mo, *day) = (m as u8, d as u8);
                    self.i += 2;
                }
                _ => return Err(Fail::NoMatch),
            }
        }
        let mut at = self.morning();
        if !self.done() {
            if rule == Rule::Minutes {
                return Err(Fail::NoMatch);
            }
            self.eat("at");
            // "every day in the morning"
            if self.peek() == Some("in") && self.peek_at(1) == Some("the") {
                self.i += 2;
            }
            at = self.clock(true)?.ok_or(Fail::NoMatch)?.time;
        }
        if !self.done() {
            return Err(Fail::NoMatch);
        }
        let mut recur = Recur {
            rule,
            every,
            at: (at.hour() * 60 + at.minute()) as u16,
            start: String::new(),
        };
        let now = self.cx.now;
        let zone = self.cx.zone;
        if recur.rule == Rule::Minutes {
            return Ok(Spec::Repeat {
                first: now + i64::from(every) * 60_000,
                recur,
            });
        }
        // Count from whichever start gives the soonest first occurrence
        // ("every 2 days at 9" at 10:00 → tomorrow, not in two days).
        let next_monday = today + TimeDelta::days(7 - today.weekday().num_days_from_monday() as i64);
        let next_month = today.with_day(1).and_then(|d| d.checked_add_months(Months::new(1)));
        let next_year = NaiveDate::from_ymd_opt(today.year() + 1, 1, 1);
        let candidates = [Some(today), Some(today + TimeDelta::days(1)), Some(next_monday), next_month, next_year];
        let mut best: Option<i64> = None;
        for start in candidates.into_iter().flatten() {
            recur.start = start.format("%Y-%m-%d").to_string();
            if let Some(t) = recur.next_after(&zone, now, now)
                && best.is_none_or(|b| t < b)
            {
                best = Some(t);
            }
        }
        let first = best.ok_or_else(|| Fail::Bad("that never happens".into()))?;
        recur.start = zone.local(first).date().format("%Y-%m-%d").to_string();
        Ok(Spec::Repeat { recur, first })
    }

    /// After `every`: `day`, `2 weeks`, `other day`, `30 minutes`, `30m`,
    /// `weekday`, `mon,wed`.
    fn every_what(&mut self, today: NaiveDate) -> R<(Rule, u32)> {
        let w = self.peek().ok_or(Fail::NoMatch)?.to_owned();
        let (n, unit_word) = if w == "other" {
            self.i += 1;
            (2, self.peek().map(str::to_owned))
        } else if let Some(n) = number(&w) {
            self.i += 1;
            (n, self.peek().map(str::to_owned))
        } else if let Some(parts) = compound(&w) {
            // every 30m / every 1h30m
            self.i += 1;
            let mut secs = 0i64;
            for (n, u) in parts {
                match u {
                    Unit::Secs(s) => secs += i64::from(n) * s,
                    _ => return bad("use every N days / weeks / months for longer intervals"),
                }
            }
            if secs < 60 || secs % 60 != 0 {
                return bad("repeats are at least a minute apart, in whole minutes");
            }
            return Ok((Rule::Minutes, (secs / 60) as u32));
        } else {
            (1, Some(w.clone()))
        };
        let u = unit_word.ok_or(Fail::NoMatch)?;
        let rule = match u.as_str() {
            "weekday" | "weekdays" => Rule::Weeks { days: WEEKDAYS },
            "weekend" | "weekends" => Rule::Weeks { days: WEEKEND },
            _ => match unit(&u) {
                Some(Unit::Secs(1)) => return bad("repeats are at least a minute apart"),
                Some(Unit::Secs(60)) => {
                    self.i += 1;
                    return Ok((Rule::Minutes, n));
                }
                Some(Unit::Secs(_)) => {
                    self.i += 1;
                    return Ok((Rule::Minutes, n.saturating_mul(60)));
                }
                Some(Unit::Days(1)) => Rule::Days,
                Some(Unit::Days(_)) => Rule::Weeks { days: day_bit(today.weekday()) },
                Some(Unit::Months(1)) => Rule::Months { day: today.day() as u8 },
                Some(Unit::Months(_)) => Rule::Years {
                    month: today.month() as u8,
                    day: today.day() as u8,
                },
                None => {
                    if n != 1 && w != "other" {
                        return Err(Fail::NoMatch);
                    }
                    let days = self.day_list().ok_or(Fail::NoMatch)?;
                    return Ok((Rule::Weeks { days }, if w == "other" { 2 } else { 1 }));
                }
            },
        };
        self.i += 1;
        Ok((rule, n))
    }

    /// `mon`, `mon wed fri`, `mon and thu`, `weekday` → a set of days.
    fn day_list(&mut self) -> Option<Days> {
        let mut days: Days = 0;
        while let Some(w) = self.peek() {
            if days != 0 && w == "and" {
                self.i += 1;
                continue;
            }
            let bits = match w {
                "weekday" | "weekdays" => WEEKDAYS,
                "weekend" | "weekends" => WEEKEND,
                _ => match weekday(w) {
                    Some(d) => day_bit(d),
                    None => break,
                },
            };
            days |= bits;
            self.i += 1;
        }
        (days != 0).then_some(days)
    }
}

fn part_of_day(w: &str) -> Option<NaiveTime> {
    let h = match w {
        "noon" | "midday" => 12,
        "midnight" => 0,
        "morning" => 9, // replaced by the setting
        "afternoon" => 14,
        "eod" => 17,
        "evening" => 18,
        "night" => 20,
        _ => return None,
    };
    NaiveTime::from_hms_opt(h, 0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Monday 2026-11-16 10:00 in Bangkok (UTC+7, no DST).
    fn cx() -> Cx {
        let zone = Zone::parse("Asia/Bangkok").unwrap();
        let now = zone.instant(NaiveDateTime::parse_from_str("2026-11-16 10:00", "%Y-%m-%d %H:%M").unwrap());
        Cx {
            now,
            zone,
            morning: 9,
            h12: false,
        }
    }

    fn local(ms: i64) -> String {
        cx().zone.local(ms).format("%Y-%m-%d %H:%M").to_string()
    }

    fn when(s: &str) -> String {
        match parse(s, &cx()) {
            Ok(spec) => local(spec.first()),
            Err(Fail::NoMatch) => "no match".into(),
            Err(Fail::Bad(m)) => format!("bad: {m}"),
        }
    }

    fn repeat(s: &str) -> (String, String) {
        match parse(s, &cx()) {
            Ok(Spec::Repeat { recur, first }) => (recur.describe(false), local(first)),
            other => panic!("{s}: {other:?}"),
        }
    }

    #[test]
    fn durations() {
        assert_eq!(when("2h"), "2026-11-16 12:00");
        assert_eq!(when("in 2h"), "2026-11-16 12:00");
        assert_eq!(when("1h30m"), "2026-11-16 11:30");
        assert_eq!(when("in 1 hour and 15 minutes"), "2026-11-16 11:15");
        assert_eq!(when("45 min"), "2026-11-16 10:45");
        assert_eq!(when("an hour"), "2026-11-16 11:00");
        assert_eq!(when("in half an hour"), "2026-11-16 10:30");
        assert_eq!(when("in 3 days"), "2026-11-19 10:00");
        assert_eq!(when("2w"), "2026-11-30 10:00");
        assert_eq!(when("in 1 month"), "2026-12-16 10:00");
        assert_eq!(when("0m"), "bad: a time from now has to be more than zero");
        assert_eq!(when("in"), "no match");
    }

    #[test]
    fn clock_times_roll_to_tomorrow_when_passed() {
        assert_eq!(when("15:30"), "2026-11-16 15:30");
        assert_eq!(when("3pm"), "2026-11-16 15:00");
        assert_eq!(when("3:30 pm"), "2026-11-16 15:30");
        assert_eq!(when("at 11"), "2026-11-16 11:00");
        assert_eq!(when("noon"), "2026-11-16 12:00");
        assert_eq!(when("this evening"), "2026-11-16 18:00");
        assert_eq!(when("9:00"), "2026-11-17 09:00");
        let Ok(Spec::Once { note, .. }) = parse("9:00", &cx()) else { panic!() };
        assert_eq!(note.as_deref(), Some("09:00 has passed today, so tomorrow"));
        assert_eq!(when("midnight"), "2026-11-17 00:00");
        assert_eq!(when("12am"), "2026-11-17 00:00");
        assert_eq!(when("12pm"), "2026-11-16 12:00");
        assert_eq!(when("9"), "no match", "a bare number is not a time");
        assert_eq!(when("25:00"), "bad: 25:00 is not a time (use 00:00–23:59)");
        assert_eq!(when("13pm"), "bad: 13pm is not a time");
    }

    #[test]
    fn days_and_dates() {
        assert_eq!(when("tomorrow"), "2026-11-17 09:00");
        assert_eq!(when("tomorrow 9am"), "2026-11-17 09:00");
        assert_eq!(when("tmr at 17:45"), "2026-11-17 17:45");
        assert_eq!(when("tomorrow 8"), "2026-11-17 08:00");
        assert_eq!(when("tonight"), "2026-11-16 20:00");
        assert_eq!(when("tonight at 9"), "2026-11-16 21:00");
        assert_eq!(when("today"), "bad: today at what time? e.g. today 5pm");
        assert_eq!(when("today 9:00"), "bad: 09:00 today has already passed");
        assert_eq!(when("fri"), "2026-11-20 09:00");
        assert_eq!(when("friday 17:00"), "2026-11-20 17:00");
        assert_eq!(when("on friday at 5pm"), "2026-11-20 17:00");
        assert_eq!(when("mon 11:00"), "2026-11-16 11:00", "later today");
        assert_eq!(when("mon 9:00"), "2026-11-23 09:00", "passed today: next week");
        assert_eq!(when("next mon 11:00"), "2026-11-23 11:00");
        assert_eq!(when("next week"), "2026-11-23 09:00");
        assert_eq!(when("next month"), "2026-12-01 09:00");
        assert_eq!(when("weekend"), "2026-11-21 09:00");
        assert_eq!(when("2026-12-01"), "2026-12-01 09:00");
        assert_eq!(when("2026-12-01 14:30"), "2026-12-01 14:30");
        assert_eq!(when("2026-12-01T14:30"), "2026-12-01 14:30");
        assert_eq!(when("dec 5"), "2026-12-05 09:00");
        assert_eq!(when("5th december 3pm"), "2026-12-05 15:00");
        assert_eq!(when("5 of dec"), "2026-12-05 09:00");
        assert_eq!(when("jan 2"), "2027-01-02 09:00", "no year: the next one");
        assert_eq!(when("nov 16 2026 8am"), "bad: 2026-11-16 08:00 is in the past");
        assert_eq!(when("2026-02-30"), "bad: 2026-02-30 is not a date");
        assert_eq!(when("2025-01-01"), "bad: 2025-01-01 09:00 is in the past");
        assert_eq!(when("feb 30"), "bad: feb 30 is not a date");
        assert_eq!(when("feb 29"), "2028-02-29 09:00");
        assert_eq!(when("yesterday"), "bad: yesterday is in the past");
        assert_eq!(when("2090-01-01"), "bad: that is too far ahead (20 years at most)");
    }

    #[test]
    fn repeats() {
        assert_eq!(repeat("every day at 9"), ("every day at 09:00".into(), "2026-11-17 09:00".into()));
        assert_eq!(repeat("daily 18:00"), ("every day at 18:00".into(), "2026-11-16 18:00".into()));
        assert_eq!(repeat("every weekday 9am").0, "every weekday at 09:00");
        assert_eq!(repeat("every mon,wed 10am"), ("every mon,wed at 10:00".into(), "2026-11-18 10:00".into()));
        assert_eq!(repeat("every mon and fri at 17:00").0, "every mon,fri at 17:00");
        assert_eq!(
            repeat("every 2 weeks on fri at 17:00"),
            ("every 2 weeks on fri at 17:00".into(), "2026-11-20 17:00".into())
        );
        assert_eq!(repeat("every other day at 8").1, "2026-11-17 08:00");
        assert_eq!(repeat("every 2 days at 9").1, "2026-11-17 09:00", "soonest start");
        assert_eq!(
            repeat("every month on the 1st"),
            ("every month on the 1st at 09:00".into(), "2026-12-01 09:00".into())
        );
        assert_eq!(repeat("monthly").1, "2026-12-16 09:00");
        assert_eq!(repeat("every year on mar 2 at 1pm").0, "every year on Mar 2 at 13:00");
        assert_eq!(repeat("every 30 minutes"), ("every 30 minutes".into(), "2026-11-16 10:30".into()));
        assert_eq!(repeat("every 2 hours").0, "every 2 hours");
        assert_eq!(repeat("every 90m").0, "every 90 minutes");
        assert_eq!(repeat("hourly").1, "2026-11-16 11:00");
        assert_eq!(repeat("every hour").0, "every hour");
        assert_eq!(when("every 0 days"), "bad: every 0? use a number of 1 or more");
        assert_eq!(when("every 30 seconds"), "bad: repeats are at least a minute apart");
        assert_eq!(when("every 30 minutes at 9"), "no match");
        assert_eq!(when("every banana"), "no match");
    }

    #[test]
    fn descriptions_read_back_as_the_same_rule() {
        for s in [
            "every day at 9",
            "every weekday at 9:05",
            "every 2 weeks on mon,fri at 17:00",
            "every month on the 31st at 08:00",
            "every year on feb 29 at 1pm",
            "every 90 minutes",
            "every 2 hours",
            "every weekend at 10:00",
            "every 3 months on the 15th at 07:30",
        ] {
            let Ok(Spec::Repeat { recur, .. }) = parse(s, &cx()) else { panic!("{s}") };
            for h12 in [false, true] {
                let again = recur.describe(h12);
                let Ok(Spec::Repeat { recur: r2, .. }) = parse(&again, &cx()) else {
                    panic!("{again} does not read back")
                };
                assert_eq!((&r2.rule, r2.every, r2.at), (&recur.rule, recur.every, recur.at), "{again}");
            }
        }
    }

    #[test]
    fn split_finds_the_time_before_or_after_the_text() {
        let s = |input: &str| match split(input, &cx()) {
            Ok((spec, text)) => (spec.map(|s| local(s.first())), text),
            Err(e) => (Some(format!("error: {e}")), String::new()),
        };
        assert_eq!(s("2h Reply to Alice"), (Some("2026-11-16 12:00".into()), "Reply to Alice".into()));
        assert_eq!(s("tomorrow 9am to buy milk"), (Some("2026-11-17 09:00".into()), "buy milk".into()));
        assert_eq!(s("in 2 hours: stretch"), (Some("2026-11-16 12:00".into()), "stretch".into()));
        assert_eq!(s("Call mom tomorrow 6pm"), (Some("2026-11-17 18:00".into()), "Call mom".into()));
        assert_eq!(s("Pay rent on dec 1"), (Some("2026-12-01 09:00".into()), "Pay rent".into()));
        assert_eq!(s("Water plants every mon 8am").1, "Water plants");
        assert_eq!(s("Buy 12 eggs"), (None, "Buy 12 eggs".into()));
        assert_eq!(s("Look at 3 items"), (None, "Look at 3 items".into()));
        assert_eq!(s("Call Tom"), (None, "Call Tom".into()));
        assert_eq!(s("Gym fri 7am"), (Some("2026-11-20 07:00".into()), "Gym".into()));
        assert_eq!(s("Tea 15:30"), (Some("2026-11-16 15:30".into()), "Tea".into()));
        assert_eq!(s("Order 5 pizzas"), (None, "Order 5 pizzas".into()));
        assert_eq!(s("Call May"), (None, "Call May".into()));
        assert_eq!(s("fri"), (Some("2026-11-20 09:00".into()), String::new()));
        assert_eq!(s("25:00 standup").0.unwrap(), "error: 25:00 is not a time (use 00:00–23:59)");
        assert_eq!(s("today 9:00 standup").0.unwrap(), "error: 09:00 today has already passed");
    }

    #[test]
    fn dst_aware_relative_days() {
        // Berlin, Saturday 2026-03-28 10:00 CET; "in 1 day" crosses the change.
        let zone = Zone::parse("Europe/Berlin").unwrap();
        let now = zone.instant(NaiveDateTime::parse_from_str("2026-03-28 10:00", "%Y-%m-%d %H:%M").unwrap());
        let cx = Cx { now, zone, morning: 9, h12: false };
        let Ok(spec) = parse("in 1 day", &cx) else { panic!() };
        assert_eq!(zone.local(spec.first()).format("%H:%M").to_string(), "10:00");
        assert_eq!(spec.first() - now, 23 * 3_600_000);
        let Ok(spec) = parse("in 24h", &cx) else { panic!() };
        assert_eq!(zone.local(spec.first()).format("%H:%M").to_string(), "11:00");
        let Ok(spec) = parse("tomorrow 2:30", &cx) else { panic!() };
        assert_eq!(zone.local(spec.first()).format("%H:%M").to_string(), "03:30", "in the gap");
    }
}
