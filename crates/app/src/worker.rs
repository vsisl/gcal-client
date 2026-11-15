//! Background thread for everything slow: signing in, syncing, the keyring.
//!
//! The window's thread never waits on the network. It sends a [`Command`] here; when the
//! cache changed or something is worth telling the user, the worker posts back to the
//! window's event loop.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gcal_google::{ClientConfig, Endpoints, GoogleClient, HttpClient, SCOPE_CALENDAR, TokenSet};
use gcal_sync::{SyncError, SyncReport};
use slint::Weak;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Mutex, watch};

use crate::gui::{AppWindow, StatusKind};
use crate::vault::Vault;
use crate::{load_config, open_store};

/// How long an opened sign-in page may sit unanswered before the attempt is dropped.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Limits on one request to Google. Without them a connection that stalls (suspend, captive
/// portal) would never finish, and the sync holding it would block every later one.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug)]
pub enum Command {
    AddAccount,
    SyncAll,
    RemoveAccount(String),
}

/// Starts the worker thread and returns the channel for sending it commands.
pub fn spawn(ui: Weak<AppWindow>) -> UnboundedSender<Command> {
    let (sender, receiver) = unbounded_channel();
    std::thread::Builder::new()
        .name("worker".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("cannot start the background runtime");
            let tasks = tokio::task::LocalSet::new();
            runtime.block_on(tasks.run_until(run(ui, receiver)));
        })
        .expect("cannot start the background thread");
    sender
}

/// What the worker is doing right now. Several commands can run at once, so the status
/// line is derived from this rather than set by whichever command finished last.
#[derive(Default)]
struct Activity {
    waiting_for_browser: u32,
    syncing: u32,
    /// The latest failure, shown until the user starts something new.
    error: Option<String>,
}

#[derive(Clone)]
struct Worker {
    ui: Weak<AppWindow>,
    http: HttpClient,
    activity: Rc<RefCell<Activity>>,
    /// Number of the newest sign-in attempt. Asking to add an account again replaces the
    /// earlier attempt (the user may have closed its tab): an attempt gives up as soon as
    /// this no longer holds its own number.
    sign_in_attempt: Rc<watch::Sender<u64>>,
    /// Held while accounts are synced or removed, so the two never interleave.
    cache: Rc<Mutex<()>>,
}

/// Counts one activity for as long as it lives, also when its task is cancelled.
struct Active<'a> {
    worker: &'a Worker,
    counter: fn(&mut Activity) -> &mut u32,
}

impl Drop for Active<'_> {
    fn drop(&mut self) {
        *(self.counter)(&mut self.worker.activity.borrow_mut()) -= 1;
        self.worker.publish();
    }
}

/// Returned by a sign-in that was replaced by a newer one; not shown to the user.
#[derive(Debug)]
struct Superseded;

impl std::fmt::Display for Superseded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sign-in replaced by a newer attempt")
    }
}

impl std::error::Error for Superseded {}

async fn run(ui: Weak<AppWindow>, mut commands: UnboundedReceiver<Command>) {
    let http = HttpClient::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .unwrap_or_default();
    let worker = Worker {
        ui,
        http,
        activity: Rc::default(),
        sign_in_attempt: Rc::new(watch::Sender::new(0)),
        cache: Rc::default(),
    };
    while let Some(command) = commands.recv().await {
        // Each command runs as its own task: a sign-in waiting on the browser must not
        // hold up a sync.
        let worker = worker.clone();
        tokio::task::spawn_local(async move {
            let result = match command {
                Command::AddAccount => worker.add_account().await,
                Command::SyncAll => worker.sync_all().await,
                Command::RemoveAccount(email) => worker.remove_account(&email).await,
            };
            if let Err(error) = result
                && !error.is::<Superseded>()
            {
                worker.activity.borrow_mut().error = Some(format!("{error:#}"));
            }
            worker.publish();
            worker.data_changed();
        });
    }
}

/// One line for the status bar about an account's sync, or `None` if all went well.
fn sync_problem(email: &str, result: &Result<SyncReport, SyncError>) -> Option<String> {
    match result {
        Err(error) => Some(format!("{email}: {error}")),
        Ok(report) => {
            let (_, reason) = report.failed_calendars.first()?;
            Some(format!(
                "{email}: {} calendar(s) could not be synced ({reason})",
                report.failed_calendars.len()
            ))
        }
    }
}

impl Worker {
    /// Shows the status that follows from what is running now.
    fn publish(&self) {
        let (kind, detail) = {
            let activity = self.activity.borrow();
            if activity.waiting_for_browser > 0 {
                (StatusKind::WaitingForBrowser, String::new())
            } else if activity.syncing > 0 {
                (StatusKind::Syncing, String::new())
            } else if let Some(error) = &activity.error {
                (StatusKind::Failed, error.clone())
            } else {
                (StatusKind::Idle, String::new())
            }
        };
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_status_kind(kind);
            ui.set_status_detail(detail.into());
        });
    }

    fn start(&self, counter: fn(&mut Activity) -> &mut u32) -> Active<'_> {
        *counter(&mut self.activity.borrow_mut()) += 1;
        self.publish();
        Active {
            worker: self,
            counter,
        }
    }

    /// Forgets the previous failure: the user has started something new.
    fn clear_error(&self) {
        self.activity.borrow_mut().error = None;
    }

    fn data_changed(&self) {
        let _ = self.ui.upgrade_in_event_loop(|ui| ui.invoke_data_changed());
    }

    fn client(&self, config: ClientConfig, refresh_token: String) -> GoogleClient {
        GoogleClient::new(
            self.http.clone(),
            config,
            Endpoints::default(),
            refresh_token,
        )
    }

    async fn add_account(&self) -> Result<()> {
        self.clear_error();
        let mut attempt = 0;
        self.sign_in_attempt.send_modify(|newest| {
            *newest += 1;
            attempt = *newest;
        });
        let mut newest = self.sign_in_attempt.subscribe();

        // Until Google has answered, a newer attempt replaces this one, wherever it is:
        // unlocking the keyring, or waiting on a browser tab that may have been closed.
        let (config, vault, tokens) = tokio::select! {
            signed_in = self.sign_in() => signed_in?,
            _ = newest.wait_for(|newest| *newest != attempt) => return Err(Superseded.into()),
        };
        let refresh_token = tokens
            .refresh_token
            .context("Google sent no refresh token")?;

        let client = self
            .client(config, refresh_token.clone())
            .with_access_token(tokens.access_token, tokens.expires_at);
        let email = client.account_email().await?;

        let _syncing = self.start(|activity| &mut activity.syncing);
        // The sign-in is saved under the same lock as the account row. Saved before it, a
        // removal of this very account that was waiting on a running sync would delete the
        // fresh sign-in and leave an account that can never sync again. The lock is handed
        // out in order, so that removal now runs first and this account is added after it.
        let _cache = self.cache.lock().await;
        vault.save(&email, &refresh_token).await?;
        let mut store = open_store()?;
        let account = store.add_account(&email)?;
        self.data_changed();
        let result = gcal_sync::sync_account(&client, &mut store, account).await;
        if let Some(problem) = sync_problem(&email, &result) {
            bail!(problem);
        }
        Ok(())
    }

    /// Opens the browser and waits for the user to finish signing in there.
    async fn sign_in(&self) -> Result<(ClientConfig, Vault, TokenSet)> {
        let config = load_config()?;
        let endpoints = Endpoints::default();
        let vault = Vault::open().await?;

        let pending = gcal_google::begin_login(&config, &endpoints, &[SCOPE_CALENDAR]).await?;
        let _waiting = self.start(|activity| &mut activity.waiting_for_browser);
        open::that_detached(&pending.auth_url).context("cannot open your browser")?;
        let finish = pending.finish(&self.http, &config, &endpoints);
        let tokens = tokio::time::timeout(SIGN_IN_TIMEOUT, finish)
            .await
            .context("sign-in was not finished in the browser")??;
        Ok((config, vault, tokens))
    }

    async fn sync_all(&self) -> Result<()> {
        // Something is already syncing or removing an account; that run's status stands.
        let Ok(_cache) = self.cache.try_lock() else {
            return Ok(());
        };
        let mut store = open_store()?;
        let accounts = store.accounts()?;
        if accounts.is_empty() {
            return Ok(());
        }
        self.clear_error();
        let _syncing = self.start(|activity| &mut activity.syncing);
        let config = load_config()?;
        let vault = Vault::open().await?;

        // One account failing (say, an expired sign-in) must not stop the others.
        let mut problems = Vec::new();
        for account in accounts {
            let Some(refresh_token) = vault.load(&account.email).await? else {
                problems.push(format!("{}: not signed in", account.email));
                continue;
            };
            let client = self.client(config.clone(), refresh_token);
            let result = gcal_sync::sync_account(&client, &mut store, account.id).await;
            problems.extend(sync_problem(&account.email, &result));
            // Redrawing closes an open details card and costs time, so the window is only
            // told when there is something new to show. A failed run may have saved part
            // of what it fetched.
            let unchanged = matches!(
                &result,
                Ok(report) if report.calendars_changed == 0 && report.events_changed == 0
            );
            if !unchanged {
                self.data_changed();
            }
        }
        if !problems.is_empty() {
            bail!("{}", problems.join("; "));
        }
        Ok(())
    }

    async fn remove_account(&self, email: &str) -> Result<()> {
        self.clear_error();
        // Waits for a running sync to finish rather than pulling the account from under it.
        let _cache = self.cache.lock().await;
        let store = open_store()?;
        let Some(account) = store.accounts()?.into_iter().find(|a| a.email == email) else {
            return Ok(());
        };
        Vault::open().await?.delete(email).await?;
        store.remove_account(account.id)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_problem_reports_errors_and_unsynced_calendars_but_not_success() {
        assert_eq!(sync_problem("a@b.c", &Ok(SyncReport::default())), None);
        let partial = SyncReport {
            calendars_synced: 3,
            failed_calendars: vec![("cal".into(), "HTTP 403".into())],
            ..SyncReport::default()
        };
        assert_eq!(
            sync_problem("a@b.c", &Ok(partial)).unwrap(),
            "a@b.c: 1 calendar(s) could not be synced (HTTP 403)"
        );
    }
}
