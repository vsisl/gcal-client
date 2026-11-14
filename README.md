# gcal-client

A native desktop calendar that works with Google Calendar and shows several Google accounts
(for example personal and work) in one view. It aims to behave like the Google Calendar web
interface, including its keyboard shortcuts.

**Status:** early development, not usable yet. `gcal-client` is a working name.

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
