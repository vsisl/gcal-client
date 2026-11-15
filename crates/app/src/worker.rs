//! Background thread for everything slow: signing in, syncing, the keyring.
//!
//! The window's thread never waits on the network. It sends a [`Command`] here; when the
//! cache changed or something is worth telling the user, the worker posts back to the
//! window's event loop.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gcal_google::{ClientConfig, Endpoints, GoogleClient, HttpClient, SCOPE_CALENDAR};
use slint::Weak;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::gui::{AppWindow, StatusKind};
use crate::vault::Vault;
use crate::{load_config, open_store};

/// How long an opened sign-in page may sit unanswered before the attempt is dropped.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

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

#[derive(Clone)]
struct Worker {
    ui: Weak<AppWindow>,
    http: HttpClient,
    /// Set while a sign-in or a sync is under way, so a second one is not started on top.
    signing_in: Rc<Cell<bool>>,
    syncing: Rc<Cell<bool>>,
}

async fn run(ui: Weak<AppWindow>, mut commands: UnboundedReceiver<Command>) {
    let worker = Worker {
        ui,
        http: HttpClient::new(),
        signing_in: Rc::default(),
        syncing: Rc::default(),
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
            match result {
                Ok(()) => worker.report(StatusKind::Idle, String::new()),
                Err(error) => worker.report(StatusKind::Failed, format!("{error:#}")),
            }
            worker.data_changed();
        });
    }
}

impl Worker {
    fn report(&self, kind: StatusKind, detail: String) {
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_status_kind(kind);
            ui.set_status_detail(detail.into());
        });
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
        if self.signing_in.replace(true) {
            return Ok(());
        }
        let result = self.sign_in_and_sync().await;
        self.signing_in.set(false);
        result
    }

    async fn sign_in_and_sync(&self) -> Result<()> {
        let config = load_config()?;
        let endpoints = Endpoints::default();
        let vault = Vault::open().await?;

        let pending = gcal_google::begin_login(&config, &endpoints, &[SCOPE_CALENDAR]).await?;
        self.report(StatusKind::WaitingForBrowser, String::new());
        open::that_detached(&pending.auth_url).context("cannot open your browser")?;
        let finish = pending.finish(&self.http, &config, &endpoints);
        let tokens = tokio::time::timeout(SIGN_IN_TIMEOUT, finish)
            .await
            .context("sign-in was not finished in the browser")??;
        let refresh_token = tokens
            .refresh_token
            .context("Google sent no refresh token")?;

        let client = self
            .client(config, refresh_token.clone())
            .with_access_token(tokens.access_token, tokens.expires_at);
        let email = client.account_email().await?;
        vault.save(&email, &refresh_token).await?;

        let mut store = open_store()?;
        let account = store.add_account(&email)?;
        self.data_changed();
        self.report(StatusKind::Syncing, String::new());
        gcal_sync::sync_account(&client, &mut store, account).await?;
        Ok(())
    }

    async fn sync_all(&self) -> Result<()> {
        if self.syncing.replace(true) {
            return Ok(());
        }
        let result = self.sync_every_account().await;
        self.syncing.set(false);
        result
    }

    async fn sync_every_account(&self) -> Result<()> {
        let mut store = open_store()?;
        let accounts = store.accounts()?;
        if accounts.is_empty() {
            return Ok(());
        }
        self.report(StatusKind::Syncing, String::new());
        let config = load_config()?;
        let vault = Vault::open().await?;

        // One account failing (say, an expired sign-in) must not stop the others.
        let mut failures = Vec::new();
        for account in accounts {
            let Some(refresh_token) = vault.load(&account.email).await? else {
                failures.push(format!("{}: not signed in", account.email));
                continue;
            };
            let client = self.client(config.clone(), refresh_token);
            if let Err(error) = gcal_sync::sync_account(&client, &mut store, account.id).await {
                failures.push(format!("{}: {error}", account.email));
            }
            self.data_changed();
        }
        if !failures.is_empty() {
            bail!("{}", failures.join("; "));
        }
        Ok(())
    }

    async fn remove_account(&self, email: &str) -> Result<()> {
        let store = open_store()?;
        let Some(account) = store.accounts()?.into_iter().find(|a| a.email == email) else {
            return Ok(());
        };
        Vault::open().await?.delete(email).await?;
        store.remove_account(account.id)?;
        Ok(())
    }
}
