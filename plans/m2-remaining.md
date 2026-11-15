# M2 (browsing parity) — what is left

Done so far: day, custom (2–7 days), week, month, schedule and year views; the list of
views as a menu; calendar tick boxes; the event details card with the event's own time
zone; the sidebar's small month; go to date; the `?` shortcuts list; background sync;
remembered series occurrences; settings for theme, first day of the week, 12/24-hour
clock, custom view length and zoom; the app reopens on the last view; a day's full list
behind "N more" in the month view.

What the design (`plans/i-want-to-create-deep-hamster.md`, milestone M2) still asks for.
None of it depends on later milestones.

## Views and navigation

- **Schedule view**: fixed 30 days per page; the web scrolls on endlessly.
- **Month view**: bars that continue from or into another week should show it
  (`continues_before` / `continues_after` are computed in `crates/core/src/month.rs` and
  unused by the window).
- **Sync on focus**: fetch changes when the window regains focus, next to the timer.
  Slint's `.slint` language offers no window-focus event; needs the winit window event.

## Settings

- Seeding the settings once from the first account's Google settings (`settings.list`):
  week start, time format, time zone.
- The editor's time fields are 24-hour whatever the time format.

## Time zones

- Display zone is the computer's (`local_zone`); a setting to choose it is open.
- Secondary time zone and world clock are M7.

## Look

- **Google's current calendar colours**: the API returns the classic palette
  (`backgroundColor`); the web maps those to its modern palette. The mapping table has to
  be taken from the web app, not guessed.
- Event colours tuned for the dark theme.
- **Strings from Rust** ("All day", "Until …", month and weekday names, messages) are
  not yet marked for translation; the `.slint` side is.
- Renderer choice (OpenGL vs CPU, memory) was left open in M0 "until M2": measure with the
  real app and decide.

## Robustness

- Cache compaction after account removal: confirmed once by the owner on the real cache.
- Shortcuts: verify the full list against the web app's `?` dialog (design, "My calls").
