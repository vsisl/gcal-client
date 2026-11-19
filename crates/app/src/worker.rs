//! Background thread for everything slow: signing in, syncing, the keyring.
//!
//! The window's thread never waits on the network. It sends a [`Command`] here; when the
//! cache changed or something is worth telling the user, the worker posts back to the
//! window's event loop.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gcal_core::{Event, EventTime};
use gcal_google::{
    ApiError, AuthError, ClientConfig, Endpoints, GoogleClient, HttpClient, Permission, TokenSet,
};
use gcal_store::{AccountId, Store};
use gcal_sync::{Progress, SyncError, SyncReport};
use serde_json::Value;
use slint::Weak;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::{Mutex, watch};

use crate::gui::{AppWindow, StatusKind};
use crate::pictures::{PICTURE_SIZE, round_picture};
use crate::vault::Vault;
use crate::{load_config, open_store};

/// How long an opened sign-in page may sit unanswered before the attempt is dropped.
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Limits on one request to Google. Without them a connection that stalls (suspend, captive
/// portal) would never finish, and the sync holding it would block every later one.
/// How long Google takes, generously, to finish setting up or removing a video meeting.
const SETTLE_TIME: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// How many people are suggested under a guests field.
const SHOWN_SUGGESTIONS: usize = 8;
/// The most pictures looked up for one request: an event can have hundreds of guests,
/// and each lookup is a request or two to Google. Those after it show their letter.
const MAX_PICTURES_AT_ONCE: usize = 40;

#[derive(Debug)]
pub enum Command {
    AddAccount,
    SyncAll,
    RemoveAccount(String),
    /// Asks an account that is signed in already, in the browser, to let the app do
    /// one thing more.
    Grant {
        email: String,
        permission: Permission,
    },
    /// Adds a calendar to the account's list at Google (a colleague's, by their
    /// address), then fetches it.
    Subscribe {
        account: AccountId,
        calendar_id: String,
    },
    /// Takes a calendar off the account's list at Google. The calendar itself stays.
    Unsubscribe {
        account: AccountId,
        calendar_id: String,
    },
    /// Looks up people to suggest as guests while a name is typed in the editor. The
    /// answer goes to the window only if `serial` is still the newest request there.
    SuggestGuests {
        account: AccountId,
        query: String,
        serial: i32,
    },
    /// Fetches the profile pictures of people the window shows, as known to `account`
    /// (its contacts, its organisation's directory, or its own profile), where a
    /// picture's address is not known yet. Each that is found goes to the window.
    Pictures {
        account: AccountId,
        people: Vec<String>,
    },
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
        /// For an occurrence the cache has no version of: fields the change was worked
        /// out from, with the values the window saw. The occurrence is fetched first,
        /// and the change is made only if it still has them, and under the version
        /// just fetched. Without this the change is sent unconditionally (issue 7).
        expected: Option<Value>,
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

/// An account, its connection to Google and where it lets the app look for people,
/// kept between two suggestions.
type Suggesting = (AccountId, Rc<GoogleClient>, Vec<Permission>);

/// Where guests are looked up, each needing a permission of its own.
const PEOPLE: [Permission; 3] = [
    Permission::Contacts,
    Permission::OtherContacts,
    Permission::Directory,
];

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
    /// The account suggestions were last asked for, with its connection to Google:
    /// a suggestion is asked for with every few letters typed, and a new connection
    /// would fetch a new access token each time.
    suggesting: Rc<RefCell<Option<Suggesting>>>,
    /// The addresses whose pictures were asked for already (in small letters), found or
    /// not. Each is looked up once while the app runs.
    pictures: Rc<RefCell<HashSet<String>>>,
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
        suggesting: Rc::default(),
        pictures: Rc::default(),
    };
    while let Some(command) = commands.recv().await {
        // Each command runs as its own task: a sign-in waiting on the browser must not
        // hold up a sync.
        let worker = worker.clone();
        tokio::task::spawn_local(async move {
            // A sync tells the window itself, and only when it changed something: every
            // reload closes an open details card and forgets the series' occurrences.
            let reload = !matches!(
                command,
                Command::SyncAll | Command::SuggestGuests { .. } | Command::Pictures { .. }
            );
            // A change the user asked for and did not get is said in a dialog, not in
            // the status line, where it is easily missed.
            let failure = match command {
                Command::DeleteEvent { .. } => Some("The event was not deleted."),
                Command::EditEvent { .. } => Some("The event was not changed."),
                Command::PatchEvent { .. } => Some("The change could not be taken back."),
                Command::CreateEvent { .. } => Some("The event was not saved."),
                Command::RestoreEvent { .. } => Some("The event could not be brought back."),
                Command::Subscribe { .. } => Some("The calendar was not added."),
                Command::Unsubscribe { .. } => Some("The calendar was not removed from the list."),
                _ => None,
            };
            // Changes run one at a time, in the order they were asked for, and never
            // while a sync is saving what it fetched: a sync that started earlier could
            // otherwise put back the older version of an event just changed. The window
            // relies on the order too, to match each answer with its undo.
            let _cache = match command {
                Command::AddAccount
                | Command::SyncAll
                | Command::RemoveAccount(_)
                | Command::Grant { .. }
                | Command::SuggestGuests { .. }
                | Command::Pictures { .. } => None,
                _ => Some(worker.cache.lock().await),
            };
            let result = match command {
                Command::AddAccount => worker.add_account().await,
                Command::SyncAll => worker.sync_all().await,
                Command::RemoveAccount(email) => worker.remove_account(&email).await,
                Command::Grant { email, permission } => worker.grant(&email, permission).await,
                Command::Subscribe {
                    account,
                    calendar_id,
                } => worker.subscribe(account, &calendar_id, true).await,
                Command::Unsubscribe {
                    account,
                    calendar_id,
                } => worker.subscribe(account, &calendar_id, false).await,
                Command::SuggestGuests {
                    account,
                    query,
                    serial,
                } => {
                    worker.suggest_guests(account, &query, serial).await;
                    Ok(())
                }
                Command::Pictures { account, people } => {
                    let wanted = people.into_iter().map(|email| (email, None)).collect();
                    worker.fetch_pictures(account, wanted).await;
                    Ok(())
                }
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

    /// Looks up people matching `query` and hands them to the editor, unless the user
    /// has typed on in the meantime. A failure is said in a line under the field, not
    /// as an error: the event can be saved without suggestions.
    async fn suggest_guests(&self, account: AccountId, query: &str, serial: i32) {
        let found = async {
            let (client, sources) = self.people_client(account).await?;
            if sources.is_empty() {
                return Ok(None);
            }
            Ok::<_, anyhow::Error>(Some(client.search_people(query, &sources).await?))
        }
        .await;
        // The suggestions' pictures follow them; their addresses came with the search.
        if let Ok(Some(people)) = &found {
            let wanted: Vec<(String, Option<String>)> = people
                .iter()
                .take(SHOWN_SUGGESTIONS)
                .filter_map(|person| Some((person.email.clone(), Some(person.photo.clone()?))))
                .collect();
            if !wanted.is_empty() {
                let worker = self.clone();
                tokio::task::spawn_local(async move {
                    worker.fetch_pictures(account, wanted).await;
                });
            }
        }
        let (people, problem) = match found {
            Ok(Some(people)) => (people, String::new()),
            Ok(None) => (Vec::new(), NO_PEOPLE_PERMISSION.to_owned()),
            Err(error) => (Vec::new(), suggestion_problem(&error)),
        };
        let _ = self.ui.upgrade_in_event_loop(move |ui| {
            if ui.get_guest_suggest_serial() != serial {
                return;
            }
            let rows: Vec<crate::gui::Suggestion> = people
                .into_iter()
                .take(SHOWN_SUGGESTIONS)
                .map(|person| crate::gui::Suggestion {
                    // A name is one line of text, whatever was put into it.
                    name: person
                        .name
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect::<String>()
                        .into(),
                    email: person.email.into(),
                })
                .collect();
            ui.set_guest_suggestions(slint::ModelRc::from(Rc::new(slint::VecModel::from(rows))));
            ui.set_guest_suggest_problem(problem.into());
        });
    }

    /// The connection to Google of `account` for looking up people, and where it lets
    /// the app look. Kept for the next lookup: suggestions are asked for with every few
    /// letters typed, and a new connection would fetch a new access token each time.
    async fn people_client(
        &self,
        account: AccountId,
    ) -> Result<(Rc<GoogleClient>, Vec<Permission>)> {
        let known = self.suggesting.borrow().clone();
        if let Some((of, client, sources)) = known
            && of == account
        {
            return Ok((client, sources));
        }
        let store = open_store()?;
        let client = Rc::new(self.account_client(&store, account).await?);
        let allowed = store
            .accounts()?
            .into_iter()
            .find(|a| a.id == account)
            .and_then(|a| a.scopes);
        let sources = people_sources(allowed.as_deref());
        self.suggesting
            .replace(Some((account, client.clone(), sources.clone())));
        Ok((client, sources))
    }

    /// Fetches the pictures of `people` (an address each, with the picture's address
    /// where it is known already) and hands each one found to the window. Each address
    /// is tried once while the app runs, unless Google could not be reached. People
    /// without a picture are left to the window, which shows their letter.
    async fn fetch_pictures(&self, account: AccountId, people: Vec<(String, Option<String>)>) {
        let wanted: Vec<(String, Option<String>)> = {
            let mut asked = self.pictures.borrow_mut();
            people
                .into_iter()
                .filter(|(email, _)| asked.insert(email.to_lowercase()))
                .take(MAX_PICTURES_AT_ONCE)
                .collect()
        };
        if wanted.is_empty() {
            return;
        }
        let Ok(own) = open_store().and_then(|store| {
            Ok(store
                .accounts()?
                .into_iter()
                .find(|a| a.id == account)
                .map(|a| a.email))
        }) else {
            self.forget_pictures(&wanted);
            return;
        };
        let Some(own) = own else { return };
        let Ok((client, sources)) = self.people_client(account).await else {
            self.forget_pictures(&wanted);
            return;
        };
        for (email, known) in wanted {
            let address = match known {
                Some(address) => Ok(Some(address)),
                None if email.eq_ignore_ascii_case(&own) => client.own_photo().await,
                None => client.find_photo(&email, &sources).await,
            };
            let bytes = match address {
                Ok(Some(address)) => {
                    gcal_google::download_picture(&self.http, &address, PICTURE_SIZE).await
                }
                Ok(None) => continue,
                Err(error) => Err(error),
            };
            let picture = match bytes {
                Ok(bytes) => round_picture(&bytes),
                // Not reachable now: tried again the next time it is shown.
                Err(ApiError::Http(_) | ApiError::Auth(AuthError::Http(_))) => {
                    self.pictures.borrow_mut().remove(&email.to_lowercase());
                    continue;
                }
                Err(_) => continue,
            };
            if let Some(picture) = picture {
                let _ = self.ui.upgrade_in_event_loop(move |ui| {
                    ui.invoke_picture_arrived(email.into(), slint::Image::from_rgba8(picture));
                });
            }
        }
    }

    /// Lets the pictures of `people` be asked for again.
    fn forget_pictures(&self, people: &[(String, Option<String>)]) {
        let mut asked = self.pictures.borrow_mut();
        for (email, _) in people {
            asked.remove(&email.to_lowercase());
        }
    }

    async fn add_account(&self) -> Result<()> {
        self.clear_error();
        let (config, vault, tokens) = self.newest_sign_in(&Permission::AT_SIGN_IN, None).await?;
        let refresh_token = tokens
            .refresh_token
            .context("Google sent no refresh token")?;
        // Google lets each line of what is asked for be unticked.
        if let Some(scopes) = &tokens.scopes
            && !Permission::Calendar.is_in(scopes)
        {
            bail!(
                "The account was not added: on Google's page the line about calendars was \
                 left unticked, and without it there is nothing to show. Add the account \
                 again and tick it."
            );
        }

        let client = self
            .client(config, refresh_token.clone())
            .with_access_token(
                tokens.access_token,
                tokens.expires_at,
                tokens.scopes.clone(),
            );
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
        if let Some(scopes) = &tokens.scopes {
            store.set_account_scopes(account, scopes)?;
        }
        // An account signed in again may allow something else than before, its own
        // picture among it.
        self.suggesting.replace(None);
        self.pictures.borrow_mut().remove(&email.to_lowercase());
        self.data_changed();
        let result = self.sync_account(&client, &mut store, account).await;
        if let Some(problem) = sync_problem(&email, &result) {
            bail!(problem);
        }
        Ok(())
    }

    /// Asks the account `email`, which is signed in already, for one permission more.
    /// What it gave before is asked for again along with it, so that nothing is lost
    /// if Google does not carry it over.
    async fn grant(&self, email: &str, permission: Permission) -> Result<()> {
        self.clear_error();
        let before = open_store()?
            .accounts()?
            .into_iter()
            .find(|a| a.email == email)
            .context("the account is no longer signed in")?
            .scopes;
        let wanted: Vec<Permission> = Permission::ALL
            .into_iter()
            .filter(|one| {
                *one == permission
                    || Permission::AT_SIGN_IN.contains(one)
                    || before.as_ref().is_some_and(|scopes| one.is_in(scopes))
            })
            .collect();
        let (config, vault, tokens) = self.newest_sign_in(&wanted, Some(email)).await?;
        let refresh_token = tokens
            .refresh_token
            .context("Google sent no refresh token")?;
        let client = self
            .client(config, refresh_token.clone())
            .with_access_token(
                tokens.access_token,
                tokens.expires_at,
                tokens.scopes.clone(),
            );
        // Google offers the account, but any other can be chosen on its page. That
        // one's sign-in must not be kept under this account's name.
        let chosen = client.account_email().await?;
        if !chosen.eq_ignore_ascii_case(email) {
            bail!(
                "Nothing was changed: the permission was asked for {email}, but in the \
                 browser {chosen} was chosen."
            );
        }

        let _cache = self.cache.lock().await;
        let store = open_store()?;
        // Removed while the browser was open: then it stays removed.
        let account = store
            .accounts()?
            .into_iter()
            .find(|a| a.email == email)
            .context("the account is no longer signed in")?;
        vault.save(email, &refresh_token).await?;
        self.suggesting.replace(None);
        // Its own picture may be allowed now.
        self.pictures.borrow_mut().remove(&email.to_lowercase());
        if let Some(scopes) = &tokens.scopes {
            store.set_account_scopes(account.id, scopes)?;
            if !permission.is_in(scopes) {
                bail!(
                    "{email} did not give the permission: its line on Google's page was \
                     left unticked."
                );
            }
        }
        Ok(())
    }

    /// Signs in in the browser. Until Google has answered, a newer attempt replaces
    /// this one, wherever it is: unlocking the keyring, or waiting on a browser tab
    /// that may have been closed.
    async fn newest_sign_in(
        &self,
        permissions: &[Permission],
        account: Option<&str>,
    ) -> Result<(ClientConfig, Vault, TokenSet)> {
        let mut attempt = 0;
        self.sign_in_attempt.send_modify(|newest| {
            *newest += 1;
            attempt = *newest;
        });
        let mut newest = self.sign_in_attempt.subscribe();
        tokio::select! {
            signed_in = self.sign_in(permissions, account) => signed_in,
            _ = newest.wait_for(|newest| *newest != attempt) => Err(Superseded.into()),
        }
    }

    /// Opens the browser and waits for the user to finish signing in there.
    async fn sign_in(
        &self,
        permissions: &[Permission],
        account: Option<&str>,
    ) -> Result<(ClientConfig, Vault, TokenSet)> {
        let config = load_config()?;
        let endpoints = Endpoints::default();
        let vault = Vault::open().await?;

        let scopes: Vec<&str> = permissions.iter().map(|one| one.scope()).collect();
        let pending = gcal_google::begin_login(&config, &endpoints, &scopes, account).await?;
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
            // Google says with every access token what the account allows: that is how
            // it is learnt for an account signed in before the app kept it, and how a
            // change made at Google is noticed.
            let news = match client.granted_scopes().await {
                Some(scopes) => store
                    .set_account_scopes(account.id, &scopes)
                    .unwrap_or(false),
                None => false,
            };
            if news {
                self.suggesting.replace(None);
            }
            // Redrawing closes an open details card and costs time, so the window is only
            // told when there is something new to show. A failed run may have saved part
            // of what it fetched.
            let unchanged = matches!(
                &result,
                Ok(report) if !report.changed
            );
            if !unchanged || news {
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

    /// Syncs once more in a few seconds. Google goes on working on an event after
    /// answering a change to its video meeting, and the event's version changes again
    /// a moment later; until the app has that version, a further change to the event
    /// would be refused as "changed somewhere else".
    fn sync_soon(&self) {
        let worker = self.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(SETTLE_TIME).await;
            let _ = worker.sync_all().await;
            worker.publish();
        });
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
                expected,
            } => {
                let (id, etag) = match known_id {
                    Some(id) => (id, etag),
                    None => {
                        let store = open_store()?;
                        let client = self.account_client(&store, account).await?;
                        let found =
                            occurrence(&client, calendar_id, &series_id, original_start).await?;
                        let id = found["id"].as_str().unwrap_or_default().to_owned();
                        match expected {
                            Some(expected) => {
                                if !still_has(&found, &expected) {
                                    // Brings the calendar up to date and says so.
                                    self.written::<()>(Err(ApiError::Changed)).await?;
                                }
                                (id, found["etag"].as_str().map(str::to_owned))
                            }
                            None => (id, etag),
                        }
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
        if changes.get("conferenceData").is_some() {
            self.sync_soon();
        }
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
        let (id, mut etag) = (text("id"), text("etag"));
        if !resource["conferenceData"]["createRequest"].is_null() {
            // Google sets the video meeting up after it has answered, and the event
            // changes once more. Without fetching that, the next change to the event
            // would be refused as "changed somewhere else".
            let _ = self.sync_accounts().await;
            if let Ok(Some(settled)) = store.event_json(account, calendar_id, &id) {
                etag = settled["etag"].as_str().unwrap_or_default().to_owned();
            }
            self.sync_soon();
        }
        Ok((id, etag))
    }

    /// Adds a calendar to the account's list at Google (`add`), or takes it off, and
    /// then syncs: the calendar and its events come, or go, with the list. The caller
    /// holds the cache lock.
    async fn subscribe(&self, account: AccountId, calendar_id: &str, add: bool) -> Result<()> {
        self.clear_error();
        let saving = self.start(|activity| &mut activity.saving);
        let client = self.account_client(&open_store()?, account).await?;
        let answer = if add {
            client.subscribe_calendar(calendar_id).await
        } else {
            client.unsubscribe_calendar(calendar_id).await
        };
        match answer {
            Ok(()) => {}
            Err(ApiError::Gone) if add => bail!(
                "Google knows no calendar “{calendar_id}” that this account may see. If it \
                 is a colleague's, check the address, or ask them to share their calendar \
                 with this account."
            ),
            // Off the list already: that is what was wanted.
            Err(ApiError::Gone) => {}
            Err(other) => return Err(other.into()),
        }
        drop(saving);
        // Done at Google; what is left is fetching the list again. If that fails, the
        // sidebar still shows the old list, which must not pass for the outcome.
        if let Err(error) = self.sync_accounts().await {
            let done = if add { "added" } else { "removed" };
            self.activity.borrow_mut().error = Some(format!(
                "The calendar was {done} at Google, but the list could not be fetched \
                 again ({error:#}). Choose Refresh to try again."
            ));
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
        self.suggesting.replace(None);
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

/// Said under the guests field for an account that allows none of the look-ups.
const NO_PEOPLE_PERMISSION: &str = "No suggestions: this account does not let the app \
    read its contacts. You can allow it in the settings, under Permissions.";

/// Where people may be looked up for an account that allows `scopes`. For an account
/// of which that is not known yet, everywhere: Google refuses what is not allowed.
fn people_sources(scopes: Option<&[String]>) -> Vec<Permission> {
    PEOPLE
        .into_iter()
        .filter(|source| scopes.is_none_or(|scopes| source.is_in(scopes)))
        .collect()
}

/// What to say under the guests field when people could not be looked up. Nothing
/// when Google cannot be reached: the banner says that already.
fn suggestion_problem(error: &anyhow::Error) -> String {
    match error.downcast_ref::<ApiError>() {
        Some(ApiError::Status { status: 403, .. }) => {
            "No suggestions: Google refused to look up people for this account. Look at \
             its permissions in the settings; if they are allowed there, the People API \
             is not enabled for the sign-in key (see README)."
                .into()
        }
        Some(ApiError::Http(_) | ApiError::Auth(AuthError::Http(_))) => String::new(),
        _ => format!("No suggestions: {}", sentence(&format!("{error:#}"))),
    }
}

/// Whether an event still has, in each field named by `expected`, the value given
/// there.
fn still_has(event: &Value, expected: &Value) -> bool {
    expected.as_object().is_some_and(|fields| {
        fields
            .iter()
            .all(|(name, value)| event.get(name) == Some(value))
    })
}

/// Finds the identifier Google gives the occurrence of a series that would start at
/// `original_start`.
async fn occurrence_id(
    client: &GoogleClient,
    calendar_id: &str,
    series_id: &str,
    original_start: EventTime,
) -> Result<String> {
    let found = occurrence(client, calendar_id, series_id, original_start).await?;
    Ok(found["id"].as_str().unwrap_or_default().to_owned())
}

/// Fetches the occurrence of a series that would start at `original_start`, as Google
/// has it now.
async fn occurrence(
    client: &GoogleClient,
    calendar_id: &str,
    series_id: &str,
    original_start: EventTime,
) -> Result<Value> {
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
        .find(|instance| {
            serde_json::from_value::<Event>(instance.clone())
                .ok()
                .and_then(|event| event.original_start_time)
                .and_then(|start| start.time())
                == Some(original_start)
        })
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

    #[test]
    fn an_occurrence_is_changed_only_if_it_still_has_what_the_window_saw() {
        use serde_json::json;
        let guests = json!([{"email": "me@x.org", "self": true, "responseStatus": "needsAction"}]);
        let event = json!({"id": "m_1", "etag": "\"1\"", "attendees": guests});
        assert!(still_has(&event, &json!({"attendees": guests})));
        // Someone's answer changed, or a guest was added, since the window's version.
        assert!(!still_has(
            &event,
            &json!({"attendees": [{"email": "me@x.org", "self": true, "responseStatus": "accepted"}]})
        ));
        // A field the event does not have at all is not "still the same".
        assert!(!still_has(
            &json!({"id": "m_1"}),
            &json!({"attendees": guests})
        ));
        assert!(!still_has(&event, &json!(null)));
    }
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
    fn people_are_looked_up_only_where_the_account_allows() {
        let scopes = |all: &[Permission]| -> Vec<String> {
            all.iter().map(|one| one.scope().to_owned()).collect()
        };
        // Not known yet: everywhere, and Google refuses what it must.
        assert_eq!(people_sources(None), PEOPLE);
        assert_eq!(
            people_sources(Some(&scopes(&[Permission::Calendar]))),
            vec![]
        );
        assert_eq!(
            people_sources(Some(&scopes(&[
                Permission::Calendar,
                Permission::Directory,
                Permission::Contacts
            ]))),
            vec![Permission::Contacts, Permission::Directory]
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
