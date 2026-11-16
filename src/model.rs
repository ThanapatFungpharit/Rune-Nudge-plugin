//! A reminder as it is kept in storage (one JSON value per reminder,
//! under `r.<id>`), and the checks applied when reading it back.

use rune_plugin::prelude::{ChatKind, ChatRef};
use serde::{Deserialize, Serialize};

use crate::recur::Recur;
use crate::zone::MAX_MS;

/// The storage format this version writes. Records with a newer version
/// are left alone (kept in storage, not shown) rather than misread.
pub const FORMAT: u32 = 1;
/// Longest reminder text.
pub const TEXT_MAX: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    #[default]
    Reminder,
    /// "Reply to …": tied to a chat, finished by replying there.
    FollowUp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Waiting for `due`.
    #[default]
    Pending,
    /// A one-time reminder that went off and is waiting to be dealt with
    /// (done, snoozed or deleted): it shows as overdue.
    Fired,
    /// Done (or, for a repeating one, stopped).
    Done,
}

/// The chat a reminder belongs to, as it was when the reminder was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatInfo {
    pub mid: String,
    pub name: String,
    pub kind: ChatKind,
}

impl From<&ChatRef> for ChatInfo {
    fn from(c: &ChatRef) -> Self {
        ChatInfo {
            mid: c.mid.clone(),
            name: c.name.clone(),
            kind: c.kind,
        }
    }
}

impl ChatInfo {
    pub fn kind_word(&self) -> &'static str {
        match self.kind {
            ChatKind::User => "contact",
            ChatKind::Group => "group",
            ChatKind::Room => "room",
            ChatKind::Other => "chat",
        }
    }
}

/// The message a follow-up answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageCtx {
    pub id: String,
    pub from_name: String,
    /// One line, as rune shows it (`[photo]`, the text…), clipped.
    pub summary: String,
    pub time: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    #[serde(default = "format_v1")]
    pub v: u32,
    pub id: u32,
    pub text: String,
    #[serde(default)]
    pub kind: Kind,
    #[serde(default)]
    pub state: State,
    pub created: i64,
    /// The next (or current, when fired) occurrence, ms since the epoch.
    pub due: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recur: Option<Recur>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat: Option<ChatInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<MessageCtx>,
    /// The occurrence (`due`) a notification was last shown for: the same
    /// occurrence never notifies twice, across timers and restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notified: Option<i64>,
    /// When it last went off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fired_at: Option<i64>,
    /// When an overdue reminder last alerted (repeat alerts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alerted_at: Option<i64>,
    /// It went off late because rune was closed (or asleep).
    #[serde(default, skip_serializing_if = "is_false")]
    pub missed: bool,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub snoozes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    /// Why it was finished, when not by hand ("you replied").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_note: Option<String>,
}

fn format_v1() -> u32 {
    FORMAT
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// What reading one stored value gave.
#[derive(Debug)]
pub enum Loaded {
    Ok(Box<Reminder>),
    /// Written by a newer version of the plugin: keep, don't touch.
    Newer,
    Bad(String),
}

fn time_ok(t: i64) -> bool {
    (0..=MAX_MS).contains(&t)
}

impl Reminder {
    pub fn new(id: u32, text: String, created: i64, due: i64) -> Self {
        Reminder {
            v: FORMAT,
            id,
            text,
            kind: Kind::Reminder,
            state: State::Pending,
            created,
            due,
            recur: None,
            chat: None,
            message: None,
            notified: None,
            fired_at: None,
            alerted_at: None,
            missed: false,
            snoozes: 0,
            done_at: None,
            done_note: None,
        }
    }

    /// Read one stored value, checking everything.
    pub fn load(key_id: u32, json: &str) -> Loaded {
        let value: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(e) => return Loaded::Bad(format!("not JSON: {e}")),
        };
        if value.get("v").and_then(|v| v.as_u64()).is_some_and(|v| v > u64::from(FORMAT)) {
            return Loaded::Newer;
        }
        let r: Reminder = match serde_json::from_value(value) {
            Ok(r) => r,
            Err(e) => return Loaded::Bad(e.to_string()),
        };
        if r.id != key_id {
            return Loaded::Bad(format!("id {} stored under {key_id}", r.id));
        }
        match r.validate() {
            Ok(()) => Loaded::Ok(Box::new(r)),
            Err(e) => Loaded::Bad(e),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        let t = self.text.trim();
        if t.is_empty() || self.text.chars().count() > TEXT_MAX * 2 {
            return Err("empty or overlong text".into());
        }
        if !time_ok(self.created) || !time_ok(self.due) {
            return Err("time out of range".into());
        }
        for t in [self.notified, self.fired_at, self.alerted_at, self.done_at].into_iter().flatten() {
            if !time_ok(t) {
                return Err("time out of range".into());
            }
        }
        if let Some(r) = &self.recur {
            r.validate()?;
        }
        if let Some(c) = &self.chat
            && (c.mid.is_empty() || c.mid.len() > 100)
        {
            return Err("bad chat".into());
        }
        if self.kind == Kind::FollowUp && self.chat.is_none() {
            return Err("follow-up without a chat".into());
        }
        if self.state == State::Fired && self.recur.is_some() {
            return Err("repeating reminders are never left fired".into());
        }
        Ok(())
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn is_open(&self) -> bool {
        self.state != State::Done
    }

    /// Normalised text, for spotting duplicates.
    pub fn same_text(&self, other: &str) -> bool {
        let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        norm(&self.text) == norm(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_checks() {
        let mut r = Reminder::new(3, "Pay rent".into(), 1_000, 2_000);
        r.chat = Some(ChatInfo {
            mid: "u1".into(),
            name: "Alice".into(),
            kind: ChatKind::User,
        });
        let json = r.to_json();
        assert!(!json.contains("snoozes"), "defaults stay out: {json}");
        match Reminder::load(3, &json) {
            Loaded::Ok(back) => assert_eq!(*back, r),
            other => panic!("{other:?}"),
        }
        assert!(matches!(Reminder::load(4, &json), Loaded::Bad(_)), "id mismatch");
        assert!(matches!(Reminder::load(3, "{"), Loaded::Bad(_)));
        assert!(matches!(Reminder::load(3, r#"{"v":99,"whatever":1}"#), Loaded::Newer));
        let mut bad = r.clone();
        bad.text = "  ".into();
        assert!(bad.validate().is_err());
        let mut bad = r.clone();
        bad.due = -5;
        assert!(bad.validate().is_err());
        let mut bad = r.clone();
        bad.kind = Kind::FollowUp;
        bad.chat = None;
        assert!(bad.validate().is_err());
        // Older records without the optional fields still load.
        assert!(matches!(
            Reminder::load(7, r#"{"id":7,"text":"x","created":1,"due":2}"#),
            Loaded::Ok(_)
        ));
    }

    #[test]
    fn duplicate_text_ignores_case_and_spacing() {
        let r = Reminder::new(1, "Reply  to Alice".into(), 0, 0);
        assert!(r.same_text("reply to alice "));
        assert!(!r.same_text("reply to bob"));
    }
}
