//! The app's Google sign-in identity and the service addresses it talks to.

use std::fmt;
use std::path::Path;

use serde::Deserialize;

/// The OAuth client the app signs in as. Read from a local file and never committed:
/// Google's terms forbid publishing client credentials in open-source repositories.
#[derive(Clone, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    /// Google issues one even to desktop apps; it is sent along but is not a real secret.
    pub client_secret: Option<String>,
}

impl fmt::Debug for ClientConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientConfig")
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("{path} is not valid: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
}

impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let shown = path.display().to_string();
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: shown.clone(),
            source,
        })?;
        toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: shown,
            source,
        })
    }
}

/// Service addresses, replaceable so tests can point the client at a local mock server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub calendar_api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            calendar_api: "https://www.googleapis.com/calendar/v3".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_hides_the_secret() {
        let config: ClientConfig = toml::from_str(
            "client_id = \"id.apps.googleusercontent.com\"\nclient_secret = \"s3cret\"",
        )
        .unwrap();
        let shown = format!("{config:?}");
        assert!(shown.contains("id.apps.googleusercontent.com"));
        assert!(!shown.contains("s3cret"));
    }

    #[test]
    fn secret_is_optional() {
        let config: ClientConfig = toml::from_str("client_id = \"id\"").unwrap();
        assert!(config.client_secret.is_none());
    }
}
