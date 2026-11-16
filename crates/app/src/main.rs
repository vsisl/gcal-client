//! The desktop application.
//!
//! Started without arguments it opens the window. The commands below are a small
//! command-line front end to the same pieces, kept for diagnosing sign-in and sync.

mod editor;
mod gui;
mod vault;
mod worker;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use gcal_google::{ClientConfig, Endpoints, GoogleClient, HttpClient, SCOPE_CALENDAR};
use gcal_store::Store;
use vault::Vault;

/// Name shown to users. Kept in one place because the public name is not final.
pub const APP_NAME: &str = "gcal-client";

const USAGE: &str = "usage: gcal-client [command]
  (no command)     open the calendar window
  login            sign in to a Google account in your browser, then sync it
  sync             fetch changes for every signed-in account
  accounts         list signed-in accounts and their calendars
  logout <email>   remove an account and its cached data";

fn config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("no configuration folder on this system")?
        .join(APP_NAME)
        .join("client.toml"))
}

pub(crate) fn open_store() -> Result<Store> {
    let dir = dirs::data_dir()
        .context("no data folder on this system")?
        .join(APP_NAME);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(Store::open(&dir.join("cache.db"))?)
}

pub(crate) fn load_config() -> Result<ClientConfig> {
    let path = config_path()?;
    ClientConfig::load(&path).context("the Google sign-in key is missing or invalid (see README)")
}

async fn login() -> Result<()> {
    let config = load_config()?;
    let endpoints = Endpoints::default();
    let http = HttpClient::new();
    let vault = Vault::open().await?;

    let pending = gcal_google::begin_login(&config, &endpoints, &[SCOPE_CALENDAR]).await?;
    println!(
        "Opening your browser to sign in. If nothing opens, visit:\n\n{}\n",
        pending.auth_url
    );
    // Failing to launch a browser is not fatal: the address is printed above.
    let _ = open::that(&pending.auth_url);
    let tokens = pending.finish(&http, &config, &endpoints).await?;
    let refresh_token = tokens
        .refresh_token
        .context("Google sent no refresh token")?;

    let client = GoogleClient::new(http, config, endpoints, refresh_token.clone())
        .with_access_token(tokens.access_token, tokens.expires_at);
    let email = client.account_email().await?;
    vault.save(&email, &refresh_token).await?;

    let mut store = open_store()?;
    let account = store.add_account(&email)?;
    println!("Signed in as {email}. Syncing…");
    let report = gcal_sync::sync_account(&client, &mut store, account).await?;
    print_report(&email, &report);
    Ok(())
}

async fn sync() -> Result<()> {
    let config = load_config()?;
    let vault = Vault::open().await?;
    let mut store = open_store()?;
    let accounts = store.accounts()?;
    if accounts.is_empty() {
        bail!("no accounts yet; run `gcal-client login` first");
    }
    for account in accounts {
        let Some(refresh_token) = vault.load(&account.email).await? else {
            println!(
                "{}: no saved sign-in; run `gcal-client login`",
                account.email
            );
            continue;
        };
        let client = GoogleClient::new(
            HttpClient::new(),
            config.clone(),
            Endpoints::default(),
            refresh_token,
        );
        match gcal_sync::sync_account(&client, &mut store, account.id).await {
            Ok(report) => print_report(&account.email, &report),
            Err(error) => println!("{}: {}", account.email, printable(&error.to_string())),
        }
    }
    Ok(())
}

/// Text that came from Google (calendar names, error bodies) with control characters
/// removed, so a crafted name on a shared calendar cannot send escape codes to the terminal.
fn printable(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn print_report(email: &str, report: &gcal_sync::SyncReport) {
    println!(
        "{email}: {} calendars synced, {} events changed",
        report.calendars_synced, report.events_changed
    );
    for (calendar, reason) in &report.failed_calendars {
        println!(
            "  could not sync {}: {}",
            printable(calendar),
            printable(reason)
        );
    }
}

fn accounts() -> Result<()> {
    let store = open_store()?;
    let calendars = store.calendars()?;
    for account in store.accounts()? {
        println!(
            "{} ({} events cached)",
            account.email,
            store.event_count(account.id)?
        );
        for calendar in calendars.iter().filter(|c| c.account_id == account.id) {
            let shown = if calendar.entry.selected {
                "shown"
            } else {
                "hidden"
            };
            println!("  {} [{shown}]", printable(calendar.entry.display_name()));
        }
    }
    Ok(())
}

async fn logout(email: &str) -> Result<()> {
    let store = open_store()?;
    let Some(account) = store.accounts()?.into_iter().find(|a| a.email == email) else {
        bail!("no account {email}");
    };
    Vault::open().await?.delete(email).await?;
    store.remove_account(account.id)?;
    println!("Removed {email}.");
    Ok(())
}

/// Creates an event from a resource in a file, as the window would. Only in the build
/// for automated tests of the window, which need events the window cannot make yet
/// (repeating ones, ones with guests) in the test account.
#[cfg(feature = "ui-testing")]
async fn insert(email: &str, calendar_id: &str, file: &str) -> Result<()> {
    let resource: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?,
    )?;
    let mut store = open_store()?;
    // Made for the one-account folders of the end-to-end tests. Refusing anything else
    // keeps a mistyped command from creating events through the real cache.
    let accounts = store.accounts()?;
    if accounts.len() != 1 {
        bail!(
            "this cache holds {} accounts; `insert` only runs on a test folder with one",
            accounts.len()
        );
    }
    let Some(account) = accounts.into_iter().find(|a| a.email == email) else {
        bail!("no account {email}");
    };
    let refresh_token = Vault::open()
        .await?
        .load(email)
        .await?
        .with_context(|| format!("{email}: not signed in"))?;
    let client = GoogleClient::new(
        HttpClient::new(),
        load_config()?,
        Endpoints::default(),
        refresh_token,
    );
    // Never with invitations: test events may name made-up guests.
    let created = client.insert_event(calendar_id, &resource, false).await?;
    store.apply_event(account.id, calendar_id, &created)?;
    println!("{}", created["id"].as_str().unwrap_or_default());
    Ok(())
}

/// Sends a change to an event as the one account of a test folder and prints Google's
/// answer; the cache is left alone. Only in builds for automated tests: for trying what
/// Google does with a change before the window is taught to make it.
#[cfg(feature = "ui-testing")]
async fn patch(
    email: &str,
    calendar_id: &str,
    event_id: &str,
    file: &str,
    notify: bool,
) -> Result<()> {
    let changes: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(file).with_context(|| format!("cannot read {file}"))?,
    )?;
    let store = open_store()?;
    let accounts = store.accounts()?;
    if accounts.len() != 1 || accounts[0].email != email {
        bail!("`patch` only runs on a test folder whose one account is {email}");
    }
    let refresh_token = Vault::open()
        .await?
        .load(email)
        .await?
        .with_context(|| format!("{email}: not signed in"))?;
    let client = GoogleClient::new(
        HttpClient::new(),
        load_config()?,
        Endpoints::default(),
        refresh_token,
    );
    let now = client
        .patch_event(calendar_id, event_id, &changes, None, notify)
        .await?;
    println!("{}", serde_json::to_string_pretty(&now)?);
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    if args.is_empty() {
        return gui::run();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        match args.as_slice() {
            ["login"] => login().await,
            ["sync"] => sync().await,
            ["accounts"] => accounts(),
            ["logout", email] => logout(email).await,
            #[cfg(feature = "ui-testing")]
            ["insert", email, calendar_id, file] => insert(email, calendar_id, file).await,
            #[cfg(feature = "ui-testing")]
            ["patch", email, calendar_id, event_id, file] => {
                patch(email, calendar_id, event_id, file, false).await
            }
            #[cfg(feature = "ui-testing")]
            ["patch", email, calendar_id, event_id, file, "--email"] => {
                patch(email, calendar_id, event_id, file, true).await
            }
            _ => {
                eprintln!("{USAGE}");
                std::process::exit(2);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printable_drops_escape_sequences_but_keeps_text() {
        assert_eq!(printable("Team\u{1b}[2J calendar\n"), "Team[2J calendar");
        assert_eq!(printable("Čeština ✓"), "Čeština ✓");
    }
}
