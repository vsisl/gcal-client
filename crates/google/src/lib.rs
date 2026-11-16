//! Google sign-in and API clients (Calendar, Tasks, People).

pub mod auth;
pub mod client;
pub mod config;
mod pkce;

pub use auth::{AuthError, PendingLogin, TokenSet, begin_login};
pub use client::{ApiError, GoogleClient, Listing, Person};
pub use config::{ClientConfig, ConfigError, Endpoints};
/// The HTTP client type the API client is built on, re-exported so callers share one.
pub use reqwest::Client as HttpClient;

/// Read/write access to calendars and events.
pub const SCOPE_CALENDAR: &str = "https://www.googleapis.com/auth/calendar";
/// Reading the account's contacts, the people it has written to ("other contacts") and,
/// in an organisation, its directory: for suggesting guests while their name is typed.
/// Nothing is written, and nothing is kept.
pub const SCOPES_PEOPLE: [&str; 3] = [
    "https://www.googleapis.com/auth/contacts.readonly",
    "https://www.googleapis.com/auth/contacts.other.readonly",
    "https://www.googleapis.com/auth/directory.readonly",
];

/// What a sign-in asks for. Reading contacts is asked for only when `people` is set:
/// whether the app should ask every user for it is the owner's decision, still open.
/// An account signed in without it keeps working; suggestions say what is missing.
pub fn scopes(people: bool) -> Vec<&'static str> {
    let mut scopes = vec![SCOPE_CALENDAR];
    if people {
        scopes.extend(SCOPES_PEOPLE);
    }
    scopes
}
