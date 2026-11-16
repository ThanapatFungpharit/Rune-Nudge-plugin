//! The plugin's settings (declared in `plugin.json`, edited in rune's
//! Settings › Plugins › Nudge). rune validates them against the manifest;
//! they are read defensively here anyway, falling back to the defaults.

use serde_json::{Map, Value};

use crate::zone::Zone;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alerts {
    /// A desktop notification and the status line.
    Both,
    Desktop,
    StatusLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missed {
    /// One notification listing what came due while rune was closed.
    Summary,
    /// One notification per reminder (a few, then a summary).
    Each,
    /// No notification: they are listed as overdue, and the status line says so.
    Quiet,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub zone: Zone,
    /// The zone setting could not be read (then UTC is used).
    pub zone_error: Option<String>,
    /// The zone setting is empty: UTC, and a hint to set it.
    pub zone_unset: bool,
    pub h12: bool,
    pub default_minutes: u32,
    pub followup_minutes: u32,
    pub snooze_minutes: u32,
    pub morning_hour: u32,
    pub alerts: Alerts,
    pub missed: Missed,
    /// Repeat alerts for overdue reminders every N minutes (0: never).
    pub nag_minutes: u32,
    /// A repeating reminder whose occurrences were all missed while rune
    /// was closed: alert once (true) or skip silently to the next.
    pub missed_repeats_alert: bool,
    pub link_chat: bool,
    pub followup_auto_done: bool,
    pub quote_message: bool,
    pub keep_done: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            zone: Zone::utc(),
            zone_error: None,
            zone_unset: true,
            h12: false,
            default_minutes: 60,
            followup_minutes: 120,
            snooze_minutes: 10,
            morning_hour: 9,
            alerts: Alerts::Both,
            missed: Missed::Summary,
            nag_minutes: 0,
            missed_repeats_alert: true,
            link_chat: true,
            followup_auto_done: true,
            quote_message: true,
            keep_done: 30,
        }
    }
}

impl Settings {
    pub fn from_config(config: &Map<String, Value>) -> Self {
        let d = Settings::default();
        let int = |k: &str, def: u32, lo: u32, hi: u32| {
            config
                .get(k)
                .and_then(Value::as_u64)
                .map_or(def, |v| v.clamp(u64::from(lo), u64::from(hi)) as u32)
        };
        let boolean = |k: &str, def: bool| config.get(k).and_then(Value::as_bool).unwrap_or(def);
        let text = |k: &str| config.get(k).and_then(Value::as_str).unwrap_or("");
        let zone_text = text("timezone");
        let (zone, zone_error) = match Zone::parse(zone_text) {
            Ok(z) => (z, None),
            Err(e) => (Zone::utc(), Some(e)),
        };
        Settings {
            zone,
            zone_error,
            zone_unset: zone_text.trim().is_empty(),
            h12: text("clock") == "12-hour",
            default_minutes: int("default_minutes", d.default_minutes, 1, 10_080),
            followup_minutes: int("followup_minutes", d.followup_minutes, 1, 10_080),
            snooze_minutes: int("snooze_minutes", d.snooze_minutes, 1, 1_440),
            morning_hour: int("morning_hour", d.morning_hour, 0, 23),
            alerts: match text("alerts") {
                "desktop" => Alerts::Desktop,
                "status line" => Alerts::StatusLine,
                _ => Alerts::Both,
            },
            missed: match text("missed") {
                "one each" => Missed::Each,
                "list only" => Missed::Quiet,
                _ => Missed::Summary,
            },
            nag_minutes: int("nag_minutes", d.nag_minutes, 0, 1_440),
            missed_repeats_alert: text("missed_repeats") != "skip silently",
            link_chat: boolean("link_chat", d.link_chat),
            followup_auto_done: boolean("followup_auto_done", d.followup_auto_done),
            quote_message: boolean("quote_message", d.quote_message),
            keep_done: int("keep_done", d.keep_done as u32, 0, 100) as usize,
        }
    }

    pub fn desktop(&self) -> bool {
        self.alerts != Alerts::StatusLine
    }

    pub fn status_line(&self) -> bool {
        self.alerts != Alerts::Desktop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_values_and_falls_back() {
        let c = serde_json::json!({
            "timezone": "Asia/Bangkok", "clock": "12-hour", "default_minutes": 30,
            "snooze_minutes": 99999, "alerts": "desktop", "missed": "list only",
            "missed_repeats": "skip silently", "link_chat": false, "keep_done": "x"
        });
        let s = Settings::from_config(c.as_object().unwrap());
        assert_eq!(s.zone.name(), "Asia/Bangkok");
        assert!(!s.zone_unset && s.h12);
        assert_eq!(s.default_minutes, 30);
        assert_eq!(s.snooze_minutes, 1_440, "clamped");
        assert_eq!(s.alerts, Alerts::Desktop);
        assert_eq!(s.missed, Missed::Quiet);
        assert!(!s.missed_repeats_alert && !s.link_chat);
        assert_eq!(s.keep_done, 30, "wrong type: default");
        let s = Settings::from_config(serde_json::json!({"timezone": "Nowhere/Land"}).as_object().unwrap());
        assert_eq!(s.zone, Zone::utc());
        assert!(s.zone_error.is_some());
        assert_eq!(Settings::from_config(&Map::new()), Settings::default());
    }
}
