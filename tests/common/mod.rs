//! A stand-in for rune's plugin host: it keeps the plugin's storage and
//! timers, applies its actions with the host's rules (forms and new views
//! only in answer to something the user did, at most 64 actions taken per
//! reply), and can restart the plugin from storage like quitting rune would.

#![allow(dead_code)]

use std::collections::BTreeMap;

use chrono::NaiveDateTime;
use nudge::Nudge;
use rune_plugin::prelude::*;
use rune_plugin::{Manifest, Plugin};
use serde_json::{Map, Value};

pub const MANIFEST: &str = include_str!("../../plugin.json");

pub fn manifest() -> Manifest {
    Manifest::parse(MANIFEST).expect("plugin.json is valid")
}

pub fn chat(mid: &str, name: &str) -> ChatRef {
    ChatRef {
        mid: mid.into(),
        name: name.into(),
        kind: if mid.starts_with('c') { ChatKind::Group } else { ChatKind::User },
    }
}

#[derive(Debug, Clone)]
pub struct Timer {
    pub at: i64,
    pub every: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct View {
    pub title: String,
    pub lines: Vec<ViewLine>,
    pub selected: Option<usize>,
}

impl View {
    pub fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }
    pub fn selected_line(&self) -> &str {
        self.selected.map_or("", |i| self.lines[i].text.as_str())
    }
}

pub struct Host {
    pub plugin: Nudge,
    pub now: i64,
    pub config: Map<String, Value>,
    pub data: BTreeMap<String, String>,
    pub timers: BTreeMap<String, Timer>,
    pub notes: Vec<(String, String)>,
    pub statuses: Vec<String>,
    pub view: Option<View>,
    pub form: Option<(String, Vec<FormField>)>,
    /// Largest reply seen (actions).
    pub max_reply: usize,
}

impl Host {
    /// A host whose clock reads `local` ("2026-11-16 10:00") in `tz`.
    pub fn new(tz: &str, local: &str) -> Host {
        let mut config = manifest().default_config();
        config.insert("timezone".into(), Value::String(tz.into()));
        let zone = nudge::zone::Zone::parse(tz).unwrap_or_default();
        Host {
            plugin: Nudge::default(),
            now: zone.instant(NaiveDateTime::parse_from_str(local, "%Y-%m-%d %H:%M").unwrap()),
            config,
            data: BTreeMap::new(),
            timers: BTreeMap::new(),
            notes: Vec::new(),
            statuses: Vec::new(),
            view: None,
            form: None,
            max_reply: 0,
        }
    }

    pub fn set(&mut self, key: &str, v: Value) -> &mut Self {
        self.config.insert(key.into(), v);
        self
    }

    pub fn zone(&self) -> nudge::zone::Zone {
        nudge::zone::Zone::parse(self.config["timezone"].as_str().unwrap()).unwrap_or_default()
    }

    pub fn local(&self) -> String {
        self.zone().local(self.now).format("%Y-%m-%d %H:%M").to_string()
    }

    pub fn local_of(&self, ms: i64) -> String {
        self.zone().local(ms).format("%Y-%m-%d %H:%M").to_string()
    }

    pub fn start(&mut self) -> Vec<Action> {
        self.timers.clear();
        self.view = None;
        self.form = None;
        let ev = Event::Init {
            config: self.config.clone(),
            data: self.data.clone(),
            host: "rune-tui test".into(),
        };
        self.send(ev)
    }

    /// Quit rune and start it again `later_ms` later (timers do not run
    /// while it is closed).
    pub fn restart_after(&mut self, later_ms: i64) -> Vec<Action> {
        self.send(Event::Shutdown);
        self.plugin = Nudge::default();
        self.now += later_ms;
        self.start()
    }

    pub fn send(&mut self, ev: Event) -> Vec<Action> {
        let interactive = ev.is_interactive();
        let shutting_down = ev == Event::Shutdown;
        let actions = self.plugin.handle(&Context { now: self.now }, ev);
        self.max_reply = self.max_reply.max(actions.len());
        assert!(actions.len() <= 64, "rune takes at most 64 actions: {}", actions.len());
        for a in &actions {
            if shutting_down {
                assert!(
                    matches!(a, Action::Status { .. } | Action::StoreSet { .. } | Action::StoreDelete { .. }),
                    "only status and storage while shutting down: {a:?}"
                );
            }
            match a.clone() {
                Action::Status { text } => {
                    assert!(text.chars().count() <= 400, "status too long: {text}");
                    self.statuses.push(text)
                }
                Action::Notify { title, body } => self.notes.push((title, body)),
                Action::OpenView { title, lines, selected, .. } => {
                    assert!(interactive || self.view.is_some(), "a new view from a background event");
                    assert!(lines.len() <= 2000);
                    if self.form.is_none() || interactive {
                        self.view = Some(View { title, lines, selected });
                    }
                }
                Action::CloseView { .. } => self.view = None,
                Action::Form { id, fields, .. } => {
                    assert!(interactive, "a form from a background event");
                    assert!(fields.len() <= 8);
                    // A form replaces the view on screen.
                    self.view = None;
                    self.form = Some((id, fields));
                }
                Action::InsertText { .. } | Action::SendMessage { .. } => {
                    panic!("Nudge never types or sends: {a:?}")
                }
                Action::StoreSet { key, value } => {
                    assert!(key.len() <= 64 && value.len() <= 16 << 10);
                    if !self.data.contains_key(&key) {
                        assert!(self.data.len() < 256, "storage is full");
                    }
                    self.data.insert(key, value);
                }
                Action::StoreDelete { key } => {
                    self.data.remove(&key);
                }
                Action::SetTimer { id, after_ms, repeat } => {
                    let after = (after_ms as i64).clamp(1000, 7 * 86_400_000);
                    self.timers.insert(
                        id,
                        Timer {
                            at: self.now + after,
                            every: repeat.then_some(after),
                        },
                    );
                    assert!(self.timers.len() <= 16);
                }
                Action::CancelTimer { id } => {
                    self.timers.remove(&id);
                }
            }
        }
        actions
    }

    pub fn cmd(&mut self, name: &str, args: &str, chat: Option<ChatRef>) -> Vec<Action> {
        self.send(Event::Command {
            name: name.into(),
            args: args.into(),
            chat,
        })
    }

    pub fn remind(&mut self, args: &str) -> Vec<Action> {
        self.cmd("remind", args, None)
    }

    pub fn key(&mut self, k: &str) -> Vec<Action> {
        assert!(self.view.is_some(), "no view open for {k}");
        self.send(Event::ViewKey {
            view: "reminders".into(),
            key: k.into(),
            selected: self.view.as_ref().and_then(|v| v.selected),
        })
    }

    pub fn submit(&mut self, values: &[&str]) -> Vec<Action> {
        let (id, _) = self.form.take().expect("a form is open");
        self.send(Event::FormSubmitted {
            form: id,
            values: values.iter().map(|s| s.to_string()).collect(),
        })
    }

    pub fn message(&mut self, chat: ChatRef, from_name: &str, text: &str, mine: bool) -> Vec<Action> {
        let n = self.now;
        self.send(Event::Message {
            chat,
            message: MessageInfo {
                id: format!("m{n}"),
                from: if mine { "me".into() } else { "them".into() },
                from_name: from_name.into(),
                text: Some(text.into()),
                summary: text.into(),
                time: n,
                mine,
            },
        })
    }

    /// Let `ms` pass with rune running: timers go off in order.
    pub fn advance(&mut self, ms: i64) {
        let end = self.now + ms;
        loop {
            let next = self
                .timers
                .iter()
                .filter(|(_, t)| t.at <= end)
                .min_by_key(|(_, t)| t.at)
                .map(|(id, t)| (id.clone(), t.clone()));
            let Some((id, t)) = next else { break };
            self.now = self.now.max(t.at);
            match t.every {
                Some(every) => self.timers.get_mut(&id).unwrap().at = t.at + every,
                None => {
                    self.timers.remove(&id);
                }
            }
            self.send(Event::Timer { id });
        }
        self.now = end;
    }

    /// The computer sleeps for `ms`: the wall clock moves on, rune's
    /// (monotonic) timers do not.
    pub fn sleep(&mut self, ms: i64) {
        self.now += ms;
    }

    pub fn last_status(&self) -> &str {
        self.statuses.last().map_or("", |s| s.as_str())
    }

    pub fn take_notes(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.notes)
    }

    /// Stored reminders (not `meta`), parsed.
    pub fn stored(&self) -> Vec<Value> {
        self.data
            .iter()
            .filter(|(k, _)| k.starts_with("r."))
            .map(|(_, v)| serde_json::from_str(v).unwrap())
            .collect()
    }

    pub fn stored_one(&self, id: u32) -> Value {
        serde_json::from_str(self.data.get(&format!("r.{id}")).expect("stored")).unwrap()
    }

    pub fn open_list(&mut self) -> View {
        self.remind("");
        self.view.clone().expect("the list opens")
    }
}

pub const MIN: i64 = 60_000;
pub const HOUR: i64 = 60 * MIN;
pub const DAY: i64 = 24 * HOUR;
