//! Background thread for everything slow: signing in, syncing, the keyring.
//!
//! The window's thread never waits on the network. It sends a [`Command`] here; when the
//! cache changed or something is worth telling the user, the worker posts back to the
//! window's event loop.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gcal_core::{Event, EventTime};
use gcal_google::{
    ApiError, AuthError, ClientConfig, Endpoints, GoogleClient, HttpClient, SCOPE_CALENDAR,
    TokenSet,
};
use gcal_store::{AccountId, Store};
use gcal_sync::{Progress, SyncError, SyncReport};
use serde_json::Value;
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
    /// Deletes an event, or part of a series, at Google and then from the cache. With
    /// `notify`, Google tells the guests.
    DeleteEvent {
        account: AccountId,
        calendar_id: String,
        deletion: Deletion,
        notify: bool,
    },
    /// Changes an event, or part of a series, as the user asked in the window.
    EditEvent {
        account: AccountId,
        calendar_id: String,
        edit: Edit,
        /// Whether Google is to tell the event's guests.
        notify: bool,
    },
    /// Changes fields of an event to take an earlier change back; with an `etag`, only
    /// if nothing else changed it since.
    PatchEvent {
        account: AccountId,
        calendar_id: String,
        event_id: String,
        etag: Option<String>,
        changes: Value,
    },
    /// Creates an event from a resource the window put together; with `notify`,
    /// Google invites its guests.
    CreateEvent {
        account: AccountId,
        calendar_id: String,
        resource: Value,
        notify: bool,
    },
    /// Undoes a deletion: creates the event again from its saved resource.
    RestoreEvent {
        account: AccountId,
        calendar_id: String,
        resource: Value,
    },
}

/// What exactly a change applies to.
#[derive(Debug, PartialEq)]
pub enum Edit {
    /// The event itself: an ordinary event, or a series with all its occurrences. With
    /// an `etag`, only if the event still is the version the window showed.
    Event {
        event_id: String,
        etag: Option<String>,
        changes: Value,
    },
    /// One occurrence of a series, told by when it would start unchanged.
    Occurrence {
        series_id: String,
        original_start: EventTime,
        /// Its own identifier and version, when the cache has them (a changed
        /// occurrence).
        known_id: Option<String>,
        etag: Option<String>,
        changes: Value,
    },
    /// An occurrence and all later ones: the series ends before it (`recurrence` is
    /// its shortened rule, `old_recurrence` the one it had) and `resource` becomes a
    /// new series from there on.
    Following {
        series_id: String,
        etag: Option<String>,
        recurrence: Vec<String>,
        old_recurrence: Vec<String>,
        resource: Value,
    },
}

/// What exactly a deletion removes.
#[derive(Debug, PartialEq)]
pub enum Deletion {
    /// The event itself: an ordinary event, or a series with all its occurrences. With
    /// an `etag`, only if the event still is the version the window showed.
    Event {
        event_id: String,
        etag: Option<String>,
    },
    /// One occurrence of a series, told by when it would start unchanged.
    Occurrence {
        series_id: String,
        original_start: EventTime,
        /// Its own identifier and version, when the cache has them (a changed
        /// occurrence).
        known_id: Option<String>,
        etag: Option<String>,
    },
    /// An occurrence and all later ones: the series gets the shortened `recurrence`.
    Following {
        series_id: String,
        recurrence: Vec<String>,
        etag: Option<String>,
    },
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
    /// Changes being sent to Google.
    saving: u32,
    /// Google could not be reached at the last sync. The window then shows what it has
    /// and lets nothing be changed.
    offline: bool,
    /// How far the running sync has come, in words; empty when there is nothing to say.
    progress: String,
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
    /// Whether the change being made is to be told to the event's guests. Changes run
    /// one at a time, so this belongs to the one that is running.
    notify: Rc<Cell<bool>>,
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
        notify: Rc::default(),
        cache: Rc::default(),
    };
    while let Some(command) = commands.recv().await {
        // Each command runs as its own task: a sign-in waiting on the browser must not
        // hold up a sync.
        let worker = worker.clone();
        tokio::task::spawn_local(async move {
            // A sync tells the window itself, and only when it changed something: every
            // reload closes an open details card and forgets the series' occurrences.
            let reload = !matches!(command, Command::SyncAll);
            // A change the user asked for and did not get is said in a dialog, not in
            // the status line, where it is easily missed.
            let failure = match command {
                Command::DeleteEvent { .. } => Some("The event was not deleted."),
                Command::EditEvent { .. } => Some("The event was not changed."),
                Command::PatchEvent { .. } => Some("The change could not be taken back."),
                Command::CreateEvent { .. } => Some("The event was not saved."),
                Command::RestoreEvent { .. } => Some("The event could not be brought back."),
                _ => None,
            };
            // Changes run one at a time, in the order they were asked for, and never
            // while a sync is saving what it fetched: a sync that started earlier could
            // otherwise put back the older version of an event just changed. The window
            // relies on the order too, to match each answer with its undo.
            let _cache = match command {
                Command::AddAccount | Command::SyncAll | Command::RemoveAccount(_) => None,
                _ => Some(worker.cache.lock().await),
            };
            let result = match command {
                Command::AddAccount => worker.add_account().await,
                Command::SyncAll => worker.sync_all().await,
                Command::RemoveAccount(email) => worker.remove_account(&email).await,
                Command::DeleteEvent {
                    account,
                    calendar_id,
                    deletion,
                    notify,
                } => {
                    let deleted = worker
                        .delete_event(account, &calendar_id, deletion, notify)
                        .await;
                    // The window offers to undo only what really happened, and for a
                    // single occurrence needs to know which event that was.
                    let (ok, id) = match &deleted {
                        Ok(id) => (true, id.clone()),
                        Err(_) => (false, String::new()),
                    };
                    let _ = worker
                        .ui
                        .upgrade_in_event_loop(move |ui| ui.invoke_delete_finished(ok, id.into()));
                    deleted.map(|_| ())
                }
                Command::EditEvent {
                    account,
                    calendar_id,
                    edit,
                    notify,
                } => {
                    worker.notify.set(notify);
                    let changed = worker.edit_event(account, &calendar_id, edit).await;
                    worker.notify.set(false);
                    // The window may have drawn the change already and takes it back on
                    // failure; to undo a change to one occurrence it needs to know
                    // which event that is.
                    // The version it now has makes the undo conditional as well.
                    let (ok, id, etag) = match &changed {
                        Ok((id, etag)) => (true, id.clone(), etag.clone()),
                        Err(_) => (false, String::new(), String::new()),
                    };
                    let _ = worker.ui.upgrade_in_event_loop(move |ui| {
                        ui.invoke_change_finished(ok, id.into(), etag.into())
                    });
                    changed.map(|_| ())
                }
                Command::PatchEvent {
                    account,
                    calendar_id,
                    event_id,
                    etag,
                    changes,
                } => worker
                    .patch_event(account, &calendar_id, &event_id, changes, etag.as_deref())
                    .await
                    .map(|_| ()),
                Command::CreateEvent {
                    account,
                    calendar_id,
                    resource,
                    notify,
                } => {
                    worker.notify.set(notify);
                    let created = worker.insert_event(account, &calendar_id, resource).await;
                    worker.notify.set(false);
                    // Always answered: on failure the window gives back what was
                    // typed.
                    let (ok, id, etag) = match &created {
                        Ok((id, etag)) => (true, id.clone(), etag.clone()),
                        Err(_) => (false, String::new(), String::new()),
                    };
                    let _ = worker.ui.upgrade_in_event_loop(move |ui| {
                        ui.invoke_event_created(ok, id.into(), etag.into())
                    });
                    created.map(|_| ())
                }
                Command::RestoreEvent {
                    account,
                    calendar_id,
                    resource,
                } => worker
                    .insert_event(account, &calendar_id, restorable(resource))
                    .await
                    .map(|_| ()),
            };
            if let Err(error) = result
                && !error.is::<Superseded>()
            {
                match failure {
                    Some(what) => {
                        let notice = format!("{what}\n\n{}", sentence(&format!("{error:#}")));
                        let _ = worker
                            .ui
                            .upgrade_in_event_loop(move |ui| ui.set_notice(notice.into()));
                    }
                    None => worker.activity.borrow_mut().error = Some(format!("{error:#}")),
                }
            }
            worker.publish();
            if reload {
                worker.data_changed();
            }
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
            } else if activity.saving > 0 {
                (StatusKind::Saving, String::new())
            } else if activity.syncing > 0 {
                (StatusKind::Syncing, activity.progress.clone())
            } else if let Some(error) = &activity.error {
                (StatusKind::Failed, error.clone())
            } else {
                (StatusKind::Idle, String::new())
            }
        };
        let offline = self.activity.borrow().offline;
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            ui.set_offline(offline);
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
        let result = self.sync_account(&client, &mut store, account).await;
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
        // Something is already syncing, changing an event or removing an account. A
        // change syncs when it is done, and a running sync's status stands.
        let Ok(_cache) = self.cache.try_lock() else {
            return Ok(());
        };
        self.sync_accounts().await
    }

    /// Syncs every account. The caller holds the cache lock.
    async fn sync_accounts(&self) -> Result<()> {
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
        let (mut reached, mut unreachable) = (false, false);
        for account in accounts {
            let Some(refresh_token) = vault.load(&account.email).await? else {
                problems.push(format!("{}: not signed in", account.email));
                continue;
            };
            let client = self.client(config.clone(), refresh_token);
            let result = self.sync_account(&client, &mut store, account.id).await;
            if let Err(error) = &result
                && no_connection(error)
            {
                // Being offline is a state, shown by a banner, not a failure to report
                // every few minutes; and nothing was fetched that would need drawing.
                unreachable = true;
                continue;
            }
            reached |= result.is_ok();
            problems.extend(sync_problem(&account.email, &result));
            // Redrawing closes an open details card and costs time, so the window is only
            // told when there is something new to show. A failed run may have saved part
            // of what it fetched.
            let unchanged = matches!(
                &result,
                Ok(report) if !report.changed
            );
            if !unchanged {
                self.data_changed();
            }
        }
        self.activity.borrow_mut().offline = unreachable && !reached;
        if !problems.is_empty() {
            bail!("{}", problems.join("; "));
        }
        Ok(())
    }

    /// Syncs one account, saying in the status line how far a long fetch has come.
    async fn sync_account(
        &self,
        client: &GoogleClient,
        store: &mut Store,
        account: AccountId,
    ) -> Result<SyncReport, SyncError> {
        let report = |progress: Progress| {
            let text = progress_text(progress);
            let changed = self.activity.borrow().progress != text;
            if changed {
                self.activity.borrow_mut().progress = text;
                self.publish();
            }
        };
        let result = gcal_sync::sync_account_reporting(client, store, account, &report).await;
        self.activity.borrow_mut().progress.clear();
        result
    }

    /// A client for the account that owns what is being changed.
    async fn account_client(&self, store: &Store, account: AccountId) -> Result<GoogleClient> {
        let email = store
            .accounts()?
            .into_iter()
            .find(|a| a.id == account)
            .map(|a| a.email)
            .context("the account is no longer signed in")?;
        let config = load_config()?;
        let refresh_token = Vault::open()
            .await?
            .load(&email)
            .await?
            .with_context(|| format!("{email}: not signed in"))?;
        Ok(self.client(config, refresh_token))
    }

    /// Deletes what `deletion` describes and returns the identifier of the occurrence
    /// that was removed, if it was a single one of a series (empty otherwise).
    async fn delete_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        deletion: Deletion,
        notify: bool,
    ) -> Result<String> {
        self.clear_error();
        let saving = self.start(|activity| &mut activity.saving);
        let mut store = open_store()?;
        let client = self.account_client(&store, account).await?;
        let removed = match deletion {
            Deletion::Event { event_id, etag } => {
                let deleted = client
                    .delete_event(calendar_id, &event_id, etag.as_deref(), notify)
                    .await;
                self.written(deleted).await?;
                store.remove_event(account, calendar_id, &event_id)?;
                return Ok(String::new());
            }
            Deletion::Occurrence {
                series_id,
                original_start,
                known_id,
                etag,
            } => {
                let id = match known_id {
                    Some(id) => id,
                    None => occurrence_id(&client, calendar_id, &series_id, original_start).await?,
                };
                let deleted = client
                    .delete_event(calendar_id, &id, etag.as_deref(), notify)
                    .await;
                self.written(deleted).await?;
                id
            }
            Deletion::Following {
                series_id,
                recurrence,
                etag,
            } => {
                let shortened = client
                    .patch_event(
                        calendar_id,
                        &series_id,
                        &serde_json::json!({ "recurrence": recurrence }),
                        etag.as_deref(),
                        notify,
                    )
                    .await;
                if let Some(series) = self.written(shortened).await? {
                    store.apply_event(account, calendar_id, &series)?;
                }
                String::new()
            }
        };
        // Google answers a change inside a series with other changes (the occurrence
        // marked as cancelled, changed occurrences past the new end removed): fetch them.
        drop(saving);
        let _ = self.sync_accounts().await;
        Ok(removed)
    }

    /// Sorts out Google's answer to a change. An event that is gone already is no
    /// failure (`None`: there is nothing to save); one changed elsewhere is, and the
    /// calendar is brought up to date so that the user sees what they would be changing.
    async fn written<T>(&self, answer: Result<T, ApiError>) -> Result<Option<T>> {
        match answer {
            Ok(answer) => Ok(Some(answer)),
            Err(ApiError::Gone) => Ok(None),
            Err(ApiError::Changed) => {
                let _ = self.sync_accounts().await;
                bail!(
                    "it was changed somewhere else in the meantime. The calendar now shows its current version; try again if you still want to"
                );
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Applies a change. Returns the identifier of the occurrence it was made to, if it
    /// was a single one of a series (empty otherwise), and the version the changed event
    /// now has (empty after a split).
    async fn edit_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        edit: Edit,
    ) -> Result<(String, String)> {
        match edit {
            Edit::Event {
                event_id,
                etag,
                changes,
            } => {
                let now = self
                    .patch_event(account, calendar_id, &event_id, changes, etag.as_deref())
                    .await?;
                Ok((String::new(), now))
            }
            Edit::Occurrence {
                series_id,
                original_start,
                known_id,
                etag,
                changes,
            } => {
                let id = match known_id {
                    Some(id) => id,
                    None => {
                        let store = open_store()?;
                        let client = self.account_client(&store, account).await?;
                        occurrence_id(&client, calendar_id, &series_id, original_start).await?
                    }
                };
                let now = self
                    .patch_event(account, calendar_id, &id, changes, etag.as_deref())
                    .await?;
                Ok((id, now))
            }
            Edit::Following {
                series_id,
                etag,
                recurrence,
                old_recurrence,
                resource,
            } => {
                // First the old series ends; only then does the new one begin, so that
                // a refusal leaves nothing doubled.
                self.patch_event(
                    account,
                    calendar_id,
                    &series_id,
                    serde_json::json!({ "recurrence": recurrence }),
                    etag.as_deref(),
                )
                .await?;
                let Err(failed) = self
                    .insert_event(account, calendar_id, restorable(resource))
                    .await
                else {
                    return Ok((String::new(), String::new()));
                };
                // The second half did not come into being, and the first must not stay
                // cut: its later occurrences would simply be gone. Give it its rule back
                // (without a second round of e-mails to the guests).
                self.notify.set(false);
                let restored = self
                    .patch_event(
                        account,
                        calendar_id,
                        &series_id,
                        serde_json::json!({ "recurrence": old_recurrence }),
                        None,
                    )
                    .await;
                match restored {
                    Ok(_) => Err(failed),
                    Err(also) => bail!(
                        "the series was cut at this event, but its later part could not be created ({failed:#}), and the cut could not be taken back ({also:#}). The later events are missing: set the series' end date back by hand"
                    ),
                }
            }
        }
    }

    /// Changes fields of an event, saves what Google made of it and returns the version
    /// it now has.
    async fn patch_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        event_id: &str,
        changes: Value,
        etag: Option<&str>,
    ) -> Result<String> {
        self.clear_error();
        let saving = self.start(|activity| &mut activity.saving);
        let mut store = open_store()?;
        let client = self.account_client(&store, account).await?;
        let answer = client
            .patch_event(calendar_id, event_id, &changes, etag, self.notify.get())
            .await;
        let Some(changed) = self.written(answer).await? else {
            let _ = self.sync_accounts().await;
            bail!("it no longer exists");
        };
        store.apply_event(account, calendar_id, &changed)?;
        drop(saving);
        let _ = self.sync_accounts().await;
        Ok(changed["etag"].as_str().unwrap_or_default().to_owned())
    }

    /// Creates an event at Google, saves what Google made of it and returns its
    /// identifier and version.
    async fn insert_event(
        &self,
        account: AccountId,
        calendar_id: &str,
        resource: Value,
    ) -> Result<(String, String)> {
        self.clear_error();
        let _saving = self.start(|activity| &mut activity.saving);
        let mut store = open_store()?;
        let client = self.account_client(&store, account).await?;
        let created = client
            .insert_event(calendar_id, &resource, self.notify.get())
            .await?;
        store.apply_event(account, calendar_id, &created)?;
        let text = |name: &str| created[name].as_str().unwrap_or_default().to_owned();
        Ok((text("id"), text("etag")))
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
        // The account is gone either way; a file left larger than needed is no failure.
        let _ = store.compact();
        Ok(())
    }
}

/// Whether a sync failed because Google could not be reached at all (no network, no
/// name resolution, a timeout), as opposed to Google answering with an error.
fn no_connection(error: &SyncError) -> bool {
    let network = match error {
        SyncError::Api(ApiError::Http(network)) => network,
        SyncError::Api(ApiError::Auth(AuthError::Http(network))) => network,
        _ => return false,
    };
    network.is_connect() || network.is_timeout() || network.is_request()
}

/// Finds the identifier Google gives the occurrence of a series that would start at
/// `original_start`.
async fn occurrence_id(
    client: &GoogleClient,
    calendar_id: &str,
    series_id: &str,
    original_start: EventTime,
) -> Result<String> {
    // Only occurrences touching this window are listed; the right one is then told by
    // its original start. An all-day occurrence has no instant of its own, so its
    // window is generous.
    let (from, to) = match original_start {
        EventTime::Timed(start) => (start, start + chrono::Duration::seconds(1)),
        EventTime::AllDay(day) => {
            let midnight = day.and_time(chrono::NaiveTime::MIN).and_utc();
            (
                midnight - chrono::Duration::days(1),
                midnight + chrono::Duration::days(2),
            )
        }
    };
    let instances = client.instances(calendar_id, series_id, from, to).await?;
    instances
        .into_iter()
        .filter_map(|instance| serde_json::from_value::<Event>(instance).ok())
        .find(|instance| {
            instance
                .original_start_time
                .as_ref()
                .and_then(|start| start.time())
                == Some(original_start)
        })
        .map(|instance| instance.id)
        .context("that occurrence no longer exists")
}

/// What the status line says about a running sync. Nothing for the usual quick one
/// that fetches a few changes; the count once a calendar comes in several pages.
fn progress_text(progress: Progress) -> String {
    if progress.events < 1000 {
        return String::new();
    }
    // 57000 reads better as 57,000.
    let digits = progress.events.to_string();
    let mut events = String::new();
    for (position, digit) in digits.chars().enumerate() {
        if position > 0 && (digits.len() - position).is_multiple_of(3) {
            events.push(',');
        }
        events.push(digit);
    }
    format!(
        "Fetching events… {events} so far (calendar {} of {})",
        progress.calendar, progress.calendars
    )
}

/// An error message as a sentence of its own: capitalised, with a full stop.
fn sentence(text: &str) -> String {
    let mut letters = text.chars();
    let Some(first) = letters.next() else {
        return String::new();
    };
    let mut sentence: String = first.to_uppercase().chain(letters).collect();
    if !sentence.ends_with(['.', '!', '?']) {
        sentence.push('.');
    }
    sentence
}

/// A deleted event's resource in the form it can be created from again: without what
/// Google assigns itself. The identifier cannot be reused, so the event comes back as a
/// new one with the same content.
fn restorable(mut resource: Value) -> Value {
    if let Some(fields) = resource.as_object_mut() {
        for assigned in [
            "id",
            "etag",
            "kind",
            "status",
            "htmlLink",
            "iCalUID",
            "created",
            "updated",
            "creator",
            "organizer",
            "sequence",
        ] {
            fields.remove(assigned);
        }
    }
    resource
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_restored_event_keeps_its_content_but_not_what_google_assigns() {
        let deleted = json!({
            "id": "e1", "etag": "\"1\"", "kind": "calendar#event", "status": "confirmed",
            "htmlLink": "https://example.com/e1", "iCalUID": "e1@google.com",
            "created": "2026-11-01T10:00:00Z", "updated": "2026-11-02T10:00:00Z",
            "creator": {"email": "me@example.com"}, "organizer": {"email": "me@example.com"},
            "sequence": 3,
            "summary": "Lunch", "location": "Canteen", "colorId": "5",
            "start": {"dateTime": "2026-11-17T12:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Prague"},
            "reminders": {"useDefault": true}
        });
        assert_eq!(
            restorable(deleted),
            json!({
                "summary": "Lunch", "location": "Canteen", "colorId": "5",
                "start": {"dateTime": "2026-11-17T12:00:00+01:00", "timeZone": "Europe/Prague"},
                "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Prague"},
                "reminders": {"useDefault": true}
            })
        );
    }

    #[test]
    fn progress_is_only_spelled_out_for_a_long_fetch() {
        let progress = |events| Progress {
            calendar: 3,
            calendars: 14,
            events,
        };
        assert_eq!(progress_text(progress(999)), "");
        assert_eq!(
            progress_text(progress(57_500)),
            "Fetching events… 57,500 so far (calendar 3 of 14)"
        );
        assert_eq!(
            progress_text(progress(1_250_000)),
            "Fetching events… 1,250,000 so far (calendar 3 of 14)"
        );
    }

    #[test]
    fn an_error_is_shown_as_a_sentence() {
        assert_eq!(
            sentence("network error: timed out"),
            "Network error: timed out."
        );
        assert_eq!(sentence("Done."), "Done.");
        assert_eq!(sentence(""), "");
    }

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
