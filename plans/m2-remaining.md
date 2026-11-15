# M2 (browsing parity) — what is left

The first M2 pull request (branch `m2-browsing`) delivered: day, 4-day, week, month and
schedule views; calendar tick boxes; the event details card; the sidebar's small month;
the `?` shortcuts list; background sync every five minutes; remembered series occurrences
(issue 2); and the four recurrence/sign-in fixes from the Codex review of PR 1.

This file lists what the design (`plans/i-want-to-create-deep-hamster.md`, milestone M2)
still asks for, to pick up later. Each item is one or a few commits; none depends on M3.

## Views and navigation

- **Year view** (`y` / `6`): twelve small months; clicking a day opens it. The small
  month's cell maths in `View::update_mini_month` (`crates/app/src/gui.rs`) can be reused.
- **Go to date** (`g`): a small in-window overlay with a text field (see the M0 finding:
  overlays, not `PopupWindow`, where typing is involved).
- **Custom view**: the web's "4 days" is really a setting (2–7 days, 2–4 weeks). Needs the
  settings store below.
- **Month view details**: the "N more" line opens the day view for now; the web shows a
  small card listing that day's events. Bars that continue from or into another week
  should show it (`continues_before` / `continues_after` are already computed in
  `crates/core/src/month.rs` and unused by the window).
- **Schedule view**: fixed 30 days per page; the web scrolls on endlessly.
- **View switcher**: five buttons crowd the top bar; the web uses a drop-down. Do it with
  the Material component set when menus arrive.
- **Sync on focus**: fetch changes when the window regains focus, next to the timer.

## Settings (local, app-wide)

- The settings table and a first settings card (theme only) exist since the M3 branch.
  Still open: seeding once from the first account's Google settings (`settings.list`).
- **Week start** (Monday is hard-coded in `week_start_of`), **12/24-hour clock** (`clock`
  in `gui.rs` is 24-hour only), custom view length, remembered last view.

## Time zones

- Display zone is the computer's (`local_zone`). Add: a setting to choose it, and the
  event's own zone in the details card when it differs.
- Secondary time zone and world clock are M7.

## Look

- **Google's current calendar colours**: the API returns the classic palette
  (`backgroundColor`); the web maps those to its modern palette. Add the mapping table.
- **Dark theme**: done on the M3 branch, with a desktop / light / dark choice in the
  settings card. Still open: event colours tuned for dark.
- **Hour labels**: the label of the first visible hour is clipped by the all-day strip's
  line.
- **Strings from Rust** ("All day", "Until …", month and weekday names) are not yet marked
  for translation; the `.slint` side is.
- Renderer choice (OpenGL vs CPU, memory) was left open in M0 "until M2": measure with the
  real app and decide.

## Robustness

- A failed background sync while offline shows the red status text every five minutes;
  distinguish "offline" (quiet, grey) from real failures.
- Cache compaction after account removal has only been checked by running `VACUUM` by
  hand on a copy; exercise the real path once.
- Shortcuts: verify the full list against the web app's `?` dialog (design, "My calls").

## At the end

`cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`, walk
through every view against the real accounts, pull request, Codex review.
