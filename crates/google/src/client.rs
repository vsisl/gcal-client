//! Authenticated access to the Calendar API for one signed-in account.

use chrono::{DateTime, Utc};
use reqwest::Method;
use reqwest::header::IF_MATCH;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::Mutex;
use url::Url;

use crate::auth::{self, AuthError};
use crate::config::{ClientConfig, Endpoints};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// HTTP 410: Google no longer accepts the stored sync token. The caller must discard
    /// what it has for that collection and list it again from scratch.
    #[error("the sync token is no longer valid")]
    SyncTokenInvalid,
    /// HTTP 412: the event was changed elsewhere since the copy the edit was based on. The
    /// caller must fetch it again and let the user decide.
    #[error("the event was changed elsewhere in the meantime")]
    Changed,
    /// HTTP 404 or 410 on a write: the event (or its calendar) no longer exists.
    #[error("the event no longer exists")]
    Gone,
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error("Google returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
}

/// Every item of a collection, plus the token to ask only for changes next time.
#[derive(Debug)]
pub struct Listing {
    /// Raw resources, kept as JSON so the store can save fields the app does not read yet.
    pub items: Vec<Value>,
    pub sync_token: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    #[serde(default)]
    items: Vec<Value>,
    next_page_token: Option<String>,
    next_sync_token: Option<String>,
}

struct Access {
    token: String,
    expires_at: DateTime<Utc>,
}

pub struct GoogleClient {
    http: reqwest::Client,
    config: ClientConfig,
    endpoints: Endpoints,
    refresh_token: String,
    access: Mutex<Option<Access>>,
}

impl GoogleClient {
    pub fn new(
        http: reqwest::Client,
        config: ClientConfig,
        endpoints: Endpoints,
        refresh_token: String,
    ) -> Self {
        Self {
            http,
            config,
            endpoints,
            refresh_token,
            access: Mutex::new(None),
        }
    }

    /// Reuses the access token from a sign-in that has just completed, saving one refresh.
    pub fn with_access_token(self, token: String, expires_at: DateTime<Utc>) -> Self {
        Self {
            access: Mutex::new(Some(Access { token, expires_at })),
            ..self
        }
    }

    /// The account's address, which is also the ID of its primary calendar.
    pub async fn account_email(&self) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Calendar {
            id: String,
        }
        let calendar: Calendar = self.get(&["calendars", "primary"], &[]).await?;
        Ok(calendar.id)
    }

    /// The account's calendar list: everything, or only changes since `sync_token`.
    pub async fn list_calendars(&self, sync_token: Option<&str>) -> Result<Listing, ApiError> {
        self.list(
            &["users", "me", "calendarList"],
            &[("maxResults", "250")],
            sync_token,
            &|_| (),
        )
        .await
    }

    /// A calendar's events: everything, or only changes since `sync_token`.
    ///
    /// Series are returned as one master event plus its changed occurrences rather than
    /// expanded, so the result stays small and a sync token can be used. The parameters must
    /// be identical on every call for the token to remain valid.
    ///
    /// A large calendar arrives in many pages; `fetched` is told after each how many
    /// events have come so far, for showing that something is happening.
    pub async fn list_events(
        &self,
        calendar_id: &str,
        sync_token: Option<&str>,
        fetched: &dyn Fn(usize),
    ) -> Result<Listing, ApiError> {
        self.list(
            &["calendars", calendar_id, "events"],
            &[("maxResults", "2500"), ("singleEvents", "false")],
            sync_token,
            fetched,
        )
        .await
    }

    async fn list(
        &self,
        path: &[&str],
        params: &[(&str, &str)],
        sync_token: Option<&str>,
        fetched: &dyn Fn(usize),
    ) -> Result<Listing, ApiError> {
        let mut items = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let mut query = params.to_vec();
            if let Some(token) = sync_token {
                query.push(("syncToken", token));
            }
            if let Some(token) = &page_token {
                query.push(("pageToken", token));
            }
            let page: Page = self.get(path, &query).await?;
            items.extend(page.items);
            fetched(items.len());
            match page.next_page_token {
                Some(next) => page_token = Some(next),
                // The sync token arrives only on the last page.
                None => {
                    return Ok(Listing {
                        items,
                        sync_token: page.next_sync_token,
                    });
                }
            }
        }
    }

    /// Creates an event from the given resource and returns it as Google stored it.
    pub async fn insert_event(&self, calendar_id: &str, event: &Value) -> Result<Value, ApiError> {
        let response = self
            .send(
                Method::POST,
                &["calendars", calendar_id, "events"],
                &[],
                Some(event),
                None,
            )
            .await
            .map_err(gone_if_missing)?;
        Ok(response.json().await?)
    }

    /// Changes the given fields of an event and returns the event as Google stored it.
    ///
    /// With `etag`, the change is made only if the event still is the version the caller
    /// has; otherwise the result is [`ApiError::Changed`] and nothing is modified. With
    /// `notify`, Google e-mails the event's guests about the change.
    pub async fn patch_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        changes: &Value,
        etag: Option<&str>,
        notify: bool,
    ) -> Result<Value, ApiError> {
        let response = self
            .send(
                Method::PATCH,
                &["calendars", calendar_id, "events", event_id],
                send_updates(notify),
                Some(changes),
                etag,
            )
            .await
            .map_err(gone_if_missing)?;
        Ok(response.json().await?)
    }

    /// Deletes an event. An event that is already gone is reported as [`ApiError::Gone`],
    /// which a caller wanting it deleted can treat as success. With `notify`, Google
    /// e-mails the event's guests that it was cancelled.
    pub async fn delete_event(
        &self,
        calendar_id: &str,
        event_id: &str,
        etag: Option<&str>,
        notify: bool,
    ) -> Result<(), ApiError> {
        self.send(
            Method::DELETE,
            &["calendars", calendar_id, "events", event_id],
            send_updates(notify),
            None,
            etag,
        )
        .await
        .map_err(gone_if_missing)?;
        Ok(())
    }

    /// The occurrences of a series between two instants, each as an event of its own
    /// (with its own identifier, which is what deleting one occurrence needs).
    pub async fn instances(
        &self,
        calendar_id: &str,
        series_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<Value>, ApiError> {
        let (from, to) = (from.to_rfc3339(), to.to_rfc3339());
        let listing = self
            .list(
                &["calendars", calendar_id, "events", series_id, "instances"],
                &[("timeMin", &from), ("timeMax", &to), ("maxResults", "250")],
                None,
                &|_| (),
            )
            .await
            .map_err(gone_if_missing)?;
        Ok(listing.items)
    }

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let response = self
            .send(Method::GET, path, query, None, None)
            .await
            .map_err(|error| match error {
                ApiError::Status { status: 410, .. } => ApiError::SyncTokenInvalid,
                other => other,
            })?;
        Ok(response.json().await?)
    }

    /// Sends one request to the Calendar API and returns the response if it succeeded.
    async fn send(
        &self,
        method: Method,
        path: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
        if_match: Option<&str>,
    ) -> Result<reqwest::Response, ApiError> {
        let mut url =
            Url::parse(&self.endpoints.calendar_api).expect("the API address must be a valid URL");
        // Calendar IDs contain characters such as `@` and `#`; pushing segments encodes them.
        url.path_segments_mut()
            .expect("the API address must be a base URL")
            .extend(path);

        let mut retried = false;
        loop {
            let token = self.access_token().await?;
            let mut request = self
                .http
                .request(method.clone(), url.clone())
                .query(query)
                .bearer_auth(token);
            if let Some(body) = body {
                request = request.json(body);
            }
            if let Some(etag) = if_match {
                request = request.header(IF_MATCH, etag);
            }
            let response = request.send().await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response);
            }
            if status.as_u16() == 401 && !retried {
                // The token was revoked or expired early: fetch a new one and try once more.
                retried = true;
                *self.access.lock().await = None;
                continue;
            }
            if status.as_u16() == 412 {
                return Err(ApiError::Changed);
            }
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::Status {
                status: status.as_u16(),
                body,
            });
        }
    }

    async fn access_token(&self) -> Result<String, AuthError> {
        let mut access = self.access.lock().await;
        if let Some(current) = access.as_ref()
            && current.expires_at > Utc::now()
        {
            return Ok(current.token.clone());
        }
        let fresh = auth::refresh(
            &self.http,
            &self.config,
            &self.endpoints,
            &self.refresh_token,
        )
        .await?;
        let token = fresh.access_token.clone();
        *access = Some(Access {
            token: fresh.access_token,
            expires_at: fresh.expires_at,
        });
        Ok(token)
    }
}

/// The query that decides whether guests are e-mailed about a change. Google's default
/// is not to.
fn send_updates(notify: bool) -> &'static [(&'static str, &'static str)] {
    if notify {
        &[("sendUpdates", "all")]
    } else {
        &[]
    }
}

/// For writes: a missing event or calendar is its own kind of error.
fn gone_if_missing(error: ApiError) -> ApiError {
    match error {
        ApiError::Status {
            status: 404 | 410, ..
        } => ApiError::Gone,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{
        body_json, header, method, path, query_param, query_param_is_missing,
    };
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn client(server: &MockServer) -> GoogleClient {
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"access_token": "fresh", "expires_in": 3600})),
            )
            .mount(server)
            .await;
        GoogleClient::new(
            reqwest::Client::new(),
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

    #[tokio::test]
    async fn follows_pages_and_returns_the_final_sync_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/team@example.com/events"))
            .and(header("authorization", "Bearer fresh"))
            .and(query_param("singleEvents", "false"))
            .and(query_param_is_missing("pageToken"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "a"}], "nextPageToken": "p2"
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/team@example.com/events"))
            .and(query_param("pageToken", "p2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [{"id": "b"}], "nextSyncToken": "sync-1"
            })))
            .mount(&server)
            .await;

        let counts = std::cell::RefCell::new(Vec::new());
        let listing = client(&server)
            .await
            .list_events("team@example.com", None, &|so_far| {
                counts.borrow_mut().push(so_far)
            })
            .await
            .unwrap();
        // Told after each page how many events have arrived.
        assert_eq!(*counts.borrow(), [1, 2]);
        let ids: Vec<_> = listing
            .items
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["a", "b"]);
        assert_eq!(listing.sync_token.as_deref(), Some("sync-1"));
    }

    #[tokio::test]
    async fn sends_the_sync_token_and_reports_410_as_invalid_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/users/me/calendarList"))
            .and(query_param("syncToken", "stale"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;

        let result = client(&server).await.list_calendars(Some("stale")).await;
        assert!(matches!(result, Err(ApiError::SyncTokenInvalid)));
    }

    #[tokio::test]
    async fn creates_changes_and_deletes_an_event() {
        let server = MockServer::start().await;
        let events = "/calendars/me@example.com/events";
        Mock::given(method("POST"))
            .and(path(events))
            .and(body_json(json!({"summary": "Lunch"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "e1", "etag": "\"1\"", "summary": "Lunch"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PATCH"))
            .and(path(format!("{events}/e1")))
            .and(header("if-match", "\"1\""))
            .and(body_json(json!({"summary": "Late lunch"})))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"id": "e1", "etag": "\"2\"", "summary": "Late lunch"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path(format!("{events}/e1")))
            .and(header("if-match", "\"2\""))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;

        let client = client(&server).await;
        let created = client
            .insert_event("me@example.com", &json!({"summary": "Lunch"}))
            .await
            .unwrap();
        assert_eq!(created["id"], "e1");
        let changed = client
            .patch_event(
                "me@example.com",
                "e1",
                &json!({"summary": "Late lunch"}),
                created["etag"].as_str(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(changed["summary"], "Late lunch");
        client
            .delete_event("me@example.com", "e1", changed["etag"].as_str(), false)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn guests_are_told_only_when_asked_and_occurrences_are_listed() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/calendars/c/events/quiet"))
            .and(query_param_is_missing("sendUpdates"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/calendars/c/events/told"))
            .and(query_param("sendUpdates", "all"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/c/events/series/instances"))
            .and(query_param("timeMin", "2026-11-17T10:00:00+00:00"))
            .and(query_param("timeMax", "2026-11-17T10:00:01+00:00"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({"items": [{"id": "series_20261117T100000Z"}]})),
            )
            .expect(1)
            .mount(&server)
            .await;

        let client = client(&server).await;
        client
            .delete_event("c", "quiet", None, false)
            .await
            .unwrap();
        client.delete_event("c", "told", None, true).await.unwrap();
        let from = "2026-11-17T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let found = client
            .instances("c", "series", from, from + chrono::Duration::seconds(1))
            .await
            .unwrap();
        assert_eq!(found[0]["id"], "series_20261117T100000Z");
    }

    #[tokio::test]
    async fn a_write_reports_a_newer_version_and_a_missing_event() {
        let server = MockServer::start().await;
        Mock::given(method("PATCH"))
            .and(path("/calendars/c/events/stale"))
            .respond_with(ResponseTemplate::new(412))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/calendars/c/events/deleted"))
            .respond_with(ResponseTemplate::new(410))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/calendars/c/events/unknown"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = client(&server).await;
        let patched = client
            .patch_event("c", "stale", &json!({"summary": "x"}), Some("\"1\""), false)
            .await;
        assert!(matches!(patched, Err(ApiError::Changed)));
        for id in ["deleted", "unknown"] {
            let deleted = client.delete_event("c", id, None, false).await;
            assert!(matches!(deleted, Err(ApiError::Gone)), "{id}");
        }
    }

    #[tokio::test]
    async fn refreshes_and_retries_once_after_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendars/primary"))
            .and(header("authorization", "Bearer revoked"))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/calendars/primary"))
            .and(header("authorization", "Bearer fresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "me@example.com"})))
            .expect(1)
            .mount(&server)
            .await;

        let client = client(&server)
            .await
            .with_access_token("revoked".into(), Utc::now() + chrono::Duration::hours(1));
        assert_eq!(client.account_email().await.unwrap(), "me@example.com");
    }
}
