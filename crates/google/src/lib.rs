//! Google sign-in and API clients (Calendar, Tasks, People).

pub mod auth;
pub mod client;
pub mod config;
mod pkce;

pub use auth::{AuthError, PendingLogin, TokenSet, begin_login};
pub use client::{
    ApiError, GoogleClient, Listing, Person, download_picture, picture_address_allowed,
};
pub use config::{ClientConfig, ConfigError, Endpoints};
/// The HTTP client type the API client is built on, re-exported so callers share one.
pub use reqwest::Client as HttpClient;

/// What an account can let the app do at Google. Each is one line on Google's consent
/// page, which the user may leave unticked, so an account can have any of them or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Calendars and events, to read and to change. The app is of no use without it.
    Calendar,
    /// The account's own contacts, to read: for suggesting guests.
    Contacts,
    /// The people the account has written to without saving them as contacts
    /// ("other contacts"), to read: for suggesting guests.
    OtherContacts,
    /// The directory of the account's organisation, to read: for suggesting colleagues
    /// as guests. Only asked for when the user says so in the settings: it covers every
    /// profile in the organisation, and an ordinary account has no directory.
    Directory,
    /// The account's own name and profile picture, to show the picture by the account
    /// as Google's web app does. Google counts it as basic, not sensitive.
    Profile,
}

impl Permission {
    pub const ALL: [Permission; 5] = [
        Permission::Calendar,
        Permission::Contacts,
        Permission::OtherContacts,
        Permission::Directory,
        Permission::Profile,
    ];
    /// What a sign-in asks for.
    pub const AT_SIGN_IN: [Permission; 4] = [
        Permission::Calendar,
        Permission::Contacts,
        Permission::OtherContacts,
        Permission::Profile,
    ];

    /// Google's name for the permission.
    pub fn scope(self) -> &'static str {
        match self {
            Permission::Calendar => "https://www.googleapis.com/auth/calendar",
            Permission::Contacts => "https://www.googleapis.com/auth/contacts.readonly",
            Permission::OtherContacts => "https://www.googleapis.com/auth/contacts.other.readonly",
            Permission::Directory => "https://www.googleapis.com/auth/directory.readonly",
            Permission::Profile => "https://www.googleapis.com/auth/userinfo.profile",
        }
    }

    /// Whether it is among the scopes Google says an account has given.
    pub fn is_in(self, granted: &[String]) -> bool {
        granted.iter().any(|scope| scope == self.scope())
    }
}
