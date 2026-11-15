//! Where each event instance goes in the month view: a grid of whole weeks.
//!
//! Follows the web interface: every event is one line high. All-day events and events
//! touching several days are bars across those days (cut at the week's edges); an event
//! within one day is a single line in that day's cell. A cell has room for only so many
//! lines; what does not fit is counted, to be offered as "N more".

use std::cmp::Reverse;

use chrono::{Duration, NaiveDate};
use chrono_tz::Tz;

use crate::model::EventTime;

const DAYS_PER_WEEK: i64 = 7;

/// One line in the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthChip {
    /// Index of the instance in the slice given to [`layout_month`].
    pub index: usize,
    /// 0 for the first week of the grid.
    pub week: i32,
    /// 0 for the first day of the week.
    pub first_day: i32,
    /// Inclusive.
    pub last_day: i32,
    /// Line within the cell, 0 at the top.
    pub row: i32,
    /// Has a clock time and stays within one day; drawn as plain text rather than a bar.
    pub within_day: bool,
    /// The event starts before / continues after this week.
    pub continues_before: bool,
    pub continues_after: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MonthLayout {
    pub chips: Vec<MonthChip>,
    /// For each day of the grid, how many events on it are not shown. Where this is not
    /// zero, the cell's last line is left free for the "N more" text.
    pub more: Vec<i32>,
}

/// Lays out instances given as `(start, end)` in the `weeks` weeks starting on
/// `grid_start`, with room for `capacity` lines in each day's cell.
pub fn layout_month(
    spans: &[(EventTime, EventTime)],
    grid_start: NaiveDate,
    weeks: i64,
    zone: Tz,
    capacity: i32,
) -> MonthLayout {
    struct Item {
        index: usize,
        /// Days from the start of the grid; `past_last` is exclusive.
        first: i64,
        past_last: i64,
        within_day: bool,
        /// Orders events within a day; only meaningful for timed ones.
        start: i64,
    }

    let items: Vec<Item> = spans
        .iter()
        .enumerate()
        .filter_map(|(index, &(start, end))| {
            let (first, last, timed, at) = match (start, end) {
                (EventTime::Timed(start), EventTime::Timed(end)) => {
                    let first = start.with_timezone(&zone).date_naive();
                    // An event ending exactly at midnight does not touch the following day.
                    let last = (end - Duration::seconds(1))
                        .with_timezone(&zone)
                        .date_naive();
                    (first, last.max(first), true, start.timestamp())
                }
                (EventTime::AllDay(first), EventTime::AllDay(past_last)) => {
                    (first, (past_last - Duration::days(1)).max(first), false, 0)
                }
                // Google never mixes the two kinds within one event.
                _ => return None,
            };
            Some(Item {
                index,
                first: (first - grid_start).num_days(),
                past_last: (last - grid_start).num_days() + 1,
                within_day: timed && first == last,
                start: at,
            })
        })
        .collect();

    let capacity = capacity.max(1);
    let mut layout = MonthLayout {
        chips: Vec::new(),
        more: vec![0; (weeks * DAYS_PER_WEEK) as usize],
    };

    for week in 0..weeks {
        let week_first = week * DAYS_PER_WEEK;
        let week_past_last = week_first + DAYS_PER_WEEK;
        let mut segments: Vec<(&Item, usize, usize)> = items
            .iter()
            .filter(|item| item.past_last > week_first && item.first < week_past_last)
            .map(|item| {
                let first = (item.first.max(week_first) - week_first) as usize;
                let last = (item.past_last.min(week_past_last) - 1 - week_first) as usize;
                (item, first, last)
            })
            .collect();
        // Bars before single lines, longer bars first, then by time: the web's order.
        segments.sort_by_key(|&(item, first, last)| {
            (
                first,
                item.within_day,
                Reverse(last - first),
                item.start,
                item.index,
            )
        });

        // Each segment takes the highest line that is free on all of its days.
        let mut taken: [Vec<bool>; DAYS_PER_WEEK as usize] = Default::default();
        let mut rows = Vec::with_capacity(segments.len());
        for &(_, first, last) in &segments {
            let mut row = 0;
            while taken[first..=last]
                .iter()
                .any(|day| day.get(row).copied().unwrap_or(false))
            {
                row += 1;
            }
            for day in &mut taken[first..=last] {
                if day.len() <= row {
                    day.resize(row + 1, false);
                }
                day[row] = true;
            }
            rows.push(row as i32);
        }

        // A day that hides anything gives up its last line to "N more", which may hide
        // more segments, on other days too; repeat until nothing changes.
        let mut limit = [capacity; DAYS_PER_WEEK as usize];
        let hidden = |limit: &[i32], segment: usize| {
            let (_, first, last) = segments[segment];
            limit[first..=last].iter().any(|l| rows[segment] >= *l)
        };
        loop {
            let mut changed = false;
            for segment in 0..segments.len() {
                if hidden(&limit, segment) {
                    let (_, first, last) = segments[segment];
                    for day in &mut limit[first..=last] {
                        if *day == capacity {
                            *day = capacity - 1;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }

        for (segment, &(item, first, last)) in segments.iter().enumerate() {
            if hidden(&limit, segment) {
                for day in first..=last {
                    layout.more[week_first as usize + day] += 1;
                }
            } else {
                layout.chips.push(MonthChip {
                    index: item.index,
                    week: week as i32,
                    first_day: first as i32,
                    last_day: last as i32,
                    row: rows[segment],
                    within_day: item.within_day,
                    continues_before: item.first < week_first,
                    continues_after: item.past_last > week_past_last,
                });
            }
        }
    }
    layout
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZONE: Tz = chrono_tz::Europe::Prague;

    fn date(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    fn timed(start: &str, end: &str) -> (EventTime, EventTime) {
        (
            EventTime::Timed(start.parse().unwrap()),
            EventTime::Timed(end.parse().unwrap()),
        )
    }

    fn all_day(first: &str, past_last: &str) -> (EventTime, EventTime) {
        (
            EventTime::AllDay(date(first)),
            EventTime::AllDay(date(past_last)),
        )
    }

    /// The grid for November 2026 with weeks starting on Monday begins on 26 October.
    fn november(spans: &[(EventTime, EventTime)], capacity: i32) -> MonthLayout {
        layout_month(spans, date("2026-10-26"), 6, ZONE, capacity)
    }

    #[test]
    fn an_event_within_one_day_is_a_line_in_that_cell() {
        // Tuesday 17 November, 9:00 to 10:00 in Prague.
        let layout = november(&[timed("2026-11-17T08:00:00Z", "2026-11-17T09:00:00Z")], 4);
        assert_eq!(
            layout.chips,
            [MonthChip {
                index: 0,
                week: 3,
                first_day: 1,
                last_day: 1,
                row: 0,
                within_day: true,
                continues_before: false,
                continues_after: false,
            }]
        );
        assert!(layout.more.iter().all(|count| *count == 0));
    }

    #[test]
    fn an_event_ending_at_midnight_stays_in_its_day() {
        // 22:00 to midnight, Prague time.
        let layout = november(&[timed("2026-11-17T21:00:00Z", "2026-11-17T23:00:00Z")], 4);
        assert_eq!(layout.chips.len(), 1);
        assert_eq!((layout.chips[0].first_day, layout.chips[0].last_day), (1, 1));
        assert!(layout.chips[0].within_day);
    }

    #[test]
    fn an_event_crossing_midnight_is_a_bar_over_both_days() {
        let layout = november(&[timed("2026-11-17T21:00:00Z", "2026-11-18T01:00:00Z")], 4);
        assert_eq!(layout.chips.len(), 1);
        assert_eq!((layout.chips[0].first_day, layout.chips[0].last_day), (1, 2));
        assert!(!layout.chips[0].within_day);
    }

    #[test]
    fn a_bar_is_cut_at_the_edges_of_weeks_and_of_the_grid() {
        // Saturday 14 November to Tuesday 17 November, and one that began before the grid.
        let layout = november(
            &[
                all_day("2026-11-14", "2026-11-18"),
                all_day("2026-10-20", "2026-10-28"),
            ],
            4,
        );
        let parts: Vec<_> = layout
            .chips
            .iter()
            .map(|c| {
                (
                    c.index,
                    c.week,
                    c.first_day,
                    c.last_day,
                    c.continues_before,
                    c.continues_after,
                )
            })
            .collect();
        assert_eq!(
            parts,
            [
                (1, 0, 0, 1, true, false),
                (0, 2, 5, 6, false, true),
                (0, 3, 0, 1, true, false),
            ]
        );
    }

    #[test]
    fn bars_come_first_and_lines_fill_the_gaps() {
        let layout = november(
            &[
                // Wednesday 9:00.
                timed("2026-11-18T08:00:00Z", "2026-11-18T09:00:00Z"),
                // Tuesday to Wednesday.
                all_day("2026-11-17", "2026-11-19"),
                // Wednesday 8:00, listed after the later one.
                timed("2026-11-18T07:00:00Z", "2026-11-18T08:00:00Z"),
                // Monday only: next to the bar, not below it.
                all_day("2026-11-16", "2026-11-17"),
            ],
            4,
        );
        let rows: Vec<_> = layout.chips.iter().map(|c| (c.index, c.row)).collect();
        assert_eq!(rows, [(3, 0), (1, 0), (2, 1), (0, 2)]);
    }

    #[test]
    fn what_does_not_fit_is_counted_and_the_last_line_is_left_free() {
        // Four events on Tuesday 17 November, room for three lines.
        let spans: Vec<_> = (8..12)
            .map(|hour| {
                timed(
                    &format!("2026-11-17T{hour:02}:00:00Z"),
                    &format!("2026-11-17T{hour:02}:30:00Z"),
                )
            })
            .collect();
        let layout = november(&spans, 3);
        let rows: Vec<_> = layout.chips.iter().map(|c| (c.index, c.row)).collect();
        assert_eq!(rows, [(0, 0), (1, 1)]);
        assert_eq!(layout.more[3 * 7 + 1], 2);
        assert_eq!(layout.more.iter().sum::<i32>(), 2);

        // Exactly as many as fit: nothing is hidden.
        let layout = november(&spans[..3], 3);
        assert_eq!(layout.chips.len(), 3);
        assert_eq!(layout.more.iter().sum::<i32>(), 0);
    }

    #[test]
    fn a_bar_hidden_on_one_day_is_counted_on_all_of_its_days() {
        // Tuesday is full; the bar from Tuesday to Wednesday lands on the third line.
        let layout = november(
            &[
                all_day("2026-11-17", "2026-11-18"),
                all_day("2026-11-17", "2026-11-18"),
                timed("2026-11-17T08:00:00Z", "2026-11-17T09:00:00Z"),
                all_day("2026-11-17", "2026-11-19"),
            ],
            2,
        );
        // The longest bar is placed first and stays; the rest of Tuesday is hidden.
        let shown: Vec<_> = layout.chips.iter().map(|c| c.index).collect();
        assert_eq!(shown, [3]);
        assert_eq!(layout.more[3 * 7 + 1], 3);
        assert_eq!(layout.more[3 * 7 + 2], 0);
    }
}
