//! Nudge driven end to end through a stand-in host (see `common`).

mod common;

use common::*;
use rune_plugin::prelude::*;
use serde_json::json;

/// Monday 2026-11-16 10:00 in Bangkok.
fn host() -> Host {
    let mut h = Host::new("Asia/Bangkok", "2026-11-16 10:00");
    h.start();
    h
}

// -- the manifest ---------------------------------------------------------------------

#[test]
fn manifest_is_valid_and_asks_only_for_what_it_uses() {
    let m = manifest();
    assert_eq!(m.id, "nudge");
    let perms: Vec<&str> = m.permissions.iter().map(|p| p.as_str()).collect();
    assert_eq!(perms, ["notify", "storage", "read_messages"], "never send_messages or compose");
    let names: Vec<&str> = m.commands.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["remind", "followup"]);
    // Every setting the code reads is declared.
    for key in [
        "timezone", "clock", "alerts", "default_minutes", "followup_minutes", "snooze_minutes",
        "morning_hour", "nag_minutes", "missed", "missed_repeats", "link_chat", "followup_auto_done",
        "quote_message", "keep_done",
    ] {
        assert!(m.default_config().contains_key(key), "{key}");
    }
}

// -- making and firing ----------------------------------------------------------------

#[test]
fn a_reminder_fires_once_and_stays_overdue() {
    let mut h = host();
    h.remind("2h Reply to Alice");
    assert!(h.last_status().contains("#1 “Reply to Alice” — today 12:00 (in 2h)"), "{}", h.last_status());
    assert_eq!(h.timers["wake"].at - h.now, 2 * HOUR, "a timer for the due time");
    h.advance(2 * HOUR - MIN);
    assert!(h.take_notes().is_empty());
    h.advance(MIN);
    assert_eq!(h.take_notes(), [("Reminder".to_string(), "Reply to Alice".to_string())]);
    assert!(h.last_status().contains("⏰ #1 Reply to Alice"));
    // Timers and beats keep coming; no second alert.
    h.advance(3 * HOUR);
    assert!(h.take_notes().is_empty());
    assert_eq!(h.stored_one(1)["state"], "fired");
    let list = h.open_list();
    assert!(list.title.contains("1 overdue"), "{}", list.title);
    assert!(list.text().contains("Overdue · 1"));
    assert!(list.selected_line().contains("#1") && list.selected_line().contains("due 3h ago"));
}

#[test]
fn no_time_means_the_default_delay_and_says_so() {
    let mut h = host();
    h.remind("Call the bank");
    assert!(h.last_status().contains("in 1h"), "{}", h.last_status());
    assert!(h.last_status().contains("no time given"));
    h.set("default_minutes", json!(15));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.remind("Stretch");
    assert!(h.last_status().contains("in 15m"), "{}", h.last_status());
}

#[test]
fn times_in_words_resolve_in_the_users_zone() {
    let mut h = host();
    h.remind("tomorrow 9am Pay rent");
    h.remind("Call mom fri 18:00");
    h.remind("every weekday 9:00 Standup");
    h.remind("9:00 Water plants");
    let due = |h: &Host, id: u32| h.local_of(h.stored_one(id)["due"].as_i64().unwrap());
    assert_eq!(due(&h, 1), "2026-11-17 09:00");
    assert_eq!(due(&h, 2), "2026-11-20 18:00");
    assert_eq!(due(&h, 3), "2026-11-17 09:00");
    assert_eq!(due(&h, 4), "2026-11-17 09:00");
    assert!(h.last_status().contains("09:00 has passed today, so tomorrow"), "{}", h.last_status());
    assert_eq!(h.stored_one(2)["text"], "Call mom");
    assert_eq!(h.stored_one(3)["recur"]["unit"], "weeks");
}

#[test]
fn bad_times_are_refused_and_nothing_is_stored() {
    let mut h = host();
    for (args, says) in [
        ("25:00 Standup", "25:00 is not a time"),
        ("2025-01-01 Old thing", "is in the past"),
        ("today 9:00 Late", "has already passed"),
        ("2026-02-30 Nope", "is not a date"),
        ("every 0 days X", "every 0?"),
    ] {
        h.remind(args);
        assert!(h.last_status().contains(says), "{args}: {}", h.last_status());
    }
    assert!(h.stored().is_empty());
    h.remind("2h");
    let (id, fields) = h.form.clone().expect("a time alone asks for the text");
    assert_eq!(id, "new");
    assert_eq!(fields[1].value, "2h");
}

#[test]
fn duplicates_are_refused() {
    let mut h = host();
    h.remind("2h Reply to Alice");
    h.remind("in 2 hours reply  to alice");
    assert!(h.last_status().contains("#1 already reminds you of that"), "{}", h.last_status());
    h.remind("3h Reply to Alice");
    assert!(h.last_status().starts_with("⏰ #2"), "a different time is not a duplicate");
    h.remind("every day 9am Vitamins");
    h.remind("daily 9:00 vitamins");
    assert!(h.last_status().contains("already reminds you"));
    assert_eq!(h.stored().len(), 3);
}

#[test]
fn many_at_once_are_summed_up() {
    let mut h = host();
    for i in 0..5 {
        h.remind(&format!("1h Thing {i}"));
    }
    h.advance(HOUR);
    let notes = h.take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, "5 reminders are due");
    assert!(notes[0].1.contains("and 2 more"));
}

// -- persistence and restarts ---------------------------------------------------------

#[test]
fn reminders_survive_restarts_and_never_alert_twice() {
    let mut h = host();
    h.remind("1h Feed the cat");
    h.restart_after(10 * MIN);
    assert!(h.statuses.last().unwrap().contains("next: Feed the cat in 50m"), "{:?}", h.statuses);
    assert_eq!(h.timers["wake"].at - h.now, 50 * MIN, "rescheduled after start-up");
    assert!(h.timers.contains_key("beat"));
    h.advance(50 * MIN);
    assert_eq!(h.take_notes().len(), 1);
    // Restarting after it went off does not alert again.
    h.restart_after(MIN);
    h.restart_after(MIN);
    h.advance(HOUR);
    assert!(h.take_notes().is_empty());
    assert!(h.statuses.last().unwrap().contains("1 overdue"), "{:?}", h.statuses);
}

#[test]
fn missed_while_closed_is_one_summary_by_default() {
    let mut h = host();
    h.remind("1h One");
    h.remind("2h Two");
    h.remind("8h Later");
    h.restart_after(3 * HOUR);
    let notes = h.take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].0, "2 reminder(s) came due while rune was closed");
    assert!(notes[0].1.contains("#1 One") && notes[0].1.contains("#2 Two"));
    assert!(h.last_status().contains("missed while closed"));
    assert_eq!(h.stored_one(1)["missed"], true);
    assert_eq!(h.stored_one(3)["state"], "pending");
    // The rest still fires on time.
    h.advance(5 * HOUR);
    assert_eq!(h.take_notes().len(), 1);
}

#[test]
fn missed_settings_each_and_list_only() {
    let mut h = host();
    h.set("missed", json!("one each"));
    h.start();
    h.remind("1h One");
    h.remind("2h Two");
    h.restart_after(3 * HOUR);
    let notes = h.take_notes();
    assert_eq!(notes.len(), 2);
    assert!(notes[0].1.contains("(was due 2h ago)"), "{notes:?}");

    let mut h = host();
    h.set("missed", json!("list only"));
    h.start();
    h.remind("1h One");
    h.restart_after(3 * HOUR);
    assert!(h.take_notes().is_empty());
    assert!(h.last_status().contains("missed while closed: #1 One"), "{}", h.last_status());
}

#[test]
fn a_reminder_due_just_now_at_start_up_is_not_called_missed() {
    let mut h = host();
    h.remind("1h One");
    h.restart_after(HOUR + 30_000);
    let notes = h.take_notes();
    assert_eq!(notes, [("Reminder".to_string(), "One".to_string())]);
}

#[test]
fn unreadable_or_newer_records_are_skipped_without_reusing_their_ids() {
    let mut h = Host::new("UTC", "2026-11-16 10:00");
    h.data.insert("r.5".into(), "{not json".into());
    h.data.insert("r.6".into(), r#"{"v":7,"future":true}"#.into());
    h.data.insert("r.2".into(), r#"{"id":2,"text":"ok","created":1,"due":4102444800000}"#.into());
    h.data.insert("unrelated".into(), "x".into());
    h.start();
    assert!(h.last_status().contains("1 stored reminder(s) could not be read"), "{}", h.last_status());
    h.remind("1h New");
    assert!(h.last_status().contains("#7"), "{}", h.last_status());
    assert!(h.data.contains_key("r.5") && h.data.contains_key("r.6"), "left alone");
    assert_eq!(h.open_list().text().matches("#").count(), 2);
}

#[test]
fn unset_time_zone_is_utc_with_a_hint() {
    let mut h = Host::new("", "2026-11-16 10:00");
    h.start();
    h.remind("1h x");
    h.restart_after(MIN);
    assert!(h.last_status().contains("times are in UTC"), "{}", h.last_status());
    let mut h = Host::new("Nowhere/Land", "2026-11-16 10:00");
    h.start();
    assert!(h.last_status().contains("unknown time zone"));
}

// -- clocks: sleep, long waits, DST ---------------------------------------------------

#[test]
fn after_sleep_the_beat_catches_up_and_says_how_late() {
    let mut h = host();
    h.remind("1h Meeting");
    h.sleep(3 * HOUR); // wall clock moved, timers did not
    h.advance(30_000); // the next beat
    let notes = h.take_notes();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].1.contains("was due 2h ago"), "{notes:?}");
}

#[test]
fn far_away_reminders_rearm_the_timer_within_rune_limits() {
    let mut h = host();
    h.remind("in 30 days Renew passport");
    assert_eq!(h.timers["wake"].at - h.now, 7 * DAY);
    h.advance(29 * DAY);
    assert!(h.take_notes().is_empty());
    h.advance(DAY);
    assert_eq!(h.take_notes().len(), 1);
}

#[test]
fn daily_reminders_follow_the_wall_clock_across_dst() {
    let mut h = Host::new("Europe/Berlin", "2026-03-27 10:00");
    h.start();
    h.remind("every day at 9 Pills");
    let mut fired = Vec::new();
    for _ in 0..4 {
        h.advance(DAY);
        for _ in h.take_notes() {
            fired.push(h.local_of(h.stored_one(1)["notified"].as_i64().unwrap()));
        }
    }
    assert_eq!(
        fired,
        ["2026-03-28 09:00", "2026-03-29 09:00", "2026-03-30 09:00", "2026-03-31 09:00"]
    );
}

#[test]
fn changing_the_time_zone_moves_repeating_reminders_only() {
    let mut h = host(); // Bangkok, UTC+7
    h.remind("every day at 9 Standup");
    h.remind("2026-11-17 09:00 One-off");
    let before_once = h.stored_one(2)["due"].as_i64().unwrap();
    h.set("timezone", json!("Europe/London")); // UTC+0 in November
    h.send(Event::ConfigChanged { config: h.config.clone() });
    assert_eq!(h.local_of(h.stored_one(1)["due"].as_i64().unwrap()), "2026-11-17 09:00");
    assert_eq!(h.stored_one(2)["due"].as_i64().unwrap(), before_once);
}

// -- repeating ------------------------------------------------------------------------

#[test]
fn repeating_reminders_fire_each_time_and_skip_what_was_missed() {
    let mut h = host();
    h.remind("every day at 9 Pills");
    h.advance(DAY);
    assert_eq!(h.take_notes().len(), 1);
    assert_eq!(h.local_of(h.stored_one(1)["due"].as_i64().unwrap()), "2026-11-18 09:00");
    // Closed for three days: one alert, then the next one.
    h.restart_after(3 * DAY);
    let notes = h.take_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].1.contains("#1 Pills (missed 3 times)"), "{notes:?}");
    assert_eq!(h.local_of(h.stored_one(1)["due"].as_i64().unwrap()), "2026-11-21 09:00");
    assert_eq!(h.stored_one(1)["state"], "pending");
}

#[test]
fn missed_repeats_can_be_skipped_silently() {
    let mut h = host();
    h.set("missed_repeats", json!("skip silently"));
    h.start();
    h.remind("every day at 9 Pills");
    h.restart_after(2 * DAY);
    assert!(h.take_notes().is_empty());
    h.advance(DAY);
    assert_eq!(h.take_notes().len(), 1, "on time again");
}

#[test]
fn skip_and_stop_a_repeating_reminder() {
    let mut h = host();
    h.remind("every mon,wed 10am Gym");
    let first = h.stored_one(1)["due"].as_i64().unwrap();
    assert_eq!(h.local_of(first), "2026-11-18 10:00");
    h.remind("skip 1");
    assert_eq!(h.local_of(h.stored_one(1)["due"].as_i64().unwrap()), "2026-11-23 10:00");
    h.remind("done 1");
    assert!(h.last_status().contains("won't repeat"), "{}", h.last_status());
    h.advance(14 * DAY);
    assert!(h.take_notes().is_empty());
    h.remind("skip 1");
    assert!(h.last_status().contains("does not repeat"));
}

// -- acting on reminders ---------------------------------------------------------------

#[test]
fn snooze_brings_it_back_once() {
    let mut h = host();
    h.remind("1h Call back");
    h.advance(HOUR);
    assert_eq!(h.take_notes().len(), 1);
    h.remind("snooze 1 30m");
    assert!(h.last_status().contains("snoozed until today 11:30 (in 30m)"), "{}", h.last_status());
    h.advance(30 * MIN);
    assert_eq!(h.take_notes().len(), 1);
    h.advance(HOUR);
    assert!(h.take_notes().is_empty());
    // Default snooze, from the list.
    h.open_list();
    h.key("s");
    assert!(h.last_status().contains("in 10m"), "{}", h.last_status());
    assert_eq!(h.stored_one(1)["snoozes"], 2);
    h.remind("snooze 1 every day");
    assert!(h.last_status().contains("snooze until a time"));
    h.remind("snooze 1 whenever");
    assert!(h.last_status().contains("can't read"));
}

#[test]
fn done_delete_and_undo() {
    let mut h = host();
    h.remind("1h A");
    h.remind("2h B");
    h.remind("done 1");
    assert_eq!(h.stored_one(1)["state"], "done");
    h.remind("undo");
    assert_eq!(h.stored_one(1)["state"], "pending");
    h.remind("delete 2");
    assert!(!h.data.contains_key("r.2"));
    h.remind("undo");
    assert_eq!(h.stored_one(2)["text"], "B");
    h.remind("undo");
    assert_eq!(h.last_status(), "nothing to undo");
    h.remind("done 9");
    assert!(h.last_status().contains("there is no reminder #9"));
    h.remind("done");
    assert!(h.last_status().contains("which one?"));
    // A verb followed by words is just a reminder.
    h.remind("delete old photos tomorrow");
    assert!(h.last_status().contains("#3 “delete old photos” — tomorrow 09:00"), "{}", h.last_status());
}

#[test]
fn the_list_view_moves_acts_and_filters() {
    let mut h = host();
    h.remind("1h A");
    h.remind("2h B");
    h.remind("3h C");
    let v = h.open_list();
    assert!(v.text().contains("Upcoming · 3"));
    assert!(v.selected_line().contains("#1"));
    h.key("Down");
    assert!(h.view.as_ref().unwrap().selected_line().contains("#2"));
    h.key("x");
    let v = h.view.clone().unwrap();
    assert!(v.text().contains("Done · latest 1"));
    assert!(v.selected_line().contains("#3"), "the selection stays in place: {}", v.selected_line());
    assert!(v.text().contains("Upcoming · 2"));
    h.key("d");
    assert!(!h.data.contains_key("r.3"));
    h.key("u");
    assert!(h.data.contains_key("r.3"));
    h.key("Enter");
    let v = h.view.clone().unwrap();
    assert!(v.title.starts_with("Reminder #"), "{}", v.title);
    h.key("Backspace");
    h.key("Tab");
    assert!(h.view.as_ref().unwrap().text().contains("No follow-ups"));
    h.key("Tab");
    let v = h.view.clone().unwrap();
    assert!(v.text().contains("Done · 1") && v.selected_line().contains("#2"));
    h.key("h");
    assert!(h.view.as_ref().unwrap().title.contains("help"));
    h.send(Event::ViewClosed { view: "reminders".into() });
    // A closed view is not reopened by background events.
    h.view = None;
    h.advance(HOUR);
    assert!(h.view.is_none());
}

#[test]
fn an_open_view_updates_when_something_fires() {
    let mut h = host();
    h.remind("1h A");
    h.open_list();
    h.advance(HOUR);
    assert!(h.view.as_ref().unwrap().text().contains("Overdue · 1"));
}

// -- forms ----------------------------------------------------------------------------

#[test]
fn new_and_edit_forms() {
    let mut h = host();
    let alice = chat("u1", "Alice");
    h.cmd("remind", "new", Some(alice.clone()));
    let (_, fields) = h.form.clone().unwrap();
    assert_eq!(fields.iter().map(|f| f.label.as_str()).collect::<Vec<_>>(), ["What", "When", "Chat"]);
    assert_eq!(fields[2].value, "Alice");
    h.submit(&["Send slides", "someday", "Alice"]);
    assert!(h.last_status().contains("can't read"), "{}", h.last_status());
    let (_, fields) = h.form.clone().expect("the form comes back");
    assert_eq!(fields[0].value, "Send slides", "with what was typed");
    h.submit(&["Send slides", "next tue 4pm", "Alice"]);
    assert_eq!(h.stored_one(1)["chat"]["name"], "Alice");
    assert_eq!(h.local_of(h.stored_one(1)["due"].as_i64().unwrap()), "2026-11-17 16:00");

    // Edit: the same "When" keeps the time; a new one moves it; clearing the chat unlinks.
    h.remind("edit 1");
    let (id, fields) = h.form.clone().unwrap();
    assert_eq!(id, "edit:1");
    assert_eq!(fields[1].value, "2026-11-17 16:00");
    h.submit(&["Send the slides", "2026-11-17 16:00", "Alice"]);
    assert_eq!(h.stored_one(1)["text"], "Send the slides");
    assert_eq!(h.local_of(h.stored_one(1)["due"].as_i64().unwrap()), "2026-11-17 16:00");
    h.remind("edit 1");
    h.submit(&["Send the slides", "every fri 5pm", ""]);
    assert_eq!(h.stored_one(1)["recur"]["unit"], "weeks");
    assert!(h.stored_one(1).get("chat").is_none());
    h.remind("edit 1");
    let (_, fields) = h.form.clone().unwrap();
    assert_eq!(fields[1].value, "every fri at 17:00", "repeats are shown in words it reads back");
}

#[test]
fn a_form_from_the_list_returns_to_the_list() {
    let mut h = host();
    h.remind("1h A");
    h.open_list();
    h.key("z");
    assert!(h.view.is_none() && h.form.is_some());
    h.submit(&["tomorrow 8am"]);
    let v = h.view.clone().expect("back to the list");
    assert!(v.selected_line().contains("tomorrow 08:00"), "{}", v.text());
    h.key("a");
    h.submit(&["", "1h"]);
    assert!(h.last_status().contains("needs some text"));
    assert!(h.form.is_some());
}

// -- follow-ups -----------------------------------------------------------------------

#[test]
fn follow_ups_quote_the_message_and_finish_when_you_reply() {
    let mut h = host();
    let alice = chat("u1", "Alice");
    h.message(alice.clone(), "Alice", "can you send the slides?", false);
    h.cmd("followup", "", Some(alice.clone()));
    assert!(h.last_status().contains("#1 “Reply to Alice”"), "{}", h.last_status());
    assert!(h.last_status().contains("in 2h"));
    let r = h.stored_one(1);
    assert_eq!(r["kind"], "follow_up");
    assert_eq!(r["message"]["summary"], "can you send the slides?");
    // Moving it rather than making a second one.
    h.cmd("followup", "tomorrow 9am", Some(alice.clone()));
    assert!(h.last_status().contains("moved to tomorrow 09:00"), "{}", h.last_status());
    assert_eq!(h.stored().len(), 1);
    // Someone else's message changes nothing; yours finishes it.
    h.message(chat("u2", "Bob"), "Bob", "hi", true);
    assert_eq!(h.stored_one(1)["state"], "pending");
    h.message(alice.clone(), "Me", "here they are", true);
    assert_eq!(h.stored_one(1)["state"], "done");
    assert_eq!(h.stored_one(1)["done_note"], "you replied");
    assert!(h.last_status().contains("done — you replied"));
    h.advance(2 * DAY);
    assert!(h.take_notes().is_empty());
}

#[test]
fn follow_up_alerts_name_the_chat_and_the_message() {
    let mut h = host();
    let team = chat("c1", "Team");
    h.message(team.clone(), "Bob", "review my PR?", false);
    h.cmd("followup", "30m check Bob's PR", Some(team.clone()));
    h.advance(30 * MIN);
    let notes = h.take_notes();
    assert_eq!(notes[0].0, "Follow up: Team");
    assert_eq!(notes[0].1, "check Bob's PR — Bob: “review my PR?”");
    h.send(Event::ChatOpened { chat: team.clone() });
    assert!(h.last_status().contains("follow-up #1 is overdue"));
    h.cmd("followup", "done", Some(team.clone()));
    assert_eq!(h.stored_one(1)["state"], "done");
}

#[test]
fn follow_up_edge_cases() {
    let mut h = host();
    h.cmd("followup", "2h", None);
    assert!(h.last_status().contains("open a chat first"));
    let alice = chat("u1", "Alice");
    h.cmd("followup", "every day", Some(alice.clone()));
    assert!(h.last_status().contains("do not repeat"));
    h.cmd("followup", "done", Some(alice.clone()));
    assert!(h.last_status().contains("no open follow-up for Alice"));
    // Without "Finish follow-ups when I reply" a reply leaves it open.
    h.set("followup_auto_done", json!(false));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.cmd("followup", "", Some(alice.clone()));
    h.message(alice.clone(), "Me", "ok", true);
    assert_eq!(h.stored_one(1)["state"], "pending");
    // A renamed chat is shown by its new name.
    h.send(Event::ChatOpened { chat: chat("u1", "Alice B.") });
    assert_eq!(h.stored_one(1)["chat"]["name"], "Alice B.");
    // Alt-f is /followup.
    h.send(Event::Key { id: "followup".into(), chat: Some(chat("u3", "Carol")) });
    assert!(h.last_status().contains("Reply to Carol"));
}

#[test]
fn remind_links_the_open_chat_unless_turned_off() {
    let mut h = host();
    h.cmd("remind", "1h Book table", Some(chat("u1", "Alice")));
    assert_eq!(h.stored_one(1)["chat"]["mid"], "u1");
    assert!(h.last_status().contains("· Alice"));
    h.set("link_chat", json!(false));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.cmd("remind", "1h Book cab", Some(chat("u1", "Alice")));
    assert!(h.stored_one(2).get("chat").is_none());
}

// -- alerts and settings --------------------------------------------------------------

#[test]
fn alert_settings() {
    let mut h = host();
    h.set("alerts", json!("status line"));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.remind("1h A");
    h.advance(HOUR);
    assert!(h.take_notes().is_empty());
    assert!(h.last_status().contains("⏰ #1 A"));
    h.set("alerts", json!("desktop"));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.remind("1h B");
    let before = h.statuses.len();
    h.advance(HOUR);
    assert_eq!(h.take_notes().len(), 1);
    assert_eq!(h.statuses.len(), before, "no status line");
}

#[test]
fn overdue_reminders_can_nag_and_the_nag_survives_restarts() {
    let mut h = host();
    h.set("nag_minutes", json!(15));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.remind("1h Take out bins");
    h.advance(HOUR);
    assert_eq!(h.take_notes().len(), 1);
    h.advance(14 * MIN);
    assert!(h.take_notes().is_empty());
    h.advance(MIN);
    let notes = h.take_notes();
    assert_eq!(notes.len(), 1);
    assert!(notes[0].0.starts_with("Still due"), "{notes:?}");
    h.restart_after(5 * MIN);
    assert!(h.take_notes().is_empty(), "not straight away after a restart");
    h.advance(10 * MIN);
    assert_eq!(h.take_notes().len(), 1);
    h.remind("done 1");
    h.advance(HOUR);
    assert!(h.take_notes().is_empty());
}

#[test]
fn twelve_hour_clock() {
    let mut h = host();
    h.set("clock", json!("12-hour"));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    h.remind("15:30 Tea");
    assert!(h.last_status().contains("today 3:30 pm"), "{}", h.last_status());
}

// -- limits ---------------------------------------------------------------------------

#[test]
fn storage_limits_are_respected() {
    let mut h = host();
    h.set("keep_done", json!(100));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    for i in 0..240 {
        h.remind(&format!("{}m Item {i}", i + 1));
    }
    assert_eq!(h.stored().len(), 240);
    h.remind("1m One too many");
    assert!(h.last_status().contains("240 open reminders"), "{}", h.last_status());
    // Finish 100 in one go (more writes than one reply may carry).
    h.advance(100 * MIN);
    assert!(h.max_reply <= 64);
    for i in 1..=100 {
        h.remind(&format!("done {i}"));
    }
    h.remind("clear done");
    assert!(h.timers.contains_key("flush"), "the rest of the writes follow");
    h.advance(10_000);
    assert_eq!(h.stored().len(), 140);
    assert!(h.max_reply <= 64);
    // Room again; done ones beyond "Keep done" are dropped as new ones finish.
    h.remind("1m Fits now");
    assert!(h.last_status().contains("#241"), "{}", h.last_status());
    assert!(h.data.len() <= 256);
}

#[test]
fn keep_done_prunes_the_oldest() {
    let mut h = host();
    h.set("keep_done", json!(2));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    for i in 1..=4 {
        h.remind(&format!("{i}h T{i}"));
        h.remind(&format!("done {i}"));
        h.advance(MIN);
    }
    let ids: Vec<i64> = h.stored().iter().map(|r| r["id"].as_i64().unwrap()).collect();
    assert_eq!(ids, [3, 4]);
}

#[test]
fn text_is_cleaned_and_limited() {
    let mut h = host();
    h.remind("1h   lots   of\tspace ");
    assert_eq!(h.stored_one(1)["text"], "lots of space");
    h.remind(&format!("1h {}", "x".repeat(501)));
    assert!(h.last_status().contains("too long"));
}

#[test]
fn help_lists_commands_and_the_zone() {
    let mut h = host();
    h.remind("help");
    let v = h.view.clone().unwrap();
    assert!(v.text().contains("/remind WHEN TEXT"));
    assert!(v.text().contains("Time zone: Asia/Bangkok"));
    assert!(v.text().contains("never sends messages"));
    h.remind("list done");
    assert!(h.view.as_ref().unwrap().text().contains("Nothing done yet"));
}

#[test]
fn a_long_absence_with_many_repeats_is_handled_in_one_go() {
    let mut h = host();
    for i in 0..200 {
        h.remind(&format!("every day at {}:{:02} Task {i}", i % 24, i % 60));
    }
    if let Ok(path) = std::env::var("NUDGE_DUMP") {
        std::fs::write(path, serde_json::to_string(&h.data).unwrap()).unwrap();
    }
    let started = std::time::Instant::now();
    h.restart_after(3 * 365 * DAY);
    assert!(started.elapsed().as_secs() < 2);
    let notes = h.take_notes();
    assert_eq!(notes.len(), 1, "one summary");
    assert!(notes[0].1.contains("times)"), "{notes:?}");
    h.advance(10_000); // queued writes go out
    let now = h.now;
    assert!(h.stored().iter().all(|r| r["state"] == "pending" && r["due"].as_i64().unwrap() > now));
    assert!(h.max_reply <= 64);
}

#[test]
fn shutdown_stays_within_the_action_limit() {
    let mut h = host();
    h.set("keep_done", json!(100));
    h.send(Event::ConfigChanged { config: h.config.clone() });
    for i in 0..120 {
        h.remind(&format!("{}m Item {i}", i + 1));
    }
    h.advance(2 * HOUR);
    for i in 1..=120 {
        h.remind(&format!("done {i}"));
    }
    h.remind("clear done");
    let out = h.send(Event::Shutdown);
    assert!(out.len() <= 64);
}

#[test]
fn an_overdue_reminder_does_not_block_the_same_text_later() {
    let mut h = host();
    h.remind("1h Take pills");
    h.advance(HOUR);
    h.remind("tomorrow Take pills");
    assert!(h.last_status().starts_with("⏰ #2"), "{}", h.last_status());
}

#[test]
fn the_missed_line_is_not_hidden_by_hints() {
    let mut h = Host::new("", "2026-11-16 10:00");
    h.start();
    h.remind("1h One");
    h.restart_after(3 * HOUR);
    assert!(h.last_status().contains("missed while closed"), "{}", h.last_status());
}

#[test]
fn usages_fit_runes_help_column() {
    for c in manifest().commands {
        assert!(c.usage.chars().count() <= 40, "{} is cut off in rune's help", c.usage);
    }
}
