//! What the schedule view lists: the events of each day, one below the other.
//!
//! Follows the web interface: an event touching several days is listed on each of them,
//! all-day entries come before timed ones, and days without events are left out by the
//! caller simply because they have no entries.

use chrono::{Duration, NaiveDate, NaiveTime};
use chrono_tz::Tz;

use crate::model::EventTime;

/// Which part of an event falls on the day it is listed under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DayPart {
    /// An all-day event, or a middle day of a longer timed one.
    AllDay,
    /// The last day of a timed event that started on an earlier day.
    Ends,
    /// A timed event that starts and ends on this day.
    Whole,
    /// The first day of a timed event that ends on a later day.
    Starts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleEntry {
    /// Index of the instance in the slice given to [`schedule`].
    pub index: usize,
    /// 0 for the first day listed.
    pub day: i32,
    pub part: DayPart,
}

/// Lists instances given as `(start, end)` under each of the `days` days from `first_day`
/// they touch, in the order they are shown.
pub fn schedule(
    spans: &[(EventTime, EventTime)],
    first_day: NaiveDate,
    days: i64,
    zone: Tz,
) -> Vec<ScheduleEntry> {
    // (day, all-day entries first, time of day it starts at, index) orders the list.
    let mut keyed = Vec::new();
    for (index, &(start, end)) in spans.iter().enumerate() {
        let (first, last, starts_at, ends_at_midnight) = match (start, end) {
            (EventTime::Timed(start), EventTime::Timed(end)) => {
                let local = start.with_timezone(&zone);
                // An event ending exactly at midnight does not touch the following day.
                let last = (end - Duration::seconds(1))
                    .with_timezone(&zone)
                    .date_naive();
                let first = local.date_naive();
                let ends_at_midnight = end.with_timezone(&zone).time() == NaiveTime::MIN;
                (first, last.max(first), Some(local.time()), ends_at_midnight)
            }
            (EventTime::AllDay(first), EventTime::AllDay(past_last)) => (
                first,
                (past_last - Duration::days(1)).max(first),
                None,
                false,
            ),
            // Google never mixes the two kinds within one event.
            _ => continue,
        };
        let from = (first - first_day).num_days();
        let to = (last - first_day).num_days();
        for day in from.max(0)..=to.min(days - 1) {
            let part = match starts_at {
                None => DayPart::AllDay,
                Some(_) if from == to => DayPart::Whole,
                Some(_) if day == from => DayPart::Starts,
                // Covered to its very end: "until 0:00" would name the next day's midnight.
                Some(_) if day == to && ends_at_midnight => DayPart::AllDay,
                Some(_) if day == to => DayPart::Ends,
                Some(_) => DayPart::AllDay,
            };
            let time = match part {
                DayPart::Whole | DayPart::Starts => starts_at,
                _ => None,
            };
            keyed.push((day, time, part, index));
        }
    }
    // `None` sorts before any time, so all-day and continuing entries come first.
    keyed.sort();
    keyed
        .into_iter()
        .map(|(day, _, part, index)| ScheduleEntry {
            index,
            day: day as i32,
            part,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZONE: Tz = chrono_tz::Europe::Prague;

    fn timed(start: &str, end: &str) -> (EventTime, EventTime) {
        (
            EventTime::Timed(start.parse().unwrap()),
            EventTime::Timed(end.parse().unwrap()),
        )
    }

    fn all_day(first: &str, past_last: &str) -> (EventTime, EventTime) {
        (
            EventTime::AllDay(first.parse().unwrap()),
            EventTime::AllDay(past_last.parse().unwrap()),
        )
    }

    fn listed(spans: &[(EventTime, EventTime)]) -> Vec<(i32, usize, DayPart)> {
        schedule(spans, "2026-11-16".parse().unwrap(), 7, ZONE)
            .into_iter()
            .map(|e| (e.day, e.index, e.part))
            .collect()
    }

    #[test]
    fn a_day_lists_all_day_events_first_and_then_by_start_time() {
        let entries = listed(&[
            timed("2026-11-17T13:00:00Z", "2026-11-17T14:00:00Z"),
            timed("2026-11-17T08:00:00Z", "2026-11-17T09:00:00Z"),
            all_day("2026-11-17", "2026-11-18"),
            timed("2026-11-16T08:00:00Z", "2026-11-16T09:00:00Z"),
        ]);
        assert_eq!(
            entries,
            [
                (0, 3, DayPart::Whole),
                (1, 2, DayPart::AllDay),
                (1, 1, DayPart::Whole),
                (1, 0, DayPart::Whole),
            ]
        );
    }

    #[test]
    fn a_long_event_is_listed_on_each_day_it_touches() {
        // Tuesday 22:00 to Thursday 10:00, Prague time.
        let entries = listed(&[timed("2026-11-17T21:00:00Z", "2026-11-19T09:00:00Z")]);
        assert_eq!(
            entries,
            [
                (1, 0, DayPart::Starts),
                (2, 0, DayPart::AllDay),
                (3, 0, DayPart::Ends),
            ]
        );
    }

    #[test]
    fn a_last_day_covered_up_to_midnight_is_a_whole_day() {
        // Monday 22:00 to the midnight that ends Tuesday, Prague time.
        let entries = listed(&[timed("2026-11-16T21:00:00Z", "2026-11-17T23:00:00Z")]);
        assert_eq!(entries, [(0, 0, DayPart::Starts), (1, 0, DayPart::AllDay)]);
    }

    #[test]
    fn only_the_days_asked_for_are_listed() {
        let entries = listed(&[
            // Began the week before, ends on Tuesday.
            all_day("2026-11-10", "2026-11-18"),
            // Ends at midnight before the first day: not listed at all.
            timed("2026-11-15T20:00:00Z", "2026-11-15T23:00:00Z"),
            // Sunday into the next week.
            all_day("2026-11-22", "2026-11-25"),
        ]);
        assert_eq!(
            entries,
            [
                (0, 0, DayPart::AllDay),
                (1, 0, DayPart::AllDay),
                (6, 2, DayPart::AllDay),
            ]
        );
    }
}
