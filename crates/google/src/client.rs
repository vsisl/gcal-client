//! Authenticated access to the Calendar API for one signed-in account.

use chrono::{DateTime, Utc};
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
        )
        .await
    }

    /// A calendar's events: everything, or only changes since `sync_token`.
    ///
    /// Series are returned as one master event plus its changed occurrences rather than
    /// expanded, so the result stays small and a sync token can be used. The parameters must
    /// be identical on every call for the token to remain valid.
    pub async fn list_events(
        &self,
        calendar_id: &str,
        sync_token: Option<&str>,
    ) -> Result<Listing, ApiError> {
        self.list(
            &["calendars", calendar_id, "events"],
            &[("maxResults", "2500"), ("singleEvents", "false")],
            sync_token,
        )
        .await
    }

    async fn list(
        &self,
        path: &[&str],
        params: &[(&str, &str)],
        sync_token: Option<&str>,
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

    async fn get<T: serde::de::DeserializeOwned>(
        &self,
        path: &[&str],
        query: &[(&str, &str)],
    ) -> Result<T, ApiError> {
        let mut url =
            Url::parse(&self.endpoints.calendar_api).expect("the API address must be a valid URL");
        // Calendar IDs contain characters such as `@` and `#`; pushing segments encodes them.
        url.path_segments_mut()
            .expect("the API address must be a base URL")
            .extend(path);

        let mut retried = false;
        loop {
            let token = self.access_token().await?;
            let response = self
                .http
                .get(url.clone())
                .query(query)
                .bearer_auth(token)
                .send()
                .await?;
            let status = response.status();
            if status.is_success() {
                return Ok(response.json().await?);
            }
            if status.as_u16() == 401 && !retried {
                // The token was revoked or expired early: fetch a new one and try once more.
                retried = true;
                *self.access.lock().await = None;
                continue;
            }
            if status.as_u16() == 410 {
                return Err(ApiError::SyncTokenInvalid);
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{header, method, path, query_param, query_param_is_missing};
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

        let listing = client(&server)
            .await
            .list_events("team@example.com", None)
            .await
            .unwrap();
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
