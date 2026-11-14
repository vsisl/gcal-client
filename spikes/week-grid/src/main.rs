//! Throwaway spike: a week grid with ~300 events built from plain Slint elements.
//!
//! Run normally to try dragging by hand. Run with `SPIKE_BENCH=1` to let it scroll and
//! move events by itself for a few seconds and print how fast it re-lays-out and redraws.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{Color, ComponentHandle, Model, ModelRc, Timer, TimerMode, VecModel};

slint::include_modules!();

const EVENT_COUNT: usize = 300;
const PALETTE: [(u8, u8, u8); 6] = [
    (0x03, 0x9b, 0xe5),
    (0x33, 0xb6, 0x79),
    (0xf4, 0x51, 0x1e),
    (0x8e, 0x24, 0xaa),
    (0xf6, 0xbf, 0x26),
    (0x61, 0x61, 0x61),
];

/// Tiny deterministic generator so the spike needs no extra dependency.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self, bound: u32) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as u32) % bound
    }
}

fn generate() -> Vec<EventItem> {
    let mut rng = Lcg(42);
    (0..EVENT_COUNT)
        .map(|i| {
            let start = 15 * rng.next(88) as i32; // 0:00 .. 21:45
            let len = 15 * (1 + rng.next(8)) as i32; // 15 min .. 2 h
            let (r, g, b) = PALETTE[rng.next(PALETTE.len() as u32) as usize];
            EventItem {
                id: i as i32,
                title: format!("Event {i}").into(),
                day: rng.next(7) as i32,
                start_min: start,
                end_min: (start + len).min(24 * 60),
                col: 0,
                cols: 1,
                color: Color::from_rgb_u8(r, g, b),
            }
        })
        .collect()
}

/// Assigns side-by-side columns to overlapping events, per day, like the web week view:
/// events that transitively overlap form a group and share the day's width equally.
fn layout(events: &mut [EventItem]) {
    for day in 0..7 {
        let mut idx: Vec<usize> = (0..events.len()).filter(|&i| events[i].day == day).collect();
        idx.sort_by_key(|&i| (events[i].start_min, -events[i].end_min));
        let mut group: Vec<usize> = Vec::new();
        let mut col_ends: Vec<i32> = Vec::new();
        let mut group_end = i32::MIN;
        let finish = |events: &mut [EventItem], group: &[usize], cols: usize| {
            for &i in group {
                events[i].cols = cols as i32;
            }
        };
        for i in idx {
            if events[i].start_min >= group_end && !group.is_empty() {
                finish(events, &group, col_ends.len());
                group.clear();
                col_ends.clear();
            }
            let col = col_ends
                .iter()
                .position(|&end| end <= events[i].start_min)
                .unwrap_or_else(|| {
                    col_ends.push(0);
                    col_ends.len() - 1
                });
            col_ends[col] = events[i].end_min;
            events[i].col = col as i32;
            group_end = group_end.max(events[i].end_min);
            group.push(i);
        }
        finish(events, &group, col_ends.len());
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let events = Rc::new(RefCell::new(generate()));
    let model = Rc::new(VecModel::<EventItem>::default());
    ui.set_events(ModelRc::from(model.clone()));

    let refresh = {
        let events = events.clone();
        let model = model.clone();
        move || {
            let started = Instant::now();
            layout(&mut events.borrow_mut());
            model.set_vec(events.borrow().clone());
            started.elapsed()
        }
    };
    refresh();

    ui.on_event_changed({
        let events = events.clone();
        let refresh = refresh.clone();
        move |id, day, start, end| {
            if let Some(ev) = events.borrow_mut().iter_mut().find(|e| e.id == id) {
                ev.day = day;
                ev.start_min = start.clamp(0, 24 * 60 - 15);
                ev.end_min = end.clamp(ev.start_min + 15, 24 * 60);
            }
            refresh();
        }
    });
    ui.on_event_created({
        let events = events.clone();
        let refresh = refresh.clone();
        move |day, start, end| {
            let id = events.borrow().len() as i32;
            events.borrow_mut().push(EventItem {
                id,
                title: "(No title)".into(),
                day,
                start_min: start,
                end_min: end,
                col: 0,
                cols: 1,
                color: Color::from_rgb_u8(0x03, 0x9b, 0xe5),
            });
            refresh();
        }
    });

    // Self-driving benchmark: every tick scroll a little and move one event (forcing a
    // full re-layout and model reset, the worst case), then report and quit.
    let bench_timer = Timer::default();
    if std::env::var_os("SPIKE_BENCH").is_some() {
        let weak = ui.as_weak();
        let began = Instant::now();
        let mut ticks = 0u32;
        let mut relayout = Duration::ZERO;
        let mut worst_gap = Duration::ZERO;
        let mut last = Instant::now();
        bench_timer.start(TimerMode::Repeated, Duration::from_millis(1), move || {
            let Some(ui) = weak.upgrade() else { return };
            let now = Instant::now();
            if ticks > 10 {
                worst_gap = worst_gap.max(now - last);
            }
            last = now;
            ticks += 1;
            let phase = (ticks % 400) as f32 / 400.0;
            let tri = if phase < 0.5 { phase * 2.0 } else { 2.0 - phase * 2.0 };
            ui.set_scroll_y(-tri * 500.0);
            {
                let mut evs = events.borrow_mut();
                let n = evs.len();
                let ev = &mut evs[(ticks as usize / 20) % n];
                let len = ev.end_min - ev.start_min;
                ev.start_min = (ev.start_min + 15) % (24 * 60 - len);
                ev.end_min = ev.start_min + len;
            }
            relayout += refresh();
            if began.elapsed() >= Duration::from_secs(6) {
                let secs = began.elapsed().as_secs_f64();
                println!(
                    "bench: {} events, {ticks} updates in {secs:.1}s = {:.0} updates/s, \
                     avg layout+model reset {:.3} ms, worst gap between updates {:.1} ms",
                    model.row_count(),
                    ticks as f64 / secs,
                    relayout.as_secs_f64() * 1000.0 / ticks as f64,
                    worst_gap.as_secs_f64() * 1000.0,
                );
                slint::quit_event_loop().ok();
            }
        });
    }

    ui.run()
}
