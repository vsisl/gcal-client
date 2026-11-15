//! Where each event instance goes in a grid of consecutive days (the day, 4-day and week
//! views), in the viewer's time zone.
//!
//! Follows the web interface: events with a clock time are blocks in the day columns, split
//! at midnight, and overlapping ones share the column's width; all-day events and events
//! lasting a day or more are bars in the strip above the grid.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;

use crate::model::EventTime;

const MINUTES_PER_DAY: i64 = 24 * 60;
/// Shorter events are laid out as if they lasted this long, so their title stays readable.
const MIN_BLOCK_MINUTES: i64 = 15;

/// A block in a day column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedBlock {
    /// Index of the instance in the slice given to [`layout_days`].
    pub index: usize,
    /// 0 for the first day of the week.
    pub day: i32,
    /// Minutes after local midnight.
    pub start_min: i32,
    pub end_min: i32,
    /// Side-by-side position among overlapping blocks: column `col` of `cols`.
    pub col: i32,
    pub cols: i32,
}

/// A bar in the all-day strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllDayBar {
    pub index: usize,
    pub first_day: i32,
    /// Inclusive.
    pub last_day: i32,
    pub row: i32,
    /// The event starts before / continues after the visible week.
    pub continues_before: bool,
    pub continues_after: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WeekLayout {
    pub timed: Vec<TimedBlock>,
    pub all_day: Vec<AllDayBar>,
    /// Number of rows the all-day strip needs.
    pub all_day_rows: i32,
}

/// The instant a local calendar day begins in `zone`.
pub fn local_day_start(day: NaiveDate, zone: Tz) -> DateTime<Utc> {
    let midnight = day.and_time(NaiveTime::MIN);
    // Midnight does not exist on days when clocks jump forward at 00:00 (Havana, Cairo, the
    // Azores); the day then starts at the first time that does. Zones shift in steps of a
    // quarter of an hour or more.
    (0..=8)
        .find_map(|step| {
            zone.from_local_datetime(&(midnight + Duration::minutes(15 * step)))
                .earliest()
        })
        .unwrap_or_else(|| zone.from_utc_datetime(&midnight))
        .with_timezone(&Utc)
}

/// Lays out instances given as `(start, end)` for the `days` days starting on `week_start`.
pub fn layout_days(
    spans: &[(EventTime, EventTime)],
    week_start: NaiveDate,
    days: i64,
    zone: Tz,
) -> WeekLayout {
    let mut timed = Vec::new();
    let mut bars = Vec::new();

    for (index, &(start, end)) in spans.iter().enumerate() {
        // First and one-past-last local day covered, as offsets from the week's first day.
        let (first, past_last) = match (start, end) {
            (EventTime::Timed(start), EventTime::Timed(end))
                if end - start < Duration::hours(24) =>
            {
                split_timed(index, start, end, week_start, days, zone, &mut timed);
                continue;
            }
            (EventTime::Timed(start), EventTime::Timed(end)) => {
                let first = start.with_timezone(&zone).date_naive();
                // An event ending exactly at midnight does not touch the following day.
                let last = (end - Duration::seconds(1))
                    .with_timezone(&zone)
                    .date_naive();
                (first, last + Duration::days(1))
            }
            (EventTime::AllDay(first), EventTime::AllDay(past_last)) => {
                (first, past_last.max(first + Duration::days(1)))
            }
            // Google never mixes the two kinds within one event.
            _ => continue,
        };
        let first = (first - week_start).num_days();
        let past_last = (past_last - week_start).num_days();
        if past_last <= 0 || first >= days {
            continue;
        }
        bars.push(AllDayBar {
            index,
            first_day: first.max(0) as i32,
            last_day: (past_last.min(days) - 1) as i32,
            row: 0,
            continues_before: first < 0,
            continues_after: past_last > days,
        });
    }

    assign_columns(&mut timed);
    let all_day_rows = assign_rows(&mut bars);
    WeekLayout {
        timed,
        all_day: bars,
        all_day_rows,
    }
}

/// Adds one block per local day the event touches.
fn split_timed(
    index: usize,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    week_start: NaiveDate,
    days: i64,
    zone: Tz,
    out: &mut Vec<TimedBlock>,
) {
    let local_start = start.with_timezone(&zone).naive_local();
    let local_end = end.with_timezone(&zone).naive_local();
    let mut day = local_start.date();
    while day <= local_end.date() {
        let offset = (day - week_start).num_days();
        let day_begin = day.and_time(NaiveTime::MIN);
        let start_min = (local_start - day_begin).num_minutes().max(0);
        let end_min = (local_end - day_begin).num_minutes().min(MINUTES_PER_DAY);
        // The part after midnight of an event ending exactly at midnight is empty.
        let is_empty_tail = day > local_start.date() && end_min <= 0;
        if (0..days).contains(&offset) && !is_empty_tail {
            let start_min = start_min.min(MINUTES_PER_DAY - MIN_BLOCK_MINUTES);
            out.push(TimedBlock {
                index,
                day: offset as i32,
                start_min: start_min as i32,
                end_min: end_min.max(start_min + MIN_BLOCK_MINUTES) as i32,
                col: 0,
                cols: 1,
            });
        }
        day += Duration::days(1);
    }
}

/// Assigns side-by-side columns per day: blocks that overlap, directly or through others,
/// form a group whose members share the day's width equally.
fn assign_columns(blocks: &mut [TimedBlock]) {
    blocks.sort_by_key(|b| (b.day, b.start_min, -b.end_min, b.index));
    let mut group_start = 0;
    let mut group_end = i32::MIN;
    let mut column_ends: Vec<i32> = Vec::new();
    for i in 0..blocks.len() {
        let new_group =
            i > 0 && (blocks[i].day != blocks[i - 1].day || blocks[i].start_min >= group_end);
        if new_group {
            close_group(&mut blocks[group_start..i], column_ends.len());
            group_start = i;
            group_end = i32::MIN;
            column_ends.clear();
        }
        let start = blocks[i].start_min;
        let col = match column_ends.iter().position(|&end| end <= start) {
            Some(col) => col,
            None => {
                column_ends.push(0);
                column_ends.len() - 1
            }
        };
        column_ends[col] = blocks[i].end_min;
        blocks[i].col = col as i32;
        group_end = group_end.max(blocks[i].end_min);
    }
    close_group(&mut blocks[group_start..], column_ends.len());
}

fn close_group(group: &mut [TimedBlock], cols: usize) {
    for block in group {
        block.cols = cols as i32;
    }
}

/// Stacks bars into as few rows as possible, earlier and longer ones first; returns the
/// number of rows used.
fn assign_rows(bars: &mut [AllDayBar]) -> i32 {
    bars.sort_by_key(|b| (b.first_day, -b.last_day, b.index));
    // Per row, the last day already occupied.
    let mut row_ends: Vec<i32> = Vec::new();
    for bar in bars.iter_mut() {
        let row = match row_ends.iter().position(|&end| end < bar.first_day) {
            Some(row) => row,
            None => {
                row_ends.push(0);
                row_ends.len() - 1
            }
        };
        row_ends[row] = bar.last_day;
        bar.row = row as i32;
    }
    row_ends.len() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRAGUE: Tz = chrono_tz::Europe::Prague;

    fn monday() -> NaiveDate {
        "2026-11-16".parse().unwrap()
    }

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

    #[test]
    fn timed_event_lands_on_its_local_day_and_minutes() {
        // 23:30 UTC on Monday is 00:30 on Tuesday in Prague (UTC+1 in November).
        let layout = layout_days(
            &[timed("2026-11-16T23:30:00Z", "2026-11-17T00:30:00Z")],
            monday(),
            7,
            PRAGUE,
        );
        assert_eq!(
            layout.timed,
            [TimedBlock {
                index: 0,
                day: 1,
                start_min: 30,
                end_min: 90,
                col: 0,
                cols: 1
            }]
        );
    }

    #[test]
    fn event_across_midnight_is_split_and_one_ending_at_midnight_is_not() {
        let layout = layout_days(
            &[
                timed("2026-11-17T21:00:00Z", "2026-11-18T01:00:00Z"), // 22:00-02:00 local
                timed("2026-11-19T21:00:00Z", "2026-11-19T23:00:00Z"), // 22:00-24:00 local
            ],
            monday(),
            7,
            PRAGUE,
        );
        let parts: Vec<_> = layout
            .timed
            .iter()
            .map(|b| (b.index, b.day, b.start_min, b.end_min))
            .collect();
        assert_eq!(
            parts,
            [(0, 1, 1320, 1440), (0, 2, 0, 120), (1, 3, 1320, 1440)]
        );
    }

    #[test]
    fn parts_outside_the_week_are_dropped() {
        // Sunday 23:00 to Monday 01:00 local: only Monday's part is in the week.
        let layout = layout_days(
            &[timed("2026-11-15T22:00:00Z", "2026-11-16T00:00:00Z")],
            monday(),
            7,
            PRAGUE,
        );
        assert_eq!(layout.timed.len(), 1);
        assert_eq!(
            (
                layout.timed[0].day,
                layout.timed[0].start_min,
                layout.timed[0].end_min
            ),
            (0, 0, 60)
        );
    }

    #[test]
    fn overlapping_events_share_the_width_and_later_ones_reuse_columns() {
        let layout = layout_days(
            &[
                timed("2026-11-16T09:00:00Z", "2026-11-16T11:00:00Z"),
                timed("2026-11-16T10:00:00Z", "2026-11-16T12:00:00Z"),
                timed("2026-11-16T11:00:00Z", "2026-11-16T12:00:00Z"),
                timed("2026-11-16T14:00:00Z", "2026-11-16T15:00:00Z"),
            ],
            monday(),
            7,
            PRAGUE,
        );
        let columns: Vec<_> = layout
            .timed
            .iter()
            .map(|b| (b.index, b.col, b.cols))
            .collect();
        // The third starts when the first ends, so it takes the first's column; the fourth
        // overlaps nothing and gets the full width.
        assert_eq!(columns, [(0, 0, 2), (1, 1, 2), (2, 0, 2), (3, 0, 1)]);
    }

    #[test]
    fn very_short_events_get_a_minimum_height() {
        let layout = layout_days(
            &[timed("2026-11-16T09:00:00Z", "2026-11-16T09:00:00Z")],
            monday(),
            7,
            PRAGUE,
        );
        assert_eq!(layout.timed[0].end_min - layout.timed[0].start_min, 15);
    }

    #[test]
    fn all_day_events_become_bars_clipped_to_the_week() {
        let layout = layout_days(
            &[
                all_day("2026-11-17", "2026-11-18"),
                all_day("2026-11-14", "2026-11-18"),
                all_day("2026-11-21", "2026-11-25"),
                all_day("2026-11-09", "2026-11-16"), // ends before the week
            ],
            monday(),
            7,
            PRAGUE,
        );
        let bars: Vec<_> = layout
            .all_day
            .iter()
            .map(|b| {
                (
                    b.index,
                    b.first_day,
                    b.last_day,
                    b.row,
                    b.continues_before,
                    b.continues_after,
                )
            })
            .collect();
        assert_eq!(
            bars,
            [
                (1, 0, 1, 0, true, false),
                (0, 1, 1, 1, false, false),
                (2, 5, 6, 0, false, true),
            ]
        );
        assert_eq!(layout.all_day_rows, 2);
    }

    #[test]
    fn timed_event_of_a_day_or_longer_goes_to_the_all_day_strip() {
        // Tuesday 10:00 to Thursday 10:00 local.
        let layout = layout_days(
            &[timed("2026-11-17T09:00:00Z", "2026-11-19T09:00:00Z")],
            monday(),
            7,
            PRAGUE,
        );
        assert!(layout.timed.is_empty());
        assert_eq!(
            (layout.all_day[0].first_day, layout.all_day[0].last_day),
            (1, 3)
        );
    }

    #[test]
    fn day_starts_after_the_gap_when_clocks_jump_forward_at_midnight() {
        // Havana, 8 March 2026: 00:00 becomes 01:00 (UTC-4), which is 05:00 UTC.
        let day: NaiveDate = "2026-03-08".parse().unwrap();
        assert_eq!(
            local_day_start(day, chrono_tz::America::Havana),
            "2026-03-08T05:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }

    #[test]
    fn local_day_start_follows_the_zone() {
        assert_eq!(
            local_day_start(monday(), PRAGUE),
            "2026-11-15T23:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }
}
