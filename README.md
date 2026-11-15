# gcal-client

A native desktop calendar that works with Google Calendar and shows several Google accounts
(for example personal and work) in one view. It aims to behave like the Google Calendar web
interface, including its keyboard shortcuts.

**Status:** early development. Viewing (day, week, month, 4 days, schedule) and editing
(create, change, move, delete, repeating events, undo) work; guests, reminders on the
desktop, tasks and packaging are still to come. To sign in you currently need your own
Google Cloud key. `gcal-client` is a working name.

Not affiliated with or endorsed by Google. Google Calendar is a trademark of Google LLC.

## Design

The design, decisions and roadmap are in [plans/i-want-to-create-deep-hamster.md](plans/i-want-to-create-deep-hamster.md).

## Building

Requires the Rust toolchain ([rustup](https://rustup.rs)) and, on Debian/Ubuntu:

```sh
sudo apt install pkg-config libfontconfig-dev libxkbcommon-dev libxcb-shape0-dev libxcb-xfixes0-dev libdbus-1-dev
```

Then:

```sh
cargo run -p gcal-client
```

## Testing

`cargo test` runs the unit tests and the tests against a mock of Google's API. The window
itself is tested end to end against a real test account with the scripts in
[tools/e2e](tools/e2e); the header of `e2e.py` says how to run them safely.

## Layout

| Crate | Purpose |
|---|---|
| `crates/core` | Pure calendar logic: model, recurrence, event layout, shortcuts |
| `crates/google` | Sign-in and Google API clients |
| `crates/store` | Local SQLite cache |
| `crates/sync` | Sync engine |
| `crates/app` | The application (Slint UI) |

## Licence

GPL-3.0-or-later, see [LICENSE](LICENSE).
