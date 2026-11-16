# Changelog

## 1.0.0 — 2026-11-16

First release, for rune plugin API 1.0.

- `/remind` with times in words (`2h`, `tomorrow 9am`, `fri 17:00`,
  `2026-12-01 09:30`, `dec 5`), before or after the text; repeating
  reminders (`every weekday 9:00`, `every 2 weeks on fri`, `monthly`,
  `every 30 minutes`).
- `/followup` for the open chat: quotes the message being answered, one per
  chat, finishes itself when you reply.
- The reminders view (`Alt-r`): overdue, upcoming and done; details; done,
  snooze, snooze until, skip, edit, delete, undo; filters; help.
- Desktop and status-line alerts, summaries for several at once, optional
  repeat alerts for overdue reminders.
- Missed reminders announced at start-up (summary, each, or list only);
  missed repeats collapsed into one alert or skipped.
- Time zones by IANA name or offset, with daylight-saving gaps and overlaps
  handled; 12- or 24-hour clock.
- Storage per reminder with validation on load; no duplicate alerts across
  timers, reloads and restarts.
