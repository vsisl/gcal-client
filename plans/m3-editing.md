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

All built on the branch `m3-editing` and tested against Google with the test account
unless said otherwise.

1. **Google client**: insert, patch, delete with `If-Match`; `Changed` / `Gone`. *Done.*
2. **Write path**: worker commands, single-event apply in the store, failures in a
   dialog. *Done.*
3. **Delete** anything in a writable calendar, with undo; this / this and following /
   all for series; optional cancellation e-mails for one's own guests. *Done. The
   e-mail option has not been tried (the test account has nobody to write to).*
4. **Quick create**: click, drag out a time, month cell, all-day strip, `c`; card styled
   after the web's; undo. *Done. Dragging out a time is untried by the scripts.*
5. **Drag to move and resize**, 15-minute steps, all-day bars by days. *Done. The
   resize handle is untried by the scripts.*
6. **Repeating events**: this / this and following / all for delete, drag and the
   editor. *Done. No undo after a split ("this and following" edit).*
7. **Editor** (`e`, "Edit", "More options"): title, dates and times, all-day, repeat
   presets, location, description, calendar (new events), colour, reminder,
   visibility, busy/free. Sends only what changed. *Done.*
8. **Offline banner**, editing off while Google cannot be reached. *Done.*
9. **Tests against Google without the user**: `tools/e2e`. *Done.*
10. End of milestone: checks, pull request, Codex review, then make the repository
    public (ask first).

## Left out of M3 on purpose, for later

- Date and time pickers (dates and times are typed), custom repeat rules, the
  description's formatting toolbar and preview (an edited description is saved as
  plain text; an untouched one keeps its formatting).
- Changing an event's time zone; moving an event to another calendar.
- Editing or dragging events that have guests (M4, with notifications).
- Dragging in the month view, and between the all-day strip and the hours.
- Undo after "this and following" edits.
- The editor uses this app's own controls; the Material component set named in the
  design was not needed and is not a dependency.

## Found while testing against Google (2026-11-15)

- `tools/e2e/e2e.py` runs create / delete / undo for a plain and a repeating event against
  the test account (see its header); all steps pass. Extend it with every new write.
- Open: after a series is deleted, the next sync brings its cancelled occurrences back
  as stubs, and the store keeps them (a cancelled occurrence is kept for its series).
  They show nowhere but pile up; drop occurrences whose series is not in the cache.
- Open: the new-event card and details only reach events on screen; `c` late in the
  evening creates an event below the visible hours without scrolling to it.

## After the review of PR 4

The Codex review (a comment on the pull request) had 15 findings; all were dealt with on
the branch. What stayed open is in GitHub issues:

- #5 a split whose second half may or may not have been created (timeout);
- #6 undo after deleting a whole series that had changed occurrences;
- #7 unchanged occurrences are written without a version check;
- #8 edge cases of repeat rules;
- #9 tests for a failing or slow Google, and for gestures the test server cannot make.
