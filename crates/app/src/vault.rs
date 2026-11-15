//! Keeps each account's refresh token in the system keyring, never in a plain file.
//!
//! Uses the Secret Service on a normal desktop and the sandbox's keyring when running as a
//! Flatpak; `oo7` chooses between them.

use anyhow::{Context, Result};

const APP_ATTRIBUTE: (&str, &str) = ("application", "gcal-client");

pub struct Vault {
    keyring: oo7::Keyring,
}

impl Vault {
    pub async fn open() -> Result<Self> {
        let keyring = oo7::Keyring::new()
            .await
            .context("cannot reach the system keyring")?;
        keyring
            .unlock()
            .await
            .context("cannot unlock the system keyring")?;
        Ok(Self { keyring })
    }

    pub async fn save(&self, email: &str, refresh_token: &str) -> Result<()> {
        self.keyring
            .create_item(
                &format!("gcal-client sign-in for {email}"),
                &[APP_ATTRIBUTE, ("account", email)],
                refresh_token,
                true,
            )
            .await
            .context("cannot save the sign-in to the system keyring")
    }

    pub async fn load(&self, email: &str) -> Result<Option<String>> {
        let items = self
            .keyring
            .search_items(&[APP_ATTRIBUTE, ("account", email)])
            .await
            .context("cannot read the system keyring")?;
        let Some(item) = items.first() else {
            return Ok(None);
        };
        let secret = item
            .secret()
            .await
            .context("cannot read the saved sign-in")?;
        Ok(Some(String::from_utf8_lossy(&secret).into_owned()))
    }

    pub async fn delete(&self, email: &str) -> Result<()> {
        self.keyring
            .delete(&[APP_ATTRIBUTE, ("account", email)])
            .await
            .context("cannot remove the sign-in from the system keyring")
    }
}
