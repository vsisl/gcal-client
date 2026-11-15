//! Calendar and event types, shaped after the Google Calendar API v3 resources.
//!
//! Only the fields the app reads so far are typed. The store keeps each resource's complete
//! JSON, so fields added here later are available without syncing again.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// One calendar as it appears in an account's calendar list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarListEntry {
    pub id: String,
    #[serde(default)]
    pub summary: String,
    /// The user's own name for the calendar; takes precedence over `summary`.
    pub summary_override: Option<String>,
    pub background_color: Option<String>,
    pub foreground_color: Option<String>,
    pub time_zone: Option<String>,
    pub access_role: Option<String>,
    #[serde(default)]
    pub primary: bool,
    /// Whether the calendar is ticked in the web interface's sidebar.
    #[serde(default)]
    pub selected: bool,
    #[serde(default)]
    pub hidden: bool,
    /// Set in incremental sync results when the calendar was removed from the list.
    #[serde(default)]
    pub deleted: bool,
}

impl CalendarListEntry {
    /// The name to show: the user's override if set, otherwise the calendar's own name.
    pub fn display_name(&self) -> &str {
        self.summary_override.as_deref().unwrap_or(&self.summary)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EventStatus {
    #[default]
    Confirmed,
    Tentative,
    /// Deleted events, and deleted single occurrences of a series, arrive as cancelled.
    Cancelled,
}

/// A start or end as Google sends it: `date` for all-day events, `dateTime` otherwise.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventDateTime {
    pub date: Option<NaiveDate>,
    pub date_time: Option<DateTime<Utc>>,
    pub time_zone: Option<String>,
}

/// A start or end with the all-day/timed distinction made explicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventTime {
    AllDay(NaiveDate),
    Timed(DateTime<Utc>),
}

impl EventDateTime {
    /// Returns `None` when Google sent neither field, as it does for cancelled stubs.
    pub fn time(&self) -> Option<EventTime> {
        match (self.date_time, self.date) {
            (Some(dt), _) => Some(EventTime::Timed(dt)),
            (None, Some(d)) => Some(EventTime::AllDay(d)),
            (None, None) => None,
        }
    }
}

impl EventTime {
    /// Seconds since the Unix epoch; all-day dates count from midnight UTC. Used only for
    /// coarse range filtering, where an all-day event's true zone shifts it by under a day.
    pub fn timestamp(&self) -> i64 {
        match self {
            EventTime::Timed(dt) => dt.timestamp(),
            EventTime::AllDay(d) => d.and_time(chrono::NaiveTime::MIN).and_utc().timestamp(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    pub id: String,
    #[serde(default)]
    pub status: EventStatus,
    pub etag: Option<String>,
    pub summary: Option<String>,
    pub start: Option<EventDateTime>,
    pub end: Option<EventDateTime>,
    pub color_id: Option<String>,
    /// RRULE/EXDATE/RDATE lines; present only on the master event of a series.
    #[serde(default)]
    pub recurrence: Vec<String>,
    /// On a changed or cancelled occurrence: the series it belongs to.
    pub recurring_event_id: Option<String>,
    /// On a changed or cancelled occurrence: when it would have started unchanged.
    pub original_start_time: Option<EventDateTime>,
}

impl Event {
    pub fn start_time(&self) -> Option<EventTime> {
        self.start.as_ref().and_then(EventDateTime::time)
    }

    pub fn end_time(&self) -> Option<EventTime> {
        self.end.as_ref().and_then(EventDateTime::time)
    }

    pub fn is_all_day(&self) -> bool {
        matches!(self.start_time(), Some(EventTime::AllDay(_)))
    }

    pub fn is_recurring_master(&self) -> bool {
        !self.recurrence.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timed_event_with_offset_into_utc() {
        let event: Event = serde_json::from_str(
            r#"{
                "id": "abc", "status": "confirmed", "summary": "Standup",
                "start": {"dateTime": "2026-11-16T10:00:00+01:00", "timeZone": "Europe/Prague"},
                "end": {"dateTime": "2026-11-16T10:15:00+01:00", "timeZone": "Europe/Prague"},
                "someFieldWeDoNotKnow": {"x": 1}
            }"#,
        )
        .unwrap();
        let expected = "2026-11-16T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(event.start_time(), Some(EventTime::Timed(expected)));
        assert!(!event.is_all_day());
        assert!(!event.is_recurring_master());
    }

    #[test]
    fn parses_all_day_recurring_master() {
        let event: Event = serde_json::from_str(
            r#"{
                "id": "abc",
                "start": {"date": "2026-11-16"}, "end": {"date": "2026-11-17"},
                "recurrence": ["RRULE:FREQ=YEARLY"]
            }"#,
        )
        .unwrap();
        let day = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap();
        assert_eq!(event.start_time(), Some(EventTime::AllDay(day)));
        assert_eq!(event.status, EventStatus::Confirmed);
        assert!(event.is_all_day());
        assert!(event.is_recurring_master());
    }

    #[test]
    fn parses_cancelled_stub_without_times() {
        let event: Event = serde_json::from_str(r#"{"id": "abc", "status": "cancelled"}"#).unwrap();
        assert_eq!(event.status, EventStatus::Cancelled);
        assert_eq!(event.start_time(), None);
    }

    #[test]
    fn calendar_display_name_prefers_override() {
        let cal: CalendarListEntry = serde_json::from_str(
            r#"{"id": "x", "summary": "team@example.com", "summaryOverride": "Team"}"#,
        )
        .unwrap();
        assert_eq!(cal.display_name(), "Team");
    }
}
