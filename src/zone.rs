//! The user's time zone: turning instants into wall-clock times and back.
//!
//! A WebAssembly plugin has no clock and no time zone of its own: rune hands
//! it the current instant (`now`, UTC milliseconds) with every event, and the
//! zone comes from the plugin's settings. Named zones (`Asia/Bangkok`) carry
//! the full IANA rules, so daylight-saving changes are handled; fixed offsets
//! (`UTC+7`) never change.
//!
//! Converting a wall-clock time to an instant is not always one-to-one:
//!
//! - **In a gap** (clocks spring forward, e.g. 02:30 when 02:00 jumps to
//!   03:00) the time does not exist; it is read with the offset in force
//!   just before the gap, which lands the same distance past the jump
//!   (02:30 → 03:30). A reminder is never dropped.
//! - **In an overlap** (clocks fall back, 01:30 happens twice) the earlier
//!   of the two is used, so a daily reminder fires once, not twice.

use chrono::{DateTime, FixedOffset, LocalResult, NaiveDateTime, TimeDelta, TimeZone};
use chrono_tz::Tz;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Zone {
    Fixed(FixedOffset),
    Named(Tz),
}

impl Default for Zone {
    fn default() -> Self {
        Zone::utc()
    }
}

/// Instants this plugin works with: 1970 … 9999.
pub const MAX_MS: i64 = 253_402_300_799_000;

pub fn utc_naive(ms: i64) -> NaiveDateTime {
    DateTime::from_timestamp_millis(ms.clamp(0, MAX_MS))
        .unwrap_or_default()
        .naive_utc()
}

impl Zone {
    pub fn utc() -> Self {
        Zone::Fixed(FixedOffset::east_opt(0).expect("0 is a valid offset"))
    }

    /// Read a setting: empty, `UTC`/`GMT`/`Z`, an offset (`UTC+7`,
    /// `+05:30`, `GMT-3`), or an IANA name in any letter case.
    pub fn parse(s: &str) -> Result<Zone, String> {
        let t = s.trim();
        if t.is_empty() || t.eq_ignore_ascii_case("z") {
            return Ok(Zone::utc());
        }
        let lower = t.to_ascii_lowercase();
        let rest = lower
            .strip_prefix("utc")
            .or_else(|| lower.strip_prefix("gmt"))
            .unwrap_or(&lower);
        if rest.is_empty() {
            return Ok(Zone::utc());
        }
        if rest.starts_with('+') || rest.starts_with('-') {
            return parse_offset(rest)
                .map(Zone::Fixed)
                .ok_or_else(|| format!("{t:?} is not an offset: write it like UTC+7, UTC+05:30 or UTC-3"));
        }
        if let Ok(tz) = t.parse::<Tz>() {
            return Ok(Zone::Named(tz));
        }
        chrono_tz::TZ_VARIANTS
            .iter()
            .find(|z| z.name().eq_ignore_ascii_case(t))
            .map(|z| Zone::Named(*z))
            .ok_or_else(|| {
                format!("unknown time zone {t:?}: use a name like Asia/Bangkok or Europe/Berlin, or an offset like UTC+7")
            })
    }

    /// The zone as the user would write it.
    pub fn name(&self) -> String {
        match self {
            Zone::Named(tz) => tz.name().to_owned(),
            Zone::Fixed(o) => {
                let s = o.local_minus_utc();
                if s == 0 {
                    "UTC".into()
                } else {
                    let sign = if s < 0 { '-' } else { '+' };
                    let a = s.abs();
                    format!("UTC{sign}{:02}:{:02}", a / 3600, a / 60 % 60)
                }
            }
        }
    }

    /// The wall-clock time at instant `ms`.
    pub fn local(&self, ms: i64) -> NaiveDateTime {
        let utc = utc_naive(ms);
        match self {
            Zone::Fixed(o) => o.from_utc_datetime(&utc).naive_local(),
            Zone::Named(tz) => tz.from_utc_datetime(&utc).naive_local(),
        }
    }

    /// The instant a wall-clock time stands for (gaps and overlaps as
    /// described in the module docs).
    pub fn instant(&self, local: NaiveDateTime) -> i64 {
        let ms = |t: NaiveDateTime| t.and_utc().timestamp_millis();
        match self {
            Zone::Fixed(o) => ms(local - TimeDelta::seconds(o.local_minus_utc().into())),
            Zone::Named(tz) => match tz.from_local_datetime(&local) {
                LocalResult::Single(t) => t.timestamp_millis(),
                LocalResult::Ambiguous(a, b) => a.timestamp_millis().min(b.timestamp_millis()),
                LocalResult::None => {
                    // The offset in force a day earlier, i.e. before the gap.
                    let before = local - TimeDelta::days(1);
                    match tz.from_local_datetime(&before).earliest() {
                        Some(t) => {
                            let off = t.naive_local() - t.naive_utc();
                            ms(local - off)
                        }
                        // Two transitions within a day: walk forward to the
                        // first time that exists.
                        None => (1..=48 * 60)
                            .find_map(|m| {
                                tz.from_local_datetime(&(local + TimeDelta::minutes(m)))
                                    .earliest()
                            })
                            .map_or_else(|| ms(local), |t| t.timestamp_millis()),
                    }
                }
            },
        }
    }
}

/// `+7`, `+07`, `+0700`, `+07:00`, `-3:30` (within ±14 h).
fn parse_offset(s: &str) -> Option<FixedOffset> {
    let (sign, body) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let (h, m) = match body.split_once(':') {
        Some((h, m)) => (h, m),
        None if body.len() == 4 => body.split_at(2),
        None => (body, "0"),
    };
    let num = |x: &str| {
        (!x.is_empty() && x.len() <= 2 && x.bytes().all(|b| b.is_ascii_digit()))
            .then(|| x.parse::<i32>().ok())
            .flatten()
    };
    let (h, m) = (num(h)?, num(m)?);
    if h > 14 || m > 59 || (h == 14 && m > 0) {
        return None;
    }
    FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> i64 {
        dt(y, mo, d, h, mi).and_utc().timestamp_millis()
    }

    #[test]
    fn parses_names_offsets_and_utc() {
        assert_eq!(Zone::parse("").unwrap(), Zone::utc());
        assert_eq!(Zone::parse(" utc ").unwrap(), Zone::utc());
        assert_eq!(Zone::parse("GMT").unwrap(), Zone::utc());
        assert_eq!(Zone::parse("UTC+7").unwrap().name(), "UTC+07:00");
        assert_eq!(Zone::parse("+05:30").unwrap().name(), "UTC+05:30");
        assert_eq!(Zone::parse("gmt-0330").unwrap().name(), "UTC-03:30");
        assert_eq!(Zone::parse("asia/bangkok").unwrap().name(), "Asia/Bangkok");
        assert_eq!(Zone::parse("Europe/Berlin").unwrap().name(), "Europe/Berlin");
        assert!(Zone::parse("Mars/Olympus").is_err());
        assert!(Zone::parse("UTC+15").is_err());
        assert!(Zone::parse("UTC+7:75").is_err());
        assert!(Zone::parse("UTC+").is_err());
    }

    #[test]
    fn fixed_offsets_round_trip() {
        let z = Zone::parse("UTC+7").unwrap();
        let t = utc_ms(2026, 11, 16, 2, 0);
        assert_eq!(z.local(t), dt(2026, 11, 16, 9, 0));
        assert_eq!(z.instant(dt(2026, 11, 16, 9, 0)), t);
    }

    #[test]
    fn spring_forward_gap_moves_later_by_the_gap() {
        // Berlin, 2026-03-29: 02:00 CET jumps to 03:00 CEST.
        let z = Zone::parse("Europe/Berlin").unwrap();
        let t = z.instant(dt(2026, 3, 29, 2, 30));
        assert_eq!(z.local(t), dt(2026, 3, 29, 3, 30));
        assert_eq!(t, utc_ms(2026, 3, 29, 1, 30));
    }

    #[test]
    fn fall_back_overlap_takes_the_earlier_time() {
        // Berlin, 2026-10-25: 03:00 CEST falls back to 02:00 CET.
        let z = Zone::parse("Europe/Berlin").unwrap();
        let t = z.instant(dt(2026, 10, 25, 2, 30));
        assert_eq!(t, utc_ms(2026, 10, 25, 0, 30), "02:30 CEST, the first one");
    }

    #[test]
    fn a_skipped_day_is_not_lost() {
        // Samoa skipped 2011-12-30 entirely (UTC-10 → UTC+14).
        let z = Zone::parse("Pacific/Apia").unwrap();
        let t = z.instant(dt(2011, 12, 30, 10, 0));
        assert_eq!(z.local(t), dt(2011, 12, 31, 10, 0));
    }
}
