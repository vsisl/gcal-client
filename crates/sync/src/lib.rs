//! Sync engine keeping the local cache in step with Google.

use gcal_google::{ApiError, GoogleClient, Listing};
use gcal_store::{AccountId, Store, StoreError};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What one sync of an account did.
#[derive(Debug, Default, PartialEq)]
pub struct SyncReport {
    /// Whether the cache may differ from before: something came back from Google, or a
    /// collection was replaced in full (which removes what Google no longer lists, even
    /// when it lists nothing).
    pub changed: bool,
    pub calendars_synced: usize,
    /// Events created, changed or deleted since the previous sync (all of them on a first one).
    pub events_changed: usize,
    /// Calendars whose events could not be fetched, with the reason. The other calendars
    /// still sync: one unreadable shared calendar must not block the whole account.
    pub failed_calendars: Vec<(String, String)>,
}

/// Brings one account's calendar list and events up to date.
///
/// Uses the stored sync tokens to fetch only changes. When Google rejects a token (it
/// expires after a while), that collection is fetched again in full and replaced.
pub async fn sync_account(
    client: &GoogleClient,
    store: &mut Store,
    account: AccountId,
) -> Result<SyncReport, SyncError> {
    let token = store.calendar_list_sync_token(account)?;
    let (list, full) = fetch(token.as_deref(), |t| client.list_calendars(t)).await?;
    store.apply_calendar_list(account, &list.items, list.sync_token.as_deref(), full)?;

    let mut report = SyncReport {
        changed: full || !list.items.is_empty(),
        ..SyncReport::default()
    };
    for calendar in store
        .calendars()?
        .into_iter()
        .filter(|c| c.account_id == account)
    {
        let id = &calendar.entry.id;
        let fetched = fetch(calendar.events_sync_token.as_deref(), |t| {
            client.list_events(id, t)
        })
        .await;
        match fetched {
            Ok((events, full)) => {
                let applied = store.apply_events(
                    account,
                    id,
                    &events.items,
                    events.sync_token.as_deref(),
                    full,
                );
                match applied {
                    Ok(()) => {
                        report.changed |= full || !events.items.is_empty();
                        report.calendars_synced += 1;
                        report.events_changed += events.items.len();
                    }
                    // An event in a shape the model does not understand. Nothing of this
                    // batch was saved (so nothing is half-applied), and the other
                    // calendars must still sync.
                    Err(StoreError::Json(error)) => report
                        .failed_calendars
                        .push((id.clone(), format!("unreadable event: {error}"))),
                    Err(error) => return Err(error.into()),
                }
            }
            // Without a working sign-in or network nothing else will succeed either.
            Err(error @ (ApiError::Auth(_) | ApiError::Http(_))) => return Err(error.into()),
            Err(error) => report
                .failed_calendars
                .push((id.clone(), error.to_string())),
        }
    }
    Ok(report)
}

/// Lists a collection incrementally if there is a token, falling back to a full listing
/// when Google no longer accepts it. The flag tells whether the result is a full listing.
async fn fetch<'a, F, Fut>(token: Option<&'a str>, list: F) -> Result<(Listing, bool), ApiError>
where
    F: Fn(Option<&'a str>) -> Fut,
    Fut: Future<Output = Result<Listing, ApiError>>,
{
    match token {
        None => Ok((list(None).await?, true)),
        Some(token) => match list(Some(token)).await {
            Err(ApiError::SyncTokenInvalid) => Ok((list(None).await?, true)),
            other => Ok((other?, false)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gcal_google::{ClientConfig, Endpoints};
    use serde_json::{Value, json};
    use wiremock::matchers::{method, path, query_param, query_param_is_missing};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const DAY_START: i64 = 1_794_787_200; // 2026-11-16T00:00:00Z
    const DAY_END: i64 = DAY_START + 86_400;

    async fn client(server: &MockServer) -> GoogleClient {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "t", "expires_in": 3600})),
            )
            .mount(server)
            .await;
        GoogleClient::new(
            reqwest_client(),
            ClientConfig {
                client_id: "id".into(),
                client_secret: None,
            },
            Endpoints {
                auth: format!("{}/auth", server.uri()),
                token: format!("{}/token", server.uri()),
                calendar_api: server.uri(),
            },
            "refresh".into(),
        )
    }

    fn reqwest_client() -> gcal_google::HttpClient {
        gcal_google::HttpClient::new()
    }

    fn event(id: &str) -> Value {
        json!({"id": id, "start": {"dateTime": "2026-11-16T10:00:00Z"},
               "end": {"dateTime": "2026-11-16T11:00:00Z"}})
    }

    async fn mount_calendar_list(server: &MockServer, body: Value) {
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(server)
            .await;
    }

    fn stored_ids(store: &Store) -> Vec<String> {
        let mut ids: Vec<_> = store
            .events_for_range(DAY_START, DAY_END)
            .unwrap()
            .into_iter()
            .map(|e| e.event.id)
            .collect();
        ids.sort();
        ids
    }

    #[tokio::test]
    async fn first_sync_is_full_and_the_next_one_sends_the_tokens() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("me@example.com").unwrap();

        // First sync: no tokens yet.
        let first = [
            Mock::given(method("GET"))
                .and(path("/users/me/calendarList"))
                .and(query_param_is_missing("syncToken"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "items": [{"id": "cal", "summary": "Cal"}], "nextSyncToken": "list-1"
                })))
                .mount_as_scoped(&server)
                .await,
            Mock::given(method("GET"))
                .and(path("/calendars/cal/events"))
                .and(query_param_is_missing("syncToken"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                    "items": [event("a"), event("b")], "nextSyncToken": "ev-1"
                })))
                .mount_as_scoped(&server)
                .await,
        ];
        let report = sync_account(&client, &mut store, account).await.unwrap();
        assert_eq!(
            report,
            SyncReport {
                changed: true,
                calendars_synced: 1,
                events_changed: 2,
                ..Default::default()
            }
        );
        assert_eq!(stored_ids(&store), ["a", "b"]);
        drop(first);

        // Second sync: tokens are sent, and only the change comes back.
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList"))
            .and(query_param("syncToken", "list-1"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({"nextSyncToken": "list-2"})),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/cal/events"))
            .and(query_param("syncToken", "ev-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "a", "status": "cancelled"}], "nextSyncToken": "ev-2"
            })))
            .mount(&server)
            .await;
        let report = sync_account(&client, &mut store, account).await.unwrap();
        assert_eq!(report.events_changed, 1);
        assert!(report.changed);
        assert_eq!(stored_ids(&store), ["b"]);
        assert_eq!(
            store.calendars().unwrap()[0].events_sync_token.as_deref(),
            Some("ev-2")
        );
    }

    #[tokio::test]
    async fn rejected_event_token_triggers_a_full_resync_that_replaces_the_cache() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("me@example.com").unwrap();
        let list = [json!({"id": "cal", "summary": "Cal"})];
        store
            .apply_calendar_list(account, &list, None, true)
            .unwrap();
        store
            .apply_events(account, "cal", &[event("stale")], Some("old"), true)
            .unwrap();

        mount_calendar_list(&server, json!({"items": list, "nextSyncToken": "list-1"})).await;
        Mock::given(method("GET"))
            .and(path("/calendars/cal/events"))
            .and(query_param("syncToken", "old"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/cal/events"))
            .and(query_param_is_missing("syncToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [event("fresh")], "nextSyncToken": "new"
            })))
            .mount(&server)
            .await;

        sync_account(&client, &mut store, account).await.unwrap();
        assert_eq!(stored_ids(&store), ["fresh"]);
    }

    #[tokio::test]
    async fn an_event_the_model_cannot_read_fails_its_calendar_only() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("me@example.com").unwrap();

        mount_calendar_list(
            &server,
            json!({"items": [{"id": "odd", "summary": "A"}, {"id": "good", "summary": "B"}]}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/calendars/odd/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [event("fine"), {"id": "x", "start": {"date": "not a date"}}],
                "nextSyncToken": "t"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/good/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [event("e")]})))
            .mount(&server)
            .await;

        let report = sync_account(&client, &mut store, account).await.unwrap();
        assert_eq!(report.calendars_synced, 1);
        assert_eq!(report.failed_calendars[0].0, "odd");
        // Nothing of the odd calendar was kept, and its token was not advanced.
        assert_eq!(stored_ids(&store), ["e"]);
        let odd = &store.calendars().unwrap()[0];
        assert_eq!(
            (odd.entry.id.as_str(), &odd.events_sync_token),
            ("odd", &None)
        );
    }

    #[tokio::test]
    async fn one_unreadable_calendar_does_not_stop_the_others() {
        let server = MockServer::start().await;
        let client = client(&server).await;
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("me@example.com").unwrap();

        mount_calendar_list(
            &server,
            json!({"items": [{"id": "bad", "summary": "A"}, {"id": "good", "summary": "B"}]}),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/calendars/bad/events"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/good/events"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": [event("e")]})))
            .mount(&server)
            .await;

        let report = sync_account(&client, &mut store, account).await.unwrap();
        assert_eq!(report.calendars_synced, 1);
        assert_eq!(report.failed_calendars.len(), 1);
        assert_eq!(report.failed_calendars[0].0, "bad");
        assert_eq!(stored_ids(&store), ["e"]);
    }
}
