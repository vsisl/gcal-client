# gcal-client

Google Calendar native desktop client, behaves just like Google calendar web interface but supports multiple Google accounts 
natively, just like the mobile Google Calendar app does.

Several accounts (for example personal and work) are shown in one view, with the web
interface's keyboard shortcuts. It aims to be fast and light on memory.

Not affiliated with or endorsed by Google. Google Calendar is a trademark of Google LLC.

**Status:** early development, Linux only so far. `gcal-client` is a working name.

| Works | Not yet |
|---|---|
| Several accounts merged in one view, calendars ticked on and off | Guests: inviting, answering, editing events that have guests |
| Day, week, 4 days, month and schedule views; small month in the sidebar | Year view, go to date, search |
| Create, edit, move, resize and delete events, with undo | Date and time pickers (dates and times are typed), custom repeat rules |
| Repeating events: "this / this and following / all" | Reminders on the desktop, tray icon, running in the background |
| Light and dark theme, zoom of the day columns | Tasks, birthdays, out of office and other special entries |
| Works offline for viewing; syncs every five minutes | Packaging (Flatpak), Windows and macOS |

There is no installer yet: you build it yourself, and you need your own Google sign-in key
(below).

## Building

Requires the Rust toolchain ([rustup](https://rustup.rs); the version is pinned in
`rust-toolchain.toml`) and, on Debian/Ubuntu:

```sh
sudo apt install pkg-config libfontconfig-dev libxkbcommon-dev libxcb-shape0-dev libxcb-xfixes0-dev libdbus-1-dev
```

Then:

```sh
cargo run --release -p gcal-client
```

## Your own Google sign-in key

Google does not allow an open-source project to publish its sign-in key, and this app has
not been through Google's review yet. Until it has, everyone who runs it creates a key of
their own, once. It is free.

1. Open the [Google Cloud Console](https://console.cloud.google.com/) and create a project.
2. Under **APIs & Services → Library**, enable the **Google Calendar API**.
3. Under **APIs & Services → OAuth consent screen** (also called "Google Auth Platform"),
   set the app up as **External**, and add every Google account you want to use as a
   **test user**.
4. Under **Credentials → Create credentials → OAuth client ID**, choose **Desktop app**.
5. Put the client ID and secret it shows into `~/.config/gcal-client/client.toml`:

   ```toml
   client_id = "1234567890-abc.apps.googleusercontent.com"
   client_secret = "GOCSPX-..."
   ```

Keep that file to yourself and never commit it. While the Google project is in "testing",
Google ends each sign-in after seven days; the app then asks you to add the account again.

## Using it

Start the app and choose **Add account**: your browser opens Google's sign-in page, and the
account's calendars appear once you have agreed. Repeat for further accounts. The first sync
of a large account can take a few minutes; a bar under the top bar and a count show that it
is alive.

- **Look around:** the buttons at the top switch views; the small month jumps to a day.
- **Create:** click an empty time (or drag one out), click the strip above the hours or a
  day in the month for an all-day event, or press `c`. "More options" opens the editor.
- **Change:** drag an event to move it, drag its lower edge to resize it, or open it and
  press `e` for the editor.
- **Delete:** open the event and press `Delete`.
- **Undo:** "Undo" in the bar that appears at the bottom left, or `z`.
- **Zoom** the day columns: `Ctrl` and the mouse wheel (or two fingers on a touchpad),
  `Ctrl +` / `Ctrl −`, pinch on a touch screen, or the buttons in the settings.

Changes go straight to Google; nothing is kept for later. Without a connection the app
shows what it fetched last and lets you change nothing until the connection is back.

### Keyboard shortcuts

| Key | What it does |
|---|---|
| `t` | Today |
| `j` or `n` / `k` or `p` | Next / previous period |
| `d` `w` `m` `x` `a` (or `1`–`5`) | Day, week, month, 4 days, schedule |
| `c` | Create an event |
| `e` | Edit the open event |
| `Delete` | Delete the open event |
| `z` | Undo the last change |
| `Ctrl+S` | Save in the editor |
| `r` | Refresh |
| `s` | Settings |
| `Ctrl +` / `Ctrl −` / `Ctrl 0` | Zoom in / out / usual size |
| `?` | Show this list |
| `Esc` | Close whatever is open |

### Where things are kept

| What | Where |
|---|---|
| Your sign-in key | `~/.config/gcal-client/client.toml` |
| The accounts' sign-ins | The system keyring (GNOME Keyring, KWallet, ...), never a plain file |
| A copy of your calendars, and the app's settings | `~/.local/share/gcal-client/cache.db` |

The app talks to Google and to nothing else: no statistics, no crash reports, no update
checks. Removing an account in the app deletes its sign-in and its cached data.

### From the terminal

`gcal-client` without arguments opens the window. For looking into sign-in and sync
problems there are also:

```
gcal-client login            sign in to a Google account in your browser, then sync it
gcal-client sync             fetch changes for every signed-in account
gcal-client accounts         list signed-in accounts and their calendars
gcal-client logout <email>   remove an account and its cached data
```

## For developers

The design, the decisions behind it and the roadmap are in
[plans/i-want-to-create-deep-hamster.md](plans/i-want-to-create-deep-hamster.md); what is
left of each milestone is in the other files under [plans](plans). Known gaps are tracked
as GitHub issues. Conventions for working on the code, by hand or with a coding agent, are
in [AGENTS.md](AGENTS.md).

| Crate | Purpose |
|---|---|
| `crates/core` | Pure calendar logic: model, repeat rules, layout of the views |
| `crates/google` | Sign-in and the Google Calendar API client |
| `crates/store` | Local SQLite cache |
| `crates/sync` | Sync engine |
| `crates/app` | The application: window (Slint), background worker, editor logic |

Checks, the same ones the pull-request CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

`cargo test` covers the logic and the API client against a mock of Google. The window is
tested end to end against a real test account with the scripts in [tools/e2e](tools/e2e);
`AGENTS.md` says how to run them safely.

## Licence

GPL-3.0-or-later, see [LICENSE](LICENSE).
