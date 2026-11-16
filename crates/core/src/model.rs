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
    /// Whether this account may create, change and delete events in the calendar.
    pub fn is_writable(&self) -> bool {
        matches!(self.access_role.as_deref(), Some("owner" | "writer"))
    }

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

/// A guest's answer to an invitation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResponseStatus {
    #[default]
    NeedsAction,
    Accepted,
    Declined,
    Tentative,
    /// An answer this app does not know. Calendars shared by others are read too, and
    /// one unfamiliar word must not make a whole calendar unreadable.
    #[serde(other)]
    Unknown,
}

impl ResponseStatus {
    /// As Google's API writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            ResponseStatus::NeedsAction | ResponseStatus::Unknown => "needsAction",
            ResponseStatus::Accepted => "accepted",
            ResponseStatus::Declined => "declined",
            ResponseStatus::Tentative => "tentative",
        }
    }
}

/// A guest of an event.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attendee {
    pub email: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub response_status: ResponseStatus,
    /// Whether it is the account the event was fetched for.
    #[serde(rename = "self", default)]
    pub is_self: bool,
    #[serde(default)]
    pub organizer: bool,
    #[serde(default)]
    pub optional: bool,
    /// A room or a piece of equipment rather than a person.
    #[serde(default)]
    pub resource: bool,
}

impl Attendee {
    /// The name to show: the guest's own, else the address.
    pub fn name(&self) -> &str {
        self.display_name
            .as_deref()
            .or(self.email.as_deref())
            .unwrap_or("")
    }
}

/// Who an event belongs to. Only what the app reads so far.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Organizer {
    /// Whether it is the account the event was fetched for.
    #[serde(rename = "self", default)]
    pub is_self: bool,
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
    pub location: Option<String>,
    /// May contain a small subset of HTML.
    pub description: Option<String>,
    /// Address of the event in Google's web interface.
    pub html_link: Option<String>,
    /// "default" for ordinary events; others are birthdays, out-of-office, focus time,
    /// working location and entries created from Gmail.
    pub event_type: Option<String>,
    /// Guests, including the organiser when there are any.
    #[serde(default)]
    pub attendees: Vec<Attendee>,
    pub organizer: Option<Organizer>,
    /// Whether the organiser lets the guests change the event.
    #[serde(default)]
    pub guests_can_modify: bool,
    /// The address of the event's video meeting, if it has one.
    pub hangout_link: Option<String>,
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

    /// This account's own entry among the guests, if it was invited.
    pub fn own_invitation(&self) -> Option<&Attendee> {
        self.attendees.iter().find(|guest| guest.is_self)
    }

    /// Whether this account invited guests to the event, who would want to hear of a
    /// change or a cancellation.
    pub fn has_guests_to_notify(&self) -> bool {
        !self.attendees.is_empty() && self.organizer.as_ref().is_some_and(|o| o.is_self)
    }

    /// Whether this account, a guest of the event, may change it: the organiser lets
    /// the guests, and Google passes a guest's change on to everyone's copy. Single
    /// events only: cutting a series in two ("this and following") would leave the
    /// guest organising the second half.
    pub fn may_change_as_guest(&self) -> bool {
        self.guests_can_modify
            && self.own_invitation().is_some()
            && !self.organizer.as_ref().is_some_and(|o| o.is_self)
            && !self.is_recurring_master()
            && self.recurring_event_id.is_none()
    }

    /// Whether a change made here reaches other people, who can be told of it.
    pub fn others_see_changes(&self) -> bool {
        self.has_guests_to_notify() || self.may_change_as_guest()
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
    fn only_the_organiser_of_an_event_with_guests_has_guests_to_notify() {
        let event = |json: &str| serde_json::from_str::<Event>(json).unwrap();
        assert!(
            event(
                r#"{"id": "a", "organizer": {"email": "me@x.org", "self": true},
                      "attendees": [{"email": "you@x.org"}]}"#
            )
            .has_guests_to_notify()
        );
        assert!(
            !event(
                r#"{"id": "a", "organizer": {"email": "you@x.org"},
                       "attendees": [{"email": "me@x.org"}]}"#
            )
            .has_guests_to_notify()
        );
        assert!(
            !event(r#"{"id": "a", "organizer": {"email": "me@x.org", "self": true}}"#)
                .has_guests_to_notify()
        );
    }

    #[test]
    fn guests_are_read_with_their_answers() {
        let event: Event = serde_json::from_str(
            r#"{"id": "a", "hangoutLink": "https://meet.google.com/abc-defg-hij",
                "attendees": [
                  {"email": "boss@x.org", "displayName": "The Boss", "organizer": true,
                   "responseStatus": "accepted"},
                  {"email": "me@x.org", "self": true, "responseStatus": "tentative"},
                  {"email": "room@x.org", "resource": true, "responseStatus": "declined"},
                  {"email": "new@x.org"}
                ]}"#,
        )
        .unwrap();
        let names: Vec<_> = event.attendees.iter().map(Attendee::name).collect();
        assert_eq!(names, ["The Boss", "me@x.org", "room@x.org", "new@x.org"]);
        let answers: Vec<_> = event.attendees.iter().map(|g| g.response_status).collect();
        assert_eq!(
            answers,
            [
                ResponseStatus::Accepted,
                ResponseStatus::Tentative,
                ResponseStatus::Declined,
                ResponseStatus::NeedsAction
            ]
        );
        assert_eq!(
            event.own_invitation().unwrap().email.as_deref(),
            Some("me@x.org")
        );
        assert!(event.attendees[2].resource && event.attendees[0].organizer);
        assert_eq!(
            event.hangout_link.as_deref(),
            Some("https://meet.google.com/abc-defg-hij")
        );
        assert_eq!(ResponseStatus::Tentative.as_str(), "tentative");

        // A word for an answer that is new to this app is read, not refused.
        let odd: Event = serde_json::from_str(
            r#"{"id": "a", "attendees": [{"email": "x@x.org", "responseStatus": "delegated"}]}"#,
        )
        .unwrap();
        assert_eq!(odd.attendees[0].response_status, ResponseStatus::Unknown);
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
