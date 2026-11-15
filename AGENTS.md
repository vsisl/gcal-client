# gcal-client

Google Calendar native desktop client, behaves just like Google calendar web interface but supports multiple Google accounts 
natively, just like the mobile Google Calendar app does.

Written in Rust with Slint, GPLv3, Linux first. It is open source: every user signs in with
their own accounts.

Read first: `README.md` (what it does, how to build and run), then
`plans/i-want-to-create-deep-hamster.md` (design, decisions, milestones M0–M9). The other
files in `plans/` say what is done and what is left of each milestone; open gaps are
GitHub issues.

## Decisions that stand

Made by the owner; do not reopen them without being asked.

- Pure-Rust native UI with Slint. No web view, no Electron.
- Look and behaviour follow the web app as closely as is legal: same layout, shortcuts
  and wording; no Google logos or product names in the app's own name.
- An event that is in two accounts is shown twice. Settings are app-wide and local.
- Offline is view-only: no change is kept for later.
- New events go to the calendar used last. A feature Google offers no API for is handed
  to the browser.
- The app talks to Google only: no telemetry, crash reports or update checks.
- The Google sign-in key is never in the repository (Google's terms forbid it); each
  developer has one in `~/.config/gcal-client/client.toml`. Google's review of the app
  is part of M8.
- Packaging will be Flatpak only. English, with every string in `.slint` marked `@tr`.

## Working rules

**Git**
- Work on a branch, never on `main`. Make granular commits as you go, one per working
  step, each with a message that explains why.
- Commit dead ends and their fixes too (what was tried, the error, what replaced it), so
  the history shows the path. Never squash.
- Ask before pushing. Pull requests are merged with a merge commit.
- Everything reaches `main` through a pull request, and every pull request gets a Codex
  review before it is merged, however small it is:
  1. CI must be green.
  2. Run a Codex review of the pull request's diff (read-only; Codex has no network
     access, so it only writes the review).
  3. Post the review, as Codex wrote it, as a comment on the pull request.
  4. Fix each finding on the branch, or file it as a GitHub issue if it is to wait, and
     answer on the pull request with what was done about each.

**Secrets and real data**
- Never print, paste or commit the contents of `~/.config/gcal-client/client.toml` or
  anything from the keyring.
- The developer's cache (`~/.local/share/gcal-client/cache.db`) holds real calendars.
  Read it only when needed, and never write to it from a script.
- Never run anything that writes to Google against the developer's own accounts. Writes
  are tested with a dedicated test account only (see "End to end" below).

**Checks before every commit**

```sh
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Do not pipe these into `tail` or `grep` when deciding whether they passed: the pipe hides
the exit status (this once let a failing commit through). CI runs the same three on pull
requests.

**Reporting**
- Say plainly what was tested and how, and what was not. "Compiles" is not "works".
- New logic gets a test. A bug gets a test that fails without the fix.

## Layout

| Path | What is there |
|---|---|
| `crates/core` | No I/O. `model` (events, calendars), `recurrence` (expanding series, cutting and splitting them), `week` / `month` / `schedule` (layout of the views), `text` |
| `crates/google` | `auth` (browser sign-in with PKCE, token refresh), `client` (Calendar API: list, insert, patch, delete, instances), `config` |
| `crates/store` | SQLite cache: accounts, calendars, events (each with its complete JSON), sync tokens, settings. Migrations by `user_version` |
| `crates/sync` | Full and incremental sync of one account, with progress |
| `crates/app/src/main.rs` | Entry point and the small command line |
| `crates/app/src/gui.rs` | The window's logic: what is shown, and planning of every change (`plan_deletion`, `plan_drag`, `plan_form`) with its undo |
| `crates/app/src/editor.rs` | The editor's form: fields ↔ Google's resource, and what to send for a change. No window, no network |
| `crates/app/src/worker.rs` | Background thread: sign-in, sync, and every write to Google |
| `crates/app/ui/app.slint` | The whole window |
| `tools/e2e` | End-to-end scripts that drive the window against a test account |
| `plans` | Design and per-milestone plans |
| `spikes` | Throwaway experiments from M0; not part of the workspace's product |

## How it hangs together

- The window's thread never waits on the network. It sends a `Command` to the worker;
  the worker posts back into the window's event loop.
- A write goes to Google first; the cache is then updated from Google's answer, and the
  window redraws from the cache. Nothing is written to the cache in the hope that Google
  will agree.
- A change to an existing event carries the `etag` of the version the user saw
  (`If-Match`). A change made elsewhere is then refused (HTTP 412), the calendar is
  synced, and a dialog says so. Never overwrite silently. This is the rule for new
  code; it does not hold everywhere yet: an occurrence of a series that was never
  changed has no cached version and is written without one (issue 7), and so is the
  repair after a failed split. Creating an event has nothing to compare with.
- Writes run one at a time, in the order sent, under the same lock a sync holds. The
  window keeps what it sent in a queue (`pending`) and matches the worker's answers
  against it in order. Every write command must therefore be answered exactly once,
  whether it succeeded or not.
- The store keeps each event's complete JSON. Send only the fields that changed (see
  `editor::diff`), so that what the app does not understand stays untouched.
- Repeating events: the cache holds the series and its changed or cancelled occurrences;
  occurrences are worked out for the range on screen (`Series`). A change asks "this /
  this and following / all": one occurrence is an event of its own, found at Google by
  its original start; "following" cuts the series and creates a second one.
- Times: instants are UTC, the display zone is the computer's. Arithmetic on days is
  done on the local clock (so a move keeps the time of day across a clock change), and
  for a series on the series' own zone.
- Input from Google is not trusted: calendars shared by strangers are among it. No
  slicing of such text at byte positions, no unbounded expansion of repeat rules.

## Slint: what we learned

- Never replace a list model; patch its rows (`patch()` in `gui.rs`). Replacing the model
  of a few hundred events drops to 12–15 frames per second.
- Cards that are typed into are overlays inside the window, not `PopupWindow` (which
  takes the keyboard away from the text field).
- One `FocusScope` (`keys`) handles the shortcuts. Whatever is open on top must take all
  keys before the calendar's shortcuts are looked at; add a new card to that order.
- Rust refers to what is on screen by an instance index that is only valid until the
  next redraw. Anything that outlives a redraw (the editor, a pending question) must
  carry the event's identifiers, not the index. Drags compare `generation`.
- A dragged element moves under the pointer, so measure a drag in window coordinates
  taken at the press (`absolute-position` + `mouse-x/y`), not inside the element.
- A `TextInput`'s `text` binding breaks once the user types; report edits with
  `edited` and rebuild the card (`if visible:`) to start fresh.
- Touchpad pinch reaches the app on macOS only (winit); on Linux the gesture for zoom
  is Ctrl + scroll.

## Testing

**Unit and mock-server tests** (`cargo test`): logic in `core` and `editor.rs`, the
planning functions in `gui.rs`, the API client and the sync against `wiremock`.

**Driving the window.** A build with Slint's test server lets a script read the element
tree, click, drag, type and take screenshots:

```sh
SLINT_EMIT_DEBUG_INFO=1 cargo build --features ui-testing -p gcal-client --target-dir target/uitest
# Under another name, so that stopping it cannot stop the owner's running app:
cp target/uitest/debug/gcal-client <scratch folder>/gcal-uitest
SLINT_MCP_PORT=8766 XDG_DATA_HOME=<data folder> <scratch folder>/gcal-uitest
```

The scripts in `tools/e2e` show how to talk to it (JSON-RPC over HTTP at `/mcp`). Things
to know: an element query returns each element once per path to it (deduplicate); only
elements with an accessible role are found, and a press goes to an element's centre, so
empty places and thin handles cannot be pressed; keys cannot be sent with Ctrl held;
elements scrolled out of view are not found. Look at the screenshots: a check that
passes for the wrong reason is worse than none.

To look at the window without a sign-in key, also point `XDG_CONFIG_HOME` at an empty
folder: nothing can then be sent to Google, and every change fails with a dialog. The
keyring is still the real one, whatever the folders: removing an account in a test app
(or `logout`) deletes the owner's real sign-in. Never do that in a test.

**End to end, against Google.** Only with the test account the owner has set aside for
this; ask the owner which one it is. It must be signed in to the app once (by the owner,
in the browser), and it must be an account made for testing: the test app can reach
every calendar that account has access to, so no real calendar may be shared to it with
write access.

```sh
# A data folder that knows the test account and no other. It prints the calendars
# the account can reach: read that list. Go on only if this succeeds (&&).
python3 tools/e2e/make_test_cache.py <test account> <data folder> &&
export XDG_DATA_HOME=<data folder> &&
# The event a script starts from, dated in the current week, and created with the
# test build's `insert` (account, calendar, file):
python3 tools/e2e/fixture.py series <scratch folder>/fixture.json
<scratch folder>/gcal-uitest insert <test account> <test account> <scratch folder>/fixture.json
SLINT_MCP_PORT=8766 timeout 300 <scratch folder>/gcal-uitest &
python3 tools/e2e/e2e.py <screenshot folder> <data folder>/gcal-client/cache.db
```

| Script | Covers | Needs first |
|---|---|---|
| `e2e.py` | create, delete, undo; a series: this / following / all | the `series` fixture |
| `drag.py` | move by dragging, undo | the `plain` fixture |
| `sdrag.py` | dragging occurrences: this / all / following | the `series` fixture |
| `ed.py` | the editor, also on a series | the `series` fixture |
| `quick.py` | quick create and its undo | — |
| `zoom.py` | zoom through the settings | — |

Each script that needs a fixture uses it up or changes it: insert a fresh one before
each. `ed.py … clean` deletes what the scripts left behind: every event on the month on
screen whose title starts with "AT " (so the owner must not keep events named like that
in the test calendar); it does not look at other months.

The fixtures and the checks are for the week with today in it (`MONDAY` in
`common.py`), which is the week the app opens on. `common.py` holds what the scripts
share. Known limits: `ed.py` types an end time of 23:30 for an event created "now", so
it fails late in the evening; a run across midnight on Sunday mixes two weeks.

Rules for these runs:
- Always with `XDG_DATA_HOME` on the one-account folder, never on the real one.
- Stop the test app with `pkill -x gcal-uitest`. `pkill -f` matches the shell that runs
  the script, and the name `gcal-client` would stop the owner's own app.
- Leave the test calendar as it was found. Events of the scripts are named "AT …";
  anything else in it is the owner's.
- The Undo offer lasts ten seconds, so a script must not wait longer before pressing `z`.
- Add a step to these scripts for every new kind of write.

Not covered by any test yet: Google failing or being slow, and the gestures the test
server cannot make (issue 9).

## Style

- Code comments say why, in plain sentences. No comment that repeats the code.
- User-facing text is plain English without jargon; errors say what happened and what
  the user can do.
- Keep dependencies few; the UI uses the app's own small components (`TextButton`,
  `Field`, `Choice`, ...) rather than a widget set.
