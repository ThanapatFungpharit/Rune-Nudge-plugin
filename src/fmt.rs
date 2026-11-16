//! How times and durations are shown.

use chrono::{Datelike, NaiveTime, Timelike, Weekday};

use crate::zone::Zone;

pub fn weekday_short(w: Weekday) -> &'static str {
    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][w.num_days_from_monday() as usize]
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `09:05` or `9:05 am`.
pub fn fmt_clock(t: NaiveTime, h12: bool) -> String {
    if h12 {
        let (pm, h) = t.hour12();
        format!("{h}:{:02} {}", t.minute(), if pm { "pm" } else { "am" })
    } else {
        format!("{:02}:{:02}", t.hour(), t.minute())
    }
}

/// A due time in words, relative to today in the user's zone:
/// `today 15:30`, `tomorrow 09:00`, `yesterday 18:00`, `Fri 17:00` (this
/// week), `Fri 21 Nov 17:00`, `21 Nov 2027 17:00`.
pub fn fmt_when(due: i64, now: i64, zone: &Zone, h12: bool) -> String {
    let d = zone.local(due);
    let today = zone.local(now).date();
    let clock = fmt_clock(d.time(), h12);
    let days = (d.date() - today).num_days();
    match days {
        0 => format!("today {clock}"),
        1 => format!("tomorrow {clock}"),
        -1 => format!("yesterday {clock}"),
        2..=6 => format!("{} {clock}", weekday_short(d.weekday())),
        _ if d.year() == today.year() => format!(
            "{} {} {} {clock}",
            weekday_short(d.weekday()),
            d.day(),
            MONTHS[d.month0() as usize]
        ),
        _ => format!("{} {} {} {clock}", d.day(), MONTHS[d.month0() as usize], d.year()),
    }
}

/// An unambiguous local date and time, as forms show it: `2026-11-17 09:00`.
pub fn fmt_exact(due: i64, zone: &Zone) -> String {
    zone.local(due).format("%Y-%m-%d %H:%M").to_string()
}

/// `45s`, `5m`, `2h 10m`, `3d 4h`, `5w`: at most two units.
pub fn fmt_dur(ms: i64) -> String {
    let s = ms.abs() / 1000;
    if s < 60 {
        return format!("{s}s");
    }
    let m = s / 60;
    if m < 60 {
        return format!("{m}m");
    }
    let (h, m) = (m / 60, m % 60);
    if h < 24 {
        return if m == 0 { format!("{h}h") } else { format!("{h}h {m}m") };
    }
    let (d, h) = (h / 24, h % 24);
    if d < 14 {
        return if h == 0 { format!("{d}d") } else { format!("{d}d {h}h") };
    }
    let (w, d) = (d / 7, d % 7);
    if d == 0 { format!("{w}w") } else { format!("{w}w {d}d") }
}

/// `in 2h 10m`, `5m ago`, `now`.
pub fn fmt_rel(due: i64, now: i64) -> String {
    let diff = due - now;
    if diff.abs() < 30_000 {
        "now".into()
    } else if diff > 0 {
        // Round up so "in 59s" never reads "in 0m".
        format!("in {}", fmt_dur(diff + 59_999 - (diff + 59_999) % 60_000).trim())
    } else {
        format!("{} ago", fmt_dur(-diff))
    }
}

/// Cut `s` to `max` characters (with `…`), on one line.
pub fn clip(s: &str, max: usize) -> String {
    let one: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let one = one.trim();
    if one.chars().count() <= max {
        one.to_owned()
    } else {
        let mut out: String = one.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;

    fn at(zone: &Zone, s: &str) -> i64 {
        zone.instant(NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap())
    }

    #[test]
    fn when_is_relative_to_the_local_day() {
        let z = Zone::parse("Asia/Bangkok").unwrap();
        let now = at(&z, "2026-11-16 10:00"); // a Monday
        let w = |s| fmt_when(at(&z, s), now, &z, false);
        assert_eq!(w("2026-11-16 15:30"), "today 15:30");
        assert_eq!(w("2026-11-17 09:00"), "tomorrow 09:00");
        assert_eq!(w("2026-11-15 18:00"), "yesterday 18:00");
        assert_eq!(w("2026-11-20 17:00"), "Fri 17:00");
        assert_eq!(w("2026-12-01 08:00"), "Tue 1 Dec 08:00");
        assert_eq!(w("2027-01-02 08:00"), "2 Jan 2027 08:00");
        assert_eq!(fmt_when(at(&z, "2026-11-16 15:30"), now, &z, true), "today 3:30 pm");
        assert_eq!(fmt_when(at(&z, "2026-11-16 00:05"), now, &z, true), "today 12:05 am");
    }

    #[test]
    fn durations() {
        assert_eq!(fmt_dur(45_000), "45s");
        assert_eq!(fmt_dur(5 * 60_000), "5m");
        assert_eq!(fmt_dur(130 * 60_000), "2h 10m");
        assert_eq!(fmt_dur(120 * 60_000), "2h");
        assert_eq!(fmt_dur(76 * 3_600_000), "3d 4h");
        assert_eq!(fmt_dur(35 * 86_400_000), "5w");
        assert_eq!(fmt_rel(10_000, 0), "now");
        assert_eq!(fmt_rel(90_000, 0), "in 2m");
        assert_eq!(fmt_rel(3_600_000, 0), "in 1h");
        assert_eq!(fmt_rel(0, 300_000), "5m ago");
    }

    #[test]
    fn clipping() {
        assert_eq!(clip("  a\nb ", 10), "a b");
        assert_eq!(clip("abcdef", 4), "abc…");
    }
}
