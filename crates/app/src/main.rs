//! The desktop application.
//!
//! Until the window exists (later in M1) this is a small command-line front end to the
//! pieces underneath: sign in, sync, and show what is cached.

mod vault;

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use gcal_google::{ClientConfig, Endpoints, GoogleClient, HttpClient, SCOPE_CALENDAR};
use gcal_store::Store;
use vault::Vault;

/// Name shown to users. Kept in one place because the public name is not final.
pub const APP_NAME: &str = "gcal-client";

const USAGE: &str = "usage: gcal-client <command>
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

fn open_store() -> Result<Store> {
    let dir = dirs::data_dir()
        .context("no data folder on this system")?
        .join(APP_NAME);
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    Ok(Store::open(&dir.join("cache.db"))?)
}

fn load_config() -> Result<ClientConfig> {
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
            Err(error) => println!("{}: {error}", account.email),
        }
    }
    Ok(())
}

fn print_report(email: &str, report: &gcal_sync::SyncReport) {
    println!(
        "{email}: {} calendars synced, {} events changed",
        report.calendars_synced, report.events_changed
    );
    for (calendar, reason) in &report.failed_calendars {
        println!("  could not sync {calendar}: {reason}");
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
            println!("  {} [{shown}]", calendar.entry.display_name());
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["login"] => login().await,
        ["sync"] => sync().await,
        ["accounts"] => accounts(),
        ["logout", email] => logout(email).await,
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}
