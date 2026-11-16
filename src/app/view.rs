//! Drawing the reminders view: the list (overdue, upcoming, done), one
//! reminder's details, and the help page.

use rune_plugin::prelude::*;

use super::{Filter, Mode, Nudge, VIEW};
use crate::fmt::{clip, fmt_dur, fmt_rel};
use crate::model::{Kind, Reminder, State};

/// Done reminders shown under "All" and "Follow-ups".
const DONE_SHOWN: usize = 10;
/// Width the "when" column is padded to.
const WHEN_W: usize = 24;
/// Wrap width for long text in the details.
const WRAP: usize = 72;

impl Nudge {
    fn overdue(&self, f: Filter) -> Vec<&Reminder> {
        let mut v: Vec<&Reminder> = self
            .rems
            .values()
            .filter(|r| r.state == State::Fired && self.in_filter(r, f))
            .collect();
        v.sort_by_key(|r| (r.due, r.id));
        v
    }

    fn upcoming(&self, f: Filter) -> Vec<&Reminder> {
        let mut v: Vec<&Reminder> = self
            .rems
            .values()
            .filter(|r| r.state == State::Pending && self.in_filter(r, f))
            .collect();
        v.sort_by_key(|r| (r.due, r.id));
        v
    }

    fn done_list(&self, f: Filter) -> Vec<&Reminder> {
        let mut v: Vec<&Reminder> = self
            .rems
            .values()
            .filter(|r| r.state == State::Done && self.in_filter(r, f))
            .collect();
        v.sort_by_key(|r| std::cmp::Reverse((r.done_at.unwrap_or(r.created), r.id)));
        if f != Filter::Done {
            v.truncate(DONE_SHOWN);
        }
        v
    }

    fn in_filter(&self, r: &Reminder, f: Filter) -> bool {
        match f {
            Filter::All => true,
            Filter::FollowUps => r.kind == Kind::FollowUp,
            Filter::Done => r.state == State::Done,
        }
    }

    /// The reminders in the list, top to bottom.
    pub(super) fn visible_ids(&self, f: Filter) -> Vec<u32> {
        let mut ids = Vec::new();
        if f != Filter::Done {
            ids.extend(self.overdue(f).iter().map(|r| r.id));
            ids.extend(self.upcoming(f).iter().map(|r| r.id));
        }
        ids.extend(self.done_list(f).iter().map(|r| r.id));
        ids
    }

    fn mark(r: &Reminder) -> &'static str {
        match (r.state, r.kind, r.recur.is_some()) {
            (State::Done, ..) => "✓",
            (State::Fired, ..) => "!",
            (_, _, true) => "↻",
            (_, Kind::FollowUp, _) => "↩",
            _ => "·",
        }
    }

    fn row(&self, r: &Reminder) -> ViewLine {
        let when = match r.state {
            State::Fired => format!("due {}", fmt_rel(r.due, self.now)),
            State::Pending if (r.due - self.now).abs() < 48 * 3_600_000 => self.when_rel(r.due),
            State::Pending => self.when(r.due),
            State::Done => format!("done {}", self.when(r.done_at.unwrap_or(r.created))),
        };
        let mut text = clip(&r.text, 80);
        if let Some(c) = &r.chat
            && !(r.kind == Kind::FollowUp && r.text.contains(c.name.as_str()))
        {
            text.push_str(&format!("  · {}", clip(&c.name, 30)));
        }
        if let Some(rule) = &r.recur
            && r.state != State::Done
        {
            text.push_str(&format!("  · {}", rule.describe(self.s.h12)));
        }
        let style = match r.state {
            State::Fired => LineStyle::Danger,
            State::Done => LineStyle::Muted,
            State::Pending => LineStyle::Normal,
        };
        ViewLine::styled(
            format!("{} #{:<4}{:<WHEN_W$} {text}", Self::mark(r), r.id, when),
            style,
        )
    }

    pub(super) fn render(&self) -> Action {
        let Some(v) = &self.view else {
            return Action::CloseView { id: VIEW.into() };
        };
        match v.mode {
            Mode::List => self.render_list(v.filter, v.sel),
            Mode::Detail(id) if self.rems.contains_key(&id) => self.render_detail(id),
            Mode::Detail(_) => self.render_list(v.filter, v.sel),
            Mode::Help => self.render_help(),
        }
    }

    fn render_list(&self, f: Filter, sel: Option<u32>) -> Action {
        let mut lines: Vec<ViewLine> = Vec::new();
        let mut ids: Vec<Option<u32>> = Vec::new();
        let mut push = |line: ViewLine, id: Option<u32>| {
            lines.push(line);
            ids.push(id);
        };
        let (overdue, upcoming, done) = if f == Filter::Done {
            (Vec::new(), Vec::new(), self.done_list(f))
        } else {
            (self.overdue(f), self.upcoming(f), self.done_list(f))
        };
        push(
            ViewLine::styled(
                format!(
                    "{}   Tab: all · follow-ups · done   — times in {}",
                    match f {
                        Filter::All => "[All]  Follow-ups   Done ",
                        Filter::FollowUps => " All  [Follow-ups]  Done ",
                        Filter::Done => " All   Follow-ups  [Done]",
                    },
                    self.s.zone.name()
                ),
                LineStyle::Muted,
            ),
            None,
        );
        if overdue.is_empty() && upcoming.is_empty() && done.is_empty() {
            push(ViewLine::new(""), None);
            let empty = match f {
                Filter::All => "No reminders yet.",
                Filter::FollowUps => "No follow-ups. In a chat, /followup or Alt-f reminds you to reply.",
                Filter::Done => "Nothing done yet.",
            };
            push(ViewLine::styled(empty, LineStyle::Muted), None);
            push(ViewLine::new(""), None);
            for ex in [
                "a                          add one here",
                "/remind 2h Reply to Alice  in two hours",
                "/remind tomorrow 9am Pay rent",
                "/remind every weekday 9:00 Standup",
                "/followup                  reply to the open chat later",
                "h                          everything Nudge understands",
            ] {
                push(ViewLine::styled(ex, LineStyle::Muted), None);
            }
        }
        for (title, list) in [("Overdue", overdue), ("Upcoming", upcoming), ("Done", done)] {
            if list.is_empty() {
                continue;
            }
            push(ViewLine::new(""), None);
            let heading = if title == "Done" && f != Filter::Done {
                format!("Done · latest {}", list.len())
            } else {
                format!("{title} · {}", list.len())
            };
            push(ViewLine::styled(heading, LineStyle::Heading), None);
            for r in list {
                push(self.row(r), Some(r.id));
            }
        }
        let selected = sel
            .and_then(|s| ids.iter().position(|i| *i == Some(s)))
            .or_else(|| ids.iter().position(Option::is_some));
        let overdue_n = self.rems.values().filter(|r| r.state == State::Fired).count();
        let upcoming_n = self.rems.values().filter(|r| r.state == State::Pending).count();
        let mut title = format!("Reminders · {upcoming_n} upcoming");
        if overdue_n > 0 {
            title = format!("Reminders · {overdue_n} overdue · {upcoming_n} upcoming");
        }
        Action::OpenView {
            id: VIEW.into(),
            title,
            lines,
            selected,
            keys: vec![
                KeyHelp::new("Enter", "details"),
                KeyHelp::new("a", "add"),
                KeyHelp::new("e", "edit"),
                KeyHelp::new("x", "done"),
                KeyHelp::new("s", "snooze"),
                KeyHelp::new("z", "snooze until…"),
                KeyHelp::new("n", "skip (repeating)"),
                KeyHelp::new("d", "delete"),
                KeyHelp::new("u", "undo"),
                KeyHelp::new("Tab", "filter"),
                KeyHelp::new("h", "help"),
            ],
        }
    }

    fn render_detail(&self, id: u32) -> Action {
        let r = &self.rems[&id];
        let mut lines = Vec::new();
        let field = |lines: &mut Vec<ViewLine>, label: &str, value: String, style: LineStyle| {
            lines.push(ViewLine::styled(format!("{label:<10}{value}"), style));
        };
        for (i, chunk) in wrap(&r.text, WRAP).into_iter().enumerate() {
            let prefix = if i == 0 { format!("#{id}  ") } else { " ".repeat(format!("#{id}  ").len()) };
            lines.push(ViewLine::styled(format!("{prefix}{chunk}"), LineStyle::Bold));
        }
        lines.push(ViewLine::new(""));
        match r.state {
            State::Pending => field(&mut lines, "Due", self.when_rel(r.due), LineStyle::Accent),
            State::Fired => field(
                &mut lines,
                "Overdue",
                format!(
                    "was due {}{}",
                    self.when_rel(r.due),
                    if r.missed { " — went off late (rune was closed or asleep)" } else { "" }
                ),
                LineStyle::Danger,
            ),
            State::Done => field(
                &mut lines,
                "Done",
                format!(
                    "{}{}",
                    self.when(r.done_at.unwrap_or(r.created)),
                    r.done_note.as_ref().map(|n| format!(" ({n})")).unwrap_or_default()
                ),
                LineStyle::Muted,
            ),
        }
        if let Some(rule) = &r.recur {
            field(&mut lines, "Repeats", rule.describe(self.s.h12), LineStyle::Normal);
            if let Some(t) = r.fired_at {
                field(&mut lines, "Last", self.when(t), LineStyle::Muted);
            }
        }
        if let Some(c) = &r.chat {
            let what = if r.kind == Kind::FollowUp { "follow-up for the" } else { "linked to the" };
            field(
                &mut lines,
                "Chat",
                format!("{} ({what} {})", c.name, c.kind_word()),
                LineStyle::Normal,
            );
        }
        if let Some(m) = &r.message {
            field(
                &mut lines,
                "Message",
                format!("{}: “{}” · {}", m.from_name, m.summary, self.when(m.time)),
                LineStyle::Normal,
            );
        }
        if r.kind == Kind::FollowUp && r.is_open() {
            field(
                &mut lines,
                "",
                if self.s.followup_auto_done {
                    "Finishes by itself when you reply in that chat.".into()
                } else {
                    "Mark it done (x) once you've replied.".into()
                },
                LineStyle::Muted,
            );
        }
        field(&mut lines, "Created", self.when(r.created), LineStyle::Muted);
        if r.snoozes > 0 {
            field(
                &mut lines,
                "Snoozed",
                format!("{} time{}", r.snoozes, if r.snoozes == 1 { "" } else { "s" }),
                LineStyle::Muted,
            );
        }
        lines.push(ViewLine::new(""));
        lines.push(ViewLine::styled(
            format!("Times in {} · Backspace: back to the list", self.s.zone.name()),
            LineStyle::Muted,
        ));
        let mut keys = vec![KeyHelp::new("e", "edit")];
        if r.is_open() {
            keys.push(KeyHelp::new("x", if r.recur.is_some() { "stop repeating" } else { "done" }));
            keys.push(KeyHelp::new("s", "snooze"));
            keys.push(KeyHelp::new("z", "snooze until…"));
        }
        if r.recur.is_some() && r.is_open() {
            keys.push(KeyHelp::new("n", "skip to next"));
        }
        keys.push(KeyHelp::new("d", "delete"));
        keys.push(KeyHelp::new("Backspace", "back"));
        Action::OpenView {
            id: VIEW.into(),
            title: format!(
                "Reminder #{id}{}",
                if r.kind == Kind::FollowUp { " · follow-up" } else { "" }
            ),
            lines,
            selected: None,
            keys,
        }
    }

    fn render_help(&self) -> Action {
        let s = &self.s;
        let h = |t: &str| ViewLine::styled(t, LineStyle::Heading);
        let n = |t: String| ViewLine::new(t);
        let m = |t: &str| ViewLine::styled(t, LineStyle::Muted);
        let lines = vec![
            h("Make a reminder"),
            n("/remind WHEN TEXT          /remind 2h Reply to Alice".into()),
            n("/remind TEXT WHEN          /remind Call mom tomorrow 6pm".into()),
            n(format!(
                "/remind TEXT               no time: in {}",
                fmt_dur(i64::from(s.default_minutes) * 60_000)
            )),
            n("/remind new                a form (also: a in the list)".into()),
            m(if s.link_chat {
                "Made in a chat, a reminder is linked to it (setting: Link /remind to the open chat)."
            } else {
                "Reminders are not linked to the open chat (setting: Link /remind to the open chat)."
            }),
            n(String::new()),
            h("Times"),
            n("in 2h · 1h30m · 45 min · an hour · in 3 days · in 2 weeks · in 1 month".into()),
            n("15:30 · 3pm · at 9 · noon · midnight · tonight · this evening".into()),
            n(format!(
                "tomorrow · fri · next mon · next week · weekend  (no time: {:02}:00)",
                s.morning_hour
            )),
            n("tomorrow 9am · fri 17:00 · 2026-12-01 09:30 · dec 5 · 5 dec 2027 3pm".into()),
            m("A clock time that has passed today means tomorrow; a past date is refused."),
            n(String::new()),
            h("Repeat"),
            n("every day at 9 · daily 18:00 · every weekday 9:00 · every mon,wed 10am".into()),
            n("every 2 weeks on fri at 17:00 · every month on the 1st · yearly".into()),
            n("every 30 minutes · every 2 hours · hourly".into()),
            m("Missed repeats are not replayed: one alert, then the next occurrence."),
            n(String::new()),
            h("Follow-ups (in a chat)"),
            n(format!(
                "/followup [WHEN] [NOTE]    remind me to reply here (default {})",
                fmt_dur(i64::from(s.followup_minutes) * 60_000)
            )),
            n("/followup done · /followup cancel · /followup list · Alt-f".into()),
            m(if s.followup_auto_done {
                "A follow-up finishes by itself when you send a message in that chat."
            } else {
                "Finish follow-ups with x or /followup done."
            }),
            n(String::new()),
            h("Manage"),
            n("/remind · /remind list [done|follow-ups] · Alt-r".into()),
            n("/remind show|edit|done|delete N · /remind snooze N [WHEN] · /remind skip N".into()),
            n("/remind undo · /remind clear done".into()),
            n(String::new()),
            h("Settings"),
            n(format!(
                "Time zone: {}{}",
                s.zone.name(),
                if s.zone_unset { " (not set — Settings › Plugins › Nudge)" } else { "" }
            )),
            m("Alerts, snooze length, defaults, missed reminders: Settings › Plugins › Nudge."),
            m("Nudge never sends messages for you."),
        ];
        Action::OpenView {
            id: VIEW.into(),
            title: "Nudge · help".into(),
            lines,
            selected: None,
            keys: vec![KeyHelp::new("Backspace", "back to the list")],
        }
    }
}

/// Wrap at word boundaries to lines of at most `w` characters.
fn wrap(s: &str, w: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in s.split_whitespace() {
        let len = line.chars().count();
        if len > 0 && len + 1 + word.chars().count() > w {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() || out.is_empty() {
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_words() {
        assert_eq!(wrap("aa bb cc", 5), ["aa bb", "cc"]);
        assert_eq!(wrap("", 5), [""]);
    }
}
