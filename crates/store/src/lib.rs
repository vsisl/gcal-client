//! Local SQLite cache of accounts, calendars, events and settings.
//!
//! Every calendar and event row carries its account, so the merged view is one query across
//! accounts. Rows keep the complete JSON Google sent next to the few columns used for
//! filtering, so fields the app starts reading later need no new sync.

use std::path::Path;

use gcal_core::{CalendarListEntry, Event, EventStatus};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored data is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, StoreError>;

/// Local identifier of a signed-in account.
pub type AccountId = i64;

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: AccountId,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredCalendar {
    pub account_id: AccountId,
    pub entry: CalendarListEntry,
    /// Token for fetching only this calendar's event changes; `None` before its first sync.
    pub events_sync_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEvent {
    pub account_id: AccountId,
    pub calendar_id: String,
    pub event: Event,
}

/// Schema versions, applied in order; the database's `user_version` counts how many ran.
const MIGRATIONS: &[&str] = &["
    CREATE TABLE accounts (
        id INTEGER PRIMARY KEY,
        email TEXT NOT NULL UNIQUE,
        calendar_list_sync_token TEXT
    );
    CREATE TABLE calendars (
        account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
        id TEXT NOT NULL,
        json TEXT NOT NULL,
        events_sync_token TEXT,
        PRIMARY KEY (account_id, id)
    );
    CREATE TABLE events (
        account_id INTEGER NOT NULL,
        calendar_id TEXT NOT NULL,
        id TEXT NOT NULL,
        -- Seconds since the Unix epoch; NULL on cancelled occurrences, which have no times.
        start_ts INTEGER,
        end_ts INTEGER,
        -- 1 for the master event of a series, which can occur in any date range.
        is_master INTEGER NOT NULL,
        recurring_event_id TEXT,
        json TEXT NOT NULL,
        PRIMARY KEY (account_id, calendar_id, id),
        FOREIGN KEY (account_id, calendar_id)
            REFERENCES calendars(account_id, id) ON DELETE CASCADE
    );
    CREATE INDEX events_by_time ON events (start_ts, end_ts);
"];

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        Self::prepare(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "foreign_keys", true)?;
        // Write-ahead logging lets the UI read while a sync is writing.
        let _: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        let done: u32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        for (index, migration) in MIGRATIONS.iter().enumerate().skip(done as usize) {
            let tx = conn.transaction()?;
            tx.execute_batch(migration)?;
            tx.pragma_update(None, "user_version", index as u32 + 1)?;
            tx.commit()?;
        }
        Ok(Self { conn })
    }

    /// Adds the account if it is new and returns its ID either way.
    pub fn add_account(&self, email: &str) -> Result<AccountId> {
        self.conn.execute(
            "INSERT OR IGNORE INTO accounts (email) VALUES (?1)",
            [email],
        )?;
        Ok(self
            .conn
            .query_row("SELECT id FROM accounts WHERE email = ?1", [email], |row| {
                row.get(0)
            })?)
    }

    /// Removes the account together with its calendars and events.
    pub fn remove_account(&self, id: AccountId) -> Result<()> {
        self.conn
            .execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        Ok(())
    }

    pub fn accounts(&self) -> Result<Vec<Account>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, email FROM accounts ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                email: row.get(1)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn calendar_list_sync_token(&self, account: AccountId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT calendar_list_sync_token FROM accounts WHERE id = ?1",
                [account],
                |row| row.get(0),
            )
            .optional()?
            .flatten())
    }

    /// Saves calendar list entries from a sync and the token for the next one.
    ///
    /// With `full` set, `items` is the complete list and calendars missing from it are
    /// removed; otherwise `items` holds only changes, with removals flagged `deleted`.
    pub fn apply_calendar_list(
        &mut self,
        account: AccountId,
        items: &[Value],
        sync_token: Option<&str>,
        full: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        let mut kept = Vec::new();
        for item in items {
            let entry: CalendarListEntry = serde_json::from_value(item.clone())?;
            if entry.deleted {
                tx.execute(
                    "DELETE FROM calendars WHERE account_id = ?1 AND id = ?2",
                    params![account, entry.id],
                )?;
                continue;
            }
            tx.execute(
                "INSERT INTO calendars (account_id, id, json) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account_id, id) DO UPDATE SET json = excluded.json",
                params![account, entry.id, item.to_string()],
            )?;
            kept.push(entry.id);
        }
        if full {
            let mut stmt = tx.prepare("SELECT id FROM calendars WHERE account_id = ?1")?;
            let existing: Vec<String> = stmt
                .query_map([account], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            drop(stmt);
            for id in existing.iter().filter(|id| !kept.contains(id)) {
                tx.execute(
                    "DELETE FROM calendars WHERE account_id = ?1 AND id = ?2",
                    params![account, id],
                )?;
            }
        }
        tx.execute(
            "UPDATE accounts SET calendar_list_sync_token = ?2 WHERE id = ?1",
            params![account, sync_token],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// All calendars of all accounts, ordered by account, then primary first, then by name.
    pub fn calendars(&self) -> Result<Vec<StoredCalendar>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, json, events_sync_token FROM calendars ORDER BY account_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, AccountId>(0)?,
                row.get::<_, String>(1)?,
                row.get(2)?,
            ))
        })?;
        let mut calendars = Vec::new();
        for row in rows {
            let (account_id, json, events_sync_token) = row?;
            calendars.push(StoredCalendar {
                account_id,
                entry: serde_json::from_str(&json)?,
                events_sync_token,
            });
        }
        calendars.sort_by(|a, b| {
            (
                a.account_id,
                !a.entry.primary,
                a.entry.display_name().to_lowercase(),
            )
                .cmp(&(
                    b.account_id,
                    !b.entry.primary,
                    b.entry.display_name().to_lowercase(),
                ))
        });
        Ok(calendars)
    }

    /// Saves events from a sync of one calendar and the token for the next one.
    ///
    /// With `full` set, `items` is everything in the calendar and replaces what was stored;
    /// otherwise `items` holds only changes. A cancelled event is a deletion, except for a
    /// cancelled occurrence of a series, which is kept: expanding the series needs to know
    /// which occurrences to leave out.
    pub fn apply_events(
        &mut self,
        account: AccountId,
        calendar_id: &str,
        items: &[Value],
        sync_token: Option<&str>,
        full: bool,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if full {
            tx.execute(
                "DELETE FROM events WHERE account_id = ?1 AND calendar_id = ?2",
                params![account, calendar_id],
            )?;
        }
        for item in items {
            let event: Event = serde_json::from_value(item.clone())?;
            if event.status == EventStatus::Cancelled && event.recurring_event_id.is_none() {
                tx.execute(
                    "DELETE FROM events WHERE account_id = ?1 AND calendar_id = ?2 AND id = ?3",
                    params![account, calendar_id, event.id],
                )?;
                continue;
            }
            tx.execute(
                "INSERT OR REPLACE INTO events
                 (account_id, calendar_id, id, start_ts, end_ts, is_master, recurring_event_id, json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    account,
                    calendar_id,
                    event.id,
                    event.start_time().map(|t| t.timestamp()),
                    event.end_time().map(|t| t.timestamp()),
                    event.is_recurring_master(),
                    event.recurring_event_id,
                    item.to_string(),
                ],
            )?;
        }
        tx.execute(
            "UPDATE calendars SET events_sync_token = ?3 WHERE account_id = ?1 AND id = ?2",
            params![account, calendar_id, sync_token],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Events that can appear between two instants (seconds since the Unix epoch): single
    /// events overlapping the range, plus every series master and changed occurrence, since
    /// only expanding a series tells whether it occurs in the range.
    pub fn events_for_range(&self, from_ts: i64, to_ts: i64) -> Result<Vec<StoredEvent>> {
        let mut stmt = self.conn.prepare(
            "SELECT account_id, calendar_id, json FROM events
             WHERE is_master = 1 OR recurring_event_id IS NOT NULL
                OR (start_ts < ?2 AND end_ts > ?1)",
        )?;
        let rows = stmt.query_map(params![from_ts, to_ts], |row| {
            Ok((
                row.get::<_, AccountId>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (account_id, calendar_id, json) = row?;
            events.push(StoredEvent {
                account_id,
                calendar_id,
                event: serde_json::from_str(&json)?,
            });
        }
        Ok(events)
    }

    pub fn event_count(&self, account: AccountId) -> Result<i64> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM events WHERE account_id = ?1",
            [account],
            |row| row.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOUR: i64 = 3600;
    /// 2026-11-16T00:00:00Z
    const DAY: i64 = 1_794_787_200;

    fn timed(id: &str, start: &str, end: &str) -> Value {
        json!({"id": id, "summary": id, "start": {"dateTime": start}, "end": {"dateTime": end}})
    }

    fn store_with_calendar() -> (Store, AccountId) {
        let mut store = Store::open_in_memory().unwrap();
        let account = store.add_account("me@example.com").unwrap();
        let list = [json!({"id": "cal", "summary": "Cal"})];
        store
            .apply_calendar_list(account, &list, Some("list-1"), true)
            .unwrap();
        (store, account)
    }

    fn ids(store: &Store, from: i64, to: i64) -> Vec<String> {
        let mut ids: Vec<_> = store
            .events_for_range(from, to)
            .unwrap()
            .into_iter()
            .map(|e| e.event.id)
            .collect();
        ids.sort();
        ids
    }

    #[test]
    fn adding_the_same_account_twice_returns_the_same_id() {
        let store = Store::open_in_memory().unwrap();
        let first = store.add_account("me@example.com").unwrap();
        assert_eq!(store.add_account("me@example.com").unwrap(), first);
        assert_eq!(store.accounts().unwrap().len(), 1);
    }

    #[test]
    fn full_calendar_list_replaces_and_incremental_one_patches() {
        let (mut store, account) = store_with_calendar();
        assert_eq!(
            store.calendar_list_sync_token(account).unwrap().as_deref(),
            Some("list-1")
        );

        let changes = [
            json!({"id": "cal", "deleted": true}),
            json!({"id": "new", "summary": "New"}),
        ];
        store
            .apply_calendar_list(account, &changes, Some("list-2"), false)
            .unwrap();
        let names: Vec<_> = store
            .calendars()
            .unwrap()
            .into_iter()
            .map(|c| c.entry.id)
            .collect();
        assert_eq!(names, ["new"]);

        let full = [json!({"id": "other", "summary": "Other"})];
        store
            .apply_calendar_list(account, &full, Some("list-3"), true)
            .unwrap();
        let names: Vec<_> = store
            .calendars()
            .unwrap()
            .into_iter()
            .map(|c| c.entry.id)
            .collect();
        assert_eq!(names, ["other"]);
    }

    #[test]
    fn calendars_are_ordered_primary_first_then_by_name() {
        let (mut store, account) = store_with_calendar();
        let list = [
            json!({"id": "b", "summary": "beta"}),
            json!({"id": "a", "summary": "Alpha"}),
            json!({"id": "me", "summary": "zed", "primary": true}),
        ];
        store
            .apply_calendar_list(account, &list, None, true)
            .unwrap();
        let order: Vec<_> = store
            .calendars()
            .unwrap()
            .into_iter()
            .map(|c| c.entry.id)
            .collect();
        assert_eq!(order, ["me", "a", "b"]);
    }

    #[test]
    fn range_query_returns_overlapping_events_and_every_series() {
        let (mut store, account) = store_with_calendar();
        let items = [
            timed("inside", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z"),
            timed("before", "2026-11-15T10:00:00Z", "2026-11-15T11:00:00Z"),
            timed(
                "spans-start",
                "2026-11-15T23:00:00Z",
                "2026-11-16T01:00:00Z",
            ),
            json!({"id": "all-day", "start": {"date": "2026-11-16"}, "end": {"date": "2026-11-17"}}),
            json!({"id": "series", "recurrence": ["RRULE:FREQ=WEEKLY"],
                   "start": {"dateTime": "2020-01-06T09:00:00Z"},
                   "end": {"dateTime": "2020-01-06T09:30:00Z"}}),
            json!({"id": "series_x", "status": "cancelled", "recurringEventId": "series",
                   "originalStartTime": {"dateTime": "2026-11-16T09:00:00Z"}}),
        ];
        store
            .apply_events(account, "cal", &items, Some("ev-1"), true)
            .unwrap();

        assert_eq!(
            ids(&store, DAY, DAY + 24 * HOUR),
            ["all-day", "inside", "series", "series_x", "spans-start"]
        );
        assert_eq!(
            store.calendars().unwrap()[0].events_sync_token.as_deref(),
            Some("ev-1")
        );
    }

    #[test]
    fn incremental_changes_update_and_delete_events() {
        let (mut store, account) = store_with_calendar();
        let first = [
            timed("keep", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z"),
            timed("gone", "2026-11-16T12:00:00Z", "2026-11-16T13:00:00Z"),
        ];
        store
            .apply_events(account, "cal", &first, Some("ev-1"), true)
            .unwrap();

        let changes = [
            json!({"id": "gone", "status": "cancelled"}),
            timed("keep", "2026-11-17T10:00:00Z", "2026-11-17T11:00:00Z"),
        ];
        store
            .apply_events(account, "cal", &changes, Some("ev-2"), false)
            .unwrap();

        assert!(ids(&store, DAY, DAY + 24 * HOUR).is_empty());
        assert_eq!(ids(&store, DAY + 24 * HOUR, DAY + 48 * HOUR), ["keep"]);
        assert_eq!(store.event_count(account).unwrap(), 1);
    }

    #[test]
    fn full_event_sync_discards_what_was_stored() {
        let (mut store, account) = store_with_calendar();
        let old = [timed("old", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z")];
        store
            .apply_events(account, "cal", &old, Some("ev-1"), true)
            .unwrap();
        let new = [timed("new", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z")];
        store
            .apply_events(account, "cal", &new, Some("ev-2"), true)
            .unwrap();
        assert_eq!(ids(&store, DAY, DAY + 24 * HOUR), ["new"]);
    }

    #[test]
    fn removing_a_calendar_or_account_removes_its_events() {
        let (mut store, account) = store_with_calendar();
        let items = [timed("e", "2026-11-16T10:00:00Z", "2026-11-16T11:00:00Z")];
        store
            .apply_events(account, "cal", &items, None, true)
            .unwrap();

        store
            .apply_calendar_list(
                account,
                &[json!({"id": "cal", "deleted": true})],
                None,
                false,
            )
            .unwrap();
        assert_eq!(store.event_count(account).unwrap(), 0);

        store.remove_account(account).unwrap();
        assert!(store.accounts().unwrap().is_empty());
    }

    #[test]
    fn reopening_a_database_file_keeps_data_and_schema_version() {
        let dir = std::env::temp_dir().join(format!("gcal-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.db");
        Store::open(&path)
            .unwrap()
            .add_account("me@example.com")
            .unwrap();
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.accounts().unwrap()[0].email, "me@example.com");
        drop(reopened);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
