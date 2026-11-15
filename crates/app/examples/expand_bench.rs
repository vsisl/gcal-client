//! Measures what moving between weeks costs: expanding every repeating series of a cache
//! for one week after another (issue 2).
//!
//! usage: cargo run --release -p gcal-client --example expand_bench -- <cache.db> [copies]
//!
//! `copies` repeats the series that many times, to stand in for a larger account.

use std::path::Path;
use std::time::Instant;

use chrono::{Duration, NaiveDate};
use gcal_core::recurrence::{Series, day_start_utc};
use gcal_store::Store;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: expand_bench <cache.db> [copies]")?;
    let copies: usize = args.next().map(|n| n.parse()).transpose()?.unwrap_or(1);

    let store = Store::open(Path::new(&path))?;
    let stored = store.recurring_events()?;
    let events: Vec<_> = (0..copies)
        .flat_map(|copy| {
            stored
                .iter()
                .map(move |e| ((copy, e.account_id, e.calendar_id.as_str()), &e.event))
        })
        .collect();

    let started = Instant::now();
    let series = Series::new(events.iter().cloned());
    println!(
        "{} masters and exceptions, prepared in {:.1?}",
        events.len(),
        started.elapsed()
    );

    let first = NaiveDate::from_ymd_opt(2026, 11, 16).ok_or("date")?;
    for pass in ["nothing remembered", "first visit", "second visit"] {
        let mut worst = std::time::Duration::ZERO;
        let mut instances = 0;
        let started = Instant::now();
        const WEEKS: u32 = 52;
        for week in 0..WEEKS {
            let from = day_start_utc(first + Duration::weeks(i64::from(week)));
            let one = Instant::now();
            // What every step cost before the dates were remembered: parse and walk anew.
            let fresh;
            let series = if pass == "nothing remembered" {
                fresh = Series::new(events.iter().cloned());
                &fresh
            } else {
                &series
            };
            instances += series
                .expand(from, from + Duration::weeks(1), |_| true)
                .len();
            worst = worst.max(one.elapsed());
        }
        println!(
            "{pass}: {:.2?} per week on average, {worst:.2?} at worst, {instances} instances",
            started.elapsed() / WEEKS
        );
    }
    Ok(())
}
