//! Turns stored events into the concrete instances that fall in a date range.
//!
//! Google stores a repeating event as one master carrying RRULE/EXDATE/RDATE lines, plus a
//! separate event for every occurrence that was changed or deleted. Expanding a series means
//! generating the master's occurrences and replacing those that have such an exception.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use rrule::RRuleSet;

use crate::model::{Event, EventStatus, EventTime};

/// One thing to draw: an event, or a single occurrence of a repeating one.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance<'a, K> {
    /// Whatever the caller uses to tell calendars apart (account and calendar).
    pub key: K,
    pub event: &'a Event,
    pub start: EventTime,
    pub end: EventTime,
}

/// Upper bound on occurrences of one series inside one range; far above any real calendar.
const MAX_OCCURRENCES: u16 = 5000;

/// The instances of `events` that may overlap `from..to`.
///
/// `events` must hold every master and exception (the store's range query does); single
/// events outside the range are filtered out here. All-day instances are kept with a day of
/// slack on either side, since which days they cover depends on the viewer's time zone.
pub fn expand<'a, K>(
    events: impl IntoIterator<Item = (K, &'a Event)>,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<Instance<'a, K>>
where
    K: Clone + Eq + Hash,
{
    let events: Vec<(K, &Event)> = events.into_iter().collect();

    // Original start times of occurrences that were changed or deleted, per series.
    let mut replaced: HashMap<(K, &str), HashSet<EventTime>> = HashMap::new();
    for (key, event) in &events {
        if let (Some(master), Some(original)) = (
            event.recurring_event_id.as_deref(),
            event.original_start_time.as_ref().and_then(|t| t.time()),
        ) {
            replaced
                .entry((key.clone(), master))
                .or_default()
                .insert(original);
        }
    }

    let mut instances = Vec::new();
    for (key, event) in events {
        if event.status == EventStatus::Cancelled {
            continue;
        }
        let (Some(start), Some(end)) = (event.start_time(), event.end_time()) else {
            continue;
        };
        if event.is_recurring_master() {
            let skip = replaced.get(&(key.clone(), event.id.as_str()));
            for (start, end) in occurrences(event, start, end, from, to) {
                if skip.is_none_or(|s| !s.contains(&start)) {
                    instances.push(Instance {
                        key: key.clone(),
                        event,
                        start,
                        end,
                    });
                }
            }
        } else if overlaps(start, end, from, to) {
            instances.push(Instance {
                key,
                event,
                start,
                end,
            });
        }
    }
    instances
}

fn overlaps(start: EventTime, end: EventTime, from: DateTime<Utc>, to: DateTime<Utc>) -> bool {
    let slack = match start {
        EventTime::AllDay(_) => 86_400,
        EventTime::Timed(_) => 0,
    };
    // A zero-length event still counts as occurring at its start.
    let end = end.timestamp().max(start.timestamp() + 1);
    start.timestamp() < to.timestamp() + slack && end > from.timestamp() - slack
}

/// Start and end of each occurrence of a series that may overlap `from..to`.
///
/// A series whose rule cannot be parsed falls back to its first occurrence alone, so the
/// event stays visible instead of disappearing.
fn occurrences(
    master: &Event,
    start: EventTime,
    end: EventTime,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<(EventTime, EventTime)> {
    let length = Duration::seconds((end.timestamp() - start.timestamp()).max(0));
    let all_day = matches!(start, EventTime::AllDay(_));
    let slack = if all_day {
        Duration::days(1)
    } else {
        Duration::zero()
    };
    let window_start = from - length - slack;
    let window_end = to + slack;

    let Some(set) = rule_set(master, start) else {
        return if overlaps(start, end, from, to) {
            vec![(start, end)]
        } else {
            Vec::new()
        };
    };
    let utc = rrule::Tz::UTC;
    let result = set
        .after(window_start.with_timezone(&utc))
        .before(window_end.with_timezone(&utc))
        .all(MAX_OCCURRENCES);

    // The start is in the set twice when the rules produce it as well (see `rule_set`).
    let mut dates = result.dates;
    dates.sort();
    dates.dedup();

    dates
        .into_iter()
        .map(|occurrence| {
            let occurrence = occurrence.with_timezone(&Utc);
            if all_day {
                let day = occurrence.date_naive();
                let days = length.num_days().max(1);
                (
                    EventTime::AllDay(day),
                    EventTime::AllDay(day + Duration::days(days)),
                )
            } else {
                (
                    EventTime::Timed(occurrence),
                    EventTime::Timed(occurrence + length),
                )
            }
        })
        .filter(|(start, end)| overlaps(*start, *end, from, to))
        .collect()
}

/// Builds the rule set for a master in the form the `rrule` crate parses.
///
/// Timed series repeat in the event's own time zone, so that a 9:00 meeting stays at 9:00
/// across daylight-saving changes. All-day series are computed at midnight UTC and only the
/// date of each result is used.
fn rule_set(master: &Event, start: EventTime) -> Option<RRuleSet> {
    // The start is handed over as an instant in its zone, not as local-time text: text
    // cannot tell apart the two 02:30s of the night clocks go back, and the parser rejects
    // such a start outright.
    let first = match start {
        EventTime::AllDay(day) => day_start_utc(day).with_timezone(&rrule::Tz::UTC),
        EventTime::Timed(instant) => {
            let zone = master
                .start
                .as_ref()
                .and_then(|s| s.time_zone.as_deref())
                .and_then(|name| name.parse::<chrono_tz::Tz>().ok())
                .unwrap_or(chrono_tz::UTC);
            instant.with_timezone(&rrule::Tz::Tz(zone))
        }
    };
    let all_day = matches!(start, EventTime::AllDay(_));
    let lines: Vec<String> = master
        .recurrence
        .iter()
        .map(|line| normalise_line(line, all_day))
        .collect();
    RRuleSet::new(first)
        .set_from_string(&lines.join("\n"))
        .ok()
        // The library yields only what the rules and extra dates produce. Google counts the
        // event's own start as an occurrence too, which matters for a series made of
        // extra dates alone. A start the rules also produce comes out twice; the caller
        // removes the duplicate.
        .map(|set| set.rdate(first))
}

/// Rewrites the date-only values Google uses into the date-time forms the parser needs.
fn normalise_line(line: &str, all_day: bool) -> String {
    let Some((name, value)) = line.split_once(':') else {
        return line.to_owned();
    };
    let property = name.split(';').next().unwrap_or(name);
    match property {
        "RRULE" | "EXRULE" => {
            let parts: Vec<String> = value
                .split(';')
                .map(|part| match part.strip_prefix("UNTIL=") {
                    // A date-only end means "through that whole day".
                    Some(until) if !until.contains('T') => format!("UNTIL={until}T235959Z"),
                    // The parser wants UTC; Google sends UTC but omits nothing we can fix
                    // otherwise, so a floating time is taken as UTC.
                    Some(until) if !until.ends_with('Z') => format!("UNTIL={until}Z"),
                    _ => part.to_owned(),
                })
                .collect();
            format!("{property}:{}", parts.join(";"))
        }
        "EXDATE" | "RDATE" if all_day => {
            let dates: Vec<String> = value
                .split(',')
                .map(|date| match date.split_once('T') {
                    Some((day, _)) => format!("{day}T000000Z"),
                    None => format!("{date}T000000Z"),
                })
                .collect();
            format!("{property}:{}", dates.join(","))
        }
        _ => line.to_owned(),
    }
}

/// Midnight UTC at the start of `day`; handy for building ranges in tests and callers.
pub fn day_start_utc(day: NaiveDate) -> DateTime<Utc> {
    day.and_time(NaiveTime::MIN).and_utc()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(value: serde_json::Value) -> Event {
        serde_json::from_value(value).unwrap()
    }

    fn date(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    fn instant(text: &str) -> DateTime<Utc> {
        text.parse().unwrap()
    }

    /// The week of Monday 16 November 2026, in UTC.
    fn week() -> (DateTime<Utc>, DateTime<Utc>) {
        (
            day_start_utc(date("2026-11-16")),
            day_start_utc(date("2026-11-23")),
        )
    }

    fn starts(events: &[Event]) -> Vec<EventTime> {
        let (from, to) = week();
        let mut starts: Vec<_> = expand(events.iter().map(|e| ((), e)), from, to)
            .into_iter()
            .map(|i| i.start)
            .collect();
        starts.sort_by_key(EventTime::timestamp);
        starts
    }

    #[test]
    fn single_events_are_kept_only_when_they_overlap_the_range() {
        let events = [
            event(
                json!({"id": "in", "start": {"dateTime": "2026-11-17T10:00:00Z"},
                         "end": {"dateTime": "2026-11-17T11:00:00Z"}}),
            ),
            event(
                json!({"id": "before", "start": {"dateTime": "2026-11-10T10:00:00Z"},
                         "end": {"dateTime": "2026-11-10T11:00:00Z"}}),
            ),
            event(
                json!({"id": "spans-in", "start": {"dateTime": "2026-11-15T22:00:00Z"},
                         "end": {"dateTime": "2026-11-16T02:00:00Z"}}),
            ),
        ];
        let (from, to) = week();
        let ids: Vec<_> = expand(events.iter().map(|e| ((), e)), from, to)
            .into_iter()
            .map(|i| i.event.id.as_str())
            .collect();
        assert_eq!(ids, ["in", "spans-in"]);
    }

    #[test]
    fn weekly_series_keeps_its_local_time_across_a_daylight_saving_change() {
        // Created in summer time (UTC+2); by November Prague is on UTC+1, so 9:00 local
        // must come out as 8:00 UTC, not 7:00.
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-06-02T09:00:00+02:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-06-02T09:30:00+02:00", "timeZone": "Europe/Prague"},
            "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=TU,TH"]
        }));
        assert_eq!(
            starts(&[master]),
            [
                EventTime::Timed(instant("2026-11-17T08:00:00Z")),
                EventTime::Timed(instant("2026-11-19T08:00:00Z")),
            ]
        );
    }

    #[test]
    fn changed_and_deleted_occurrences_replace_the_generated_ones() {
        let events = [
            event(json!({
                "id": "m",
                "start": {"dateTime": "2026-11-02T10:00:00Z", "timeZone": "UTC"},
                "end": {"dateTime": "2026-11-02T11:00:00Z", "timeZone": "UTC"},
                "recurrence": ["RRULE:FREQ=DAILY"]
            })),
            // Tuesday's occurrence was moved to the afternoon.
            event(json!({
                "id": "m_1", "recurringEventId": "m",
                "originalStartTime": {"dateTime": "2026-11-17T10:00:00Z"},
                "start": {"dateTime": "2026-11-17T15:00:00Z"},
                "end": {"dateTime": "2026-11-17T16:00:00Z"}
            })),
            // Wednesday's was deleted.
            event(json!({
                "id": "m_2", "recurringEventId": "m", "status": "cancelled",
                "originalStartTime": {"dateTime": "2026-11-18T10:00:00Z"}
            })),
        ];
        let found = starts(&events);
        assert_eq!(found.len(), 6);
        assert!(found.contains(&EventTime::Timed(instant("2026-11-17T15:00:00Z"))));
        assert!(!found.contains(&EventTime::Timed(instant("2026-11-17T10:00:00Z"))));
        assert!(!found.contains(&EventTime::Timed(instant("2026-11-18T10:00:00Z"))));
    }

    #[test]
    fn exceptions_only_affect_the_series_in_their_own_calendar() {
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-11-16T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-16T11:00:00Z", "timeZone": "UTC"},
            "recurrence": ["RRULE:FREQ=DAILY;COUNT=2"]
        }));
        let cancelled = event(json!({
            "id": "m_1", "recurringEventId": "m", "status": "cancelled",
            "originalStartTime": {"dateTime": "2026-11-16T10:00:00Z"}
        }));
        let (from, to) = week();
        let found = expand(
            [("a", &master), ("b", &master), ("b", &cancelled)],
            from,
            to,
        );
        let count = |key| found.iter().filter(|i| i.key == key).count();
        assert_eq!((count("a"), count("b")), (2, 1));
    }

    #[test]
    fn all_day_series_with_date_only_until_and_exdate() {
        let master = event(json!({
            "id": "m",
            "start": {"date": "2026-11-02"}, "end": {"date": "2026-11-03"},
            "recurrence": [
                "RRULE:FREQ=DAILY;UNTIL=20261119",
                "EXDATE;VALUE=DATE:20261117"
            ]
        }));
        // 15th and 23rd fall in the one-day slack; the 20th onwards is past UNTIL.
        assert_eq!(
            starts(&[master]),
            [
                EventTime::AllDay(date("2026-11-15")),
                EventTime::AllDay(date("2026-11-16")),
                EventTime::AllDay(date("2026-11-18")),
                EventTime::AllDay(date("2026-11-19")),
            ]
        );
    }

    #[test]
    fn timed_series_with_utc_until_and_zoned_exdate() {
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-11-16T09:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-16T10:00:00+01:00", "timeZone": "Europe/Prague"},
            "recurrence": [
                "RRULE:FREQ=DAILY;UNTIL=20261118T225959Z",
                "EXDATE;TZID=Europe/Prague:20261117T090000"
            ]
        }));
        assert_eq!(
            starts(&[master]),
            [
                EventTime::Timed(instant("2026-11-16T08:00:00Z")),
                EventTime::Timed(instant("2026-11-18T08:00:00Z")),
            ]
        );
    }

    #[test]
    fn a_daily_series_started_decades_ago_still_reaches_this_week() {
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2001-01-01T07:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2001-01-01T07:30:00Z", "timeZone": "UTC"},
            "recurrence": ["RRULE:FREQ=DAILY"]
        }));
        assert_eq!(starts(&[master]).len(), 7);
    }

    #[test]
    fn yearly_all_day_event_such_as_a_birthday() {
        let master = event(json!({
            "id": "m",
            "start": {"date": "1990-11-20"}, "end": {"date": "1990-11-21"},
            "recurrence": ["RRULE:FREQ=YEARLY"]
        }));
        assert_eq!(starts(&[master]), [EventTime::AllDay(date("2026-11-20"))]);
    }

    #[test]
    fn series_that_began_in_the_repeated_hour_of_a_clock_change_still_repeats() {
        // 02:30 happens twice in Prague on 25 October 2026; this is the first (UTC+2).
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-10-25T02:30:00+02:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-10-25T03:00:00+02:00", "timeZone": "Europe/Prague"},
            "recurrence": ["RRULE:FREQ=WEEKLY"]
        }));
        assert_eq!(
            starts(&[master]),
            [EventTime::Timed(instant("2026-11-22T01:30:00Z"))]
        );
    }

    #[test]
    fn exrule_removes_the_occurrences_it_matches() {
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-11-02T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-02T11:00:00Z", "timeZone": "UTC"},
            "recurrence": ["RRULE:FREQ=DAILY", "EXRULE:FREQ=WEEKLY;BYDAY=SA,SU"]
        }));
        assert_eq!(starts(&[master]).len(), 5);
    }

    #[test]
    fn series_made_only_of_extra_dates_keeps_its_first_occurrence() {
        let timed = event(json!({
            "id": "t",
            "start": {"dateTime": "2026-11-16T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-16T11:00:00Z", "timeZone": "UTC"},
            "recurrence": ["RDATE:20261118T100000Z"]
        }));
        assert_eq!(
            starts(&[timed]),
            [
                EventTime::Timed(instant("2026-11-16T10:00:00Z")),
                EventTime::Timed(instant("2026-11-18T10:00:00Z")),
            ]
        );
        let all_day = event(json!({
            "id": "a",
            "start": {"date": "2026-11-17"}, "end": {"date": "2026-11-18"},
            "recurrence": ["RDATE;VALUE=DATE:20261119"]
        }));
        assert_eq!(
            starts(&[all_day]),
            [
                EventTime::AllDay(date("2026-11-17")),
                EventTime::AllDay(date("2026-11-19")),
            ]
        );
    }

    #[test]
    fn an_excluded_first_occurrence_stays_excluded() {
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-11-16T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-16T11:00:00Z", "timeZone": "UTC"},
            "recurrence": ["RRULE:FREQ=DAILY;COUNT=3", "EXDATE:20261116T100000Z"]
        }));
        assert_eq!(
            starts(&[master]),
            [
                EventTime::Timed(instant("2026-11-17T10:00:00Z")),
                EventTime::Timed(instant("2026-11-18T10:00:00Z")),
            ]
        );
    }

    #[test]
    fn unparseable_rule_falls_back_to_the_first_occurrence() {
        let master = event(json!({
            "id": "m",
            "start": {"dateTime": "2026-11-17T10:00:00Z"},
            "end": {"dateTime": "2026-11-17T11:00:00Z"},
            "recurrence": ["RRULE:FREQ=SOMETIMES"]
        }));
        assert_eq!(starts(&[master]).len(), 1);
    }
}
