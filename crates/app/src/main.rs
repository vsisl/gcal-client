//! The desktop application.

/// Name shown to users. Kept in one place because the public name is not final.
pub const APP_NAME: &str = "gcal-client";

fn main() {
    println!("{APP_NAME} {}", env!("CARGO_PKG_VERSION"));
}
