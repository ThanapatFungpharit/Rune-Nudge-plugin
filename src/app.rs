//! The plugin: events in, actions out.
//!
//! Everything a reminder goes through:
//!
//! ```text
//!  /remind, /followup, form ──▶ Pending ──due──▶ Fired (one-time: overdue until dealt with)
//!                                 ▲   │            │
//!                      snooze/edit│   └─due──▶ next occurrence (repeating: stays Pending)
//!                                 └────────────────┤
//!                                       done/stop ─┴──▶ Done ──(kept up to "Keep done")──▶ gone
//! ```
//!
//! **Firing.** Every event first runs [`Nudge::tick`], which fires whatever is
//! due by the event's clock (`now`). Timers only make sure an event arrives:
//! one `wake` timer for the earliest due time, and a 30-second `beat`, because
//! rune's timers run on a monotonic clock that stops while the computer
//! sleeps, while due times are wall-clock instants.
//!
//! **No duplicates.** A reminder records the occurrence it notified for
//! (`notified`); the same occurrence never alerts twice, whether timers race,
//! the plugin reloads, or rune restarts. The record is stored in the same
//! reply as the notification, before it.
//!
//! **Storage.** One key per reminder (`r.<id>`) plus `meta`. rune takes at
//! most 64 actions from one reply, so writes are queued and flushed in
//! batches (a 1-second `flush` timer continues a long batch).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rune_plugin::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::fmt::{clip, fmt_exact, fmt_rel, fmt_when};
use crate::model::{ChatInfo, Kind, Loaded, MessageCtx, Reminder, State, TEXT_MAX};
use crate::settings::{Missed, Settings};
use crate::when::{self, Cx, Fail, Spec};

mod view;

/// Reminders kept at most (open and done): rune gives a plugin 256 keys.
pub const MAX_REMINDERS: usize = 240;
/// Later than this past its time counts as "went off late".
pub const LATE_MS: i64 = 2 * 60_000;
pub const BEAT_MS: u64 = 30_000;
pub const WAKE: &str = "wake";
pub const BEAT: &str = "beat";
pub const FLUSH: &str = "flush";
pub const VIEW: &str = "reminders";
pub const META: &str = "meta";
pub const PREFIX: &str = "r.";
/// Store writes per reply (rune takes 64 actions from one reply).
const WRITES_PER_REPLY: usize = 40;
/// Separate notifications before they are summed up in one.
const ALERTS_EACH: usize = 3;
/// Chats whose last incoming message is remembered (in memory only).
const CHATS_REMEMBERED: usize = 200;
/// Longest a timer may be (rune's limit is 7 days).
const TIMER_MAX_MS: i64 = 7 * 86_400_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Meta {
    next_id: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Filter {
    #[default]
    All,
    FollowUps,
    Done,
}

impl Filter {
    fn parse(s: &str) -> Option<Filter> {
        Some(match s.trim().to_lowercase().as_str() {
            "" | "all" | "open" | "upcoming" | "overdue" => Filter::All,
            "followups" | "follow-ups" | "followup" | "follow-up" => Filter::FollowUps,
            "done" | "completed" | "finished" => Filter::Done,
            _ => return None,
        })
    }
    fn next(self, back: bool) -> Filter {
        const ORDER: [Filter; 3] = [Filter::All, Filter::FollowUps, Filter::Done];
        let i = ORDER.iter().position(|f| *f == self).unwrap_or(0);
        ORDER[(i + if back { 2 } else { 1 }) % 3]
    }
    pub fn label(self) -> &'static str {
        match self {
            Filter::All => "All",
            Filter::FollowUps => "Follow-ups",
            Filter::Done => "Done",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    List,
    Detail(u32),
    Help,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ViewState {
    pub mode: Mode,
    pub filter: Filter,
    /// The selected reminder (by id, so it stays put as the list changes).
    pub sel: Option<u32>,
    /// The chat open when the view was opened ("a" links new reminders to it).
    pub chat: Option<ChatRef>,
}

/// One step back.
#[derive(Debug, Clone)]
struct Undo {
    what: String,
    before: Vec<(u32, Option<Reminder>)>,
}

/// Something to alert about.
#[derive(Debug, Clone)]
struct Alert {
    title: String,
    body: String,
    line: String,
    /// Came due while rune was closed.
    missed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Show,
    Edit,
    Done,
    Snooze,
    Skip,
    Delete,
}

impl Op {
    fn parse(w: &str) -> Option<Op> {
        Some(match w {
            "show" | "view" | "info" => Op::Show,
            "edit" | "change" => Op::Edit,
            "done" | "complete" | "finish" | "stop" | "x" => Op::Done,
            "snooze" | "later" | "postpone" => Op::Snooze,
            "skip" | "next" => Op::Skip,
            "delete" | "del" | "rm" | "remove" | "cancel" => Op::Delete,
            _ => return None,
        })
    }
}

#[derive(Debug, Default)]
pub struct Nudge {
    pub(crate) rems: BTreeMap<u32, Reminder>,
    next_id: u32,
    pub(crate) s: Settings,
    started: bool,
    pub(crate) view: Option<ViewState>,
    /// The view a form replaced, shown again once the form is submitted.
    resume: Option<ViewState>,
    /// The chat a "new" form links to.
    form_chat: Option<ChatRef>,
    /// What an edit form's "When" said at first (unchanged: keep the timing).
    edit_when: Option<(u32, String)>,
    last_in: HashMap<String, MessageCtx>,
    undo: Option<Undo>,
    wake_at: Option<i64>,
    /// Writes not sent yet.
    to_save: BTreeSet<u32>,
    to_delete: BTreeSet<u32>,
    meta_dirty: bool,
    flush_pending: bool,
    /// Stored records this version cannot read (kept, ids not reused).
    skipped: usize,
    out: Vec<Action>,
    dirty: bool,
    pub(crate) now: i64,
}

impl Plugin for Nudge {
    fn handle(&mut self, cx: &Context, event: Event) -> Vec<Action> {
        let now = cx.now;
        self.now = now;
        match event {
            Event::Init { config, data, .. } => self.init(now, &config, data),
            // Everything is stored as it changes; anything still queued goes now.
            Event::Shutdown => {
                self.flush(60);
                return std::mem::take(&mut self.out);
            }
            ev => {
                if !self.started {
                    return Vec::new();
                }
                self.tick(now, false);
                self.event(now, ev);
            }
        }
        self.after(now);
        std::mem::take(&mut self.out)
    }
}

impl Nudge {
    // -- plumbing ---------------------------------------------------------------------

    fn status(&mut self, text: impl Into<String>) {
        self.out.push(Action::status(text));
    }

    fn save(&mut self, id: u32) {
        self.to_delete.remove(&id);
        self.to_save.insert(id);
        self.dirty = true;
    }

    fn drop_reminder(&mut self, id: u32) -> Option<Reminder> {
        let r = self.rems.remove(&id)?;
        self.to_save.remove(&id);
        self.to_delete.insert(id);
        self.dirty = true;
        Some(r)
    }

    /// Send queued writes, at most `budget`: saves first (they carry the
    /// "already alerted" marks), then deletes.
    fn flush(&mut self, budget: usize) {
        let mut n = 0;
        if self.meta_dirty {
            self.out.push(Action::StoreSet {
                key: META.into(),
                value: serde_json::to_string(&Meta { next_id: self.next_id }).unwrap_or_default(),
            });
            self.meta_dirty = false;
            n += 1;
        }
        while n < budget {
            let Some(id) = self.to_save.pop_first() else { break };
            if let Some(r) = self.rems.get(&id) {
                self.out.push(Action::StoreSet {
                    key: format!("{PREFIX}{id}"),
                    value: r.to_json(),
                });
                n += 1;
            }
        }
        while n < budget {
            let Some(id) = self.to_delete.pop_first() else { break };
            self.out.push(Action::StoreDelete { key: format!("{PREFIX}{id}") });
            n += 1;
        }
    }

    fn cx(&self, now: i64) -> Cx {
        Cx {
            now,
            zone: self.s.zone,
            morning: self.s.morning_hour,
            h12: self.s.h12,
        }
    }

    pub(crate) fn when(&self, t: i64) -> String {
        fmt_when(t, self.now, &self.s.zone, self.s.h12)
    }

    /// "today 15:30 (in 2h)".
    pub(crate) fn when_rel(&self, t: i64) -> String {
        format!("{} ({})", self.when(t), fmt_rel(t, self.now))
    }

    /// Work done after every event: writes, the next wake-up, the view.
    fn after(&mut self, now: i64) {
        // Writes go before notifications (a crash in between means a missed
        // alert, never a repeated one).
        let others = std::mem::take(&mut self.out);
        self.flush(WRITES_PER_REPLY);
        self.out.extend(others);
        let more = !self.to_save.is_empty() || !self.to_delete.is_empty();
        if more && !self.flush_pending {
            self.flush_pending = true;
            self.out.push(Action::SetTimer {
                id: FLUSH.into(),
                after_ms: 1000,
                repeat: false,
            });
        }
        self.schedule(now);
        if self.dirty && self.view.is_some() {
            let v = self.render();
            self.out.push(v);
        }
        self.dirty = false;
    }

    /// Point the `wake` timer at the next thing to do.
    fn schedule(&mut self, now: i64) {
        let nag = i64::from(self.s.nag_minutes) * 60_000;
        let target = self
            .rems
            .values()
            .filter_map(|r| match r.state {
                State::Pending => Some(r.due),
                State::Fired if nag > 0 => Some(r.alerted_at.or(r.fired_at).unwrap_or(r.due) + nag),
                _ => None,
            })
            .min();
        match target {
            None => {
                if self.wake_at.take().is_some() {
                    self.out.push(Action::CancelTimer { id: WAKE.into() });
                }
            }
            Some(t) if self.wake_at != Some(t) => {
                self.wake_at = Some(t);
                self.out.push(Action::SetTimer {
                    id: WAKE.into(),
                    after_ms: (t - now).clamp(1000, TIMER_MAX_MS) as u64,
                    repeat: false,
                });
            }
            Some(_) => {}
        }
    }

    // -- start-up ---------------------------------------------------------------------

    fn init(&mut self, now: i64, config: &Map<String, Value>, data: BTreeMap<String, String>) {
        self.s = Settings::from_config(config);
        self.rems.clear();
        let mut bad = Vec::new();
        let mut max_id = 0;
        let mut meta = Meta::default();
        for (key, value) in &data {
            if key == META {
                meta = serde_json::from_str(value).unwrap_or_default();
                continue;
            }
            let Some(id) = key.strip_prefix(PREFIX).and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            max_id = max_id.max(id);
            match Reminder::load(id, value) {
                Loaded::Ok(r) => {
                    self.rems.insert(id, *r);
                }
                Loaded::Newer => self.skipped += 1,
                Loaded::Bad(why) => {
                    log::warn(&format!("reminder {key} skipped: {why}"));
                    bad.push(id);
                }
            }
        }
        self.skipped += bad.len();
        let next = meta.next_id.max(max_id.saturating_add(1)).max(1);
        if next != meta.next_id {
            self.meta_dirty = true;
        }
        self.next_id = next;
        self.started = true;
        self.out.push(Action::SetTimer {
            id: BEAT.into(),
            after_ms: BEAT_MS,
            repeat: true,
        });
        self.prune_done();
        let fired_before = self.out.len();
        self.tick(now, true);
        let alerted = self.out.len() > fired_before;
        let mut notes = Vec::new();
        // What was missed matters more than hints: those wait for the next start.
        if alerted && self.s.zone_error.is_none() {
            log::info(&format!("{} reminders loaded", self.rems.len()));
            return;
        }
        if let Some(e) = &self.s.zone_error {
            notes.push(format!("time zone setting: {e}; using UTC"));
        } else if self.s.zone_unset && !self.rems.is_empty() {
            notes.push("times are in UTC: set your time zone on Nudge's page in Settings › Plugins".into());
        }
        if !bad.is_empty() {
            notes.push(format!(
                "{} stored reminder(s) could not be read and were skipped (see the log)",
                bad.len()
            ));
        }
        if !alerted && notes.is_empty() {
            let overdue = self.rems.values().filter(|r| r.state == State::Fired).count();
            let next = self
                .rems
                .values()
                .filter(|r| r.state == State::Pending)
                .min_by_key(|r| r.due);
            match (overdue, next) {
                (0, None) => {}
                (0, Some(r)) => notes.push(format!("next: {} {}", clip(&r.text, 40), fmt_rel(r.due, now))),
                (n, _) => notes.push(format!("{n} overdue — Alt-r to see them")),
            }
        }
        if !notes.is_empty() {
            self.status(notes.join(" · "));
        }
        log::info(&format!("{} reminders loaded", self.rems.len()));
    }

    /// Drop the oldest done reminders beyond the "Keep done" setting.
    fn prune_done(&mut self) {
        let mut done: Vec<(i64, u32)> = self
            .rems
            .values()
            .filter(|r| r.state == State::Done)
            .map(|r| (r.done_at.unwrap_or(r.created), r.id))
            .collect();
        if done.len() <= self.s.keep_done {
            return;
        }
        done.sort();
        let extra = done.len() - self.s.keep_done;
        for (_, id) in done.into_iter().take(extra) {
            self.drop_reminder(id);
        }
    }

    // -- firing -----------------------------------------------------------------------

    /// Fire everything due by `now`; repeat alerts for overdue ones.
    pub(crate) fn tick(&mut self, now: i64, startup: bool) {
        let zone = self.s.zone;
        let due: Vec<u32> = self
            .rems
            .values()
            .filter(|r| r.state == State::Pending && r.due <= now)
            .map(|r| r.id)
            .collect();
        let mut alerts = Vec::new();
        for id in due {
            let Some(r) = self.rems.get_mut(&id) else { continue };
            let occ = r.due;
            let late = now - occ > LATE_MS;
            let already = r.notified == Some(occ);
            r.notified = Some(occ);
            r.fired_at = Some(now);
            r.missed = late;
            let mut alert = !already;
            match r.recur.clone() {
                Some(rule) => {
                    let skipped = if late { rule.count_between(&zone, occ, now, 9) } else { 0 };
                    match rule.next_after(&zone, occ, now) {
                        Some(next) => r.due = next,
                        None => {
                            r.state = State::Done;
                            r.done_at = Some(now);
                            r.done_note = Some("no more occurrences".into());
                        }
                    }
                    if late && !self.s.missed_repeats_alert {
                        alert = false;
                    }
                    if alert {
                        let mut a = alert_for(r, now, late);
                        if skipped > 0 {
                            a.body.push_str(&format!(" · {skipped} more missed"));
                            a.line.push_str(&format!(
                                " (missed {}{} times)",
                                skipped + 1,
                                if skipped >= 9 { "+" } else { "" }
                            ));
                        }
                        a.missed = late && startup;
                        alerts.push(a);
                    }
                }
                None => {
                    r.state = State::Fired;
                    r.alerted_at = Some(now);
                    if alert {
                        let mut a = alert_for(r, now, late);
                        a.missed = late && startup;
                        alerts.push(a);
                    }
                }
            }
            self.save(id);
        }
        let nag = i64::from(self.s.nag_minutes) * 60_000;
        if nag > 0 && !startup {
            let ids: Vec<u32> = self
                .rems
                .values()
                .filter(|r| {
                    r.state == State::Fired && now - r.alerted_at.or(r.fired_at).unwrap_or(r.due) >= nag
                })
                .map(|r| r.id)
                .collect();
            for id in ids {
                let Some(r) = self.rems.get_mut(&id) else { continue };
                r.alerted_at = Some(now);
                let mut a = alert_for(r, now, true);
                a.title = format!("Still due: {}", a.title);
                alerts.push(a);
                self.save(id);
            }
        }
        if !alerts.is_empty() {
            self.alert(alerts);
        }
    }

    fn alert(&mut self, alerts: Vec<Alert>) {
        let (missed, timely): (Vec<Alert>, Vec<Alert>) = alerts.into_iter().partition(|a| a.missed);
        let desktop = self.s.desktop();
        let mut lines = Vec::new();
        let notify = |out: &mut Vec<Action>, title: String, body: String| {
            if desktop {
                out.push(Action::Notify { title, body });
            }
        };
        let summary = |list: &[Alert]| {
            list.iter()
                .take(ALERTS_EACH)
                .map(|a| a.line.clone())
                .collect::<Vec<_>>()
                .join(" · ")
                + &if list.len() > ALERTS_EACH {
                    format!(" · and {} more", list.len() - ALERTS_EACH)
                } else {
                    String::new()
                }
        };
        if !timely.is_empty() {
            if timely.len() <= ALERTS_EACH {
                for a in &timely {
                    notify(&mut self.out, a.title.clone(), a.body.clone());
                }
            } else {
                notify(&mut self.out, format!("{} reminders are due", timely.len()), summary(&timely));
            }
            lines.push(if timely.len() == 1 {
                format!("⏰ {}", timely[0].line)
            } else {
                format!("⏰ {} due: {}", timely.len(), summary(&timely))
            });
        }
        if !missed.is_empty() {
            match self.s.missed {
                Missed::Summary => notify(
                    &mut self.out,
                    format!("{} reminder(s) came due while rune was closed", missed.len()),
                    summary(&missed),
                ),
                Missed::Each => {
                    for a in missed.iter().take(ALERTS_EACH) {
                        notify(&mut self.out, a.title.clone(), a.body.clone());
                    }
                    if missed.len() > ALERTS_EACH {
                        notify(
                            &mut self.out,
                            format!("…and {} more came due while rune was closed", missed.len() - ALERTS_EACH),
                            summary(&missed[ALERTS_EACH..]),
                        );
                    }
                }
                Missed::Quiet => {}
            }
            lines.push(format!("missed while closed: {} — Alt-r to see them", summary(&missed)));
        }
        // "Desktop only" still says something on the status line when the
        // desktop gets nothing (missed reminders in "list only" mode).
        if self.s.status_line() || self.s.missed == Missed::Quiet && !missed.is_empty() {
            self.status(lines.join(" · "));
        }
    }

    // -- events -----------------------------------------------------------------------

    fn event(&mut self, now: i64, ev: Event) {
        match ev {
            Event::ConfigChanged { config } => self.config_changed(now, &config),
            Event::Command { name, args, chat } => {
                self.resume = None;
                match name.as_str() {
                    "followup" => self.followup_cmd(now, &args, chat),
                    _ => self.remind_cmd(now, &args, chat),
                }
            }
            Event::Key { id, chat } => {
                self.resume = None;
                match id.as_str() {
                    "followup" => self.followup_cmd(now, "", chat),
                    _ => self.open_view(Mode::List, None, chat),
                }
            }
            Event::ViewKey { key, .. } => self.view_key(now, &key),
            Event::ViewClosed { .. } => self.view = None,
            Event::FormSubmitted { form, values } => {
                self.view = self.resume.take();
                self.form_submitted(now, &form, &values);
            }
            Event::FormCancelled { .. } => {
                self.resume = None;
                self.edit_when = None;
            }
            Event::Message { chat, message } => self.on_message(now, &chat, &message),
            Event::ChatOpened { chat } => self.on_chat_opened(now, &chat),
            Event::Timer { id } => match id.as_str() {
                WAKE => self.wake_at = None,
                FLUSH => self.flush_pending = false,
                _ => {}
            },
            _ => {}
        }
    }

    fn config_changed(&mut self, now: i64, config: &Map<String, Value>) {
        let old_zone = self.s.zone;
        self.s = Settings::from_config(config);
        if let Some(e) = self.s.zone_error.clone() {
            self.status(format!("time zone setting: {e}; using UTC"));
        }
        if self.s.zone != old_zone {
            // Repeating reminders follow the new zone's wall clock from now on;
            // one-time reminders keep their instant.
            let zone = self.s.zone;
            let ids: Vec<u32> = self
                .rems
                .values()
                .filter(|r| {
                    r.state == State::Pending && r.recur.as_ref().is_some_and(|c| c.rule != crate::recur::Rule::Minutes)
                })
                .map(|r| r.id)
                .collect();
            for id in ids {
                let Some(r) = self.rems.get_mut(&id) else { continue };
                let Some(rule) = r.recur.clone() else { continue };
                let prev = r.fired_at.unwrap_or(r.created);
                if let Some(next) = rule.next_after(&zone, prev, prev.max(now)) {
                    r.due = next;
                    self.save(id);
                }
            }
        }
        self.prune_done();
        self.dirty = true;
    }

    fn on_chat_opened(&mut self, _now: i64, chat: &ChatRef) {
        self.rename_chat(chat);
        let pending: Vec<String> = self
            .rems
            .values()
            .filter(|r| r.kind == Kind::FollowUp && r.is_open() && r.chat.as_ref().is_some_and(|c| c.mid == chat.mid))
            .map(|r| {
                if r.state == State::Fired {
                    format!("↩ follow-up #{} is overdue — /followup done when you've replied", r.id)
                } else {
                    format!("↩ follow-up #{} for this chat: {}", r.id, self.when_rel(r.due))
                }
            })
            .collect();
        if let Some(line) = pending.into_iter().next() {
            self.status(line);
        }
    }

    /// Keep stored chat names current.
    fn rename_chat(&mut self, chat: &ChatRef) {
        if chat.name.trim().is_empty() {
            return;
        }
        let ids: Vec<u32> = self
            .rems
            .values()
            .filter(|r| r.chat.as_ref().is_some_and(|c| c.mid == chat.mid && c.name != chat.name))
            .map(|r| r.id)
            .collect();
        for id in ids {
            if let Some(c) = self.rems.get_mut(&id).and_then(|r| r.chat.as_mut()) {
                c.name = chat.name.clone();
            }
            self.save(id);
        }
    }

    fn on_message(&mut self, now: i64, chat: &ChatRef, m: &MessageInfo) {
        self.rename_chat(chat);
        if !m.mine {
            let text = m.text.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(&m.summary);
            self.last_in.insert(
                chat.mid.clone(),
                MessageCtx {
                    id: m.id.clone(),
                    from_name: clip(&m.from_name, 48),
                    summary: clip(text, 160),
                    time: m.time,
                },
            );
            if self.last_in.len() > CHATS_REMEMBERED
                && let Some(oldest) = self.last_in.iter().min_by_key(|(_, c)| c.time).map(|(k, _)| k.clone())
            {
                self.last_in.remove(&oldest);
            }
            return;
        }
        if !self.s.followup_auto_done {
            return;
        }
        let ids: Vec<u32> = self
            .rems
            .values()
            .filter(|r| {
                r.kind == Kind::FollowUp
                    && r.is_open()
                    && r.chat.as_ref().is_some_and(|c| c.mid == chat.mid)
                    && m.time >= r.created
            })
            .map(|r| r.id)
            .collect();
        for id in ids {
            if let Some(r) = self.rems.get_mut(&id) {
                r.state = State::Done;
                r.done_at = Some(now);
                r.done_note = Some("you replied".into());
                let line = format!("✓ follow-up #{id} with {} done — you replied", chat.name);
                self.save(id);
                self.status(line);
            }
        }
        self.prune_done();
    }

    // -- /remind ----------------------------------------------------------------------

    fn remind_cmd(&mut self, now: i64, args: &str, chat: Option<ChatRef>) {
        let args = args.trim();
        if args.is_empty() {
            return self.open_view(Mode::List, None, chat);
        }
        let (verb, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let verb_l = verb.to_lowercase();
        let rest = rest.trim();
        match verb_l.as_str() {
            "help" | "?" | "-h" | "--help" if rest.is_empty() => return self.open_view(Mode::Help, None, chat),
            "list" | "ls" => {
                if let Some(f) = Filter::parse(rest) {
                    return self.open_view(Mode::List, Some(f), chat);
                }
            }
            "new" | "add" if rest.is_empty() => return self.form_new(chat, "", ""),
            "undo" if rest.is_empty() => return self.undo(),
            "clear" if matches!(rest, "done" | "completed") => return self.clear_done(),
            _ => {}
        }
        if let Some(op) = Op::parse(&verb_l) {
            let (id_tok, more) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if let Some(id) = parse_id(id_tok) {
                return self.op(now, op, id, more.trim(), chat);
            }
            if rest.is_empty() {
                return self.status(format!(
                    "which one? /remind {verb_l} NUMBER — the numbers are in /remind list"
                ));
            }
        }
        self.create(now, args, chat);
    }

    fn op(&mut self, now: i64, op: Op, id: u32, more: &str, chat: Option<ChatRef>) {
        if !self.rems.contains_key(&id) {
            return self.status(format!("there is no reminder #{id} — /remind list shows them"));
        }
        match op {
            Op::Show => self.open_view(Mode::Detail(id), None, chat),
            Op::Edit => self.form_edit(id),
            Op::Done => self.done(now, id),
            Op::Snooze => self.snooze(now, id, more),
            Op::Skip => self.skip(now, id),
            Op::Delete => self.delete(id),
        }
    }

    fn create(&mut self, now: i64, args: &str, chat: Option<ChatRef>) {
        let cx = self.cx(now);
        match when::split(args, &cx) {
            Err(msg) => self.status(format!("{msg} — e.g. /remind 2h Call Bob · /remind tomorrow 9am Pay rent")),
            Ok((spec, text)) if text.is_empty() => {
                // A time but no text: ask for the text.
                let _ = spec;
                self.form_new(chat, "", args);
            }
            Ok((spec, text)) => {
                let spec = spec.unwrap_or_else(|| self.default_spec(now, self.s.default_minutes));
                let chat = chat.filter(|_| self.s.link_chat).as_ref().map(ChatInfo::from);
                self.add(now, &text, spec, chat, None, Kind::Reminder);
            }
        }
    }

    fn default_spec(&self, now: i64, minutes: u32) -> Spec {
        Spec::Once {
            due: now + i64::from(minutes) * 60_000,
            note: Some(format!(
                "no time given, so in {} — add one like 2h or tomorrow 9am",
                crate::fmt::fmt_dur(i64::from(minutes) * 60_000)
            )),
        }
    }

    /// Make a reminder. `None` (with a word on the status line) if refused.
    fn add(
        &mut self,
        now: i64,
        text: &str,
        spec: Spec,
        chat: Option<ChatInfo>,
        message: Option<MessageCtx>,
        kind: Kind,
    ) -> Option<u32> {
        let text = clean_text(text);
        if text.is_empty() {
            self.status("a reminder needs some text — /remind 2h Call Bob");
            return None;
        }
        if text.chars().count() > TEXT_MAX {
            self.status(format!("that is too long: {TEXT_MAX} characters at most"));
            return None;
        }
        let first = spec.first();
        let recur = match &spec {
            Spec::Repeat { recur, .. } => Some(recur.clone()),
            Spec::Once { .. } => None,
        };
        let mid = chat.as_ref().map(|c| c.mid.clone());
        if let Some(dup) = self.rems.values().find(|r| {
            r.is_open()
                && r.same_text(&text)
                && r.chat.as_ref().map(|c| &c.mid) == mid.as_ref()
                && match (&r.recur, &recur) {
                    (Some(a), Some(b)) => a.rule == b.rule && a.every == b.every && a.at == b.at,
                    (None, None) => (r.due - first).abs() < 60_000,
                    _ => false,
                }
        }) {
            let msg = format!(
                "#{} already reminds you of that ({}) — /remind edit {} to change it",
                dup.id,
                if dup.state == State::Fired { "overdue".into() } else { self.when(dup.due) },
                dup.id
            );
            self.status(msg);
            return None;
        }
        if self.rems.len() >= MAX_REMINDERS {
            // Make room from the oldest done ones.
            let oldest = self
                .rems
                .values()
                .filter(|r| r.state == State::Done)
                .min_by_key(|r| r.done_at.unwrap_or(r.created))
                .map(|r| r.id);
            match oldest {
                Some(id) => {
                    self.drop_reminder(id);
                }
                None => {
                    self.status(format!(
                        "you have {MAX_REMINDERS} open reminders, the most Nudge can keep — finish or delete some"
                    ));
                    return None;
                }
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.meta_dirty = true;
        let mut r = Reminder::new(id, text, now, first);
        r.kind = kind;
        r.recur = recur;
        r.chat = chat;
        r.message = message;
        let what = clip(&r.text, 40);
        let place = r.chat.as_ref().map(|c| format!(" · {}", c.name)).unwrap_or_default();
        let msg = match &spec {
            Spec::Once { note, due } => {
                let mut m = format!("⏰ #{id} “{what}”{place} — {}", self.when_rel(*due));
                if let Some(n) = note {
                    m.push_str(&format!(" ({n})"));
                }
                m
            }
            Spec::Repeat { recur, first } => format!(
                "↻ #{id} “{what}”{place} — {}, first {}",
                recur.describe(self.s.h12),
                self.when_rel(*first)
            ),
        };
        self.rems.insert(id, r);
        self.save(id);
        self.status(msg);
        if let Some(v) = self.view.as_mut() {
            v.sel = Some(id);
            if let Mode::Detail(_) = v.mode {
                v.mode = Mode::List;
            }
        }
        Some(id)
    }

    // -- /followup --------------------------------------------------------------------

    fn followup_cmd(&mut self, now: i64, args: &str, chat: Option<ChatRef>) {
        let args = args.trim();
        let (verb, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let verb_l = verb.to_lowercase();
        if rest.trim().is_empty() {
            match verb_l.as_str() {
                "list" | "ls" => return self.open_view(Mode::List, Some(Filter::FollowUps), chat),
                "help" | "?" => return self.open_view(Mode::Help, None, chat),
                _ => {}
            }
        }
        let Some(chat) = chat else {
            return self.status("open a chat first — /followup reminds you to reply to the open chat");
        };
        let existing = self
            .rems
            .values()
            .find(|r| r.kind == Kind::FollowUp && r.is_open() && r.chat.as_ref().is_some_and(|c| c.mid == chat.mid))
            .map(|r| r.id);
        if rest.trim().is_empty() {
            match (verb_l.as_str(), existing) {
                ("done" | "replied", Some(id)) => return self.done(now, id),
                ("cancel" | "rm" | "delete" | "remove", Some(id)) => return self.delete(id),
                ("done" | "replied" | "cancel" | "rm" | "delete" | "remove", None) => {
                    return self.status(format!("no open follow-up for {}", chat.name));
                }
                _ => {}
            }
        }
        let cx = self.cx(now);
        let (spec, note) = match when::split(args, &cx) {
            Err(msg) => return self.status(format!("{msg} — e.g. /followup 2h · /followup tomorrow 9am")),
            Ok((spec, note)) => (spec, note),
        };
        // No time is the usual case for a follow-up: no "no time given" note.
        let spec = spec.unwrap_or(Spec::Once {
            due: now + i64::from(self.s.followup_minutes) * 60_000,
            note: None,
        });
        let Spec::Once { due, note: time_note } = spec else {
            return self.status("follow-ups do not repeat — use /remind every … for that");
        };
        let message = if self.s.quote_message {
            self.last_in.get(&chat.mid).cloned()
        } else {
            None
        };
        if let Some(id) = existing {
            self.snapshot("moving a follow-up", &[id]);
            if let Some(r) = self.rems.get_mut(&id) {
                r.due = due;
                r.state = State::Pending;
                r.notified = None;
                r.missed = false;
                r.alerted_at = None;
                if !note.is_empty() {
                    r.text = clean_text(&note);
                }
                if message.is_some() {
                    r.message = message;
                }
            }
            self.save(id);
            let line = format!("↩ follow-up #{id} with {} moved to {}", chat.name, self.when_rel(due));
            return self.status(line);
        }
        let text = if note.is_empty() { format!("Reply to {}", chat.name) } else { note };
        self.add(
            now,
            &text,
            Spec::Once { due, note: time_note },
            Some(ChatInfo::from(&chat)),
            message,
            Kind::FollowUp,
        );
    }

    // -- operations -------------------------------------------------------------------

    fn snapshot(&mut self, what: &str, ids: &[u32]) {
        self.undo = Some(Undo {
            what: what.into(),
            before: ids.iter().map(|id| (*id, self.rems.get(id).cloned())).collect(),
        });
    }

    fn done(&mut self, now: i64, id: u32) {
        let Some(r) = self.rems.get(&id) else { return };
        if r.state == State::Done {
            return self.status(format!("#{id} is already done"));
        }
        let repeating = r.recur.is_some();
        let what = clip(&r.text, 40);
        self.snapshot(&format!("finishing #{id}"), &[id]);
        if let Some(r) = self.rems.get_mut(&id) {
            r.state = State::Done;
            r.done_at = Some(now);
            r.done_note = None;
        }
        self.save(id);
        self.prune_done();
        self.status(if repeating {
            format!("✓ #{id} “{what}” won't repeat any more (u in the list or /remind undo to undo)")
        } else {
            format!("✓ done: #{id} “{what}” (u in the list or /remind undo to undo)")
        });
    }

    fn snooze(&mut self, now: i64, id: u32, until: &str) {
        let Some(r) = self.rems.get(&id) else { return };
        if r.state == State::Done {
            return self.status(format!("#{id} is done — /remind edit {id} gives it a new time"));
        }
        let due = if until.trim().is_empty() {
            now + i64::from(self.s.snooze_minutes) * 60_000
        } else {
            match when::parse(until, &self.cx(now)) {
                Ok(Spec::Once { due, .. }) => due,
                Ok(Spec::Repeat { .. }) => {
                    return self.status("snooze until a time, like 30m or tomorrow 9am");
                }
                Err(Fail::Bad(m)) => return self.status(m),
                Err(Fail::NoMatch) => {
                    return self.status(format!("can't read “{}” as a time — try 10m, 2h or tomorrow 9am", clip(until, 30)));
                }
            }
        };
        self.snapshot(&format!("snoozing #{id}"), &[id]);
        if let Some(r) = self.rems.get_mut(&id) {
            r.due = due;
            r.state = State::Pending;
            r.snoozes = r.snoozes.saturating_add(1);
            r.notified = None;
            r.missed = false;
            r.alerted_at = None;
        }
        self.save(id);
        let msg = format!("💤 #{id} snoozed until {}", self.when_rel(due));
        self.status(msg);
    }

    fn skip(&mut self, now: i64, id: u32) {
        let Some(r) = self.rems.get(&id) else { return };
        let Some(rule) = r.recur.clone().filter(|_| r.is_open()) else {
            return self.status(format!("#{id} does not repeat — snooze, edit or finish it instead"));
        };
        let Some(next) = rule.next_after(&self.s.zone, r.due, r.due.max(now)) else { return };
        self.snapshot(&format!("skipping #{id}"), &[id]);
        if let Some(r) = self.rems.get_mut(&id) {
            r.due = next;
            r.notified = None;
        }
        self.save(id);
        let msg = format!("⏭ #{id} skipped — next {}", self.when_rel(next));
        self.status(msg);
    }

    fn delete(&mut self, id: u32) {
        self.snapshot(&format!("deleting #{id}"), &[id]);
        if let Some(r) = self.drop_reminder(id) {
            self.status(format!(
                "deleted #{id} “{}” (u in the list or /remind undo to undo)",
                clip(&r.text, 40)
            ));
        }
    }

    fn clear_done(&mut self) {
        let ids: Vec<u32> = self.rems.values().filter(|r| r.state == State::Done).map(|r| r.id).collect();
        if ids.is_empty() {
            return self.status("nothing done to clear");
        }
        self.snapshot(&format!("clearing {} done", ids.len()), &ids);
        for id in &ids {
            self.drop_reminder(*id);
        }
        self.status(format!("cleared {} done reminder(s) (/remind undo to undo)", ids.len()));
    }

    fn undo(&mut self) {
        let Some(u) = self.undo.take() else {
            return self.status("nothing to undo");
        };
        for (id, before) in u.before {
            match before {
                Some(r) => {
                    self.rems.insert(id, r);
                    self.save(id);
                }
                None => {
                    self.drop_reminder(id);
                }
            }
        }
        self.status(format!("undid {}", u.what));
    }

    // -- forms ------------------------------------------------------------------------

    fn open_form(&mut self, id: String, title: &str, fields: Vec<FormField>) {
        if self.view.is_some() {
            self.resume = self.view.take();
        }
        self.out.push(Action::Form {
            id,
            title: title.into(),
            fields,
        });
    }

    fn form_new(&mut self, chat: Option<ChatRef>, text: &str, when: &str) {
        let chat = chat.filter(|_| self.s.link_chat);
        let mut fields = vec![
            FormField::new("What", text, "e.g. Call the bank"),
            FormField::new("When", when, "2h · tomorrow 9am · fri 17:00 · every mon 10am"),
        ];
        if let Some(c) = &chat {
            fields.push(FormField::new("Chat", c.name.as_str(), "linked to this chat; clear to unlink"));
        }
        self.form_chat = chat;
        self.open_form("new".into(), "New reminder", fields);
    }

    fn form_edit(&mut self, id: u32) {
        let Some(r) = self.rems.get(&id) else { return };
        let when = match &r.recur {
            Some(rule) => rule.describe(self.s.h12),
            None => fmt_exact(r.due, &self.s.zone),
        };
        let mut fields = vec![
            FormField::new("What", r.text.as_str(), ""),
            FormField::new("When", when.as_str(), "2h · tomorrow 9am · 2026-12-01 09:00 · every day 9am"),
        ];
        if let Some(c) = &r.chat {
            fields.push(FormField::new("Chat", c.name.as_str(), "clear to unlink"));
        }
        self.edit_when = Some((id, when));
        self.open_form(format!("edit:{id}"), &format!("Edit reminder #{id}"), fields);
    }

    fn form_snooze(&mut self, id: u32) {
        if !self.rems.get(&id).is_some_and(Reminder::is_open) {
            return self.status(format!("#{id} is done — e gives it a new time"));
        }
        let hint = format!("10m · 1h · tomorrow 9am (empty: {}m)", self.s.snooze_minutes);
        self.open_form(
            format!("snooze:{id}"),
            &format!("Snooze #{id} until"),
            vec![FormField::new("Until", "", hint)],
        );
    }

    /// Read a form's "When": empty is the default delay.
    fn form_when(&self, now: i64, when: &str) -> Result<Spec, String> {
        if when.trim().is_empty() {
            return Ok(self.default_spec(now, self.s.default_minutes));
        }
        match when::parse(when, &self.cx(now)) {
            Ok(s) => Ok(s),
            Err(Fail::Bad(m)) => Err(m),
            Err(Fail::NoMatch) => Err(format!(
                "can't read “{}” as a time — try 2h, tomorrow 9am, fri 17:00 or every mon 10am",
                clip(when, 30)
            )),
        }
    }

    fn form_submitted(&mut self, now: i64, form: &str, values: &[String]) {
        let get = |i: usize| values.get(i).map_or("", |s| s.as_str());
        if form == "new" {
            let (text, when, chat) = (get(0), get(1), get(2));
            if clean_text(text).is_empty() {
                self.status("a reminder needs some text");
                let c = self.form_chat.take();
                return self.form_new(c, text, when);
            }
            let spec = match self.form_when(now, when) {
                Ok(s) => s,
                Err(m) => {
                    self.status(m);
                    let c = self.form_chat.take();
                    return self.form_new(c, text, when);
                }
            };
            let chat = self
                .form_chat
                .take()
                .filter(|_| !chat.trim().is_empty())
                .as_ref()
                .map(ChatInfo::from);
            self.add(now, text, spec, chat, None, Kind::Reminder);
        } else if let Some(id) = form.strip_prefix("edit:").and_then(|s| s.parse::<u32>().ok()) {
            self.edit_submitted(now, id, get(0), get(1), values.get(2).map(|s| s.as_str()));
        } else if let Some(id) = form.strip_prefix("snooze:").and_then(|s| s.parse::<u32>().ok())
            && self.rems.contains_key(&id) {
                self.snooze(now, id, get(0));
            }
    }

    fn edit_submitted(&mut self, now: i64, id: u32, text: &str, when: &str, chat: Option<&str>) {
        let prefill = self.edit_when.take().filter(|(i, _)| *i == id).map(|(_, w)| w);
        let Some(r) = self.rems.get(&id) else {
            return self.status(format!("#{id} is gone"));
        };
        let text = clean_text(text);
        let reopen = |me: &mut Self, msg: String| {
            me.status(msg);
            me.form_edit(id);
        };
        if text.is_empty() || text.chars().count() > TEXT_MAX {
            return reopen(self, format!("the text must be 1–{TEXT_MAX} characters"));
        }
        let timing_changed = prefill.as_deref().is_none_or(|p| p.trim() != when.trim());
        let spec = if timing_changed {
            match self.form_when(now, when) {
                Ok(s) => Some(s),
                Err(m) => return reopen(self, m),
            }
        } else {
            None
        };
        if let Some(Spec::Repeat { .. }) = spec
            && r.kind == Kind::FollowUp
        {
            return reopen(self, "follow-ups do not repeat".into());
        }
        self.snapshot(&format!("editing #{id}"), &[id]);
        let Some(r) = self.rems.get_mut(&id) else { return };
        r.text = text;
        if chat.is_some_and(|c| c.trim().is_empty()) {
            r.chat = None;
            r.message = None;
            r.kind = Kind::Reminder;
        }
        if let Some(spec) = spec {
            r.due = spec.first();
            r.recur = match spec {
                Spec::Repeat { recur, .. } => Some(recur),
                Spec::Once { .. } => None,
            };
            r.state = State::Pending;
            r.notified = None;
            r.missed = false;
            r.alerted_at = None;
            r.done_at = None;
            r.done_note = None;
        }
        let due = r.due;
        let open = r.is_open();
        self.save(id);
        let msg = if open {
            format!("saved #{id} — {}", self.when_rel(due))
        } else {
            format!("saved #{id}")
        };
        self.status(msg);
    }

    // -- the view ---------------------------------------------------------------------

    fn open_view(&mut self, mode: Mode, filter: Option<Filter>, chat: Option<ChatRef>) {
        let prev = self.view.take();
        let filter = filter.or(prev.as_ref().map(|v| v.filter)).unwrap_or_default();
        let sel = match mode {
            Mode::Detail(id) => Some(id),
            _ => prev.and_then(|v| v.sel),
        };
        self.view = Some(ViewState { mode, filter, sel, chat });
        self.dirty = true;
    }

    fn view_key(&mut self, now: i64, key: &str) {
        let Some(v) = self.view.clone() else { return };
        self.dirty = true;
        let back = |me: &mut Self| {
            if let Some(v) = me.view.as_mut() {
                v.mode = Mode::List;
            }
        };
        match v.mode {
            Mode::Help => {
                if matches!(key, "Backspace" | "Left" | "Enter" | "h" | "q") {
                    back(self);
                }
            }
            Mode::Detail(id) => match key {
                "Backspace" | "Left" | "q" => back(self),
                _ => self.act_on(now, id, key),
            },
            Mode::List => {
                let ids = self.visible_ids(v.filter);
                let pos = v.sel.and_then(|s| ids.iter().position(|i| *i == s)).unwrap_or(0);
                let set = |me: &mut Self, p: usize| {
                    if let Some(vs) = me.view.as_mut() {
                        vs.sel = ids.get(p).copied();
                    }
                };
                match key {
                    "Up" | "k" => set(self, pos.saturating_sub(1)),
                    "Down" | "j" => set(self, (pos + 1).min(ids.len().saturating_sub(1))),
                    "Home" => set(self, 0),
                    "End" => set(self, ids.len().saturating_sub(1)),
                    "Tab" | "Shift-Tab" => {
                        if let Some(vs) = self.view.as_mut() {
                            vs.filter = vs.filter.next(key == "Shift-Tab");
                        }
                    }
                    "h" => {
                        if let Some(vs) = self.view.as_mut() {
                            vs.mode = Mode::Help;
                        }
                    }
                    "a" => {
                        let chat = v.chat.clone();
                        self.form_new(chat, "", "");
                    }
                    "u" => self.undo(),
                    _ => {
                        let Some(id) = ids.get(pos).copied() else { return };
                        if key == "Enter" || key == "Right" {
                            if let Some(vs) = self.view.as_mut() {
                                vs.mode = Mode::Detail(id);
                                vs.sel = Some(id);
                            }
                        } else {
                            self.act_on(now, id, key);
                            // Keep the selection on the same row after a removal.
                            let ids_after = self.visible_ids(v.filter);
                            let removed = matches!(key, "x" | "d" | "Delete");
                            if let Some(vs) = self.view.as_mut()
                                && (removed || vs.sel.is_none_or(|s| !ids_after.contains(&s)))
                            {
                                vs.sel = ids_after.get(pos.min(ids_after.len().saturating_sub(1))).copied();
                            }
                        }
                    }
                }
            }
        }
    }

    /// A key on one reminder (list or detail).
    fn act_on(&mut self, now: i64, id: u32, key: &str) {
        match key {
            "e" => self.form_edit(id),
            "x" => self.done(now, id),
            "s" => self.snooze(now, id, ""),
            "z" => self.form_snooze(id),
            "n" => self.skip(now, id),
            "d" | "Delete" => {
                self.delete(id);
                if let Some(v) = self.view.as_mut()
                    && v.mode == Mode::Detail(id)
                {
                    v.mode = Mode::List;
                }
            }
            "u" => self.undo(),
            _ => self.dirty = false,
        }
    }
}

/// `3` or `#3`.
fn parse_id(s: &str) -> Option<u32> {
    let s = s.strip_prefix('#').unwrap_or(s);
    (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// One line, no control characters, single spaces.
fn clean_text(s: &str) -> String {
    s.split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn alert_for(r: &Reminder, now: i64, late: bool) -> Alert {
    let chat = r.chat.as_ref().map(|c| c.name.as_str());
    let title = match (r.kind, chat) {
        (Kind::FollowUp, Some(c)) => format!("Follow up: {c}"),
        (_, Some(c)) => format!("Reminder · {c}"),
        _ => "Reminder".into(),
    };
    let mut body = clip(&r.text, 200);
    if let Some(m) = &r.message {
        body.push_str(&format!(" — {}: “{}”", m.from_name, clip(&m.summary, 60)));
    }
    if late {
        body.push_str(&format!(" (was due {})", fmt_rel(r.due.min(r.notified.unwrap_or(r.due)), now)));
    }
    let mut line = format!("#{} {}", r.id, clip(&r.text, 50));
    if let Some(c) = chat {
        line.push_str(&format!(" · {c}"));
    }
    Alert {
        title,
        body,
        line,
        missed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_text() {
        assert_eq!(parse_id("#12"), Some(12));
        assert_eq!(parse_id("3"), Some(3));
        assert_eq!(parse_id("x3"), None);
        assert_eq!(parse_id(""), None);
        assert_eq!(clean_text("  a \n b\tc "), "a b c");
        assert_eq!(Filter::parse("follow-ups"), Some(Filter::FollowUps));
        assert_eq!(Filter::All.next(false), Filter::FollowUps);
        assert_eq!(Filter::All.next(true), Filter::Done);
    }
}
