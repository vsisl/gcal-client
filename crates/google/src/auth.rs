//! Sign-in for a desktop app: system browser, loopback redirect and PKCE.
//!
//! The app listens on a random local port, sends the user to Google in their browser, and
//! Google redirects the browser back to that port with a one-time code, which is then
//! exchanged for tokens.

use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::config::{ClientConfig, Endpoints};
use crate::pkce;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("cannot listen for the sign-in redirect: {0}")]
    Listen(std::io::Error),
    #[error("sign-in was refused: {0}")]
    Denied(String),
    /// The refresh token no longer works; the user has to sign in again. Expected weekly
    /// while the app is unreviewed, and whenever access is revoked.
    #[error("sign-in has expired; please sign in again")]
    Expired,
    #[error("Google rejected the token request ({status}): {body}")]
    Rejected { status: u16, body: String },
    #[error("Google's reply had no refresh token")]
    NoRefreshToken,
    #[error("network error during sign-in: {0}")]
    Http(#[from] reqwest::Error),
}

/// Tokens from a completed sign-in or refresh.
#[derive(Clone)]
pub struct TokenSet {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    /// Present after a sign-in; a refresh usually returns none and the old one stays valid.
    pub refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: i64,
    refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
}

/// A sign-in waiting for the user to finish in their browser.
pub struct PendingLogin {
    listener: TcpListener,
    redirect_uri: String,
    verifier: String,
    state: String,
    /// The address to open in the user's browser.
    pub auth_url: String,
}

/// Starts a sign-in: opens the local listener and builds the address for the browser.
pub async fn begin_login(
    config: &ClientConfig,
    endpoints: &Endpoints,
    scopes: &[&str],
) -> Result<PendingLogin, AuthError> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(AuthError::Listen)?;
    let port = listener.local_addr().map_err(AuthError::Listen)?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let verifier = pkce::random_token();
    let state = pkce::random_token();

    let mut auth_url =
        Url::parse(&endpoints.auth).expect("the sign-in address must be a valid URL");
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &config.client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", &scopes.join(" "))
        .append_pair("code_challenge", &pkce::challenge(&verifier))
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        // Always show the account chooser: adding a second account is the point of the app.
        .append_pair("prompt", "consent select_account")
        .append_pair("access_type", "offline");

    Ok(PendingLogin {
        listener,
        redirect_uri,
        verifier,
        state,
        auth_url: auth_url.into(),
    })
}

impl PendingLogin {
    /// Waits for the browser to come back, then exchanges the code for tokens.
    pub async fn finish(
        self,
        http: &reqwest::Client,
        config: &ClientConfig,
        endpoints: &Endpoints,
    ) -> Result<TokenSet, AuthError> {
        let code = self.wait_for_code().await?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("code_verifier", &self.verifier),
            ("redirect_uri", &self.redirect_uri),
            ("client_id", &config.client_id),
        ];
        if let Some(secret) = &config.client_secret {
            form.push(("client_secret", secret));
        }
        let tokens = request_tokens(http, endpoints, &form).await?;
        if tokens.refresh_token.is_none() {
            return Err(AuthError::NoRefreshToken);
        }
        Ok(tokens)
    }

    /// Serves the loopback port until a request carries this sign-in's result. Browsers also
    /// ask for things like `/favicon.ico`; those get a 404 and are otherwise ignored.
    async fn wait_for_code(&self) -> Result<String, AuthError> {
        loop {
            let (mut stream, _) = self.listener.accept().await.map_err(AuthError::Listen)?;
            let mut buf = vec![0u8; 8192];
            let mut len = 0;
            while len < buf.len() && !buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf[len..]).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => len += n,
                }
            }
            let request = String::from_utf8_lossy(&buf[..len]);
            let outcome = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|target| Url::parse(&format!("http://127.0.0.1{target}")).ok())
                .and_then(|url| self.read_redirect(&url));

            let (status, body) = match &outcome {
                Some(Ok(_)) => (
                    "200 OK",
                    "Signed in. You can close this tab and return to the app.",
                ),
                Some(Err(_)) => (
                    "400 Bad Request",
                    "Sign-in did not complete. Return to the app and try again.",
                ),
                None => ("404 Not Found", ""),
            };
            let page =
                format!("<!doctype html><meta charset=utf-8><title>Sign-in</title><p>{body}</p>");
            let reply = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                page.len()
            );
            // The browser may already have gone away; the sign-in result does not depend on it.
            let _ = stream.write_all(reply.as_bytes()).await;
            let _ = stream.shutdown().await;

            if let Some(result) = outcome {
                return result;
            }
        }
    }

    /// Extracts the code or the error from a redirect. `None` means the request was not a
    /// redirect for this sign-in (wrong or missing `state`).
    fn read_redirect(&self, url: &Url) -> Option<Result<String, AuthError>> {
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
        };
        if param("state").as_deref() != Some(self.state.as_str()) {
            return None;
        }
        if let Some(error) = param("error") {
            return Some(Err(AuthError::Denied(error)));
        }
        param("code").map(Ok)
    }
}

/// Trades a refresh token for a fresh access token.
pub async fn refresh(
    http: &reqwest::Client,
    config: &ClientConfig,
    endpoints: &Endpoints,
    refresh_token: &str,
) -> Result<TokenSet, AuthError> {
    let mut form = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("client_id", &config.client_id),
    ];
    if let Some(secret) = &config.client_secret {
        form.push(("client_secret", secret));
    }
    request_tokens(http, endpoints, &form).await
}

async fn request_tokens(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    form: &[(&str, &str)],
) -> Result<TokenSet, AuthError> {
    let response = http.post(&endpoints.token).form(form).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let invalid_grant = serde_json::from_str::<TokenErrorResponse>(&body)
            .is_ok_and(|e| e.error == "invalid_grant");
        return Err(if invalid_grant {
            AuthError::Expired
        } else {
            AuthError::Rejected {
                status: status.as_u16(),
                body,
            }
        });
    }
    let tokens: TokenResponse = response.json().await?;
    Ok(TokenSet {
        access_token: tokens.access_token,
        // Treat the token as expiring a minute early so a request never starts with a
        // token that lapses in flight.
        expires_at: Utc::now() + Duration::seconds(tokens.expires_in - 60),
        refresh_token: tokens.refresh_token,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn config() -> ClientConfig {
        ClientConfig {
            client_id: "test-client".into(),
            client_secret: Some("test-secret".into()),
        }
    }

    fn endpoints(server: &MockServer) -> Endpoints {
        Endpoints {
            auth: format!("{}/auth", server.uri()),
            token: format!("{}/token", server.uri()),
            calendar_api: server.uri(),
        }
    }

    fn query(url: &str, name: &str) -> String {
        Url::parse(url)
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == name)
            .unwrap()
            .1
            .into_owned()
    }

    #[tokio::test]
    async fn full_sign_in_exchanges_the_code_with_the_pkce_verifier() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .and(body_string_contains("code_verifier="))
            .and(body_string_contains("client_secret=test-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access-1", "expires_in": 3600, "refresh_token": "refresh-1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let endpoints = endpoints(&server);
        let pending = begin_login(&config(), &endpoints, &["scope-a", "scope-b"])
            .await
            .unwrap();
        assert_eq!(query(&pending.auth_url, "scope"), "scope-a scope-b");
        assert_eq!(query(&pending.auth_url, "code_challenge_method"), "S256");
        let redirect = query(&pending.auth_url, "redirect_uri");
        let state = query(&pending.auth_url, "state");

        let http = reqwest::Client::new();
        let browser = http.clone();
        tokio::spawn(async move {
            // A stray request first, as a real browser would send, then the redirect.
            let _ = browser.get(format!("{redirect}/favicon.ico")).send().await;
            let _ = browser
                .get(format!("{redirect}/?state={state}&code=the-code"))
                .send()
                .await;
        });

        let tokens = pending.finish(&http, &config(), &endpoints).await.unwrap();
        assert_eq!(tokens.access_token, "access-1");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh-1"));
        assert!(tokens.expires_at > Utc::now() + Duration::minutes(50));
    }

    #[tokio::test]
    async fn redirect_with_wrong_state_is_ignored_and_denial_is_reported() {
        let server = MockServer::start().await;
        let endpoints = endpoints(&server);
        let pending = begin_login(&config(), &endpoints, &["s"]).await.unwrap();
        let redirect = query(&pending.auth_url, "redirect_uri");
        let state = query(&pending.auth_url, "state");

        let http = reqwest::Client::new();
        let browser = http.clone();
        tokio::spawn(async move {
            let _ = browser
                .get(format!("{redirect}/?state=forged&code=evil"))
                .send()
                .await;
            let _ = browser
                .get(format!("{redirect}/?state={state}&error=access_denied"))
                .send()
                .await;
        });

        let result = pending.finish(&http, &config(), &endpoints).await;
        assert!(matches!(result, Err(AuthError::Denied(reason)) if reason == "access_denied"));
    }

    #[tokio::test]
    async fn refresh_reports_expired_sign_in_on_invalid_grant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({"error": "invalid_grant"})),
            )
            .mount(&server)
            .await;

        let result = refresh(
            &reqwest::Client::new(),
            &config(),
            &endpoints(&server),
            "old",
        )
        .await;
        assert!(matches!(result, Err(AuthError::Expired)));
    }
}
