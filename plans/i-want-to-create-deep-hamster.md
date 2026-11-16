# Native multi-account client for Google Calendar — design and roadmap

## Context

Google Calendar's web interface shows one Google account at a time; the Android app merges several.
The goal is an open-source native desktop app (Linux first, Windows/macOS later) that behaves like the
web interface — same views, same keyboard shortcuts, same features — but shows all of a user's Google
accounts (e.g. personal + work) in one calendar. It must be fast and light on memory.

Rust is not installed. Machine: Ubuntu 26.04, GNOME, Wayland.

## Decisions

Decided by the user:

- Open-source, hosted on GitHub; each end user signs in with their own Google accounts.
- Full parity with the web interface is the target, delivered in milestones (nothing cut by choice).
- Pure-Rust native UI (not Tauri/Electron).
- An event present in two accounts is shown twice, as on Android.
- Sign-in: the user's own Google Cloud key now (read from local config, never committed);
  Google's app review is a late milestone before public release; bring-your-own key stays supported.
- Claude builds; the user reviews milestones and handles steps only they can do (Google Cloud Console, domain).
- **Toolkit: Slint 1.18** with its official Material 3 component set, **licence GPLv3**.
  (Only stable-API candidate; pickers, menus, dialogs, tray, key bindings, screen-reader support.)
- **Look: as close to the web app as legally possible** — same layout, spacing, fonts, colours and
  icon shapes using open Material assets; only logo, name and Google product icons differ.
  Light/dark/system theme like the web.
- **New events** default to the calendar the previous event was created in (remembered across
  restarts; initially the first account's main calendar); quick-create and the editor always show
  a calendar picker grouped by account.
- **Settings are app-wide**, seeded once from the first account added, then local. Reminder defaults
  stay per calendar as Google stores them.
- **Closing the window** keeps the app running (sync + reminders) by default; configurable in
  settings (quit on close), with optional start-at-login. Tray icon where the desktop supports one;
  Ctrl+Q quits.
- **Offline is view-only**, like the web: no queued edits, so no conflict resolution UI.
- **Descriptions:** shown formatted; edited as lightly marked-up text with the web's toolbar buttons
  and Ctrl+B/I/U, plus a formatted preview; saved in Google's HTML subset.
- **Features without a public API** keep their menu entries/buttons, marked "opens in browser", and
  open the matching Google Calendar page for the right account.
- **Repo:** private on GitHub from M0 (for CI); made public at the end of M3. Pushes only after asking.
- **Packaging: an AppImage first** (changed 2026-11-16; it was "Flatpak only"): published
  as GitHub releases, with update information so that Gear Lever keeps it up to date. It
  needs no sandbox work and no store review. A Flatpak (Flathub) later; token storage
  already goes through `oo7`, which works in the sandbox too.
- **Languages:** English, with every interface string marked for translation from day one (`@tr`);
  dates, times and week start follow settings/locale.
- **Privacy:** the app talks only to Google. No statistics, crash reporting or update checks;
  a "copy diagnostics" button supports GitHub bug reports.
- **Roadmap order** as listed below, unchanged.

My calls (change any of these on review):

- **Fallback toolkit:** iced, only if the M0 spikes show Slint cannot do the week grid or popovers.
- **Name:** working name `gcal-client` (crate, binary, repo). "GCal" is not an official Google product
  name, but Google's reviewers may still judge it too close; I could not verify either way. The display
  name lives in one constant and is settled in the release milestone, before Google's review.
- **Tagline:** "Works with Google Calendar" (wording Google's branding rules allow).
- **Google API access:** a small hand-written client (`reqwest` + `serde`), not the generated
  `google-calendar3` crate (maintenance mode, missing newer fields).
- **Storage/sync:** local SQLite cache (`rusqlite`), incremental sync with sync tokens, polling
  (push needs a public HTTPS server); online writes use ETag checks and re-fetch on conflict.
- **Time:** `chrono` + `chrono-tz` (what the `rrule` recurrence crate uses).
- **Shortcuts:** the 21 on Google's current help page plus the long-standing `k`/`p`, `y`, `q`;
  verified against the web app's `?` dialog during M2.

## Known parity gaps (no public Google API)

Cannot be built by any third-party app; each is handed off to the browser where a web page exists:

- Appointment schedules / booking pages (bookings still appear as normal events).
- "Propose a new time".
- Room browser by building/floor for non-admin users (rooms can still be added by name/address).
- Task times and repeating tasks (the Tasks API keeps only the date).
- Writing settings back to Google (week start, etc. are read from Google, changes stored locally);
  secondary time zone, world clock and density are local-only.
- Trash list (replaced by a local "recently deleted" with restore).
- Gemini features, Keep side panel.
- Working hours, out-of-office, focus time, working location: Workspace accounts only.
- File attachments: links first; Drive picker later using the non-restricted `drive.file` scope.

## Architecture

Cargo workspace; UI thread runs Slint's event loop, a background `tokio` runtime does network and
database work, and they talk over channels (`slint::invoke_from_event_loop` for UI updates).

| Crate | Responsibility |
|---|---|
| `crates/core` | Pure logic, no I/O: event/calendar model, recurrence expansion (`rrule`), overlapping-event layout, date-range maths per view, shortcut table, "this / following / all" series splitting. Heavily unit-tested. |
| `crates/google` | OAuth (system browser, loopback redirect, PKCE via `oauth2`), token storage (`keyring`/`oo7`), typed clients for Calendar v3, Tasks, People. |
| `crates/store` | SQLite schema + migrations: accounts, calendars, events, sync tokens, local settings. |
| `crates/sync` | Per-account sync engine: full + incremental sync, 410 reset, polling and sync-on-focus, backoff on rate limits. |
| `crates/app` | The binary: `.slint` UI files, view-models, notifications (`notify-rust`), tray, opening the browser (`open`). |

Every row in the store carries its account ID, so the merged view is a query across accounts and
every write goes back through the owning account's token.

## Milestones

Each milestone ends with a runnable app, passing tests, and commits per working step (dead ends
included, no co-author trailer, no pushing without asking). Each gets its own detailed task plan
when it starts; M0 and M1 are detailed here.

**M0 — Foundations and risk spikes**
1. Install Rust via `rustup` plus Slint's Linux build dependencies; add `rust-toolchain.toml`.
2. Initial commit on `main`: workspace skeleton, GPLv3 `LICENSE`, `README.md`, `.gitignore`,
   this design saved as `docs/design.md`; then work on branches.
3. Private GitHub repo (created and pushed only after asking); GitHub Actions: `cargo fmt --check`,
   `clippy -D warnings`, `cargo test`.
4. Throwaway spikes under `spikes/` to test Slint's two weak points:
   - week grid with ~300 events: scrolling, drag to create/move/resize, on Wayland and X11;
   - popover + autocomplete dropdown near the window edge.
5. Gate: if either spike fails, repeat it in iced and report before going further.

**M1 — Sign in and see your calendars (read-only)**
1. User creates a Google Cloud project and Desktop OAuth client (I provide click-by-click steps);
   key goes in `~/.config/gcal-client/client.toml`.
2. OAuth flow, multiple accounts, tokens in the system keyring, add/remove account UI.
3. Fetch calendar lists and events into SQLite (full sync).
4. Week view showing all accounts merged, calendar colours, all-day row, current-time line.

**M2 — Browsing parity**: day/week/month/year/schedule/custom views, sidebar (mini month, calendars
grouped by account with toggles), navigation, all keyboard shortcuts and the `?` help, recurrence
expansion, time zones, incremental sync with polling, event detail popover.

**M3 — Editing**: quick-create popover, full editor, drag move/resize, delete, undo, recurring edits
(this / this and following / all), colours, reminders, visibility, conflict handling, description
editor. Editing is disabled with a clear banner while offline. Repo goes public at the end of M3.

**M4 — People**: guests, RSVP, guest permissions, contact autocomplete, "find a time" from free/busy,
Meet links, rooms, attachment links.

**M5 — Desktop integration**: reminder notifications, tray icon, run in background (configurable
close behaviour, start-at-login).

**M6 — Other entry types**: Tasks, out of office, focus time, working location, birthdays, working hours.

**M7 — Calendar management and the rest**: create/delete/subscribe calendars, sharing, ICS
import/export and URL subscriptions, search, settings, recently deleted, secondary time zone, world
clock, print.

**M8 — Public release**: final name, homepage + privacy policy on a domain, Google review, Flatpak
packaging and Flathub submission, release CI injecting the official key, accessibility pass.
*Brought forward in part (2026-11-16): the AppImage and its release workflow, the README for
the public, the privacy policy and homepage, and the checklist for Google's review
(`docs/google-review.md`). Left: the name, the review itself, the official key in releases,
the Flatpak, the accessibility pass.*

**M9 — Windows and macOS** builds, keychains, installers.

## Verification

- `crates/core`, `store`, `sync`: test-first unit tests; sync and API tests run against a local mock
  HTTP server (`wiremock`) with recorded Google responses, including 410, 412 and rate-limit cases.
- UI: Slint's testing backend for view-model and shortcut tests; screenshots of each view compared
  by eye against the web app.
- End of each milestone: `cargo fmt --check && cargo clippy -- -D warnings && cargo test`, then run
  the app against the user's real accounts and walk through that milestone's features.
- M0 gate: spike results (frame smoothness with 300 events, popup behaviour) reported with numbers.

## M0 findings (2026-11-14)

Both Slint spikes passed; Slint stays the toolkit. Rules the real app must follow:

- **Never replace a list model wholesale.** Resetting the model of 300 events gave 12-15 fps
  because Slint rebuilds every element; patching only changed rows gives ~90-100 fps (OpenGL)
  and ~45-55 fps (CPU renderer). View-models diff and patch rows.
- **Autocomplete lists are in-window overlay elements, not `PopupWindow`.** A `PopupWindow` takes
  keyboard focus, so typing and arrow keys stop reaching the text field (confirmed by hand and by
  the automated driver). `PopupWindow` is fine for click-to-open popovers such as event details.
- **Popover placement is computed by us**: shift left / flip above the anchor to stay inside the
  window (works in the spike; same behaviour as the web app, where popovers never leave the page).
- **Memory:** ~110 MB peak with the OpenGL renderer, ~54 MB with the CPU renderer, for the
  300-event week. Renderer choice is open; revisit with the real app in M2.
- **Automated UI driving:** building with `slint = { features = ["mcp"] }` and
  `SLINT_EMIT_DEBUG_INFO=1`, then running with `SLINT_MCP_PORT=<port>`, starts Slint's embedded
  MCP server (HTTP JSON-RPC at `/mcp`): element tree, click, drag, type, screenshots. Use it to
  verify UI work from M1 on; the feature must be off in release builds.
