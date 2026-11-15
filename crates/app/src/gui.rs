//! The window: turns the cache into what the week view shows and wires up its controls.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use gcal_core::month::{MonthLayout, layout_month};
use gcal_core::recurrence::{Series, end_series_before, single_instance};
use gcal_core::schedule::{DayPart, schedule};
use gcal_core::text::plain_text;
use gcal_core::week::{WeekLayout, layout_days, local_day_start};
use gcal_core::{CalendarListEntry, Event, EventTime};
use gcal_store::{Account, AccountId, Store, StoredCalendar};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::open_store;
use crate::worker::{self, Command, Deletion};
use tokio::sync::mpsc::UnboundedSender;

slint::include_modules!();

/// How often changes are fetched from Google while the window is open. (Google can only
/// push changes to a public web server, so a desktop app has to ask.)
const SYNC_INTERVAL: StdDuration = StdDuration::from_secs(5 * 60);
/// The setting that remembers which calendar the previous new event went to.
const NEW_EVENT_CALENDAR: &str = "new_event_calendar";
/// Setting: light or dark, as the number the window uses (0 as the desktop, 1 light,
/// 2 dark).
const THEME: &str = "theme";
/// How long "Undo" stays on offer after a deletion.
const UNDO_PERIOD: StdDuration = StdDuration::from_secs(10);
/// The sidebar's small month always shows six weeks.
const MINI_MONTH_DAYS: i64 = 42;
/// How many days ahead the schedule view lists.
const SCHEDULE_DAYS: i64 = 30;
/// Hour at the top of the grid when the window opens, like the web interface.
const FIRST_VISIBLE_HOUR: i32 = 7;
const FALLBACK_COLOR: Color = Color::from_rgb_u8(0x03, 0x9b, 0xe5);

/// Google's event colours, by the `colorId` an event may carry.
const EVENT_COLORS: [(&str, u32); 11] = [
    ("1", 0x7986cb),  // Lavender
    ("2", 0x33b679),  // Sage
    ("3", 0x8e24aa),  // Grape
    ("4", 0xe67c73),  // Flamingo
    ("5", 0xf6bf26),  // Banana
    ("6", 0xf4511e),  // Tangerine
    ("7", 0x039be5),  // Peacock
    ("8", 0x616161),  // Graphite
    ("9", 0x3f51b5),  // Blueberry
    ("10", 0x0b8043), // Basil
    ("11", 0xd50000), // Tomato
];

fn rgb(value: u32) -> Color {
    Color::from_rgb_u8((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// Parses the `#rrggbb` form Google uses for calendar colours.
fn parse_color(text: &str) -> Option<Color> {
    let hex = text.strip_prefix('#')?;
    (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
        .map(rgb)
}

/// The colour an event is drawn in: its own if it has one, otherwise its calendar's.
fn event_color(event: &Event, calendar: &CalendarListEntry) -> Color {
    event
        .color_id
        .as_deref()
        .and_then(|id| EVENT_COLORS.iter().find(|(known, _)| *known == id))
        .map(|(_, value)| rgb(*value))
        .or_else(|| calendar.background_color.as_deref().and_then(parse_color))
        .unwrap_or(FALLBACK_COLOR)
}

/// Dark text on light backgrounds, white on dark ones.
fn text_color_on(background: Color) -> Color {
    let luminance = 0.299 * f32::from(background.red())
        + 0.587 * f32::from(background.green())
        + 0.114 * f32::from(background.blue());
    if luminance > 160.0 {
        rgb(0x202124)
    } else {
        rgb(0xffffff)
    }
}

/// The computer's time zone, or UTC when it cannot be determined.
fn local_zone() -> Tz {
    iana_time_zone::get_timezone()
        .ok()
        .and_then(|name| name.parse().ok())
        .unwrap_or(chrono_tz::UTC)
}

/// The Monday on or before `day`. (The first day of the week becomes a setting in M2.)
fn week_start_of(day: NaiveDate) -> NaiveDate {
    day - Duration::days(i64::from(day.weekday().num_days_from_monday()))
}

/// Which stretch of days the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Span {
    Day,
    Week,
    Month,
    FourDays,
    /// A list of the coming days' events.
    Schedule,
}

impl Span {
    /// The order used by the interface's view switcher and the number keys, as on the web.
    const ALL: [Span; 5] = [
        Span::Day,
        Span::Week,
        Span::Month,
        Span::FourDays,
        Span::Schedule,
    ];

    /// The first day shown and the number of days, when the user is looking at `anchor`.
    /// The week view snaps to the start of the week, the month view shows the whole weeks
    /// that cover the month, and the others begin on the day itself.
    fn range(self, anchor: NaiveDate) -> (NaiveDate, i64) {
        match self {
            Span::Day => (anchor, 1),
            Span::FourDays => (anchor, 4),
            Span::Schedule => (anchor, SCHEDULE_DAYS),
            Span::Week => (week_start_of(anchor), 7),
            Span::Month => {
                let first = week_start_of(anchor.with_day(1).unwrap_or(anchor));
                let next_month = (anchor.with_day(1).unwrap_or(anchor)) + Months::new(1);
                let days = (next_month - first).num_days();
                (first, (days + 6) / 7 * 7)
            }
        }
    }

    /// The day to look at after moving one screenful back (-1) or forward (1).
    fn step(self, anchor: NaiveDate, direction: i64) -> NaiveDate {
        match self {
            // Lands on the same day of the neighbouring month, or its last day.
            Span::Month if direction < 0 => anchor - Months::new(1),
            Span::Month => anchor + Months::new(1),
            _ => anchor + Duration::days(direction * self.range(anchor).1),
        }
    }

    /// The text next to the navigation buttons.
    fn title(self, anchor: NaiveDate) -> String {
        match self {
            Span::Month => anchor.format("%B %Y").to_string(),
            _ => {
                let (first, days) = self.range(anchor);
                period_title(first, days)
            }
        }
    }
}

/// "November 2026", or "Nov – Dec 2026" / "Dec 2026 – Jan 2027" when the days straddle.
fn period_title(first: NaiveDate, days: i64) -> String {
    let last = first + Duration::days(days - 1);
    if first.month() == last.month() {
        first.format("%B %Y").to_string()
    } else if first.year() == last.year() {
        format!("{} – {}", first.format("%b"), last.format("%b %Y"))
    } else {
        format!("{} – {}", first.format("%b %Y"), last.format("%b %Y"))
    }
}

/// When an event happens, as shown in its details: "Tuesday, 17 November · 9:00 – 12:00".
fn when_text(start: EventTime, end: EventTime, zone: Tz) -> String {
    const DAY: &str = "%A, %-d %B";
    const DATE: &str = "%-d %B %Y";
    match (start, end) {
        (EventTime::Timed(start), EventTime::Timed(end)) => {
            let (first, last) = (start.with_timezone(&zone), end.with_timezone(&zone));
            if first.date_naive() == last.date_naive() {
                format!(
                    "{} · {} – {}",
                    first.format(DAY),
                    clock(start, zone),
                    clock(end, zone)
                )
            } else {
                format!(
                    "{}, {} – {}, {}",
                    first.format(DATE),
                    clock(start, zone),
                    last.format(DATE),
                    clock(end, zone)
                )
            }
        }
        (EventTime::AllDay(first), EventTime::AllDay(past_last)) => {
            // The end of an all-day event is the day after its last day.
            let last = past_last - Duration::days(1);
            if last <= first {
                first.format(DAY).to_string()
            } else {
                format!("{} – {}", first.format(DATE), last.format(DATE))
            }
        }
        // Google never mixes the two kinds within one event.
        _ => String::new(),
    }
}

/// The event's page in Google's web interface, opened for the account that owns it (the
/// browser may be signed in to several).
fn web_link(event: &Event, account_email: &str) -> String {
    match &event.html_link {
        Some(link) => {
            let separator = if link.contains('?') { '&' } else { '?' };
            format!("{link}{separator}authuser={account_email}")
        }
        None => String::new(),
    }
}

fn clock(instant: DateTime<Utc>, zone: Tz) -> String {
    let local = instant.with_timezone(&zone);
    format!("{}:{:02}", local.hour(), local.minute())
}

/// Makes `model` equal to `rows` while touching only the rows that differ. Replacing a
/// model wholesale makes Slint rebuild every element (see the M0 findings).
fn patch<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    while model.row_count() > rows.len() {
        model.remove(model.row_count() - 1);
    }
    for (index, row) in rows.into_iter().enumerate() {
        match model.row_data(index) {
            Some(old) if old == row => {}
            Some(_) => model.set_row_data(index, row),
            None => model.push(row),
        }
    }
}

/// What every week needs from the cache whatever its dates. Read once per change to the
/// cache rather than on each move between weeks: with tens of thousands of events, parsing
/// every series each time made navigation visibly lag.
#[derive(Default)]
struct Loaded {
    accounts: Vec<Account>,
    calendars: Vec<StoredCalendar>,
    /// Series masters and their changed or deleted occurrences, each under the position
    /// of its calendar in `calendars`. Remembers the occurrences it has worked out.
    series: Series<usize, Event>,
}

impl Loaded {
    fn calendar_index(&self) -> HashMap<(AccountId, &str), usize> {
        self.calendars
            .iter()
            .enumerate()
            .map(|(index, c)| ((c.account_id, c.entry.id.as_str()), index))
            .collect()
    }
}

/// A stored event an instance on screen belongs to.
#[derive(Clone)]
struct Target {
    account: AccountId,
    calendar_id: String,
    event_id: String,
    etag: Option<String>,
    /// Set when the instance is one occurrence of a series.
    occurrence: Option<Occurrence>,
    /// Whether it can be dragged to another time.
    movable: bool,
}

/// Whether the event can be moved and resized by dragging. Not yet: repeating events
/// (which need the "this / following / all" question) and events with guests (who
/// would have to be told); never the special kinds of entry (birthdays, ...).
fn can_move(event: &Event, calendar: &CalendarListEntry) -> bool {
    calendar.is_writable()
        && matches!(event.event_type.as_deref(), None | Some("default"))
        && !event.is_recurring_master()
        && event.recurring_event_id.is_none()
        && event.attendees.is_empty()
}

/// An instant moved by whole days and minutes on the local clock, so that a move across
/// a change of the clocks keeps the time of day.
fn shift(instant: DateTime<Utc>, days: i64, minutes: i64, zone: Tz) -> DateTime<Utc> {
    let by = Duration::days(days) + Duration::minutes(minutes);
    let local = instant.with_timezone(&zone).naive_local() + by;
    zone.from_local_datetime(&local)
        .earliest()
        .map(|moved| moved.with_timezone(&Utc))
        // A local time the clocks skip: the same distance in real time instead.
        .unwrap_or(instant + by)
}

/// Start and end of an event dragged by `days` and `minutes`; its length stays.
fn moved_times(
    start: EventTime,
    end: EventTime,
    days: i64,
    minutes: i64,
    zone: Tz,
) -> (EventTime, EventTime) {
    match (start, end) {
        (EventTime::AllDay(first), EventTime::AllDay(last)) => (
            EventTime::AllDay(first + Duration::days(days)),
            EventTime::AllDay(last + Duration::days(days)),
        ),
        (EventTime::Timed(from), EventTime::Timed(to)) => {
            let moved = shift(from, days, minutes, zone);
            (
                EventTime::Timed(moved),
                EventTime::Timed(moved + (to - from)),
            )
        }
        unchanged => unchanged,
    }
}

/// The end of a timed event whose lower edge was dragged by `minutes`; never less than
/// a quarter of an hour after the start. `None` for all-day events.
fn resized_end(start: EventTime, end: EventTime, minutes: i64, zone: Tz) -> Option<EventTime> {
    let (EventTime::Timed(from), EventTime::Timed(to)) = (start, end) else {
        return None;
    };
    let dragged = shift(to, 0, minutes, zone);
    Some(EventTime::Timed(dragged.max(from + Duration::minutes(15))))
}

/// The fields to send for new times of an event, keeping the time zone each end had.
fn time_changes(
    resource: &serde_json::Value,
    start: EventTime,
    end: EventTime,
) -> serde_json::Value {
    let field = |name: &str, time: EventTime| match time {
        EventTime::AllDay(day) => serde_json::json!({ "date": day.to_string() }),
        EventTime::Timed(instant) => {
            let mut field = serde_json::json!({
                "dateTime": instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            });
            if let Some(zone) = resource[name]["timeZone"].as_str() {
                field["timeZone"] = zone.into();
            }
            field
        }
    };
    serde_json::json!({ "start": field("start", start), "end": field("end", end) })
}

/// Which occurrence of which series an instance on screen is.
#[derive(Clone)]
struct Occurrence {
    series_id: String,
    /// When it would start had it not been changed; this is what tells occurrences apart.
    original_start: EventTime,
    /// Whether it is a changed occurrence, stored as an event of its own.
    changed: bool,
}

fn occurrence_of(event: &Event, start: EventTime) -> Option<Occurrence> {
    if event.is_recurring_master() {
        return Some(Occurrence {
            series_id: event.id.clone(),
            original_start: start,
            changed: false,
        });
    }
    Some(Occurrence {
        series_id: event.recurring_event_id.clone()?,
        original_start: event
            .original_start_time
            .as_ref()
            .and_then(|original| original.time())
            .unwrap_or(start),
        changed: true,
    })
}

/// How much of a series a deletion takes, as asked in the window.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Scope {
    This,
    Following,
    All,
}

/// What to send to delete `target` and how to take that back; `series` is the cached
/// resource of the series the target belongs to, if it belongs to one.
fn plan_deletion(
    target: &Target,
    scope: Scope,
    own: Option<serde_json::Value>,
    series: Option<serde_json::Value>,
) -> Option<(Deletion, UndoKind)> {
    let Some(occurrence) = &target.occurrence else {
        return Some((
            Deletion::Event {
                event_id: target.event_id.clone(),
                etag: target.etag.clone(),
            },
            UndoKind::Recreate(own?),
        ));
    };
    if scope == Scope::This {
        return Some((
            Deletion::Occurrence {
                series_id: occurrence.series_id.clone(),
                original_start: occurrence.original_start,
                known_id: occurrence.changed.then(|| target.event_id.clone()),
            },
            UndoKind::Occurrence,
        ));
    }
    let resource = series?;
    let master: Event = serde_json::from_value(resource.clone()).ok()?;
    // "This and following" from the first occurrence on is the whole series.
    if scope == Scope::Following && master.start_time() != Some(occurrence.original_start) {
        return Some((
            Deletion::Following {
                series_id: master.id.clone(),
                recurrence: end_series_before(&master.recurrence, occurrence.original_start),
                etag: master.etag.clone(),
            },
            UndoKind::Patch {
                event_id: master.id,
                changes: serde_json::json!({ "recurrence": master.recurrence }),
            },
        ));
    }
    Some((
        Deletion::Event {
            event_id: master.id,
            etag: master.etag,
        },
        UndoKind::Recreate(resource),
    ))
}

/// What undoing a deletion needs.
struct Undo {
    account: AccountId,
    calendar_id: String,
    kind: UndoKind,
}

#[derive(Debug, PartialEq)]
enum UndoKind {
    /// Create the event again from the resource it had. For a series this brings back
    /// the series, not the changes that had been made to single occurrences.
    Recreate(serde_json::Value),
    /// Give an event some of its fields back.
    Patch {
        event_id: String,
        changes: serde_json::Value,
    },
    /// Bring back one occurrence of a series; which event that is, the worker tells
    /// once it has deleted it.
    Occurrence,
}

/// A new event being typed in.
struct Draft {
    start: EventTime,
    end: EventTime,
    /// The calendars on offer, in the order shown: account, calendar, and the text under
    /// which the choice is remembered.
    calendars: Vec<(AccountId, String, String)>,
}

/// When a new event starts and ends: an hour from the time clicked, or the whole day.
fn draft_times(day: NaiveDate, minute: i32, all_day: bool, zone: Tz) -> (EventTime, EventTime) {
    if all_day {
        return (
            EventTime::AllDay(day),
            EventTime::AllDay(day + Duration::days(1)),
        );
    }
    let local = day.and_time(chrono::NaiveTime::MIN) + Duration::minutes(i64::from(minute));
    // A time skipped by the clocks going forward starts at the day's first valid instant.
    let start = zone
        .from_local_datetime(&local)
        .earliest()
        .map(|start| start.with_timezone(&Utc))
        .unwrap_or_else(|| local_day_start(day, zone));
    (
        EventTime::Timed(start),
        EventTime::Timed(start + Duration::hours(1)),
    )
}

/// The resource Google creates an event from.
fn event_resource(title: &str, start: EventTime, end: EventTime, zone: Tz) -> serde_json::Value {
    let time = |time: EventTime| match time {
        EventTime::AllDay(day) => serde_json::json!({ "date": day.to_string() }),
        EventTime::Timed(instant) => serde_json::json!({
            "dateTime": instant.with_timezone(&zone).to_rfc3339(),
            "timeZone": zone.name(),
        }),
    };
    serde_json::json!({
        "summary": title.trim(),
        "start": time(start),
        "end": time(end),
    })
}

struct View {
    ui: slint::Weak<AppWindow>,
    store: Store,
    loaded: RefCell<Loaded>,
    zone: Tz,
    /// The day the user is looking at; which days are shown follows from it and `span`.
    anchor: Cell<NaiveDate>,
    /// A day of the month the sidebar's small month shows. It follows `anchor`, but can
    /// be paged on its own to pick a day further away.
    mini_month: Cell<NaiveDate>,
    mini_days: Rc<VecModel<MiniDay>>,
    span: Cell<Span>,
    /// The date the day headers were last drawn for, to notice midnight passing.
    drawn_today: Cell<Option<NaiveDate>>,
    days: Rc<VecModel<DayHeader>>,
    timed: Rc<VecModel<TimedItem>>,
    /// Details of every instance now on screen, by the index its block or bar carries.
    details: RefCell<Vec<EventDetail>>,
    /// Which stored event each of those instances is, for acting on it.
    targets: RefCell<Vec<Target>>,
    /// The new event being typed in, if its card is open.
    draft: RefCell<Option<Draft>>,
    /// The event deleted last, while its deletion can still be undone.
    undo: RefCell<Option<Undo>>,
    /// Hides the message about the last change after a while.
    toast_timer: Timer,
    commands: UnboundedSender<Command>,
    all_day: Rc<VecModel<AllDayItem>>,
    month_cells: Rc<VecModel<MonthCell>>,
    month_chips: Rc<VecModel<MonthChip>>,
    schedule: Rc<VecModel<ScheduleRow>>,
    sidebar: Rc<VecModel<SidebarRow>>,
}

impl View {
    /// The first day shown and how many days are.
    fn range(&self) -> (NaiveDate, i64) {
        self.span.get().range(self.anchor.get())
    }

    fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.zone).date_naive()
    }

    /// Re-reads the cache after it changed, then redraws.
    fn reload(&self) {
        let loaded = (|| -> Result<Loaded> {
            let mut loaded = Loaded {
                accounts: self.store.accounts()?,
                calendars: self.store.calendars()?,
                series: Series::default(),
            };
            let index = loaded.calendar_index();
            let recurring: Vec<(usize, Event)> = self
                .store
                .recurring_events()?
                .into_iter()
                .filter_map(|e| {
                    let calendar = *index.get(&(e.account_id, e.calendar_id.as_str()))?;
                    Some((calendar, e.event))
                })
                .collect();
            loaded.series = Series::new(recurring);
            Ok(loaded)
        })();
        match loaded {
            Ok(loaded) => {
                self.loaded.replace(loaded);
                self.refresh();
            }
            Err(error) => self.show_error(&error),
        }
    }

    /// Updates everything the window shows for the current week.
    fn refresh(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        if let Err(error) = self.try_refresh(&ui) {
            self.show_error(&error);
        }
    }

    fn show_error(&self, error: &anyhow::Error) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_status_kind(StatusKind::Failed);
            ui.set_status_detail(format!("{error:#}").into());
        }
    }

    fn try_refresh(&self, ui: &AppWindow) -> Result<()> {
        let span = self.span.get();
        let (week_start, days) = self.range();
        let is_month = span == Span::Month;
        let is_schedule = span == Span::Schedule;
        let today = self.today();
        ui.set_view_index(Span::ALL.iter().position(|s| *s == span).unwrap_or(0) as i32);
        self.drawn_today.set(Some(today));
        ui.set_period_title(span.title(self.anchor.get()).into());
        if is_month {
            ui.set_month_weeks((days / 7) as i32);
        } else if !is_schedule {
            ui.set_day_count(days as i32);
            patch(
                &self.days,
                (0..days)
                    .map(|offset| {
                        let day = week_start + Duration::days(offset);
                        DayHeader {
                            weekday: day.format("%a").to_string().to_uppercase().into(),
                            number: day.day().to_string().into(),
                            today: day == today,
                        }
                    })
                    .collect(),
            );
        }
        self.update_now(ui);
        self.update_mini_month(ui);

        let loaded = self.loaded.borrow();
        let (accounts, calendars) = (&loaded.accounts, &loaded.calendars);
        let mut sidebar = Vec::new();
        for account in accounts {
            sidebar.push(SidebarRow {
                account_id: account.id as i32,
                calendar_id: SharedString::new(),
                is_account: true,
                text: account.email.as_str().into(),
                email: account.email.as_str().into(),
                color: FALLBACK_COLOR,
                shown: true,
            });
            for calendar in calendars.iter().filter(|c| c.account_id == account.id) {
                let entry = &calendar.entry;
                sidebar.push(SidebarRow {
                    account_id: account.id as i32,
                    calendar_id: entry.id.as_str().into(),
                    is_account: false,
                    text: entry.display_name().into(),
                    email: account.email.as_str().into(),
                    color: entry
                        .background_color
                        .as_deref()
                        .and_then(parse_color)
                        .unwrap_or(FALLBACK_COLOR),
                    shown: calendar.shown,
                });
            }
        }
        patch(&self.sidebar, sidebar);

        let from = local_day_start(week_start, self.zone);
        let to = local_day_start(week_start + Duration::days(days), self.zone);
        let singles = self
            .store
            .single_events_for_range(from.timestamp() - 86_400, to.timestamp() + 86_400)?;
        // An instance's key is the position of its calendar in `calendars`.
        let index = loaded.calendar_index();
        let mut instances = loaded
            .series
            .expand(from, to, |calendar| calendars[*calendar].shown);
        instances.extend(singles.iter().filter_map(|e| {
            let calendar = *index.get(&(e.account_id, e.calendar_id.as_str()))?;
            let shown = calendars[calendar].shown;
            single_instance(calendar, &e.event, from, to).filter(|_| shown)
        }));
        let spans: Vec<_> = instances.iter().map(|i| (i.start, i.end)).collect();
        // Only one of the views is on screen; the models of the others are emptied.
        let layout = if is_month || is_schedule {
            WeekLayout::default()
        } else {
            layout_days(&spans, week_start, days, self.zone)
        };
        let month = if is_month {
            layout_month(
                &spans,
                week_start,
                days / 7,
                self.zone,
                ui.get_month_capacity(),
            )
        } else {
            MonthLayout::default()
        };

        let movable = |index: usize| {
            let instance = &instances[index];
            can_move(instance.event, &calendars[instance.key].entry)
        };
        // An event drawn as several blocks (it crosses midnight) is not resized by one.
        let mut blocks: HashMap<usize, u32> = HashMap::new();
        for block in &layout.timed {
            *blocks.entry(block.index).or_default() += 1;
        }
        let title = |index: usize| -> SharedString {
            instances[index]
                .event
                .summary
                .as_deref()
                .unwrap_or("")
                .into()
        };
        let color = |index: usize| {
            let instance = &instances[index];
            event_color(instance.event, &calendars[instance.key].entry)
        };

        let entries = if is_schedule {
            schedule(&spans, week_start, days, self.zone)
        } else {
            Vec::new()
        };
        let mut previous_day = None;
        patch(
            &self.schedule,
            entries
                .iter()
                .map(|entry| {
                    let day = week_start + Duration::days(i64::from(entry.day));
                    let first_of_day = previous_day.replace(entry.day) != Some(entry.day);
                    let time = match (entry.part, spans[entry.index]) {
                        (DayPart::Whole, (EventTime::Timed(start), EventTime::Timed(end))) => {
                            format!("{} – {}", clock(start, self.zone), clock(end, self.zone))
                        }
                        (DayPart::Starts, (EventTime::Timed(start), _)) => clock(start, self.zone),
                        (DayPart::Ends, (_, EventTime::Timed(end))) => {
                            format!("Until {}", clock(end, self.zone))
                        }
                        _ => "All day".to_owned(),
                    };
                    ScheduleRow {
                        instance: entry.index as i32,
                        first_of_day,
                        day_number: day.day().to_string().into(),
                        day_name: day.format("%b, %a").to_string().to_uppercase().into(),
                        today: day == today,
                        time: time.into(),
                        title: title(entry.index),
                        color: color(entry.index),
                    }
                })
                .collect(),
        );
        patch(
            &self.timed,
            layout
                .timed
                .iter()
                .map(|block| {
                    let color = color(block.index);
                    let time = match spans[block.index] {
                        (EventTime::Timed(start), EventTime::Timed(end)) => {
                            format!("{} – {}", clock(start, self.zone), clock(end, self.zone))
                        }
                        _ => String::new(),
                    };
                    TimedItem {
                        instance: block.index as i32,
                        title: title(block.index),
                        time: time.into(),
                        day: block.day,
                        start_min: block.start_min,
                        end_min: block.end_min,
                        col: block.col,
                        cols: block.cols,
                        movable: movable(block.index),
                        resizable: movable(block.index) && blocks[&block.index] == 1,
                        color,
                        text_color: text_color_on(color),
                    }
                })
                .collect(),
        );
        patch(
            &self.all_day,
            layout
                .all_day
                .iter()
                .map(|bar| {
                    let color = color(bar.index);
                    AllDayItem {
                        instance: bar.index as i32,
                        title: title(bar.index),
                        first_day: bar.first_day,
                        last_day: bar.last_day,
                        row: bar.row,
                        movable: movable(bar.index),
                        color,
                        text_color: text_color_on(color),
                    }
                })
                .collect(),
        );
        ui.set_all_day_rows(layout.all_day_rows);

        let shown_month = self.anchor.get().month();
        patch(
            &self.month_cells,
            month
                .more
                .iter()
                .enumerate()
                .map(|(offset, more)| {
                    let day = week_start + Duration::days(offset as i64);
                    // The first of a month says which month, as on the web.
                    let label = if day.day() == 1 {
                        day.format("%-d %b").to_string()
                    } else {
                        day.day().to_string()
                    };
                    MonthCell {
                        label: label.into(),
                        today: day == today,
                        dim: day.month() != shown_month,
                        more: *more,
                    }
                })
                .collect(),
        );
        patch(
            &self.month_chips,
            month
                .chips
                .iter()
                .map(|chip| {
                    let color = color(chip.index);
                    let time = match spans[chip.index].0 {
                        EventTime::Timed(start) if chip.within_day => clock(start, self.zone),
                        _ => String::new(),
                    };
                    MonthChip {
                        instance: chip.index as i32,
                        title: title(chip.index),
                        time: time.into(),
                        week: chip.week,
                        first_day: chip.first_day,
                        last_day: chip.last_day,
                        row: chip.row,
                        filled: !chip.within_day,
                        color,
                        text_color: text_color_on(color),
                    }
                })
                .collect(),
        );

        let emails: HashMap<AccountId, &str> =
            accounts.iter().map(|a| (a.id, a.email.as_str())).collect();
        let details = instances
            .iter()
            .enumerate()
            .map(|(index, instance)| {
                let event = instance.event;
                let calendar = &calendars[instance.key];
                let email = emails.get(&calendar.account_id).copied().unwrap_or("");
                EventDetail {
                    title: title(index),
                    when: when_text(instance.start, instance.end, self.zone).into(),
                    location: event.location.as_deref().unwrap_or("").into(),
                    description: plain_text(event.description.as_deref().unwrap_or("")).into(),
                    calendar: calendar.entry.display_name().into(),
                    account: email.into(),
                    color: color(index),
                    link: web_link(event, email).into(),
                    // Anything in a calendar this account may write to. Should Google
                    // refuse a particular entry, it says why.
                    can_delete: calendar.entry.is_writable(),
                    repeating: event.is_recurring_master() || event.recurring_event_id.is_some(),
                    notify_guests: event.has_guests_to_notify(),
                }
            })
            .collect();
        self.details.replace(details);
        let targets = instances
            .iter()
            .map(|instance| {
                let calendar = &calendars[instance.key];
                Target {
                    account: calendar.account_id,
                    calendar_id: calendar.entry.id.clone(),
                    event_id: instance.event.id.clone(),
                    etag: instance.event.etag.clone(),
                    occurrence: occurrence_of(instance.event, instance.start),
                    movable: can_move(instance.event, &calendar.entry),
                }
            })
            .collect();
        self.targets.replace(targets);
        // What they described may have moved or gone.
        ui.set_detail_visible(false);
        ui.set_delete_ask_visible(false);
        Ok(())
    }

    /// Moves the current-time line.
    fn update_now(&self, ui: &AppWindow) {
        let now = Utc::now().with_timezone(&self.zone);
        let (first, days) = self.range();
        let offset = (now.date_naive() - first).num_days();
        let has_columns = !matches!(self.span.get(), Span::Month | Span::Schedule);
        if has_columns && (0..days).contains(&offset) {
            ui.set_now_day(offset as i32);
            ui.set_now_min((now.hour() * 60 + now.minute()) as i32);
        } else {
            ui.set_now_day(-1);
        }
    }

    /// Shows or hides a calendar, remembers the choice, and redraws.
    fn set_calendar_shown(&self, account: AccountId, calendar_id: &str, shown: bool) {
        match self.store.set_calendar_shown(account, calendar_id, shown) {
            Ok(()) => {
                // Changed in place: the series refer to calendars by their position, and
                // need no re-reading for this.
                let mut loaded = self.loaded.borrow_mut();
                if let Some(calendar) = loaded
                    .calendars
                    .iter_mut()
                    .find(|c| c.account_id == account && c.entry.id == calendar_id)
                {
                    calendar.shown = shown;
                }
                drop(loaded);
                self.refresh();
            }
            Err(error) => self.show_error(&error.into()),
        }
    }

    fn show(&self, anchor: NaiveDate) {
        self.anchor.set(anchor);
        self.mini_month.set(anchor);
        self.refresh();
    }

    /// Draws the sidebar's small month: always six weeks, so its height never changes.
    fn update_mini_month(&self, ui: &AppWindow) {
        let month = self.mini_month.get();
        let (first, _) = Span::Month.range(month);
        let (shown_first, shown_days) = self.range();
        let today = self.today();
        // Marking every day of a month or of the schedule would say nothing.
        let marks_range = !matches!(self.span.get(), Span::Month | Span::Schedule);
        ui.set_mini_title(month.format("%B %Y").to_string().into());
        patch(
            &self.mini_days,
            (0..MINI_MONTH_DAYS)
                .map(|offset| {
                    let day = first + Duration::days(offset);
                    MiniDay {
                        label: day.day().to_string().into(),
                        today: day == today,
                        dim: day.month() != month.month(),
                        shown: marks_range
                            && (0..shown_days).contains(&(day - shown_first).num_days()),
                    }
                })
                .collect(),
        );
    }

    /// Pages the small month back (-1) or forward (1) without moving the main view.
    fn step_mini_month(&self, direction: i64) {
        self.mini_month
            .set(Span::Month.step(self.mini_month.get(), direction));
        if let Some(ui) = self.ui.upgrade() {
            self.update_mini_month(&ui);
        }
    }

    /// Shows the day picked in the small month, in the view that is open.
    fn pick_mini_day(&self, cell: i64) {
        let (first, _) = Span::Month.range(self.mini_month.get());
        self.show(first + Duration::days(cell));
    }

    /// Moves one screenful back (-1) or forward (1).
    fn step(&self, direction: i64) {
        self.show(self.span.get().step(self.anchor.get(), direction));
    }

    /// Deletes the event behind an instance on screen. The copy kept for undoing is taken
    /// from the cache before anything is sent.
    fn delete_event(&self, instance: usize, scope: Scope, notify: bool) {
        let Some(target) = self.targets.borrow().get(instance).cloned() else {
            return;
        };
        let cached = |id: &str| {
            self.store
                .event_json(target.account, &target.calendar_id, id)
        };
        let resources = cached(&target.event_id).and_then(|own| {
            let series = match &target.occurrence {
                Some(occurrence) => cached(&occurrence.series_id)?,
                None => None,
            };
            Ok((own, series))
        });
        let (own, series) = match resources {
            Ok(resources) => resources,
            Err(error) => return self.show_error(&error.into()),
        };
        // Nothing to send if it is gone from the cache: a sync has removed it meanwhile.
        let Some((deletion, kind)) = plan_deletion(&target, scope, own, series) else {
            return;
        };
        self.undo.replace(Some(Undo {
            account: target.account,
            calendar_id: target.calendar_id.clone(),
            kind,
        }));
        if let Some(ui) = self.ui.upgrade() {
            ui.set_detail_visible(false);
            ui.set_delete_ask_visible(false);
            ui.set_toast(SharedString::new());
        }
        let _ = self.commands.send(Command::DeleteEvent {
            account: target.account,
            calendar_id: target.calendar_id,
            deletion,
            notify,
        });
    }

    /// Gives the event behind an instance new times, worked out by `change` from its
    /// present ones. The undo is prepared from the cache before anything is sent.
    fn change_times(
        &self,
        instance: usize,
        change: impl Fn(EventTime, EventTime) -> Option<(EventTime, EventTime)>,
    ) -> bool {
        let Some(target) = self.targets.borrow().get(instance).cloned() else {
            return false;
        };
        if !target.movable {
            return false;
        }
        let resource =
            match self
                .store
                .event_json(target.account, &target.calendar_id, &target.event_id)
            {
                Ok(Some(resource)) => resource,
                Ok(None) => return false,
                Err(error) => {
                    self.show_error(&error.into());
                    return false;
                }
            };
        let Ok(event) = serde_json::from_value::<Event>(resource.clone()) else {
            return false;
        };
        let (Some(start), Some(end)) = (event.start_time(), event.end_time()) else {
            return false;
        };
        let Some((new_start, new_end)) = change(start, end).filter(|new| *new != (start, end))
        else {
            return false;
        };
        self.undo.replace(Some(Undo {
            account: target.account,
            calendar_id: target.calendar_id.clone(),
            kind: UndoKind::Patch {
                event_id: target.event_id.clone(),
                changes: serde_json::json!({ "start": resource["start"], "end": resource["end"] }),
            },
        }));
        if let Some(ui) = self.ui.upgrade() {
            ui.set_toast(SharedString::new());
        }
        let _ = self.commands.send(Command::ChangeEvent {
            account: target.account,
            calendar_id: target.calendar_id,
            event_id: target.event_id,
            etag: target.etag,
            changes: time_changes(&resource, new_start, new_end),
        });
        true
    }

    /// An event was dragged by days and minutes. It is drawn at its new place at once,
    /// where that is simple, rather than jumping back until Google has answered.
    fn move_event(&self, instance: usize, days: i32, minutes: i32) {
        let zone = self.zone;
        let sent = self.change_times(instance, |start, end| {
            Some(moved_times(start, end, days.into(), minutes.into(), zone))
        });
        if !sent {
            return;
        }
        let shown_days = self.range().1 as i32;
        let on_screen = |day: i32| (0..shown_days).contains(&day);
        let blocks: Vec<usize> = (0..self.timed.row_count())
            .filter(|row| {
                self.timed
                    .row_data(*row)
                    .is_some_and(|b| b.instance == instance as i32)
            })
            .collect();
        if let [row] = blocks[..]
            && let Some(mut block) = self.timed.row_data(row)
        {
            let (start, end) = (block.start_min + minutes, block.end_min + minutes);
            if start >= 0 && end <= 24 * 60 && on_screen(block.day + days) {
                block.day += days;
                block.start_min = start;
                block.end_min = end;
                self.timed.set_row_data(row, block);
            }
        }
        for row in 0..self.all_day.row_count() {
            if let Some(mut bar) = self.all_day.row_data(row)
                && bar.instance == instance as i32
                && on_screen(bar.first_day + days)
                && on_screen(bar.last_day + days)
            {
                bar.first_day += days;
                bar.last_day += days;
                self.all_day.set_row_data(row, bar);
            }
        }
    }

    /// The lower edge of an event's block was dragged by minutes.
    fn resize_event(&self, instance: usize, minutes: i32) {
        let zone = self.zone;
        let sent = self.change_times(instance, |start, end| {
            Some((start, resized_end(start, end, minutes.into(), zone)?))
        });
        if !sent {
            return;
        }
        for row in 0..self.timed.row_count() {
            if let Some(mut block) = self.timed.row_data(row)
                && block.instance == instance as i32
            {
                block.end_min = (block.end_min + minutes).clamp(block.start_min + 15, 24 * 60);
                self.timed.set_row_data(row, block);
            }
        }
    }

    /// The worker has answered a move or resize.
    fn change_finished(&self, changed: bool) {
        if changed {
            self.show_toast("Event saved", true);
        } else {
            // A dialog says why. Put the event back where it is.
            self.undo.take();
            self.refresh();
        }
    }

    /// The worker has answered a deletion: offer to undo it, for a while. `occurrence`
    /// is the event that was removed, if it was one occurrence of a series.
    fn delete_finished(&self, deleted: bool, occurrence: &str) {
        if !deleted {
            // A dialog says why; there is nothing to undo.
            self.undo.take();
            return;
        }
        let mut undo = self.undo.borrow_mut();
        if let Some(Undo { kind, .. }) = undo.as_mut()
            && *kind == UndoKind::Occurrence
        {
            if occurrence.is_empty() {
                undo.take();
            } else {
                // Google keeps a deleted occurrence as a cancelled event.
                *kind = UndoKind::Patch {
                    event_id: occurrence.to_owned(),
                    changes: serde_json::json!({ "status": "confirmed" }),
                };
            }
        }
        let can_undo = undo.is_some();
        drop(undo);
        self.show_toast("Event deleted", can_undo);
    }

    /// Says what just happened, for a while; with "Undo" if it can be taken back.
    fn show_toast(&self, text: &str, undo: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        ui.set_toast(text.into());
        ui.set_toast_undo(undo);
        let ui = self.ui.clone();
        self.toast_timer
            .start(TimerMode::SingleShot, UNDO_PERIOD, move || {
                if let Some(ui) = ui.upgrade() {
                    ui.set_toast(SharedString::new());
                }
            });
    }

    /// Opens the card for a new event. `day` counts from the first day shown; a negative
    /// one means "about now" (the `c` key).
    fn open_draft(&self, day: i32, minute: i32, all_day: bool) {
        let Some(ui) = self.ui.upgrade() else { return };
        let (first, days) = self.range();
        let has_columns = !matches!(self.span.get(), Span::Month | Span::Schedule);
        let (day, minute) = if day < 0 {
            // The next half hour, today if today is on screen.
            let now = Utc::now().with_timezone(&self.zone);
            let offset = (now.date_naive() - first).num_days();
            let offset = if (0..days).contains(&offset) {
                offset
            } else {
                0
            };
            let minute = ((now.hour() * 60 + now.minute()) / 30 + 1) * 30;
            (offset, (minute as i32).min(23 * 60))
        } else {
            (i64::from(day), minute)
        };
        let (start, end) = draft_times(first + Duration::days(day), minute, all_day, self.zone);

        let loaded = self.loaded.borrow();
        let emails: HashMap<AccountId, &str> = loaded
            .accounts
            .iter()
            .map(|a| (a.id, a.email.as_str()))
            .collect();
        let writable: Vec<&StoredCalendar> = loaded
            .calendars
            .iter()
            .filter(|c| c.entry.is_writable())
            .collect();
        if writable.is_empty() {
            return self.show_error(&anyhow::anyhow!(
                "there is no calendar this app may add events to"
            ));
        }
        // The calendar the previous event was created in, else the first account's own.
        let remembered = self.store.setting(NEW_EVENT_CALENDAR).ok().flatten();
        let key = |c: &StoredCalendar| {
            let email = emails.get(&c.account_id).copied().unwrap_or("");
            format!("{email}\t{}", c.entry.id)
        };
        let chosen = writable
            .iter()
            .position(|c| Some(key(c)) == remembered)
            .or_else(|| writable.iter().position(|c| c.entry.primary))
            .unwrap_or(0);

        let rows: Vec<DraftCalendar> = writable
            .iter()
            .map(|c| DraftCalendar {
                name: c.entry.display_name().into(),
                account: emails.get(&c.account_id).copied().unwrap_or("").into(),
                color: c
                    .entry
                    .background_color
                    .as_deref()
                    .and_then(parse_color)
                    .unwrap_or(FALLBACK_COLOR),
            })
            .collect();
        self.draft.replace(Some(Draft {
            start,
            end,
            calendars: writable
                .iter()
                .map(|c| (c.account_id, c.entry.id.clone(), key(c)))
                .collect(),
        }));
        ui.set_draft_calendars(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_draft_calendar(chosen as i32);
        ui.set_draft_when(when_text(start, end, self.zone).into());
        // A ghost of the new event where it will go, in the views that have columns.
        let ghost = has_columns && !all_day;
        ui.set_draft_day(if ghost { day as i32 } else { -1 });
        ui.set_draft_min(minute);
        ui.set_detail_visible(false);
        ui.set_draft_visible(true);
    }

    /// Creates the event the open card describes.
    fn save_draft(&self, title: &str, calendar: usize) {
        let Some(draft) = self.draft.take() else {
            return;
        };
        let Some((account, calendar_id, key)) = draft.calendars.get(calendar).cloned() else {
            return;
        };
        // Remembered for the next new event; failing to remember is no reason not to save.
        let _ = self.store.set_setting(NEW_EVENT_CALENDAR, &key);
        let _ = self.commands.send(Command::CreateEvent {
            account,
            calendar_id,
            resource: event_resource(title, draft.start, draft.end, self.zone),
        });
    }

    /// Brings back the event deleted last.
    fn undo_delete(&self) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_toast(SharedString::new());
        }
        let Some(Undo {
            account,
            calendar_id,
            kind,
        }) = self.undo.take()
        else {
            return;
        };
        let _ = self.commands.send(match kind {
            UndoKind::Recreate(resource) => Command::RestoreEvent {
                account,
                calendar_id,
                resource,
            },
            UndoKind::Patch { event_id, changes } => Command::PatchEvent {
                account,
                calendar_id,
                event_id,
                changes,
            },
            // The worker has not said which occurrence it removed.
            UndoKind::Occurrence => return,
        });
    }

    /// Opens the day view on the day of a cell of the month grid.
    fn open_day(&self, cell: i64) {
        let day = self.range().0 + Duration::days(cell);
        self.span.set(Span::Day);
        self.show(day);
    }
}

pub fn run() -> Result<()> {
    let ui = AppWindow::new()?;
    let zone = local_zone();
    let commands = worker::spawn(ui.as_weak());
    let view = Rc::new(View {
        ui: ui.as_weak(),
        store: open_store()?,
        loaded: RefCell::default(),
        zone,
        anchor: Cell::new(Utc::now().with_timezone(&zone).date_naive()),
        mini_month: Cell::new(Utc::now().with_timezone(&zone).date_naive()),
        mini_days: Rc::default(),
        span: Cell::new(Span::Week),
        drawn_today: Cell::new(None),
        days: Rc::default(),
        timed: Rc::default(),
        details: RefCell::default(),
        targets: RefCell::default(),
        draft: RefCell::default(),
        undo: RefCell::default(),
        toast_timer: Timer::default(),
        commands: commands.clone(),
        all_day: Rc::default(),
        month_cells: Rc::default(),
        month_chips: Rc::default(),
        schedule: Rc::default(),
        sidebar: Rc::default(),
    });
    ui.set_days(ModelRc::from(view.days.clone()));
    ui.set_timed(ModelRc::from(view.timed.clone()));
    ui.set_all_day(ModelRc::from(view.all_day.clone()));
    ui.set_month_cells(ModelRc::from(view.month_cells.clone()));
    ui.set_month_chips(ModelRc::from(view.month_chips.clone()));
    ui.set_schedule(ModelRc::from(view.schedule.clone()));
    ui.set_sidebar(ModelRc::from(view.sidebar.clone()));
    // Monday first, like the week view. 2026-11-16 is a Monday.
    let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap_or_default();
    ui.set_weekday_names(ModelRc::from(Rc::new(VecModel::from(
        (0..7)
            .map(|offset| {
                let day = monday + Duration::days(offset);
                SharedString::from(day.format("%a").to_string().to_uppercase())
            })
            .collect::<Vec<_>>(),
    ))));

    let theme = view.store.setting(THEME).ok().flatten();
    ui.set_theme_mode(theme.and_then(|mode| mode.parse().ok()).unwrap_or(0));
    ui.on_theme_chosen({
        let view = view.clone();
        move |mode| {
            if let Err(error) = view.store.set_setting(THEME, &mode.to_string()) {
                view.show_error(&error.into());
            }
        }
    });
    ui.on_delete_event({
        let view = view.clone();
        move |instance, scope, notify| {
            let scope = match scope {
                1 => Scope::Following,
                2 => Scope::All,
                _ => Scope::This,
            };
            view.delete_event(instance as usize, scope, notify);
        }
    });
    ui.on_event_moved({
        let view = view.clone();
        move |instance, days, minutes| view.move_event(instance as usize, days, minutes)
    });
    ui.on_event_resized({
        let view = view.clone();
        move |instance, minutes| view.resize_event(instance as usize, minutes)
    });
    ui.on_change_finished({
        let view = view.clone();
        move |changed| view.change_finished(changed)
    });
    ui.on_delete_finished({
        let view = view.clone();
        move |deleted, occurrence| view.delete_finished(deleted, &occurrence)
    });
    ui.on_draft_requested({
        let view = view.clone();
        move |day, minute, all_day| view.open_draft(day, minute, all_day)
    });
    ui.on_draft_save({
        let view = view.clone();
        move |title, calendar| view.save_draft(&title, calendar as usize)
    });
    ui.on_event_created({
        let view = view.clone();
        move || view.show_toast("Event saved", false)
    });
    ui.on_undo({
        let view = view.clone();
        move || view.undo_delete()
    });
    ui.on_go_today({
        let view = view.clone();
        move || view.show(view.today())
    });
    ui.on_go_previous({
        let view = view.clone();
        move || view.step(-1)
    });
    ui.on_go_next({
        let view = view.clone();
        move || view.step(1)
    });
    ui.on_set_view({
        let view = view.clone();
        move |index| {
            if let Some(span) = Span::ALL.get(index as usize) {
                view.span.set(*span);
                view.refresh();
            }
        }
    });
    ui.set_mini_days(ModelRc::from(view.mini_days.clone()));
    ui.on_mini_step({
        let view = view.clone();
        move |direction| view.step_mini_month(i64::from(direction))
    });
    ui.on_mini_pick({
        let view = view.clone();
        move |cell| view.pick_mini_day(i64::from(cell))
    });
    ui.on_open_day({
        let view = view.clone();
        move |cell| view.open_day(i64::from(cell))
    });
    ui.on_month_resized({
        let view = view.clone();
        move || {
            // How many lines fit in a cell changed; only the month view cares.
            if view.span.get() == Span::Month {
                view.refresh();
            }
        }
    });
    ui.on_toggle_calendar({
        let view = view.clone();
        move |account, calendar_id, shown| {
            view.set_calendar_shown(AccountId::from(account), &calendar_id, shown);
        }
    });
    ui.on_event_clicked({
        let view = view.clone();
        move |instance| {
            let detail = view.details.borrow().get(instance as usize).cloned();
            if let (Some(detail), Some(ui)) = (detail, view.ui.upgrade()) {
                ui.set_detail(detail);
                ui.set_detail_visible(true);
            }
        }
    });
    ui.on_open_link(|link| {
        // Nothing useful to do if no browser can be started.
        let _ = open::that_detached(link.as_str());
    });
    ui.on_data_changed({
        let view = view.clone();
        move || view.reload()
    });
    ui.on_add_account({
        let commands = commands.clone();
        move || {
            let _ = commands.send(Command::AddAccount);
        }
    });
    ui.on_sync_now({
        let commands = commands.clone();
        move || {
            let _ = commands.send(Command::SyncAll);
        }
    });
    ui.on_remove_account({
        let commands = commands.clone();
        move |email| {
            let _ = commands.send(Command::RemoveAccount(email.into()));
        }
    });

    // Keeps the current-time line and the "today" highlight moving.
    let clock_timer = Timer::default();
    clock_timer.start(TimerMode::Repeated, StdDuration::from_secs(30), {
        let view = view.clone();
        move || {
            if view.drawn_today.get() != Some(view.today()) {
                // Midnight passed: the "today" highlight has to move too.
                view.refresh();
            } else if let Some(ui) = view.ui.upgrade() {
                view.update_now(&ui);
            }
        }
    });

    let sync_timer = Timer::default();
    sync_timer.start(TimerMode::Repeated, SYNC_INTERVAL, {
        let commands = commands.clone();
        move || {
            let _ = commands.send(Command::SyncAll);
        }
    });

    view.reload();
    ui.invoke_scroll_to_hour(FIRST_VISIBLE_HOUR);
    // Show what is cached at once, and fetch changes in the background.
    let _ = commands.send(Command::SyncAll);

    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> NaiveDate {
        text.parse().unwrap()
    }

    #[test]
    fn week_starts_on_the_monday_on_or_before() {
        assert_eq!(week_start_of(date("2026-11-15")), date("2026-11-09")); // a Sunday
        assert_eq!(week_start_of(date("2026-11-16")), date("2026-11-16")); // a Monday
    }

    #[test]
    fn period_title_names_both_months_when_the_week_straddles() {
        assert_eq!(period_title(date("2026-11-16"), 7), "November 2026");
        assert_eq!(period_title(date("2026-11-30"), 7), "Nov – Dec 2026");
        assert_eq!(period_title(date("2026-12-28"), 7), "Dec 2026 – Jan 2027");
        assert_eq!(period_title(date("2026-11-30"), 1), "November 2026");
    }

    #[test]
    fn only_the_week_view_snaps_to_the_start_of_the_week() {
        let wednesday = date("2026-11-18");
        assert_eq!(Span::Week.range(wednesday), (date("2026-11-16"), 7));
        assert_eq!(Span::FourDays.range(wednesday), (wednesday, 4));
        assert_eq!(Span::Day.range(wednesday), (wednesday, 1));
    }

    #[test]
    fn the_month_view_shows_the_whole_weeks_that_cover_the_month() {
        // November 2026: Sunday the 1st to Monday the 30th, so six weeks.
        assert_eq!(
            Span::Month.range(date("2026-11-18")),
            (date("2026-10-26"), 42)
        );
        // February 2027 starts on a Monday and has 28 days: exactly four weeks.
        assert_eq!(
            Span::Month.range(date("2027-02-10")),
            (date("2027-02-01"), 28)
        );
        assert_eq!(Span::Month.title(date("2026-11-30")), "November 2026");
    }

    #[test]
    fn stepping_moves_one_screenful() {
        assert_eq!(Span::Week.step(date("2026-11-18"), 1), date("2026-11-25"));
        assert_eq!(Span::Day.step(date("2026-11-18"), -1), date("2026-11-17"));
        assert_eq!(Span::Month.step(date("2026-11-18"), 1), date("2026-12-18"));
        // There is no 31 November.
        assert_eq!(Span::Month.step(date("2026-12-31"), -1), date("2026-11-30"));
    }

    #[test]
    fn calendar_colour_is_used_unless_the_event_has_its_own() {
        let calendar: CalendarListEntry =
            serde_json::from_str(r##"{"id": "c", "backgroundColor": "#16a765"}"##).unwrap();
        let plain = Event::default();
        let coloured = Event {
            color_id: Some("11".into()),
            ..Event::default()
        };
        assert_eq!(event_color(&plain, &calendar), rgb(0x16a765));
        assert_eq!(event_color(&coloured, &calendar), rgb(0xd50000));
    }

    #[test]
    fn text_is_dark_on_light_colours_and_white_on_dark_ones() {
        assert_eq!(text_color_on(rgb(0xf6bf26)), rgb(0x202124));
        assert_eq!(text_color_on(rgb(0x3f51b5)), rgb(0xffffff));
    }

    #[test]
    fn when_text_covers_timed_and_all_day_events() {
        let zone = chrono_tz::Europe::Prague;
        let timed = |start: &str, end: &str| {
            (
                EventTime::Timed(start.parse().unwrap()),
                EventTime::Timed(end.parse().unwrap()),
            )
        };
        let (start, end) = timed("2026-11-17T08:00:00Z", "2026-11-17T11:00:00Z");
        assert_eq!(
            when_text(start, end, zone),
            "Tuesday, 17 November · 9:00 – 12:00"
        );
        let (start, end) = timed("2026-11-17T21:00:00Z", "2026-11-18T01:00:00Z");
        assert_eq!(
            when_text(start, end, zone),
            "17 November 2026, 22:00 – 18 November 2026, 2:00"
        );
        let all_day = |first: &str, past_last: &str| {
            when_text(
                EventTime::AllDay(date(first)),
                EventTime::AllDay(date(past_last)),
                zone,
            )
        };
        assert_eq!(all_day("2026-11-17", "2026-11-18"), "Tuesday, 17 November");
        assert_eq!(
            all_day("2026-11-17", "2026-11-20"),
            "17 November 2026 – 19 November 2026"
        );
    }

    #[test]
    fn web_link_names_the_owning_account() {
        let event = Event {
            html_link: Some("https://www.google.com/calendar/event?eid=abc".into()),
            ..Event::default()
        };
        assert_eq!(
            web_link(&event, "me@example.com"),
            "https://www.google.com/calendar/event?eid=abc&authuser=me@example.com"
        );
        assert_eq!(web_link(&Event::default(), "me@example.com"), "");
    }

    #[test]
    fn only_calendars_this_account_may_write_to_allow_deleting() {
        let calendar = |role: &str| -> CalendarListEntry {
            serde_json::from_value(serde_json::json!({"id": "c", "accessRole": role})).unwrap()
        };
        assert!(calendar("owner").is_writable());
        assert!(calendar("writer").is_writable());
        assert!(!calendar("reader").is_writable());
        assert!(!calendar("freeBusyReader").is_writable());
    }

    #[test]
    fn only_plain_events_in_writable_calendars_can_be_dragged() {
        let calendar = |role: &str| -> CalendarListEntry {
            serde_json::from_value(serde_json::json!({"id": "c", "accessRole": role})).unwrap()
        };
        let event = |extra: serde_json::Value| -> Event {
            let mut value = serde_json::json!({"id": "e"});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(value).unwrap()
        };
        let owner = calendar("owner");
        assert!(can_move(&event(serde_json::json!({})), &owner));
        assert!(can_move(
            &event(serde_json::json!({"eventType": "default"})),
            &owner
        ));
        assert!(!can_move(
            &event(serde_json::json!({})),
            &calendar("reader")
        ));
        for other in [
            serde_json::json!({"recurrence": ["RRULE:FREQ=DAILY"]}),
            serde_json::json!({"recurringEventId": "m"}),
            serde_json::json!({"attendees": [{"email": "a@example.com"}]}),
            serde_json::json!({"eventType": "birthday"}),
        ] {
            assert!(!can_move(&event(other), &owner));
        }
    }

    #[test]
    fn a_dragged_event_keeps_its_length_and_its_time_of_day() {
        let zone: Tz = "Europe/Prague".parse().unwrap();
        let instant = |text: &str| text.parse::<DateTime<Utc>>().unwrap();
        let timed = |text: &str| EventTime::Timed(instant(text));
        // A day later and half an hour down.
        assert_eq!(
            moved_times(
                timed("2026-11-16T09:00:00Z"),
                timed("2026-11-16T10:00:00Z"),
                1,
                30,
                zone
            ),
            (timed("2026-11-17T09:30:00Z"), timed("2026-11-17T10:30:00Z"))
        );
        // Across the end of summer time (25 October 2026): still 10:00 on the clock.
        assert_eq!(
            moved_times(
                timed("2026-10-24T08:00:00Z"),
                timed("2026-10-24T09:00:00Z"),
                1,
                0,
                zone
            ),
            (timed("2026-10-25T09:00:00Z"), timed("2026-10-25T10:00:00Z"))
        );
        // All-day events move by days only.
        let day = |d: u32| EventTime::AllDay(NaiveDate::from_ymd_opt(2026, 11, d).unwrap());
        assert_eq!(
            moved_times(day(16), day(18), -2, 45, zone),
            (day(14), day(16))
        );

        // The lower edge: down, up, and never above a quarter of an hour.
        let (start, end) = (timed("2026-11-16T09:00:00Z"), timed("2026-11-16T10:00:00Z"));
        assert_eq!(
            resized_end(start, end, 30, zone),
            Some(timed("2026-11-16T10:30:00Z"))
        );
        assert_eq!(
            resized_end(start, end, -30, zone),
            Some(timed("2026-11-16T09:30:00Z"))
        );
        assert_eq!(
            resized_end(start, end, -120, zone),
            Some(timed("2026-11-16T09:15:00Z"))
        );
        assert_eq!(resized_end(day(16), day(17), 30, zone), None);
    }

    #[test]
    fn new_times_are_sent_in_the_zone_the_event_had() {
        let instant = |text: &str| EventTime::Timed(text.parse::<DateTime<Utc>>().unwrap());
        let resource = serde_json::json!({
            "start": {"dateTime": "2026-11-16T10:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-16T11:00:00+01:00"}
        });
        assert_eq!(
            time_changes(
                &resource,
                instant("2026-11-17T09:30:00Z"),
                instant("2026-11-17T10:30:00Z")
            ),
            serde_json::json!({
                "start": {"dateTime": "2026-11-17T09:30:00Z", "timeZone": "Europe/Prague"},
                "end": {"dateTime": "2026-11-17T10:30:00Z"}
            })
        );
        let day = |d: u32| EventTime::AllDay(NaiveDate::from_ymd_opt(2026, 11, d).unwrap());
        assert_eq!(
            time_changes(&serde_json::json!({}), day(16), day(17)),
            serde_json::json!({"start": {"date": "2026-11-16"}, "end": {"date": "2026-11-17"}})
        );
    }

    #[test]
    fn a_deletion_is_planned_with_its_way_back() {
        use serde_json::json;
        let instant = |text: &str| EventTime::Timed(text.parse::<DateTime<Utc>>().unwrap());
        let target = |event_id: &str, occurrence: Option<Occurrence>| Target {
            account: 1,
            calendar_id: "c".into(),
            event_id: event_id.into(),
            etag: Some("\"own\"".into()),
            occurrence,
            movable: false,
        };
        let series = json!({
            "id": "m", "etag": "\"series\"",
            "start": {"dateTime": "2026-11-16T10:00:00Z"},
            "end": {"dateTime": "2026-11-16T11:00:00Z"},
            "recurrence": ["RRULE:FREQ=DAILY;COUNT=5"]
        });
        let third = Occurrence {
            series_id: "m".into(),
            original_start: instant("2026-11-18T10:00:00Z"),
            changed: false,
        };

        // An ordinary event: deleted as the version shown, recreated to undo.
        let plain = json!({"id": "e", "summary": "Lunch"});
        assert_eq!(
            plan_deletion(&target("e", None), Scope::This, Some(plain.clone()), None),
            Some((
                Deletion::Event {
                    event_id: "e".into(),
                    etag: Some("\"own\"".into())
                },
                UndoKind::Recreate(plain),
            ))
        );
        // Gone from the cache: nothing to do.
        assert_eq!(
            plan_deletion(&target("e", None), Scope::This, None, None),
            None
        );

        // One occurrence: found by its original start, or by its own identifier when it
        // is a changed one.
        assert_eq!(
            plan_deletion(
                &target("m", Some(third.clone())),
                Scope::This,
                None,
                Some(series.clone())
            ),
            Some((
                Deletion::Occurrence {
                    series_id: "m".into(),
                    original_start: third.original_start,
                    known_id: None,
                },
                UndoKind::Occurrence,
            ))
        );
        let changed = Occurrence {
            changed: true,
            ..third.clone()
        };
        assert!(matches!(
            plan_deletion(&target("m_x", Some(changed)), Scope::This, None, Some(series.clone())),
            Some((Deletion::Occurrence { known_id: Some(id), .. }, _)) if id == "m_x"
        ));

        // This and following: the series ends before it, and gets its rule back to undo.
        assert_eq!(
            plan_deletion(
                &target("m", Some(third.clone())),
                Scope::Following,
                None,
                Some(series.clone())
            ),
            Some((
                Deletion::Following {
                    series_id: "m".into(),
                    recurrence: vec!["RRULE:FREQ=DAILY;UNTIL=20261118T095959Z".into()],
                    etag: Some("\"series\"".into()),
                },
                UndoKind::Patch {
                    event_id: "m".into(),
                    changes: json!({"recurrence": ["RRULE:FREQ=DAILY;COUNT=5"]}),
                },
            ))
        );

        // All, and "following" from the first occurrence: the series itself.
        let whole = Some((
            Deletion::Event {
                event_id: "m".into(),
                etag: Some("\"series\"".into()),
            },
            UndoKind::Recreate(series.clone()),
        ));
        assert_eq!(
            plan_deletion(
                &target("m", Some(third)),
                Scope::All,
                None,
                Some(series.clone())
            ),
            whole
        );
        let first = Occurrence {
            series_id: "m".into(),
            original_start: instant("2026-11-16T10:00:00Z"),
            changed: false,
        };
        assert_eq!(
            plan_deletion(
                &target("m", Some(first)),
                Scope::Following,
                None,
                Some(series)
            ),
            whole
        );
    }

    #[test]
    fn a_new_event_lasts_an_hour_from_the_time_clicked_or_the_whole_day() {
        let zone = chrono_tz::Europe::Prague;
        let day = date("2026-11-17");
        let (start, end) = draft_times(day, 9 * 60 + 30, false, zone);
        assert_eq!(
            start,
            EventTime::Timed("2026-11-17T08:30:00Z".parse().unwrap())
        );
        assert_eq!(
            end,
            EventTime::Timed("2026-11-17T09:30:00Z".parse().unwrap())
        );
        assert_eq!(
            event_resource(" Lunch ", start, end, zone),
            serde_json::json!({
                "summary": "Lunch",
                "start": {"dateTime": "2026-11-17T09:30:00+01:00", "timeZone": "Europe/Prague"},
                "end": {"dateTime": "2026-11-17T10:30:00+01:00", "timeZone": "Europe/Prague"},
            })
        );

        let (start, end) = draft_times(day, 0, true, zone);
        assert_eq!(
            event_resource("Trip", start, end, zone),
            serde_json::json!({
                "summary": "Trip",
                "start": {"date": "2026-11-17"},
                "end": {"date": "2026-11-18"},
            })
        );

        // 02:30 does not exist on the night clocks go forward.
        let (start, _) = draft_times(date("2027-03-28"), 150, false, zone);
        assert!(matches!(start, EventTime::Timed(_)));
    }

    #[test]
    fn patch_changes_only_what_differs() {
        let model = VecModel::from(vec![1, 2, 3, 4]);
        patch(&model, vec![1, 5, 3]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5, 3]);
        patch(&model, vec![1, 5, 3, 9]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5, 3, 9]);
    }
}
