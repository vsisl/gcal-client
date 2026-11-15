# M3 (editing) — task plan

From the design (`plans/i-want-to-create-deep-hamster.md`): quick-create popover, full
editor, drag move/resize, delete, undo, recurring edits (this / this and following / all),
colours, reminders, visibility, conflict handling, description editor. Editing is disabled
with a clear banner while offline. The repo goes public at the end of M3.

The rest of M2 is parked in `plans/m2-remaining.md`.

## Rules for every write

- Online only: a write goes to Google first; the cache is updated from Google's answer
  (the returned event is applied through the store like a synced one). No queued edits.
- Every change and delete carries the event's `etag` (`If-Match`). On "changed elsewhere"
  (HTTP 412) the event is fetched again, the window shows the new version and says so;
  nothing is overwritten silently.
- A write goes through the owning account's token; the calendar's `accessRole` decides
  whether editing is offered at all (reader / freeBusyReader calendars are read-only).
- Anything in a writable calendar can be deleted in the app (user's decision after the
  first try: only read-only calendars are exempt). Deleting an event whose guests this
  account invited asks whether to send cancellation e-mails (`sendUpdates=all`).
  *Editing* events with guests waits for M4.
- Because these change real calendars and the test setup has no sign-in key, each slice is
  covered by mock-server tests and then tried by the user on a throwaway event.

## Slices, in order

1. **Google client**: `insert_event`, `patch_event`, `delete_event` with `If-Match`,
   `Changed` / `Gone` errors. *(done, first commit of the branch)*
2. **Write path**: worker commands `DeleteEvent`, `PatchEvent`, `CreateEvent`; store keeps
   each event's `etag` and applies a single returned event; the window reloads; status
   text for failures; `accessRole` in the calendar model.
3. **Delete** from the details card (and `Delete`/`Backspace` with the card open), with
   **undo** in a bottom bar (re-insert the saved resource; for a single occurrence,
   un-cancel it). *(done, including this / this and following / all for series; refused
   changes are shown in a dialog)*
4. **Quick create**: click or drag on empty time in the day columns (and a click in a
   month cell) opens a small card: title, time, calendar picker grouped by account,
   defaulting to the calendar last created in (stored locally). `c` opens it for "now".
   *(click, month cell, all-day strip and `c` done. Open, asked for by the user: the
   card should look as close to the web's as possible — title line, "Event" tab, date
   and time row that can be edited, calendar drop-down, "More options" and "Save" —
   which comes with the pickers of slice 7. Drag to set the length is also open.)*
5. **Drag to move and resize** in the day columns, 15-minute steps; all-day bars move by
   days. Optimistic draw, rolled back on failure.
6. **Recurring events**: "this event / this and following / all events" for delete, move
   and edit. Pure logic for splitting a series goes in `crates/core` with tests.
7. **Full editor** (`e` or "More options"): title, dates and times, all-day, time zone,
   repeat (presets plus custom), location, description (light markup, preview), calendar,
   colour, reminders, visibility, busy/free. Needs the Material component set for
   pickers and menus.
8. **Offline banner** and disabling of all editing controls when Google cannot be reached.
9. **Testing against Google without the user**: so far only the user can try writes,
   since the test copy of the app has no sign-in. Proposed: a calendar made for this
   ("gcal-client test") in one account, and the user's go-ahead for a scripted run
   that signs in as usual but creates, changes and deletes only in that calendar.
10. End of milestone: checks, walk-through on real accounts, pull request, Codex review,
   then make the repository public (ask first).
