//! Turns stored events into the concrete instances that fall in a date range.
//!
//! Google stores a repeating event as one master carrying RRULE/EXDATE/RDATE lines, plus a
//! separate event for every occurrence that was changed or deleted. Expanding a series means
//! generating the master's occurrences and replacing those that have such an exception.

use std::borrow::Borrow;
use std::cell::RefCell;
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

/// Upper bound on the occurrences of one series that are worked out and remembered; far
/// above any real calendar (a daily event reaches it after 179 years).
const MAX_OCCURRENCES: u16 = u16::MAX;
/// How far past the range asked for an endless series is worked out, so that moving on
/// through the calendar rarely has to do it again.
const LOOKAHEAD_DAYS: i64 = 400;

/// The repeating events of a cache, prepared for being asked again and again which
/// occurrences fall in a range.
///
/// Moving through the calendar asks once per step. Parsing every rule and walking every
/// series from its first date each time made that visibly slow, so the rules are parsed
/// once here and each series remembers the dates it has worked out.
pub struct Series<K, E> {
    entries: Vec<Entry<K, E>>,
}

struct Entry<K, E> {
    key: K,
    event: E,
    start: EventTime,
    end: EventTime,
    /// `None` for a changed occurrence, which is an ordinary event at its new time.
    master: Option<Master>,
}

struct Master {
    /// `None` when the rule cannot be parsed: the series is then its first occurrence alone,
    /// so the event stays visible instead of disappearing.
    set: Option<RRuleSet>,
    /// Whether every rule ends by itself (UNTIL or COUNT), so all dates can be listed.
    finite: bool,
    /// Original start times of the occurrences that were changed or deleted.
    replaced: HashSet<EventTime>,
    known: RefCell<Known>,
}

/// The occurrence starts of a series worked out so far: all of them, in order, up to
/// `through`.
#[derive(Default)]
struct Known {
    dates: Vec<DateTime<Utc>>,
    through: Option<DateTime<Utc>>,
    /// There are more occurrences than are remembered; ranges are then computed directly.
    too_many: bool,
}

impl<K, E> Default for Series<K, E> {
    fn default() -> Self {
        Series {
            entries: Vec::new(),
        }
    }
}

impl<K, E> Series<K, E>
where
    K: Clone + Eq + Hash,
    E: Borrow<Event>,
{
    /// `events` must hold every master and every changed or deleted occurrence. Other
    /// events may be among them and are treated as occurring once.
    pub fn new(events: impl IntoIterator<Item = (K, E)>) -> Self {
        let events: Vec<(K, E)> = events.into_iter().collect();

        // Original start times of occurrences that were changed or deleted, per series.
        let mut replaced: HashMap<(K, String), HashSet<EventTime>> = HashMap::new();
        for (key, event) in &events {
            let event = event.borrow();
            if let (Some(master), Some(original)) = (
                event.recurring_event_id.as_deref(),
                event.original_start_time.as_ref().and_then(|t| t.time()),
            ) {
                replaced
                    .entry((key.clone(), master.to_owned()))
                    .or_default()
                    .insert(original);
            }
        }

        let entries = events
            .into_iter()
            .filter_map(|(key, owner)| {
                let event = owner.borrow();
                if event.status == EventStatus::Cancelled {
                    return None;
                }
                let (start, end) = (event.start_time()?, event.end_time()?);
                let master = event.is_recurring_master().then(|| {
                    let set = rule_set(event, start);
                    let finite = set.as_ref().is_some_and(|set| {
                        set.get_rrule()
                            .iter()
                            .all(|rule| rule.get_until().is_some() || rule.get_count().is_some())
                    });
                    Master {
                        set,
                        finite,
                        replaced: replaced
                            .remove(&(key.clone(), event.id.clone()))
                            .unwrap_or_default(),
                        known: RefCell::default(),
                    }
                });
                Some(Entry {
                    key,
                    event: owner,
                    start,
                    end,
                    master,
                })
            })
            .collect();
        Series { entries }
    }

    /// The instances that may overlap `from..to`, of the events whose key `keep` accepts.
    ///
    /// All-day instances are kept with a day of slack on either side, since which days they
    /// cover depends on the viewer's time zone.
    pub fn expand(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        keep: impl Fn(&K) -> bool,
    ) -> Vec<Instance<'_, K>> {
        let mut instances = Vec::new();
        for entry in self.entries.iter().filter(|entry| keep(&entry.key)) {
            let event = entry.event.borrow();
            match &entry.master {
                Some(master) => {
                    for (start, end) in master.occurrences(entry.start, entry.end, from, to) {
                        if !master.replaced.contains(&start) {
                            instances.push(Instance {
                                key: entry.key.clone(),
                                event,
                                start,
                                end,
                            });
                        }
                    }
                }
                None if overlaps(entry.start, entry.end, from, to) => instances.push(Instance {
                    key: entry.key.clone(),
                    event,
                    start: entry.start,
                    end: entry.end,
                }),
                None => {}
            }
        }
        instances
    }
}

/// An event that does not repeat, as an instance if it may overlap `from..to`.
pub fn single_instance<K>(
    key: K,
    event: &Event,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Option<Instance<'_, K>> {
    if event.status == EventStatus::Cancelled {
        return None;
    }
    let (start, end) = (event.start_time()?, event.end_time()?);
    overlaps(start, end, from, to).then_some(Instance {
        key,
        event,
        start,
        end,
    })
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

impl Master {
    /// Start and end of each occurrence that may overlap `from..to`, for a series whose
    /// first occurrence is `start..end`.
    fn occurrences(
        &self,
        start: EventTime,
        end: EventTime,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Vec<(EventTime, EventTime)> {
        let Some(set) = &self.set else {
            return if overlaps(start, end, from, to) {
                vec![(start, end)]
            } else {
                Vec::new()
            };
        };
        let length = Duration::seconds((end.timestamp() - start.timestamp()).max(0));
        let all_day = matches!(start, EventTime::AllDay(_));
        let slack = if all_day {
            Duration::days(1)
        } else {
            Duration::zero()
        };
        let window_start = from - length - slack;
        let window_end = to + slack;

        let mut known = self.known.borrow_mut();
        if !known.too_many && known.through.is_none_or(|through| through < window_end) {
            // Work out the series from its beginning again, further than asked for. The
            // library walks from the first date whatever range it is given, so doing it
            // rarely is what saves the time.
            let through = if self.finite {
                DateTime::<Utc>::MAX_UTC
            } else {
                window_end + Duration::days(LOOKAHEAD_DAYS)
            };
            let result = if self.finite {
                set.clone().all(MAX_OCCURRENCES)
            } else {
                set.clone()
                    .before(through.with_timezone(&rrule::Tz::UTC))
                    .all(MAX_OCCURRENCES)
            };
            if result.limited {
                *known = Known {
                    too_many: true,
                    ..Known::default()
                };
            } else {
                let mut dates: Vec<_> = result
                    .dates
                    .into_iter()
                    .map(|date| date.with_timezone(&Utc))
                    .collect();
                // The start is in the set twice when the rules produce it as well (see
                // `rule_set`).
                dates.sort();
                dates.dedup();
                *known = Known {
                    dates,
                    through: Some(through),
                    too_many: false,
                };
            }
        }

        let direct;
        let dates: &[DateTime<Utc>] = if known.too_many {
            let utc = rrule::Tz::UTC;
            let result = set
                .clone()
                .after(window_start.with_timezone(&utc))
                .before(window_end.with_timezone(&utc))
                .all(MAX_OCCURRENCES);
            let mut dates: Vec<_> = result
                .dates
                .into_iter()
                .map(|date| date.with_timezone(&Utc))
                .collect();
            dates.sort();
            dates.dedup();
            direct = dates;
            &direct
        } else {
            let first = known.dates.partition_point(|date| *date < window_start);
            let past_last = known.dates.partition_point(|date| *date <= window_end);
            &known.dates[first..past_last]
        };

        dates
            .iter()
            .map(|&occurrence| {
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
        let mut starts: Vec<_> = Series::new(events.iter().map(|e| ((), e)))
            .expand(from, to, |()| true)
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
        let series = Series::new(events.iter().map(|e| ((), e)));
        let ids: Vec<_> = series
            .expand(from, to, |()| true)
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
        let series = Series::new([("a", &master), ("b", &master), ("b", &cancelled)]);
        let found = series.expand(from, to, |_| true);
        let count = |key| found.iter().filter(|i| i.key == key).count();
        assert_eq!((count("a"), count("b")), (2, 1));
    }

    #[test]
    fn asking_again_in_any_order_gives_the_same_answers() {
        // An endless and an ending series; the remembered dates must serve ranges far
        // ahead of, and then again before, the ones asked for earlier.
        let events = [
            event(json!({
                "id": "endless",
                "start": {"dateTime": "2020-01-06T10:00:00Z"},
                "end": {"dateTime": "2020-01-06T11:00:00Z"},
                "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO,TH"]
            })),
            event(json!({
                "id": "ending",
                "start": {"date": "2026-01-01"}, "end": {"date": "2026-01-02"},
                "recurrence": ["RRULE:FREQ=DAILY;UNTIL=20290101"]
            })),
        ];
        let weeks = [
            "2026-11-16",
            "2029-06-04",
            "2021-03-01",
            "2026-11-16",
            "2028-12-25",
        ];
        let series = Series::new(events.iter().map(|e| ((), e)));
        for monday in weeks {
            let (from, to) = (
                day_start_utc(date(monday)),
                day_start_utc(date(monday) + Duration::days(7)),
            );
            let fresh = Series::new(events.iter().map(|e| ((), e)));
            let expected = fresh.expand(from, to, |()| true);
            assert_eq!(series.expand(from, to, |()| true), expected, "{monday}");
            let endless = expected.iter().filter(|i| i.event.id == "endless").count();
            assert_eq!(endless, 2, "{monday}");
        }
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
