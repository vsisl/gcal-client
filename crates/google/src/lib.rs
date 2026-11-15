//! Google sign-in and API clients (Calendar, Tasks, People).

pub mod auth;
pub mod client;
pub mod config;
mod pkce;

pub use auth::{AuthError, PendingLogin, TokenSet, begin_login};
pub use client::{ApiError, GoogleClient, Listing};
pub use config::{ClientConfig, ConfigError, Endpoints};
/// The HTTP client type the API client is built on, re-exported so callers share one.
pub use reqwest::Client as HttpClient;

/// Read/write access to calendars and events. The only scope requested so far; later
/// milestones add Tasks and contacts when their features arrive.
pub const SCOPE_CALENDAR: &str = "https://www.googleapis.com/auth/calendar";
