# Nudge: reminders and follow-ups for rune

Nudge is a [rune-tui](https://github.com/ThanapatFungpharit/Rune) plugin for
personal reminders inside the chat app:

```text
/remind 2h Reply to Alice
/remind tomorrow 9am Pay rent
/remind Call mom fri 18:00
/remind every weekday 9:00 Standup
/followup                       ← in a chat: remind me to reply here (finishes when I do)
```

- **One-time and repeating reminders**, written the way you'd say them, in
  your time zone, with daylight-saving changes handled.
- **Follow-ups for conversations.** `/followup` reminds you to reply to the
  open chat, quotes the message you're answering, and finishes on its own
  once you reply.
- **A view of everything** (`Alt-r`): overdue, upcoming and done, with keys
  to finish, snooze, skip, edit, delete and undo.
- **Desktop notifications** when a reminder is due, or only the status line.
- **Reliable.** Reminders are stored as they change. They are rescheduled when
  rune starts, reminders that came due while rune was closed are announced,
  and no reminder ever alerts twice for the same time.
- **Never sends messages.** Nudge doesn't have the permission to.

---

## Contents

- [Install](#install)
- [Commands](#commands)
- [Writing times](#writing-times)
- [The reminders view](#the-reminders-view)
- [Follow-ups](#follow-ups)
- [Notifications and missed reminders](#notifications-and-missed-reminders)
- [Settings](#settings)
- [Permissions](#permissions)
- [How it works](#how-it-works)
- [Limitations](#limitations)
- [Building and testing](#building-and-testing)

## Install

Requires rune-tui with plugin API 1.0 (*Settings › Plugins* exists).

**From Git (recommended).** In rune: `Ctrl-x` › *Plugins* › *Install from a
Git repository…*

| Field      | Value                                                      |
|------------|------------------------------------------------------------|
| Repository | `https://github.com/ThanapatFungpharit/Rune-Nudge-plugin`  |
| Version    | `latest` (newest release), or empty for the newest commit  |
| Folder     | empty                                                      |

rune fetches the repository, checks the prebuilt `nudge.wasm` without running
it, and lists **Nudge** as off. Open its page, turn on **Enabled**, and approve
its permissions. Then set your **Time zone** on the same page (for example
`Asia/Bangkok`). Until you do, times are in UTC.

Updates come through the plugin's page (*Check for a new version*, or
*Updates* to follow releases or pin a version).

**From a local checkout.**

```sh
rustup target add wasm32-unknown-unknown
scripts/build.sh install        # builds, copies to ~/.local/share/rune/plugins/nudge
```

A running rune picks it up within a couple of seconds; enable it as above.

## Commands

`/remind` makes reminders and manages them. `/remind help` shows everything
inside rune.

| Command | What it does |
|---|---|
| `/remind WHEN TEXT` | A reminder, e.g. `/remind 2h Reply to Alice`. |
| `/remind TEXT WHEN` | The time may also come last: `/remind Call mom tomorrow 6pm`. |
| `/remind TEXT` | No time: due after the **Default delay** (1 hour). The status line says so. |
| `/remind WHEN` | A time without text opens the form to ask for the text. |
| `/remind` · `Alt-r` | The reminders view. |
| `/remind new` | A form: *What*, *When*, and *Chat* when one is open. |
| `/remind list [done\|follow-ups]` | The view, filtered. |
| `/remind show N` | One reminder's details. |
| `/remind edit N` | Edit text, time or repeat, or unlink the chat. |
| `/remind done N` | Finish it. For a repeating reminder this stops it. |
| `/remind snooze N [WHEN]` | Postpone by the **Snooze** length, or until `WHEN` (`30m`, `tomorrow 9am`). |
| `/remind skip N` | Repeating reminders: move to the next occurrence without an alert. |
| `/remind delete N` | Delete it (also `rm`, `cancel`). |
| `/remind undo` | Undo the last done, snooze, skip, edit, delete or clear. |
| `/remind clear done` | Delete all finished reminders. |
| `/remind help` | The help page. |

`N` is the number shown as `#N` (`#3` and `3` both work). If a command word is
followed by something that isn't a number, the whole line becomes a reminder:
`/remind delete old photos tomorrow` reminds you to delete old photos.

`/followup` is for chats; see [Follow-ups](#follow-ups).

| Command | What it does |
|---|---|
| `/followup [WHEN] [NOTE]` · `Alt-f` | Remind me to reply to the open chat (default: in 2 hours). |
| `/followup done` | Finish the open chat's follow-up. |
| `/followup cancel` | Delete it. |
| `/followup list` | The view, showing follow-ups. |

## Writing times

Times are read in your **Time zone** setting.

| Kind | Examples |
|---|---|
| From now | `2h` · `1h30m` · `45 min` · `in 2 hours` · `an hour` · `half an hour` · `in 3 days` · `in 2 weeks` · `in 1 month` |
| Clock time | `15:30` · `3pm` · `3:30 pm` · `at 9` · `noon` · `midnight` · `tonight` · `this evening` · `morning` |
| Day | `today 5pm` · `tomorrow` · `tmr 9am` · `fri` · `friday 17:00` · `next mon` · `next week` · `next month` · `weekend` |
| Date | `2026-12-01` · `2026-12-01 09:30` · `2026-12-01T09:30` · `dec 5` · `5th december 3pm` · `5 dec 2027` |
| Repeat | `every day at 9` · `daily 18:00` · `every weekday 9:00` · `every mon,wed 10am` · `every mon and fri` · `every weekend` |
| | `every other day` · `every 2 weeks on fri at 17:00` · `every month on the 1st` · `monthly` · `every year on mar 2` · `yearly` |
| | `every 30 minutes` · `every 2 hours` · `hourly` |

How times are resolved:

- A **day without a time** (`tomorrow`, `fri`, `every day`) uses the
  **Hour for a day without a time** setting (09:00). `tonight` means 20:00.
- A **clock time that has passed today** means tomorrow, and the status line
  says so (`09:00 has passed today, so tomorrow`).
- A **weekday** means the next one: today if the time is still ahead,
  otherwise next week. `next fri` is never today.
- A **date without a year** means the next such date. An **explicit date in
  the past** is refused, and so is `today` at a time that has passed.
- `in 3 days` and `tomorrow` keep the **wall-clock time** across a DST change.
  `in 72h` is exactly 72 hours.
- At the end of the text, the time must start with `in`, `at`, `on`,
  `every`, `today`, `tomorrow`, `tonight`, `next`, `this`, a weekday, a month,
  or a number. So `/remind Buy 12 eggs` and `/remind Call Tom` stay plain text.
- A bare number is not a time (`/remind 5 things`); use `at 5` or `5pm`.
- Mistakes are explained instead of guessed: `25:00 is not a time`,
  `2026-02-30 is not a date`, `2025-01-01 09:00 is in the past`.

**Repeating reminders** keep their wall-clock time across daylight-saving
changes. On the 31st of a short month, a monthly reminder falls on the
month's last day. Each year a 29 February reminder falls on the 28th when there
is no 29th. A time that doesn't exist on a DST day (02:30 when clocks jump
02:00 → 03:00) moves forward by the jump (03:30). A time that happens twice
(clocks fall back) fires once, the first time. Minute and hour intervals are
exact durations.

## The reminders view

`Alt-r` or `/remind`:

```text
 [All]  Follow-ups   Done    Tab: all · follow-ups · done   — times in Asia/Bangkok

Overdue · 1
! #3   due 2h ago               Reply to Alice  · Alice
Upcoming · 2
· #5   today 15:30 (in 2h)      Call the bank
↻ #6   tomorrow 09:00 (in 23h)  Standup  · every weekday at 09:00
Done · latest 1
✓ #1   done today 10:02         Pay rent
```

Marks: `!` overdue, `↻` repeating, `↩` follow-up, `✓` done.

| Key | |
|---|---|
| `↑` `↓` `j` `k` `Home` `End` | move |
| `Enter` | details: chat, quoted message, repeat rule, created, snoozes |
| `a` | add (linked to the chat that was open, if any) |
| `e` | edit |
| `x` | done (repeating reminders: stop) |
| `s` | snooze by the **Snooze** length |
| `z` | snooze until… (a form) |
| `n` | skip to the next occurrence (repeating) |
| `d` · `Delete` | delete |
| `u` | undo |
| `Tab` · `Shift-Tab` | All · Follow-ups · Done |
| `h` | help |
| `Backspace` | back (from details or help) |
| `Esc` | close |

The view refreshes while it's open, so a reminder moves to *Overdue* as soon
as it goes off.

## Follow-ups

In a chat, `/followup` (or `Alt-f`) makes a reminder called *Reply to
NAME*, due after the **Follow-up delay** (2 hours). `/followup tomorrow 9am`
picks the time, and `/followup 30m send the slides` adds a note.

- **The message you're answering** is quoted: the last message you received in
  that chat while Nudge was running. The quote shows in the alert and the
  details.
- **One per chat.** A second `/followup` in the same chat moves the existing
  follow-up instead of adding another.
- **It finishes itself** when you send a message in that chat (from rune or
  any other device). The status line confirms (`follow-up #4 with Alice done
  — you replied`). Turn this off with **Finish follow-ups when I reply**.
- Opening a chat with an open follow-up shows it on the status line.
- Follow-ups don't repeat. Use `/remind every …` for that.

Made in a chat, `/remind` reminders are **linked** to that chat too. The chat's
name shows in the list, the alert and the details. Turn this off with **Link
/remind to the open chat**, or unlink one reminder by clearing *Chat* in its
edit form. If a chat is renamed, its reminders show the new name.

## Notifications and missed reminders

When a reminder is due:

- **Alerts: desktop + status line** (default): a desktop notification
  (*Reminder · Alice*, *Follow up: Team*, or *Reminder*) with the text, the
  quoted message, and how late it is if it is late. The status line shows it
  too. Desktop notifications follow rune's own notification setting: if that
  is off, they're off.
- **desktop** or **status line** for only one of the two.
- More than three due at once arrive as one summary notification.
- A one-time reminder stays **overdue** until you finish, snooze, edit or
  delete it. **Repeat overdue alerts** alerts again every N minutes (off by
  default).
- A repeating reminder alerts, then moves to its next occurrence.

**While rune is closed** nothing can run. At the next start, Nudge goes
through what came due:

| Missed while rune was closed | |
|---|---|
| one summary (default) | One notification: *N reminder(s) came due while rune was closed*. |
| one each | One notification per reminder (three, then a summary). |
| list only | No notification. The status line says what was missed, and the view lists it as overdue. |

A repeating reminder whose occurrences were all missed alerts **once**
(*missed 3 times*) and moves on to its next future occurrence. It never
replays them one by one. With **Missed repeats: skip silently** it just moves
on. If the computer was asleep, reminders go off within 30 seconds of waking,
marked late (*was due 2h ago*).

**No double alerts.** Each reminder records which occurrence it alerted for,
and that record is stored in the same reply as the notification, before it.
Racing timers, reloading the plugin, an update, and restarting rune all
leave it alone.

## Settings

On Nudge's page in *Settings › Plugins*:

| Setting | Default | |
|---|---|---|
| Time zone | *(empty = UTC)* | IANA name (`Asia/Bangkok`, `Europe/Berlin`, `America/New_York`, any letter case) or an offset (`UTC+7`, `UTC+05:30`, `GMT-3`). An unknown name falls back to UTC and says so. |
| Clock | 24-hour | `24-hour` (15:30) or `12-hour` (3:30 pm). |
| Alerts | desktop + status line | `desktop + status line`, `desktop`, `status line`. |
| Default delay (minutes) | 60 | `/remind TEXT` with no time. |
| Follow-up delay (minutes) | 120 | `/followup` with no time. |
| Snooze (minutes) | 10 | `s` and `/remind snooze N`. |
| Hour for a day without a time | 9 | `tomorrow`, `fri`, `every day`… |
| Repeat overdue alerts (minutes) | 0 | 0 = alert once. |
| Missed while rune was closed | one summary | `one summary`, `one each`, `list only`. |
| Missed repeats | alert once | `alert once`, `skip silently`. |
| Link /remind to the open chat | on | |
| Finish follow-ups when I reply | on | |
| Quote the last message in follow-ups | on | Off: nothing from your messages is stored. |
| Keep done reminders | 30 | 0–100. Older finished reminders are deleted. |

Changing the time zone moves **repeating** reminders to the new zone's wall
clock ("every day at 9" stays at 9). **One-time** reminders keep the moment
they were set for.

## Permissions

Nudge asks for three of rune's five permissions. rune shows them when you
enable it, and a new version asking for more has to be approved again.

| Permission | Why |
|---|---|
| `notify` | Desktop notifications when a reminder is due. |
| `storage` | Keeping reminders across restarts, under Nudge's own prefix in rune's local store. Other plugins can't see them. |
| `read_messages` | Follow-ups: knowing when **you** reply in a chat (to finish its follow-up), and the last message you received there (to quote it). |

What Nudge does with `read_messages`: it keeps the last received message of
each chat **in memory only** (up to 200 chats, gone when rune quits). It writes
a message to storage only when you make a follow-up for that chat, with
*Quote the last message in follow-ups* on. Nothing leaves your machine:
plugins have no network access.

Nudge does **not** ask for `send_messages` or `compose`. It can't send
messages or type into the message line, so nothing is ever sent for you.

rune's permissions are all or nothing. Without `read_messages`, follow-ups
would still work, but they could not finish themselves or quote messages. rune
can't grant only some of a plugin's permissions, so to run without it you
would remove it from `plugin.json` and build your own copy.

## How it works

```text
 rune (host)                          Nudge (wasm)
 ───────────                          ────────────
 /remind, /followup, Alt-r/f  ──▶     every event: fire what is due (by the event's clock)
 view keys, forms             ──▶                  then handle the event
 message, chat_opened         ──▶                  then queue storage writes, re-arm the timer
 timers: wake, beat, flush    ──▶     ◀── actions: store_set, notify, status, open_view, set_timer
```

- **Clock.** A WebAssembly plugin has no clock, so Nudge uses the time rune
  sends with every event. Due times are stored as UTC instants.
  Repeating rules are stored as wall-clock times in your zone and expanded
  with the IANA time-zone database built into the module (`chrono-tz`).
- **Timers.** One `wake` timer points at the earliest due time (re-armed up
  to rune's 7-day timer limit). A 30-second `beat` covers sleep: rune's
  timers use a monotonic clock that stops while the computer sleeps, and wall
  time doesn't. A reminder that comes due while you're typing a command is
  fired by that command's event.
- **Storage.** One key per reminder (`r.N`, JSON with a format version) plus
  `meta` (the next number, so numbers are never reused). rune takes at most 64
  actions from one reply, so writes are queued and sent in batches. A short
  `flush` timer sends the rest.
- **Start-up.** Every stored record is checked: JSON, ids, times, rules,
  lengths. Unreadable records are skipped, logged and reported on the status
  line, and left in storage. Records written by a newer version are left
  alone. Then everything due is fired, and the timer is armed.
- **Cost.** Plugins run on a fuel budget of 200 M per event. Nudge uses about
  0.1 M for a command. The worst case is start-up with 200 daily reminders
  after rune was closed for three years, at about 30 M. Repeat rules jump
  straight to the next aligned day, week, month or year instead of walking
  the calendar.

## Limitations

These come from rune's plugin API (1.0). Nudge works around them where it can.

| Limitation | Effect on Nudge |
|---|---|
| Nothing runs while rune is closed | Reminders can't go off then. They're announced at the next start (see [missed reminders](#notifications-and-missed-reminders)). |
| No time zone from the host, and no clock in WebAssembly | You set the time zone once. The default is UTC, with a hint on the status line. |
| Commands get the open chat, not a selected message | A reminder can't be attached to an arbitrary message. Follow-ups quote the last message received in the chat, and only while Nudge is running, because plugins can't read chat history. |
| Plugins can't open a chat | Alerts and the view name the chat, but can't jump to it. |
| Background events can't open views or forms | A due reminder alerts by notification and status line. You act on it with `Alt-r`. |
| Notifications have no buttons, and rune's notification setting applies | Snooze and done happen in the view or with commands. If rune's notifications are off, so are Nudge's desktop alerts. |
| Forms are text fields only | *When* is typed (`tomorrow 9am`). There is no date picker. |
| 256 storage keys per plugin | At most 240 reminders, open and done. Finished ones are pruned first when room is needed (**Keep done reminders**). |
| `?` and `Esc` belong to rune in plugin views | Nudge's help is on `h`, and the view closes with `Esc`. |
| Permissions are all or nothing | `read_messages` can't be optional (see [Permissions](#permissions)). |

Deliberately not included: **sending messages** ("send *happy birthday* at
9am"). rune's API could do it with the `send_messages` permission. Nudge
leaves it out so that it can never send anything in your name.

## Building and testing

```sh
rustup target add wasm32-unknown-unknown
cargo test                  # 70 tests, natively
cargo clippy --all-targets
scripts/build.sh            # → nudge.wasm (commit it: rune installs plugins prebuilt)
scripts/build.sh check      # fails if the committed nudge.wasm is out of date
scripts/build.sh install    # build and copy into ~/.local/share/rune/plugins/nudge
```

The build remaps local paths, so the module contains no file names from your
machine, and is reproducible with the same toolchain. The SDK
(`rune-plugin`) comes from the rune repository, pinned to a commit in
`Cargo.toml`.

The tests run the plugin logic directly:

- `src/zone.rs`, `src/recur.rs`, `src/when.rs`, `src/fmt.rs`,
  `src/model.rs`, `src/settings.rs`: zones and DST gaps and overlaps
  (Berlin, New York, Samoa's skipped day), repeat rules (checked against a
  day-by-day scan in four zones), the time parser, display, stored-record
  validation, settings.
- `tests/plugin.rs`, through a stand-in host (`tests/common`) that enforces
  rune's rules: forms and new views only after a user action, 64 actions per
  reply, storage and timer limits, and only status and storage during
  shutdown. Covered: firing, no duplicates across timers and restarts,
  missed reminders in each mode, sleep, long waits beyond the 7-day timer
  limit, DST, time zone changes, repeating and skipped reminders, snooze,
  done, delete and undo, the view and forms, follow-ups (quoting, moving,
  finishing on reply), alerts, nagging, the 12-hour clock, the storage limit,
  batching, corrupt and newer records, and a three-year absence with 200
  repeating reminders.

Layout:

```text
plugin.json        manifest: commands, keys, settings, permissions
nudge.wasm         the built module (committed)
src/lib.rs         the plugin entry point (export_plugin!)
src/app.rs         events → actions: firing, commands, follow-ups, forms, storage
src/app/view.rs    the list, details and help views
src/when.rs        reading times
src/recur.rs       repeat rules
src/zone.rs        time zones, DST gaps and overlaps
src/model.rs       the stored reminder and its checks
src/settings.rs    settings
src/fmt.rs         showing times
scripts/build.sh   build, check, install
```

## License

MIT. See [LICENSE](LICENSE).
