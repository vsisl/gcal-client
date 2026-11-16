//! The window: turns the cache into what the week view shows and wires up its controls.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use gcal_core::month::{MonthLayout, layout_month};
use gcal_core::recurrence::{
    Series, end_series_before, occurrences_before, remaining_series, shift_weekdays,
    single_instance, split_dates,
};
use gcal_core::schedule::{DayPart, schedule};
use gcal_core::text::plain_text;
use gcal_core::week::{WeekLayout, layout_days, local_day_start};
use gcal_core::{CalendarListEntry, Event, EventTime, ResponseStatus};
use gcal_store::{Account, AccountId, Store, StoredCalendar};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::editor::{self, Form, Kept};
use crate::open_store;
use crate::worker::{self, Command, Deletion, Edit};
use tokio::sync::mpsc::UnboundedSender;

slint::include_modules!();

/// How often changes are fetched from Google while the window is open. (Google can only
/// push changes to a public web server, so a desktop app has to ask.)
const SYNC_INTERVAL: StdDuration = StdDuration::from_secs(5 * 60);
/// The setting that remembers which calendar the previous new event went to.
/// How often to look for Google again while it cannot be reached.
const OFFLINE_RETRY: StdDuration = StdDuration::from_secs(30);
const NEW_EVENT_CALENDAR: &str = "new_event_calendar";
/// Setting: light or dark, as the number the window uses (0 as the desktop, 1 light,
/// 2 dark).
const THEME: &str = "theme";
/// Settings: the first day of the week (0 Monday to 6 Sunday), whether times are shown
/// on the 12-hour clock (1) and the view that was open last (its place in `Span::ALL`).
const WEEK_START: &str = "week_start";
const TWELVE_HOUR: &str = "twelve_hour";
const LAST_VIEW: &str = "last_view";
/// Setting: how many days the custom view shows.
const CUSTOM_DAYS: &str = "custom_days";
/// Setting: the height of an hour in the day columns, in pixels (the zoom).
const HOUR_HEIGHT: &str = "hour_height";
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

/// The date someone typed into "go to date", if it can be read as one: `2026-11-17`,
/// `17.11.2026`, `17/11/2026`, `17.11.` (this year), or today, tomorrow, yesterday.
fn typed_date(text: &str, today: NaiveDate) -> Option<NaiveDate> {
    let text = text.trim().to_lowercase();
    match text.as_str() {
        "today" => return Some(today),
        "tomorrow" => return Some(today + Duration::days(1)),
        "yesterday" => return Some(today - Duration::days(1)),
        _ => {}
    }
    let parts: Vec<&str> = text
        .split(['.', '/', '-', ' '])
        .filter(|part| !part.is_empty())
        .collect();
    let number = |part: &&str| part.parse::<u32>().ok();
    let date = match parts.as_slice() {
        [day, month] => NaiveDate::from_ymd_opt(today.year(), number(month)?, number(day)?),
        // A year of four digits in front: year, month, day.
        [year, month, day] if year.len() == 4 => {
            NaiveDate::from_ymd_opt(year.parse().ok()?, number(month)?, number(day)?)
        }
        // Else day first, as in most of the world.
        [day, month, year] => {
            let year = year.parse::<i32>().ok()?;
            // "26" means 2026.
            let year = if (0..100).contains(&year) {
                2000 + year
            } else {
                year
            };
            NaiveDate::from_ymd_opt(year, number(month)?, number(day)?)
        }
        _ => None,
    }?;
    // Only what the calendar can show; see `shown_dates`.
    (date == shown_dates(date)).then_some(date)
}

/// The day nearest to `day` that the calendar shows: nothing before 1900 or after 2200.
/// Dates near the limits of what the date type can hold would overflow when the views
/// work out the days around them.
fn shown_dates(day: NaiveDate) -> NaiveDate {
    let first = NaiveDate::from_ymd_opt(1900, 1, 1).unwrap_or_default();
    let last = NaiveDate::from_ymd_opt(2200, 12, 31).unwrap_or_default();
    day.clamp(first, last)
}

/// How dates and times are shown, as chosen in the settings.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Prefs {
    /// The first day of the week, counted from Monday (0) to Sunday (6).
    week_start: u32,
    twelve_hour: bool,
    /// How many days the custom view shows: 2 to 7.
    custom_days: i64,
}

thread_local! {
    /// The window's thread is the only one that formats dates for the screen, and it
    /// does so in dozens of places far from the window's state; the choice is kept
    /// here for them rather than handed through every one. Tests, each on a thread of
    /// its own, see the defaults: Monday and the 24-hour clock.
    static PREFS: Cell<Prefs> = const {
        Cell::new(Prefs {
            week_start: 0,
            twelve_hour: false,
            custom_days: 4,
        })
    };
}

fn prefs() -> Prefs {
    PREFS.with(Cell::get)
}

/// The first day of the week `day` is in, by the setting.
fn week_start_of(day: NaiveDate) -> NaiveDate {
    week_start_from(day, prefs().week_start)
}

/// The day on or before `day` that weeks start on; `first` counts from Monday (0).
fn week_start_from(day: NaiveDate, first: u32) -> NaiveDate {
    let into_week = (day.weekday().num_days_from_monday() + 7 - first % 7) % 7;
    day - Duration::days(i64::from(into_week))
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
    /// Twelve small months, for finding a date; no events.
    Year,
}

impl Span {
    /// The order used by the interface's view switcher and the number keys, as on the web.
    const ALL: [Span; 6] = [
        Span::Day,
        Span::Week,
        Span::Month,
        Span::FourDays,
        Span::Schedule,
        Span::Year,
    ];

    /// Whether the view is the one with a column per day.
    fn has_columns(self) -> bool {
        !matches!(self, Span::Month | Span::Schedule | Span::Year)
    }

    /// The first day shown and the number of days, when the user is looking at `anchor`.
    /// The week view snaps to the start of the week, the month view shows the whole weeks
    /// that cover the month, and the others begin on the day itself.
    fn range(self, anchor: NaiveDate) -> (NaiveDate, i64) {
        match self {
            Span::Day => (anchor, 1),
            Span::FourDays => (anchor, prefs().custom_days),
            Span::Schedule => (anchor, SCHEDULE_DAYS),
            Span::Week => (week_start_of(anchor), 7),
            Span::Year => {
                let first = anchor.with_ordinal(1).unwrap_or(anchor);
                let next = NaiveDate::from_ymd_opt(anchor.year() + 1, 1, 1).unwrap_or(first);
                (first, (next - first).num_days())
            }
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
            // The same day a year on, or the last of February for the 29th.
            Span::Year if direction < 0 => anchor - Months::new(12),
            Span::Year => anchor + Months::new(12),
            _ => anchor + Duration::days(direction * self.range(anchor).1),
        }
    }

    /// The text next to the navigation buttons.
    fn title(self, anchor: NaiveDate) -> String {
        match self {
            Span::Month => anchor.format("%B %Y").to_string(),
            Span::Year => anchor.year().to_string(),
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
/// For an event made in another time zone than the one times are shown in: its times
/// on its own clock, as "10:00 – 11:00 in Asia/Tokyo". Empty otherwise.
fn own_zone_text(event: &Event, start: EventTime, end: EventTime, shown_in: Tz) -> String {
    let (EventTime::Timed(start), EventTime::Timed(end)) = (start, end) else {
        return String::new();
    };
    let Some(own) = event
        .start
        .as_ref()
        .and_then(|start| start.time_zone.as_deref())
        .and_then(|name| name.parse::<Tz>().ok())
    else {
        return String::new();
    };
    // Zones with other names may still keep the same time (Prague and Zurich).
    let offset = |zone: Tz, at: DateTime<Utc>| at.with_timezone(&zone).naive_local();
    if offset(own, start) == offset(shown_in, start) && offset(own, end) == offset(shown_in, end) {
        return String::new();
    }
    // The day is said wherever it is not the one the main line names: the start's when
    // it falls on another day there, the end's when the event crosses midnight there.
    let day = |at: DateTime<Utc>| at.with_timezone(&own).format("%-d %b ").to_string();
    let date = |zone: Tz, at: DateTime<Utc>| at.with_timezone(&zone).date_naive();
    let start_day = if date(own, start) == date(shown_in, start) {
        String::new()
    } else {
        day(start)
    };
    let end_day = if date(own, end) == date(own, start) {
        String::new()
    } else {
        day(end)
    };
    format!(
        "{start_day}{} – {end_day}{} in {}",
        clock(start, own),
        clock(end, own),
        own.name().replace('_', " ")
    )
}

/// The address that organises events created in a calendar, if it is the account's:
/// that is so in the account's own calendar, whose identifier is the account's address.
/// Events in any other calendar (another of the account's, or one shared by someone
/// else) are organised by that calendar, and the account is not to invite itself.
fn organiser_address(account_email: &str, calendar_id: &str) -> String {
    if account_email.eq_ignore_ascii_case(calendar_id) {
        account_email.to_owned()
    } else {
        String::new()
    }
}

/// Whether the account itself is invited and can answer here: only in its own
/// calendar. In a calendar shared by someone else, or a room's, the entry Google marks
/// as "self" is that calendar's owner, and answering would answer for them.
fn can_answer(event: &Event, calendar: &CalendarListEntry) -> bool {
    calendar.primary
        && calendar.is_writable()
        && event.own_invitation().is_some_and(|own| !own.resource)
}

/// A guest's answer as the window counts it: 0 none yet, 1 yes, 2 no, 3 maybe.
fn answer_number(answer: ResponseStatus) -> i32 {
    match answer {
        ResponseStatus::NeedsAction | ResponseStatus::Unknown => 0,
        ResponseStatus::Accepted => 1,
        ResponseStatus::Declined => 2,
        ResponseStatus::Tentative => 3,
    }
}

/// What to send to answer the invitation in `resource` for this account, and what
/// takes it back: the list of guests with only this account's answer changed. `None`
/// if the account is not among the guests or has given this answer already.
fn rsvp_changes(
    resource: &serde_json::Value,
    answer: ResponseStatus,
) -> Option<(serde_json::Value, serde_json::Value)> {
    let before = resource.get("attendees")?.as_array()?;
    let mut after = before.clone();
    let own = after
        .iter_mut()
        .find(|guest| guest["self"].as_bool() == Some(true))?;
    if own["responseStatus"].as_str() == Some(answer.as_str()) {
        return None;
    }
    own["responseStatus"] = answer.as_str().into();
    Some((
        serde_json::json!({ "attendees": after }),
        serde_json::json!({ "attendees": before }),
    ))
}

/// What to send to answer an invitation, and how to take it back. An occurrence of a
/// series that was not changed on its own is answered alone (`Scope::This`, which
/// makes it a changed one) or with its whole series; anything else answers itself.
/// `None` if there is nothing to answer or the answer is given already.
fn plan_rsvp(
    target: &Target,
    answer: ResponseStatus,
    scope: Scope,
    own: Option<serde_json::Value>,
    series: Option<serde_json::Value>,
) -> Option<(Edit, UndoKind)> {
    let unchanged = target.occurrence.as_ref().filter(|o| !o.changed);
    let resource = if unchanged.is_some() { series } else { own }?;
    if let (Some(occurrence), Scope::This) = (unchanged, scope) {
        // The occurrence has the guests of its series until it is changed. No "already
        // given" short cut here: the series' answer says nothing sent for this event.
        let before = resource.get("attendees")?.as_array()?;
        let mut after = before.clone();
        let own = after
            .iter_mut()
            .find(|guest| guest["self"].as_bool() == Some(true))?;
        own["responseStatus"] = answer.as_str().into();
        return Some((
            Edit::Occurrence {
                series_id: occurrence.series_id.clone(),
                original_start: occurrence.original_start,
                known_id: None,
                etag: None,
                changes: serde_json::json!({ "attendees": after }),
            },
            UndoKind::Occurrence(serde_json::json!({ "attendees": before })),
        ));
    }
    let event_id = resource["id"].as_str()?.to_owned();
    let (changes, before) = rsvp_changes(&resource, answer)?;
    Some((
        Edit::Event {
            event_id: event_id.clone(),
            etag: resource["etag"].as_str().map(str::to_owned),
            changes,
        },
        UndoKind::Patch {
            event_id,
            changes: before,
        },
    ))
}

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
    clock_text(local.hour(), local.minute(), prefs().twelve_hour)
}

/// A time of day as 14:05, or as the web writes the 12-hour clock: 2:05pm, 2pm.
fn clock_text(hour: u32, minute: u32, twelve_hour: bool) -> String {
    if !twelve_hour {
        return format!("{hour}:{minute:02}");
    }
    let half = if hour < 12 { "am" } else { "pm" };
    let hour = match hour % 12 {
        0 => 12,
        hour => hour,
    };
    if minute == 0 {
        format!("{hour}{half}")
    } else {
        format!("{hour}:{minute:02}{half}")
    }
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
    /// Whether this account invited guests to it, who can be told of a change.
    guests: bool,
    /// Whether this account is invited to it and can answer.
    can_answer: bool,
    /// When this instance starts and ends.
    start: EventTime,
    end: EventTime,
}

/// A drag of a repeating event, waiting for the answer to "this, following or all?".
#[derive(Clone, Copy)]
struct Drag {
    instance: usize,
    days: i64,
    minutes: i64,
    /// Whether only the lower edge was dragged.
    resize: bool,
}

impl Drag {
    /// The times an event has after this drag, given the ones it had.
    fn apply(self, start: EventTime, end: EventTime, zone: Tz) -> Option<(EventTime, EventTime)> {
        if self.resize {
            Some((start, resized_end(start, end, self.minutes, zone)?))
        } else {
            Some(moved_times(start, end, self.days, self.minutes, zone))
        }
    }
}

/// What to send for a drag of `target` and how to take it back, if it can be. `own` and
/// `series` are the cached resources of the event and of the series it belongs to.
fn plan_drag(
    target: &Target,
    scope: Scope,
    drag: Drag,
    own: Option<serde_json::Value>,
    series: Option<serde_json::Value>,
    zone: Tz,
) -> Option<(Edit, Option<UndoKind>)> {
    let times = |resource: &serde_json::Value| serde_json::json!({ "start": resource["start"], "end": resource["end"] });
    let Some(occurrence) = &target.occurrence else {
        let own = own?;
        let event: Event = serde_json::from_value(own.clone()).ok()?;
        let (start, end) = drag.apply(event.start_time()?, event.end_time()?, zone)?;
        return Some((
            Edit::Event {
                event_id: target.event_id.clone(),
                etag: target.etag.clone(),
                changes: time_changes(&own, start, end),
            },
            Some(UndoKind::Patch {
                event_id: target.event_id.clone(),
                changes: times(&own),
            }),
        ));
    };
    let (start, end) = drag.apply(target.start, target.end, zone)?;
    if scope == Scope::This {
        // A changed occurrence is an event of its own; an unchanged one takes its
        // time zone from the series.
        let resource = if occurrence.changed { own? } else { series? };
        return Some((
            Edit::Occurrence {
                series_id: occurrence.series_id.clone(),
                original_start: occurrence.original_start,
                known_id: occurrence.changed.then(|| target.event_id.clone()),
                etag: target.etag.clone().filter(|_| occurrence.changed),
                changes: time_changes(&resource, start, end),
            },
            Some(UndoKind::Occurrence(time_changes(
                &resource,
                target.start,
                target.end,
            ))),
        ));
    }
    let resource = series?;
    let master: Event = serde_json::from_value(resource.clone()).ok()?;
    if scope == Scope::Following && master.start_time() != Some(occurrence.original_start) {
        // The series is split: it ends before this occurrence, and a copy of it starts
        // at the occurrence's new time with what was left of its rule.
        let earlier = occurrences_before(&master, occurrence.original_start);
        let left = split_dates(
            &remaining_series(&master.recurrence, earlier),
            occurrence.original_start,
            true,
        );
        let rules = shift_weekdays(&left, drag.days);
        let mut copy = resource.clone();
        let new_times = time_changes(&resource, start, end);
        copy["start"] = new_times["start"].clone();
        copy["end"] = new_times["end"].clone();
        copy["recurrence"] = serde_json::json!(rules);
        return Some((
            Edit::Following {
                series_id: master.id.clone(),
                etag: master.etag.clone(),
                recurrence: end_series_before(&master.recurrence, occurrence.original_start),
                old_recurrence: master.recurrence.clone(),
                resource: copy,
            },
            // Taking this back would mean deleting the new series and restoring the
            // old one's rule; not offered yet.
            None,
        ));
    }
    // All: the series itself moves by as much as the occurrence was dragged.
    let (start, end) = drag.apply(master.start_time()?, master.end_time()?, zone)?;
    let mut changes = time_changes(&resource, start, end);
    changes["recurrence"] = serde_json::json!(shift_weekdays(&master.recurrence, drag.days));
    let mut before = times(&resource);
    before["recurrence"] = serde_json::json!(master.recurrence);
    Some((
        Edit::Event {
            event_id: master.id.clone(),
            etag: master.etag,
            changes,
        },
        Some(UndoKind::Patch {
            event_id: master.id,
            changes: before,
        }),
    ))
}

/// Whether the event can be changed in the app: dragged, resized, opened in the
/// editor. An event with guests only by the account that invited them (a guest's copy
/// would drift from the organiser's); never the special kinds of entry (birthdays, ...).
fn can_move(event: &Event, calendar: &CalendarListEntry) -> bool {
    calendar.is_writable()
        && matches!(event.event_type.as_deref(), None | Some("default"))
        && (event.attendees.is_empty() || event.has_guests_to_notify())
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
                etag: target.etag.clone().filter(|_| occurrence.changed),
            },
            // Google keeps a deleted occurrence as a cancelled event.
            UndoKind::Occurrence(serde_json::json!({ "status": "confirmed" })),
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

/// A change to a repeating event that waits for "this, following or all?".
enum Asked {
    Drag(Drag),
    Form(Box<Saved>),
    /// A new event with guests, waiting for "send them e-mails?".
    Create(Box<NewEvent>),
    /// An answer to an invitation to a repeating event, waiting for "this event or
    /// all?". It carries the event, not its place on screen.
    Answer(Box<Target>, ResponseStatus),
}

/// A new event ready to be created.
struct NewEvent {
    account: AccountId,
    calendar_id: String,
    resource: serde_json::Value,
    /// What it was made from, to give back if creating it fails.
    form: Form,
    calendar: usize,
    draft: Draft,
}

/// The editor while it is open: the event it is for (`None` for a new one), and the
/// form as it was opened, to tell what the user changed.
#[derive(Clone)]
struct Editing {
    target: Option<Target>,
    opened: Form,
    kept: Kept,
    /// The event, and the series it belongs to, as they were when the editor opened.
    /// What is saved is a change to these versions: should a sync bring newer ones
    /// while the editor is open, Google refuses the change rather than have it
    /// overwrite something the user never saw.
    own: Option<serde_json::Value>,
    series: Option<serde_json::Value>,
}

/// What the editor was closed with.
struct Saved {
    target: Target,
    form: Form,
    opened: Form,
    kept: Kept,
    /// The fields the user changed, with their new values.
    changes: serde_json::Value,
    /// All fields as the form was opened, and as it was saved.
    before: serde_json::Value,
    after: serde_json::Value,
    /// The event and its series as they were when the editor opened.
    own: Option<serde_json::Value>,
    series: Option<serde_json::Value>,
}

fn to_window(form: &Form) -> EditorForm {
    EditorForm {
        title: form.title.as_str().into(),
        all_day: form.all_day,
        start_date: form.start_date.as_str().into(),
        end_date: form.end_date.as_str().into(),
        start_time: form.start_time.as_str().into(),
        end_time: form.end_time.as_str().into(),
        repeat: form.repeat as i32,
        location: form.location.as_str().into(),
        description: form.description.as_str().into(),
        color: form.color as i32,
        reminder: form.reminder as i32,
        visibility: form.visibility as i32,
        busy: form.busy,
        guests: form.guests.as_str().into(),
        meet: form.meet,
        may_modify: form.may_modify,
        may_invite: form.may_invite,
        may_see: form.may_see,
    }
}

fn from_window(form: &EditorForm) -> Form {
    Form {
        title: form.title.to_string(),
        all_day: form.all_day,
        start_date: form.start_date.to_string(),
        end_date: form.end_date.to_string(),
        start_time: form.start_time.to_string(),
        end_time: form.end_time.to_string(),
        repeat: form.repeat.max(0) as usize,
        location: form.location.to_string(),
        description: form.description.to_string(),
        color: form.color.max(0) as usize,
        reminder: form.reminder.max(0) as usize,
        visibility: form.visibility.max(0) as usize,
        busy: form.busy,
        guests: form.guests.to_string(),
        meet: form.meet,
        may_modify: form.may_modify,
        may_invite: form.may_invite,
        may_see: form.may_see,
    }
}

/// What to send for the editor's changes to `saved.target` and how to take them back,
/// if that is offered. `Ok(None)` when there is nothing to send.
fn plan_form(
    saved: &Saved,
    scope: Scope,
    own: Option<serde_json::Value>,
    series: Option<serde_json::Value>,
    zone: Tz,
) -> Result<Option<(Edit, Option<UndoKind>)>, String> {
    let target = &saved.target;
    let Some(occurrence) = &target.occurrence else {
        let Some(own) = own else { return Ok(None) };
        return Ok(Some((
            Edit::Event {
                event_id: target.event_id.clone(),
                etag: target.etag.clone(),
                changes: saved.changes.clone(),
            },
            Some(UndoKind::Patch {
                event_id: target.event_id.clone(),
                changes: editor::inverse(&saved.changes, &own),
            }),
        )));
    };
    if scope == Scope::This {
        // One occurrence cannot repeat differently from its series.
        let mut changes = saved.changes.clone();
        if let Some(changes) = changes.as_object_mut() {
            changes.remove("recurrence");
            if changes.is_empty() {
                return Ok(None);
            }
        }
        let Some(resource) = (if occurrence.changed { own } else { series }) else {
            return Ok(None);
        };
        // What it had comes from the event, except its times, which are its own.
        let mut was = editor::inverse(&changes, &resource);
        for time in ["start", "end"] {
            if changes.get(time).is_some() {
                was[time] = saved.before[time].clone();
            }
        }
        return Ok(Some((
            Edit::Occurrence {
                series_id: occurrence.series_id.clone(),
                original_start: occurrence.original_start,
                known_id: occurrence.changed.then(|| target.event_id.clone()),
                etag: target.etag.clone().filter(|_| occurrence.changed),
                changes,
            },
            Some(UndoKind::Occurrence(was)),
        )));
    }
    let Some(resource) = series else {
        return Ok(None);
    };
    let Ok(master) = serde_json::from_value::<Event>(resource.clone()) else {
        return Ok(None);
    };
    // The rule the form keeps is the series' own, also when a changed occurrence
    // (which has none) was opened.
    let kept = Kept {
        recurrence: master.recurrence.clone(),
        ..saved.kept.clone()
    };
    if scope == Scope::Following && master.start_time() != Some(occurrence.original_start) {
        let earlier = occurrences_before(&master, occurrence.original_start);
        let days = (editor::start_day(&saved.form)? - editor::start_day(&saved.opened)?).num_days();
        return Ok(Some((
            Edit::Following {
                series_id: master.id.clone(),
                etag: master.etag.clone(),
                recurrence: end_series_before(&master.recurrence, occurrence.original_start),
                old_recurrence: master.recurrence.clone(),
                resource: editor::following_series(
                    &resource,
                    &saved.changes,
                    &saved.after,
                    earlier,
                    days,
                    occurrence.original_start,
                ),
            },
            None,
        )));
    }
    let changes = editor::for_series(
        &saved.changes,
        &saved.before,
        &saved.after,
        &kept,
        &master,
        zone,
    )?;
    Ok(Some((
        Edit::Event {
            event_id: master.id.clone(),
            etag: master.etag,
            changes: changes.clone(),
        },
        Some(UndoKind::Patch {
            event_id: master.id,
            changes: editor::inverse(&changes, &resource),
        }),
    )))
}

/// What undoing a change needs.
struct Undo {
    account: AccountId,
    calendar_id: String,
    kind: UndoKind,
    /// The version the change left the event in, once the worker has said. The undo
    /// is then made only if nothing else changed the event since.
    etag: Option<String>,
}

/// A change sent to the worker and not answered yet. The worker makes changes one at
/// a time in the order they were sent, so its answers match these in order.
struct Pending {
    undo: Option<Undo>,
    /// For a new event: what to show again should it not be created.
    retry: Option<Retry>,
}

/// A new event as it was sent, to give back what was typed if creating it fails.
struct Retry {
    form: Form,
    calendar: usize,
    draft: Draft,
}

#[derive(Debug, PartialEq)]
enum UndoKind {
    /// Create the event again from the resource it had. For a series this brings back
    /// the series, not the changes that had been made to single occurrences.
    Recreate(serde_json::Value),
    /// Delete an event that was just created.
    Delete(String),
    /// Give an event some of its fields back.
    Patch {
        event_id: String,
        changes: serde_json::Value,
    },
    /// Give one occurrence of a series these fields back; which event that is, the
    /// worker tells once it has changed or deleted it.
    Occurrence(serde_json::Value),
}

/// A new event being typed in.
#[derive(Clone)]
struct Draft {
    start: EventTime,
    end: EventTime,
    /// The calendars on offer, in the order shown: account, calendar, and the text under
    /// which the choice is remembered.
    calendars: Vec<(AccountId, String, String)>,
}

/// When a new event starts and ends: `length` minutes from the time clicked, or the
/// whole day.
fn draft_times(
    day: NaiveDate,
    minute: i32,
    all_day: bool,
    length: i32,
    zone: Tz,
) -> (EventTime, EventTime) {
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
        EventTime::Timed(start + Duration::minutes(i64::from(length))),
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
    /// A change to a repeating event while the window asks how much of the series it is
    /// meant for.
    asked: RefCell<Option<Asked>>,
    /// Changes the worker has not answered yet, oldest first.
    pending: RefCell<VecDeque<Pending>>,
    /// Counts the redraws from the cache. The window notes it when a drag starts and
    /// drops the drag if it has changed by the end: the dragged block may by then stand
    /// for another event.
    generation: Cell<i32>,
    /// The event the editor is open for.
    editing: RefCell<Option<Editing>>,
    /// Hides the message about the last change after a while.
    toast_timer: Timer,
    commands: UnboundedSender<Command>,
    all_day: Rc<VecModel<AllDayItem>>,
    month_cells: Rc<VecModel<MonthCell>>,
    /// For each day of the month view, all its events: shown as a list when "N more"
    /// is chosen.
    day_lists: RefCell<Vec<Vec<ScheduleRow>>>,
    year_days: Rc<VecModel<MiniDay>>,
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

    /// Shows the settings for dates and times in the window: the names of the weekdays
    /// in the week's order, the hours' labels, and the choices in the settings card.
    fn apply_prefs(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Prefs {
            week_start,
            twelve_hour,
            custom_days,
        } = prefs();
        ui.set_custom_days(custom_days as i32);
        ui.set_custom_days_choice(custom_days as i32 - 2);
        // 2026-11-16 is a Monday.
        let monday = NaiveDate::from_ymd_opt(2026, 11, 16).unwrap_or_default();
        let names: Vec<SharedString> = (0..7)
            .map(|offset| {
                let day = monday + Duration::days(i64::from(week_start) + offset);
                SharedString::from(day.format("%a").to_string().to_uppercase())
            })
            .collect();
        ui.set_weekday_names(ModelRc::from(Rc::new(VecModel::from(names))));
        let hours: Vec<SharedString> = (0..24)
            .map(|hour| {
                if hour == 0 {
                    SharedString::new()
                } else {
                    clock_text(hour, 0, twelve_hour).into()
                }
            })
            .collect();
        ui.set_hour_labels(ModelRc::from(Rc::new(VecModel::from(hours))));
        ui.set_week_start_choice(match week_start {
            6 => 1,
            5 => 2,
            _ => 0,
        });
        ui.set_clock_choice(i32::from(twelve_hour));
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
        let is_year = span == Span::Year;
        let today = self.today();
        ui.set_view_index(Span::ALL.iter().position(|s| *s == span).unwrap_or(0) as i32);
        self.drawn_today.set(Some(today));
        ui.set_period_title(span.title(self.anchor.get()).into());
        if is_month {
            ui.set_month_weeks((days / 7) as i32);
        } else if is_year {
            self.update_year(ui, today);
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
        // The year shows no events, so none are looked up for it.
        let shown_days = if is_year { 0 } else { days };
        let to = local_day_start(week_start + Duration::days(shown_days), self.zone);
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
        let layout = if !span.has_columns() {
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

        // Every event of each day of the month view, for the list behind "N more".
        let mut day_lists: Vec<Vec<ScheduleRow>> = Vec::new();
        if is_month {
            day_lists.resize(days as usize, Vec::new());
            for entry in schedule(&spans, week_start, days, self.zone) {
                let day = week_start + Duration::days(i64::from(entry.day));
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
                if let Some(list) = day_lists.get_mut(entry.day as usize) {
                    list.push(ScheduleRow {
                        instance: entry.index as i32,
                        first_of_day: list.is_empty(),
                        day_number: day.day().to_string().into(),
                        day_name: day.format("%A").to_string().into(),
                        today: day == today,
                        time: time.into(),
                        title: title(entry.index),
                        color: color(entry.index),
                    });
                }
            }
        }
        self.day_lists.replace(day_lists);
        ui.set_day_list_visible(false);

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
                    zone: own_zone_text(event, instance.start, instance.end, self.zone).into(),
                    location: event.location.as_deref().unwrap_or("").into(),
                    description: plain_text(event.description.as_deref().unwrap_or("")).into(),
                    calendar: calendar.entry.display_name().into(),
                    account: email.into(),
                    color: color(index),
                    link: web_link(event, email).into(),
                    // Anything in a calendar this account may write to. Should Google
                    // refuse a particular entry, it says why.
                    can_delete: calendar.entry.is_writable(),
                    can_edit: can_move(event, &calendar.entry),
                    repeating: event.is_recurring_master() || event.recurring_event_id.is_some(),
                    notify_guests: event.has_guests_to_notify(),
                    guests: ModelRc::from(Rc::new(VecModel::from(
                        event
                            .attendees
                            .iter()
                            .map(|guest| Guest {
                                name: guest.name().into(),
                                status: answer_number(guest.response_status),
                                organizer: guest.organizer,
                                optional: guest.optional,
                            })
                            .collect::<Vec<_>>(),
                    ))),
                    meet: event.hangout_link.as_deref().unwrap_or("").into(),
                    // An answer is given in the account's own calendar.
                    rsvp: match event.own_invitation() {
                        Some(own) if can_answer(event, &calendar.entry) => {
                            answer_number(own.response_status)
                        }
                        _ => -1,
                    },
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
                    guests: instance.event.has_guests_to_notify(),
                    can_answer: can_answer(instance.event, &calendar.entry),
                    start: instance.start,
                    end: instance.end,
                }
            })
            .collect();
        self.targets.replace(targets);
        self.generation.set(self.generation.get().wrapping_add(1));
        ui.set_generation(self.generation.get());
        // What they described may have moved or gone. The question about a change from
        // the editor stays: that change carries its event with it.
        ui.set_detail_visible(false);
        if !matches!(
            *self.asked.borrow(),
            Some(Asked::Form(_) | Asked::Create(_))
        ) {
            ui.set_delete_ask_visible(false);
        }
        // A drag refers to an instance on screen, which may now be another one. An
        // edit from the editor carries its event with it and stays.
        if matches!(
            *self.asked.borrow(),
            Some(Asked::Drag(_) | Asked::Answer(..))
        ) {
            self.asked.take();
        }
        Ok(())
    }

    /// Moves the current-time line.
    fn update_now(&self, ui: &AppWindow) {
        let now = Utc::now().with_timezone(&self.zone);
        let (first, days) = self.range();
        let offset = (now.date_naive() - first).num_days();
        let has_columns = self.span.get().has_columns();
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
        let anchor = shown_dates(anchor);
        self.anchor.set(anchor);
        self.mini_month.set(anchor);
        self.refresh();
    }

    /// Changes the view and remembers it: the app opens on the view it was closed with.
    fn set_span(&self, span: Span) {
        self.span.set(span);
        if let Some(index) = Span::ALL.iter().position(|s| *s == span) {
            let _ = self.store.set_setting(LAST_VIEW, &index.to_string());
        }
    }

    /// Draws the sidebar's small month: always six weeks, so its height never changes.
    fn update_mini_month(&self, ui: &AppWindow) {
        let month = self.mini_month.get();
        let (first, _) = Span::Month.range(month);
        let (shown_first, shown_days) = self.range();
        let today = self.today();
        // Marking every day of a month or of the schedule would say nothing.
        let marks_range = self.span.get().has_columns();
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

    /// Draws the year: twelve months of six weeks each, in the week's order.
    fn update_year(&self, ui: &AppWindow, today: NaiveDate) {
        let year = self.anchor.get().year();
        let mut names = Vec::with_capacity(12);
        let mut days = Vec::with_capacity(12 * MINI_MONTH_DAYS as usize);
        for month in 1..=12 {
            let Some(first_of_month) = NaiveDate::from_ymd_opt(year, month, 1) else {
                continue;
            };
            names.push(SharedString::from(first_of_month.format("%B").to_string()));
            let (first, _) = Span::Month.range(first_of_month);
            for offset in 0..MINI_MONTH_DAYS {
                let day = first + Duration::days(offset);
                days.push(MiniDay {
                    label: day.day().to_string().into(),
                    today: day == today,
                    dim: day.month() != month,
                    shown: false,
                });
            }
        }
        ui.set_year_months(ModelRc::from(Rc::new(VecModel::from(names))));
        patch(&self.year_days, days);
    }

    /// Opens the day view on a day picked in the year: `cell` counts the days of all
    /// twelve months' grids.
    fn pick_year_day(&self, cell: i64) {
        let month = (cell / MINI_MONTH_DAYS) as u32 + 1;
        let Some(first_of_month) = NaiveDate::from_ymd_opt(self.anchor.get().year(), month, 1)
        else {
            return;
        };
        let (first, _) = Span::Month.range(first_of_month);
        self.set_span(Span::Day);
        self.show(first + Duration::days(cell % MINI_MONTH_DAYS));
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
        // A whole series is brought back from its own resource, which does not hold
        // what was changed or deleted in single occurrences. With such occurrences the
        // undo would bring back another schedule than the one deleted; none is offered.
        let incomplete = matches!(kind, UndoKind::Recreate(_))
            && target.occurrence.as_ref().is_some_and(|occurrence| {
                self.store
                    .has_occurrences(target.account, &target.calendar_id, &occurrence.series_id)
                    .unwrap_or(true)
            });
        self.pending.borrow_mut().push_back(Pending {
            undo: (!incomplete).then(|| Undo {
                account: target.account,
                calendar_id: target.calendar_id.clone(),
                kind,
                etag: None,
            }),
            retry: None,
        });
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

    /// An event was dragged. A repeating one first asks how much of the series is meant.
    fn drag_event(&self, drag: Drag) {
        let Some(target) = self.targets.borrow().get(drag.instance).cloned() else {
            return;
        };
        if !target.movable {
            return;
        }
        // How much of a series, and whether the guests are told: asked where there is
        // a choice.
        if target.occurrence.is_some() || target.guests {
            self.asked.replace(Some(Asked::Drag(drag)));
            if let Some(ui) = self.ui.upgrade() {
                ui.invoke_ask_edit_scope(target.occurrence.is_some(), target.guests);
            }
        } else {
            self.send_drag(drag, Scope::This, false);
        }
    }

    /// Sends a drag to Google. The undo is prepared from the cache before anything is
    /// sent.
    fn send_drag(&self, drag: Drag, scope: Scope, notify: bool) {
        let Some(target) = self.targets.borrow().get(drag.instance).cloned() else {
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
        let Some((edit, undo)) = plan_drag(&target, scope, drag, own, series, self.zone) else {
            return;
        };
        self.send_edit(&target, edit, undo, notify);
        if scope == Scope::This {
            self.draw_drag(drag);
        }
    }

    /// Answers the invitation to the event behind an instance. An occurrence of a
    /// series that was not changed on its own first asks whether the answer is for
    /// this event or for all of them.
    fn rsvp(&self, instance: usize, answer: ResponseStatus) {
        let Some(target) = self.targets.borrow().get(instance).cloned() else {
            return;
        };
        if !target.can_answer {
            return;
        }
        if target.occurrence.as_ref().is_some_and(|o| !o.changed) {
            self.asked
                .replace(Some(Asked::Answer(Box::new(target), answer)));
            if let Some(ui) = self.ui.upgrade() {
                ui.invoke_ask_answer_scope();
            }
        } else {
            self.send_rsvp(&target, answer, Scope::This);
        }
    }

    fn send_rsvp(&self, target: &Target, answer: ResponseStatus, scope: Scope) {
        let (own, series) = match self.resources(target) {
            Ok(resources) => resources,
            Err(error) => return self.show_error(&error),
        };
        let Some((edit, undo)) = plan_rsvp(target, answer, scope, own, series) else {
            return;
        };
        // The organiser sees the answer in the event; no e-mail on top.
        self.send_edit(target, edit, Some(undo), false);
    }

    /// Sends a change to Google and remembers how to take it back.
    fn send_edit(&self, target: &Target, edit: Edit, undo: Option<UndoKind>, notify: bool) {
        self.pending.borrow_mut().push_back(Pending {
            undo: undo.map(|kind| Undo {
                account: target.account,
                calendar_id: target.calendar_id.clone(),
                kind,
                etag: None,
            }),
            retry: None,
        });
        if let Some(ui) = self.ui.upgrade() {
            ui.set_toast(SharedString::new());
        }
        let _ = self.commands.send(Command::EditEvent {
            account: target.account,
            calendar_id: target.calendar_id.clone(),
            edit,
            notify,
        });
    }

    /// The cached resources of an event and of the series it belongs to, if any.
    fn resources(
        &self,
        target: &Target,
    ) -> Result<(Option<serde_json::Value>, Option<serde_json::Value>)> {
        let cached = |id: &str| {
            self.store
                .event_json(target.account, &target.calendar_id, id)
        };
        let own = cached(&target.event_id)?;
        let series = match &target.occurrence {
            Some(occurrence) => cached(&occurrence.series_id)?,
            None => None,
        };
        Ok((own, series))
    }

    /// Opens the editor for the event behind an instance on screen.
    fn open_editor(&self, instance: usize) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(target) = self.targets.borrow().get(instance).cloned() else {
            return;
        };
        if !target.movable {
            return;
        }
        let (own, series) = match self.resources(&target) {
            Ok(resources) => resources,
            Err(error) => return self.show_error(&error),
        };
        // An unchanged occurrence is described by its series.
        let changed = target.occurrence.as_ref().is_none_or(|o| o.changed);
        let Some(resource) = (if changed { own.clone() } else { series.clone() }) else {
            return;
        };
        let (mut form, mut kept) = editor::form_of(&resource, target.start, target.end, self.zone);
        if let (true, Some(series)) = (target.occurrence.is_some() && changed, &series) {
            editor::adopt_series_rule(&mut form, &mut kept, series, self.zone);
        }
        let detail = self.details.borrow().get(instance).cloned();
        let calendar_color = self.calendar_color(&target);
        ui.set_editor_new(false);
        ui.set_editor_calendar_name(detail.map(|d| d.calendar).unwrap_or_default());
        kept.own_email = self
            .loaded
            .borrow()
            .accounts
            .iter()
            .find(|a| a.id == target.account)
            .map(|a| organiser_address(&a.email, &target.calendar_id))
            .unwrap_or_default();
        self.show_editor(&ui, form, kept, Some(target), calendar_color);
        if let Some(editing) = self.editing.borrow_mut().as_mut() {
            editing.own = own;
            editing.series = series;
        }
    }

    fn calendar_color(&self, target: &Target) -> Color {
        let loaded = self.loaded.borrow();
        loaded
            .calendars
            .iter()
            .find(|c| c.account_id == target.account && c.entry.id == target.calendar_id)
            .and_then(|c| c.entry.background_color.as_deref().and_then(parse_color))
            .unwrap_or(FALLBACK_COLOR)
    }

    /// The question about a change was closed without an answer. What came from the
    /// editor goes back into it as it was saved; a drag is simply dropped.
    fn ask_cancelled(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        match self.asked.take() {
            Some(Asked::Create(new)) => {
                let color = ui
                    .get_draft_calendars()
                    .row_data(new.calendar)
                    .map_or(FALLBACK_COLOR, |c| c.color);
                self.draft.replace(Some(new.draft));
                ui.set_editor_new(true);
                ui.set_draft_calendar(new.calendar as i32);
                self.show_editor(&ui, new.form, Kept::default(), None, color);
            }
            Some(Asked::Form(saved)) => {
                let Saved {
                    target,
                    form,
                    opened,
                    kept,
                    own,
                    series,
                    ..
                } = *saved;
                let color = self.calendar_color(&target);
                ui.set_editor_new(false);
                self.show_editor(&ui, form, kept, Some(target), color);
                // Still a change to the event as it was first opened.
                if let Some(editing) = self.editing.borrow_mut().as_mut() {
                    editing.opened = opened;
                    editing.own = own;
                    editing.series = series;
                }
            }
            Some(Asked::Drag(_) | Asked::Answer(..)) | None => {}
        }
    }

    /// Opens the editor for the new event of the quick card, with what was typed there.
    fn open_editor_for_draft(&self, title: &str, calendar: usize) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some((start, end)) = self.draft.borrow().as_ref().map(|d| (d.start, d.end)) else {
            return;
        };
        let mut form = editor::new_form(start, end, self.zone);
        form.title = title.to_owned();
        let color = ui
            .get_draft_calendars()
            .row_data(calendar)
            .map_or(FALLBACK_COLOR, |c| c.color);
        ui.set_editor_new(true);
        ui.set_draft_calendar(calendar as i32);
        ui.set_draft_visible(false);
        self.show_editor(&ui, form, Kept::default(), None, color);
    }

    fn show_editor(
        &self,
        ui: &AppWindow,
        form: Form,
        kept: Kept,
        target: Option<Target>,
        calendar_color: Color,
    ) {
        let colors: Vec<Color> = std::iter::once(calendar_color)
            .chain(EVENT_COLORS.iter().map(|(_, value)| rgb(*value)))
            .collect();
        ui.set_editor_colors(ModelRc::from(Rc::new(VecModel::from(colors))));
        ui.set_editor_custom_repeat(form.repeat == editor::REPEAT_CUSTOM);
        ui.set_editor_custom_reminder(form.reminder == editor::REMINDER_CUSTOM);
        ui.set_editor_error(SharedString::new());
        ui.set_editor(to_window(&form));
        self.editing.replace(Some(Editing {
            target,
            opened: form,
            kept,
            own: None,
            series: None,
        }));
        ui.set_detail_visible(false);
        ui.set_editor_visible(true);
    }

    /// "Save" in the editor: checks the form and sends what changed. For a repeating
    /// event it first asks how much of the series is meant.
    fn save_editor(&self, form: Form, calendar: usize) {
        let Some(ui) = self.ui.upgrade() else { return };
        let Some(editing) = self.editing.borrow().clone() else {
            return;
        };
        let mut kept = editing.kept.clone();
        if editing.target.is_none() {
            // A new event belongs to the account of the calendar chosen for it; the
            // text the choice is remembered under starts with that account's address.
            kept.own_email = self
                .draft
                .borrow()
                .as_ref()
                .and_then(|draft| draft.calendars.get(calendar))
                .and_then(|(_, calendar_id, key)| {
                    Some(organiser_address(key.split('\t').next()?, calendar_id))
                })
                .unwrap_or_default();
        }
        let after = match editor::fields(&form, &kept, self.zone) {
            Ok(after) => after,
            Err(problem) => return ui.set_editor_error(problem.into()),
        };
        let Some(target) = editing.target.clone() else {
            // A new event.
            let Some((account, calendar_id, key)) = self
                .draft
                .borrow()
                .as_ref()
                .and_then(|draft| draft.calendars.get(calendar).cloned())
            else {
                return;
            };
            let _ = self.store.set_setting(NEW_EVENT_CALENDAR, &key);
            let Some(draft) = self.draft.take() else {
                return;
            };
            let invites = after["attendees"]
                .as_array()
                .is_some_and(|guests| !guests.is_empty());
            let new = NewEvent {
                account,
                calendar_id,
                resource: editor::creation(after),
                form,
                calendar,
                draft,
            };
            self.close_editor(&ui);
            if invites {
                self.asked.replace(Some(Asked::Create(Box::new(new))));
                ui.invoke_ask_edit_scope(false, true);
            } else {
                self.create(new, false);
            }
            return;
        };
        let before = match editor::fields(&editing.opened, &editing.kept, self.zone) {
            Ok(before) => before,
            Err(problem) => return ui.set_editor_error(problem.into()),
        };
        let changes = editor::diff(&before, &after);
        self.close_editor(&ui);
        if changes.as_object().is_none_or(|changes| changes.is_empty()) {
            return;
        }
        let saved = Saved {
            target,
            form,
            opened: editing.opened,
            kept: editing.kept,
            changes,
            before,
            after,
            own: editing.own,
            series: editing.series,
        };
        // Guests are asked about when the event has some, or gets its first ones now.
        let repeating = saved.target.occurrence.is_some();
        let guests = saved.target.guests || saved.changes.get("attendees").is_some();
        if repeating || guests {
            self.asked.replace(Some(Asked::Form(Box::new(saved))));
            ui.invoke_ask_edit_scope(repeating, guests);
        } else {
            self.send_form(saved, Scope::This, false);
        }
    }

    fn close_editor(&self, ui: &AppWindow) {
        self.editing.take();
        ui.set_editor_visible(false);
    }

    /// Sends what the editor changed, for as much of a series as was chosen.
    fn send_form(&self, saved: Saved, scope: Scope, notify: bool) {
        match plan_form(
            &saved,
            scope,
            saved.own.clone(),
            saved.series.clone(),
            self.zone,
        ) {
            Ok(Some((edit, undo))) => self.send_edit(&saved.target, edit, undo, notify),
            Ok(None) => {}
            Err(problem) => {
                if let Some(ui) = self.ui.upgrade() {
                    ui.set_notice(problem.into());
                }
            }
        }
    }

    /// Draws a dragged event at its new place at once, where that is simple, rather than
    /// letting it jump back until Google has answered.
    fn draw_drag(&self, drag: Drag) {
        let instance = drag.instance as i32;
        let (days, minutes) = (drag.days as i32, drag.minutes as i32);
        let shown_days = self.range().1 as i32;
        let on_screen = |day: i32| (0..shown_days).contains(&day);
        let blocks: Vec<usize> = (0..self.timed.row_count())
            .filter(|row| {
                self.timed
                    .row_data(*row)
                    .is_some_and(|b| b.instance == instance)
            })
            .collect();
        if let [row] = blocks[..]
            && let Some(mut block) = self.timed.row_data(row)
        {
            let start = if drag.resize {
                block.start_min
            } else {
                block.start_min + minutes
            };
            let end = (block.end_min + minutes).max(start + 15);
            if start >= 0 && end <= 24 * 60 && on_screen(block.day + days) {
                block.day += days;
                block.start_min = start;
                block.end_min = end;
                self.timed.set_row_data(row, block);
            }
        }
        for row in 0..self.all_day.row_count() {
            if let Some(mut bar) = self.all_day.row_data(row)
                && bar.instance == instance
                && on_screen(bar.first_day + days)
                && on_screen(bar.last_day + days)
            {
                bar.first_day += days;
                bar.last_day += days;
                self.all_day.set_row_data(row, bar);
            }
        }
    }

    /// Sends a new event to be created; with `notify`, its guests get an invitation.
    fn create(&self, new: NewEvent, notify: bool) {
        self.creating(
            new.account,
            &new.calendar_id,
            new.form,
            new.calendar,
            new.draft,
        );
        let _ = self.commands.send(Command::CreateEvent {
            account: new.account,
            calendar_id: new.calendar_id,
            resource: new.resource,
            notify,
        });
    }

    /// Notes a new event as sent: how to take it back once it exists, and what to show
    /// again should it not come into being.
    fn creating(
        &self,
        account: AccountId,
        calendar_id: &str,
        form: Form,
        calendar: usize,
        draft: Draft,
    ) {
        self.pending.borrow_mut().push_back(Pending {
            undo: Some(Undo {
                account,
                calendar_id: calendar_id.to_owned(),
                // Which event, the worker says when it has created it.
                kind: UndoKind::Delete(String::new()),
                etag: None,
            }),
            retry: Some(Retry {
                form,
                calendar,
                draft,
            }),
        });
    }

    /// The worker has answered the oldest change still waiting. Makes its undo the one
    /// on offer, completed with the event `id` and version `etag` the worker names,
    /// and says whether there is one; hands back what a failed creation was made from.
    fn answered(&self, ok: bool, id: &str, etag: &str) -> (bool, Option<Retry>) {
        let Some(Pending { undo, retry }) = self.pending.borrow_mut().pop_front() else {
            return (false, None);
        };
        if !ok {
            return (false, retry);
        }
        let undo = undo.and_then(|mut undo| {
            undo.etag = (!etag.is_empty()).then(|| etag.to_owned());
            undo.kind = match undo.kind {
                // These wait to learn which event it is; without that, no undo.
                UndoKind::Occurrence(_) | UndoKind::Delete(_) if id.is_empty() => return None,
                UndoKind::Occurrence(changes) => UndoKind::Patch {
                    event_id: id.to_owned(),
                    changes,
                },
                UndoKind::Delete(_) => UndoKind::Delete(id.to_owned()),
                complete => complete,
            };
            Some(undo)
        });
        let can_undo = undo.is_some();
        self.undo.replace(undo);
        (can_undo, None)
    }

    /// The worker has answered the creation of an event: say so and offer to take it
    /// back, or give back what was typed.
    fn event_created(&self, created: bool, id: &str, etag: &str) {
        let (can_undo, retry) = self.answered(created, id, etag);
        if created {
            return self.show_toast("Event saved", can_undo);
        }
        // A dialog says why. The editor comes back with everything that was entered.
        let (Some(retry), Some(ui)) = (retry, self.ui.upgrade()) else {
            return;
        };
        let color = ui
            .get_draft_calendars()
            .row_data(retry.calendar)
            .map_or(FALLBACK_COLOR, |c| c.color);
        self.draft.replace(Some(retry.draft));
        ui.set_editor_new(true);
        ui.set_draft_calendar(retry.calendar as i32);
        self.show_editor(&ui, retry.form, Kept::default(), None, color);
    }

    /// The worker has answered a change. `occurrence` is the event it was made to, if
    /// that was one occurrence of a series, and `etag` the version the event now has.
    fn change_finished(&self, changed: bool, occurrence: &str, etag: &str) {
        let (can_undo, _) = self.answered(changed, occurrence, etag);
        if changed {
            self.show_toast("Event saved", can_undo);
        } else {
            // A dialog says why. Put the event back where it is.
            self.refresh();
        }
    }

    /// The worker has answered a deletion: offer to undo it, for a while. `occurrence`
    /// is the event that was removed, if it was one occurrence of a series.
    fn delete_finished(&self, deleted: bool, occurrence: &str) {
        let (can_undo, _) = self.answered(deleted, occurrence, "");
        if deleted {
            self.show_toast("Event deleted", can_undo);
        }
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
    fn open_draft(&self, day: i32, minute: i32, all_day: bool, length: i32) {
        let length = if length > 0 { length } else { 60 };
        let Some(ui) = self.ui.upgrade() else { return };
        let (first, days) = self.range();
        let has_columns = self.span.get().has_columns();
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
        let (start, end) = draft_times(
            first + Duration::days(day),
            minute,
            all_day,
            length,
            self.zone,
        );

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
        let names: Vec<SharedString> = rows
            .iter()
            .map(|row| {
                if row.name == row.account {
                    row.name.clone()
                } else {
                    format!("{} · {}", row.name, row.account).into()
                }
            })
            .collect();
        ui.set_editor_calendars(ModelRc::from(Rc::new(VecModel::from(names))));
        ui.set_draft_calendars(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_draft_calendar(chosen as i32);
        ui.set_draft_when(when_text(start, end, self.zone).into());
        // A ghost of the new event where it will go, in the views that have columns.
        let ghost = has_columns && !all_day;
        ui.set_draft_day(if ghost { day as i32 } else { -1 });
        ui.set_draft_min(minute);
        ui.set_draft_len(length);
        if ghost {
            // "About now" may be below the hours on screen.
            ui.invoke_reveal_minute(minute);
        }
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
        let resource = event_resource(title, draft.start, draft.end, self.zone);
        let mut form = editor::new_form(draft.start, draft.end, self.zone);
        form.title = title.to_owned();
        self.create(
            NewEvent {
                account,
                calendar_id,
                resource,
                form,
                calendar,
                draft,
            },
            false,
        );
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
            etag,
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
            UndoKind::Delete(event_id) => Command::DeleteEvent {
                account,
                calendar_id,
                deletion: Deletion::Event { event_id, etag },
                notify: false,
            },
            UndoKind::Patch { event_id, changes } => Command::PatchEvent {
                account,
                calendar_id,
                event_id,
                etag,
                changes,
            },
            // The worker has not said which occurrence it was.
            UndoKind::Occurrence(_) => return,
        });
    }

    /// Opens the day view on the day of a cell of the month grid.
    fn open_day(&self, cell: i64) {
        let day = self.range().0 + Duration::days(cell);
        self.set_span(Span::Day);
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
        asked: RefCell::default(),
        editing: RefCell::default(),
        pending: RefCell::default(),
        generation: Cell::new(0),
        toast_timer: Timer::default(),
        commands: commands.clone(),
        all_day: Rc::default(),
        month_cells: Rc::default(),
        day_lists: RefCell::default(),
        year_days: Rc::default(),
        month_chips: Rc::default(),
        schedule: Rc::default(),
        sidebar: Rc::default(),
    });
    ui.set_days(ModelRc::from(view.days.clone()));
    ui.set_timed(ModelRc::from(view.timed.clone()));
    ui.set_all_day(ModelRc::from(view.all_day.clone()));
    ui.set_month_cells(ModelRc::from(view.month_cells.clone()));
    ui.set_year_days(ModelRc::from(view.year_days.clone()));
    ui.on_day_list_requested({
        let view = view.clone();
        move |cell| {
            let rows = view.day_lists.borrow().get(cell as usize).cloned();
            if let (Some(rows), Some(ui)) = (rows, view.ui.upgrade()) {
                let title = rows
                    .first()
                    .map(|row| format!("{}, {}", row.day_name, row.day_number))
                    .unwrap_or_default();
                ui.set_day_list_title(title.into());
                ui.set_day_list(ModelRc::from(Rc::new(VecModel::from(rows))));
                ui.set_day_list_visible(true);
            }
        }
    });
    ui.on_year_pick({
        let view = view.clone();
        move |cell| view.pick_year_day(i64::from(cell))
    });
    ui.set_month_chips(ModelRc::from(view.month_chips.clone()));
    ui.set_schedule(ModelRc::from(view.schedule.clone()));
    ui.set_sidebar(ModelRc::from(view.sidebar.clone()));
    let number =
        |key: &str| -> Option<u32> { view.store.setting(key).ok().flatten()?.parse().ok() };
    PREFS.set(Prefs {
        week_start: number(WEEK_START).filter(|day| *day < 7).unwrap_or(0),
        twelve_hour: number(TWELVE_HOUR) == Some(1),
        custom_days: number(CUSTOM_DAYS)
            .filter(|days| (2..=7).contains(days))
            .map_or(4, i64::from),
    });
    view.apply_prefs();
    if let Some(span) = number(LAST_VIEW).and_then(|index| Span::ALL.get(index as usize)) {
        view.span.set(*span);
    }
    ui.on_week_start_chosen({
        let view = view.clone();
        move |choice| {
            // The list offers Monday, Sunday, Saturday.
            let week_start = [0, 6, 5].get(choice as usize).copied().unwrap_or(0);
            PREFS.set(Prefs {
                week_start,
                ..prefs()
            });
            let _ = view.store.set_setting(WEEK_START, &week_start.to_string());
            view.apply_prefs();
            view.refresh();
        }
    });
    ui.on_custom_days_chosen({
        let view = view.clone();
        move |choice| {
            // The list offers 2 to 7 days.
            let custom_days = i64::from(choice.clamp(0, 5)) + 2;
            PREFS.set(Prefs {
                custom_days,
                ..prefs()
            });
            let _ = view
                .store
                .set_setting(CUSTOM_DAYS, &custom_days.to_string());
            view.apply_prefs();
            view.refresh();
        }
    });
    ui.on_clock_chosen({
        let view = view.clone();
        move |choice| {
            PREFS.set(Prefs {
                twelve_hour: choice == 1,
                ..prefs()
            });
            let _ = view.store.set_setting(TWELVE_HOUR, &choice.to_string());
            view.apply_prefs();
            view.refresh();
        }
    });

    let theme = view.store.setting(THEME).ok().flatten();
    ui.set_theme_mode(theme.and_then(|mode| mode.parse().ok()).unwrap_or(0));
    if let Some(height) = view
        .store
        .setting(HOUR_HEIGHT)
        .ok()
        .flatten()
        .and_then(|height| height.parse::<f32>().ok())
    {
        // The window keeps its own limits; this only guards against nonsense.
        ui.set_hour_h(height.clamp(20.0, 192.0));
    }
    // Zooming changes the height many times a second; it is saved once it rests.
    let zoom_timer = Timer::default();
    ui.on_zoom_changed({
        let view = view.clone();
        move |height| {
            let view = view.clone();
            zoom_timer.start(
                TimerMode::SingleShot,
                StdDuration::from_millis(500),
                move || {
                    let _ = view.store.set_setting(HOUR_HEIGHT, &height.to_string());
                },
            );
        }
    });
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
        move |instance, days, minutes| {
            view.drag_event(Drag {
                instance: instance as usize,
                days: days.into(),
                minutes: minutes.into(),
                resize: false,
            })
        }
    });
    ui.on_event_resized({
        let view = view.clone();
        move |instance, minutes| {
            view.drag_event(Drag {
                instance: instance as usize,
                days: 0,
                minutes: minutes.into(),
                resize: true,
            })
        }
    });
    ui.on_change_finished({
        let view = view.clone();
        move |changed, occurrence, etag| view.change_finished(changed, &occurrence, &etag)
    });
    ui.on_rsvp({
        let view = view.clone();
        move |instance, answer| {
            let answer = match answer {
                1 => ResponseStatus::Accepted,
                2 => ResponseStatus::Declined,
                _ => ResponseStatus::Tentative,
            };
            view.rsvp(instance as usize, answer);
        }
    });
    ui.on_editor_requested({
        let view = view.clone();
        move |instance| view.open_editor(instance as usize)
    });
    ui.on_editor_from_draft({
        let view = view.clone();
        move |title, calendar| view.open_editor_for_draft(&title, calendar as usize)
    });
    ui.on_editor_save({
        let view = view.clone();
        move |form, calendar| view.save_editor(from_window(&form), calendar as usize)
    });
    ui.on_edit_scope_cancelled({
        let view = view.clone();
        move || view.ask_cancelled()
    });
    ui.on_edit_scope_chosen({
        let view = view.clone();
        move |scope, notify| {
            let scope = match scope {
                1 => Scope::Following,
                2 => Scope::All,
                _ => Scope::This,
            };
            match view.asked.take() {
                Some(Asked::Drag(drag)) => view.send_drag(drag, scope, notify),
                Some(Asked::Form(saved)) => view.send_form(*saved, scope, notify),
                Some(Asked::Create(new)) => view.create(*new, notify),
                Some(Asked::Answer(target, answer)) => view.send_rsvp(&target, answer, scope),
                None => {}
            }
        }
    });
    ui.on_delete_finished({
        let view = view.clone();
        move |deleted, occurrence| view.delete_finished(deleted, &occurrence)
    });
    ui.on_draft_requested({
        let view = view.clone();
        move |day, minute, all_day, length| view.open_draft(day, minute, all_day, length)
    });
    ui.on_draft_save({
        let view = view.clone();
        move |title, calendar| view.save_draft(&title, calendar as usize)
    });
    ui.on_event_created({
        let view = view.clone();
        move |created, id, etag| view.event_created(created, &id, &etag)
    });
    ui.on_undo({
        let view = view.clone();
        move || view.undo_delete()
    });
    ui.on_goto_date({
        let view = view.clone();
        move |text| {
            let Some(ui) = view.ui.upgrade() else { return };
            match typed_date(&text, view.today()) {
                Some(date) => {
                    ui.set_goto_error(SharedString::new());
                    ui.set_goto_visible(false);
                    view.show(date);
                }
                None => ui.set_goto_error(
                    "That is not a date this understands. Try 17.11.2026 or 2026-11-17.".into(),
                ),
            }
        }
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
                view.set_span(*span);
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

    // Without a connection, look for one more often than the usual sync does.
    let retry_timer = Timer::default();
    retry_timer.start(TimerMode::Repeated, OFFLINE_RETRY, {
        let commands = commands.clone();
        let ui = ui.as_weak();
        move || {
            if ui.upgrade().is_some_and(|ui| ui.get_offline()) {
                let _ = commands.send(Command::SyncAll);
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
    fn a_typed_date_is_read_in_the_usual_ways() {
        let today = date("2026-11-15");
        let read = |text: &str| typed_date(text, today);
        assert_eq!(read("2026-11-17"), Some(date("2026-11-17")));
        assert_eq!(read(" 17.11.2026 "), Some(date("2026-11-17")));
        // Day first with any separator, also with a short year; year first only with
        // four digits.
        assert_eq!(read("17-11-26"), Some(date("2026-11-17")));
        assert_eq!(read("17-11-2026"), Some(date("2026-11-17")));
        assert_eq!(read("2026/11/17"), Some(date("2026-11-17")));
        assert_eq!(read("2026.1.5"), Some(date("2026-01-05")));
        // Outside what the calendar shows.
        assert_eq!(read("31.12.262142"), None);
        assert_eq!(read("1.1.1899"), None);
        assert_eq!(read("31.12.2200"), Some(date("2200-12-31")));
        assert_eq!(shown_dates(NaiveDate::MAX), date("2200-12-31"));
        assert_eq!(shown_dates(NaiveDate::MIN), date("1900-01-01"));
        assert_eq!(read("17/11/26"), Some(date("2026-11-17")));
        assert_eq!(read("1. 2."), Some(date("2026-02-01")));
        assert_eq!(read("Tomorrow"), Some(date("2026-11-16")));
        assert_eq!(read("today"), Some(today));
        assert_eq!(read("yesterday"), Some(date("2026-11-14")));
        for nonsense in ["", "soon", "31.2.2026", "17", "1.2.3.4", "é.ÿ"] {
            assert_eq!(read(nonsense), None, "{nonsense}");
        }
    }

    #[test]
    fn the_week_can_start_on_another_day_and_the_clock_have_twelve_hours() {
        // Wednesday 18 November 2026.
        let day = date("2026-11-18");
        assert_eq!(week_start_from(day, 0), date("2026-11-16"));
        assert_eq!(week_start_from(day, 6), date("2026-11-15"));
        assert_eq!(week_start_from(day, 5), date("2026-11-14"));
        // A day that starts the week is its own week's start.
        assert_eq!(week_start_from(date("2026-11-15"), 6), date("2026-11-15"));
        assert_eq!(week_start_from(date("2026-11-15"), 0), date("2026-11-09"));

        assert_eq!(clock_text(14, 5, false), "14:05");
        assert_eq!(clock_text(0, 0, false), "0:00");
        assert_eq!(clock_text(14, 5, true), "2:05pm");
        assert_eq!(clock_text(14, 0, true), "2pm");
        assert_eq!(clock_text(0, 30, true), "12:30am");
        assert_eq!(clock_text(12, 0, true), "12pm");
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
        assert_eq!(Span::Year.step(date("2026-11-18"), 1), date("2027-11-18"));
        assert_eq!(Span::Year.step(date("2028-02-29"), -1), date("2027-02-28"));
        assert_eq!(
            Span::Year.range(date("2026-11-18")),
            (date("2026-01-01"), 365)
        );
        assert_eq!(
            Span::Year.range(date("2028-03-01")),
            (date("2028-01-01"), 366)
        );
        assert_eq!(Span::Year.title(date("2026-11-18")), "2026");
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
    fn an_event_from_another_time_zone_says_its_own_time() {
        let prague: Tz = "Europe/Prague".parse().unwrap();
        let event = |zone: &str| -> Event {
            serde_json::from_value(serde_json::json!({
                "id": "e",
                "start": {"dateTime": "2026-11-17T01:00:00Z", "timeZone": zone},
                "end": {"dateTime": "2026-11-17T02:00:00Z", "timeZone": zone}
            }))
            .unwrap()
        };
        let (start, end) = (
            EventTime::Timed("2026-11-17T01:00:00Z".parse().unwrap()),
            EventTime::Timed("2026-11-17T02:00:00Z".parse().unwrap()),
        );
        assert_eq!(
            own_zone_text(&event("Asia/Tokyo"), start, end, prague),
            "10:00 – 11:00 in Asia/Tokyo"
        );
        // There it is still the evening before, which is said.
        assert_eq!(
            own_zone_text(&event("America/New_York"), start, end, prague),
            "16 Nov 20:00 – 21:00 in America/New York"
        );
        // Across midnight there: both days.
        let late_end = EventTime::Timed("2026-11-17T06:00:00Z".parse().unwrap());
        assert_eq!(
            own_zone_text(&event("America/New_York"), start, late_end, prague),
            "16 Nov 20:00 – 17 Nov 1:00 in America/New York"
        );
        // The same clock under another name, no zone at all, or nonsense: nothing.
        assert_eq!(
            own_zone_text(&event("Europe/Zurich"), start, end, prague),
            ""
        );
        assert_eq!(own_zone_text(&event("Not/AZone"), start, end, prague), "");
        let day = EventTime::AllDay(date("2026-11-17"));
        assert_eq!(own_zone_text(&event("Asia/Tokyo"), day, day, prague), "");
    }

    #[test]
    fn an_invitation_is_answered_only_in_the_accounts_own_calendar() {
        use serde_json::json;
        let calendar = |extra: serde_json::Value| -> CalendarListEntry {
            let mut value = json!({"id": "me@x.org", "accessRole": "owner"});
            value
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(value).unwrap()
        };
        let invited = |own: serde_json::Value| -> Event {
            serde_json::from_value(json!({"id": "e", "attendees": [own]})).unwrap()
        };
        let me = invited(json!({"email": "me@x.org", "self": true}));
        assert!(can_answer(&me, &calendar(json!({"primary": true}))));
        // A colleague's calendar shared with write access: "self" there is the colleague.
        assert!(!can_answer(
            &me,
            &calendar(json!({"id": "colleague@x.org"}))
        ));
        // A room's calendar, and an event the account is not a guest of.
        let room = invited(json!({"email": "room@x.org", "self": true, "resource": true}));
        assert!(!can_answer(&room, &calendar(json!({"primary": true}))));
        let other = invited(json!({"email": "you@x.org"}));
        assert!(!can_answer(&other, &calendar(json!({"primary": true}))));

        // The account joins the first guests only where it organises: its own calendar.
        assert_eq!(organiser_address("me@x.org", "ME@x.org"), "me@x.org");
        assert_eq!(
            organiser_address("me@x.org", "family123@group.calendar.google.com"),
            ""
        );
        assert_eq!(organiser_address("me@x.org", "colleague@x.org"), "");
    }

    #[test]
    fn an_answer_goes_to_one_occurrence_or_to_the_series() {
        use serde_json::json;
        let instant = |text: &str| EventTime::Timed(text.parse::<DateTime<Utc>>().unwrap());
        let guests = |answer: &str| {
            json!([
                {"email": "boss@x.org", "organizer": true, "responseStatus": "accepted"},
                {"email": "me@x.org", "self": true, "responseStatus": answer}
            ])
        };
        let series = json!({"id": "m", "etag": "\"s\"", "attendees": guests("accepted"),
            "recurrence": ["RRULE:FREQ=DAILY"]});
        let target = |event_id: &str, changed: Option<bool>| Target {
            account: 1,
            calendar_id: "c".into(),
            event_id: event_id.into(),
            etag: None,
            occurrence: changed.map(|changed| Occurrence {
                series_id: "m".into(),
                original_start: instant("2026-11-23T10:00:00Z"),
                changed,
            }),
            movable: false,
            guests: false,
            can_answer: true,
            start: instant("2026-11-23T10:00:00Z"),
            end: instant("2026-11-23T11:00:00Z"),
        };

        // One occurrence of the series: an event of its own is made of it, with the
        // series' guests and this answer. Also when the answer is the series' own:
        // nothing says it was ever given for this event.
        let unchanged = target("m", Some(false));
        for answer in [ResponseStatus::Declined, ResponseStatus::Accepted] {
            assert_eq!(
                plan_rsvp(
                    &unchanged,
                    answer,
                    Scope::This,
                    Some(series.clone()),
                    Some(series.clone())
                ),
                Some((
                    Edit::Occurrence {
                        series_id: "m".into(),
                        original_start: instant("2026-11-23T10:00:00Z"),
                        known_id: None,
                        etag: None,
                        changes: json!({"attendees": guests(answer.as_str())}),
                    },
                    UndoKind::Occurrence(json!({"attendees": guests("accepted")})),
                ))
            );
        }
        // All events: the series itself, under its version.
        assert_eq!(
            plan_rsvp(
                &unchanged,
                ResponseStatus::Declined,
                Scope::All,
                Some(series.clone()),
                Some(series.clone())
            ),
            Some((
                Edit::Event {
                    event_id: "m".into(),
                    etag: Some("\"s\"".into()),
                    changes: json!({"attendees": guests("declined")}),
                },
                UndoKind::Patch {
                    event_id: "m".into(),
                    changes: json!({"attendees": guests("accepted")}),
                },
            ))
        );
        // The answer the series has already: nothing to send.
        assert_eq!(
            plan_rsvp(
                &unchanged,
                ResponseStatus::Accepted,
                Scope::All,
                Some(series.clone()),
                Some(series.clone())
            ),
            None
        );
        // A changed occurrence and a single event answer for themselves.
        let exception = json!({"id": "m_1", "etag": "\"x\"", "recurringEventId": "m",
            "attendees": guests("needsAction")});
        for target in [target("m_1", Some(true)), target("m_1", None)] {
            assert_eq!(
                plan_rsvp(
                    &target,
                    ResponseStatus::Tentative,
                    Scope::This,
                    Some(exception.clone()),
                    Some(series.clone())
                ),
                Some((
                    Edit::Event {
                        event_id: "m_1".into(),
                        etag: Some("\"x\"".into()),
                        changes: json!({"attendees": guests("tentative")}),
                    },
                    UndoKind::Patch {
                        event_id: "m_1".into(),
                        changes: json!({"attendees": guests("needsAction")}),
                    },
                ))
            );
        }
        // Not among the guests: nothing.
        let foreign = json!({"id": "m", "attendees": [{"email": "boss@x.org"}]});
        assert_eq!(
            plan_rsvp(
                &unchanged,
                ResponseStatus::Accepted,
                Scope::This,
                None,
                Some(foreign)
            ),
            None
        );
    }

    #[test]
    fn answering_changes_only_this_accounts_entry() {
        use serde_json::json;
        let event = json!({
            "id": "e",
            "attendees": [
                {"email": "boss@x.org", "organizer": true, "responseStatus": "accepted"},
                {"email": "me@x.org", "self": true, "responseStatus": "needsAction", "comment": "hm"}
            ]
        });
        let (changes, before) = rsvp_changes(&event, ResponseStatus::Declined).unwrap();
        assert_eq!(
            changes,
            json!({"attendees": [
                {"email": "boss@x.org", "organizer": true, "responseStatus": "accepted"},
                {"email": "me@x.org", "self": true, "responseStatus": "declined", "comment": "hm"}
            ]})
        );
        assert_eq!(before, json!({"attendees": event["attendees"]}));
        // The same answer again, or an event this account is not a guest of: nothing.
        assert_eq!(rsvp_changes(&event, ResponseStatus::NeedsAction), None);
        assert_eq!(
            rsvp_changes(&json!({"id": "e"}), ResponseStatus::Accepted),
            None
        );
        assert_eq!(
            rsvp_changes(
                &json!({"id": "e", "attendees": [{"email": "a@x.org"}]}),
                ResponseStatus::Accepted
            ),
            None
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
    fn events_without_guests_in_writable_calendars_can_be_dragged() {
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
        for repeating in [
            serde_json::json!({"recurrence": ["RRULE:FREQ=DAILY"]}),
            serde_json::json!({"recurringEventId": "m"}),
        ] {
            assert!(can_move(&event(repeating), &owner));
        }
        // With guests: only the one who invited them.
        assert!(can_move(
            &event(serde_json::json!({
                "organizer": {"self": true}, "attendees": [{"email": "a@example.com"}]
            })),
            &owner
        ));
        for other in [
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
    fn a_drag_of_a_repeating_event_is_planned_for_each_scope() {
        use serde_json::json;
        let zone: Tz = "UTC".parse().unwrap();
        let instant = |text: &str| EventTime::Timed(text.parse::<DateTime<Utc>>().unwrap());
        let series = json!({
            "id": "m", "etag": "\"s\"", "summary": "Standup",
            "start": {"dateTime": "2026-11-16T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-16T11:00:00Z", "timeZone": "UTC"},
            "recurrence": ["RRULE:FREQ=WEEKLY;COUNT=5;BYDAY=MO,WE"]
        });
        // The third occurrence: Monday 23 November (16th, 18th, 23rd, ...).
        let target = Target {
            account: 1,
            calendar_id: "c".into(),
            event_id: "m".into(),
            etag: Some("\"s\"".into()),
            occurrence: Some(Occurrence {
                series_id: "m".into(),
                original_start: instant("2026-11-23T10:00:00Z"),
                changed: false,
            }),
            movable: true,
            guests: false,
            can_answer: false,
            start: instant("2026-11-23T10:00:00Z"),
            end: instant("2026-11-23T11:00:00Z"),
        };
        // A day later and half an hour down.
        let drag = Drag {
            instance: 0,
            days: 1,
            minutes: 30,
            resize: false,
        };
        let plan =
            |scope| plan_drag(&target, scope, drag, None, Some(series.clone()), zone).unwrap();
        let moved = json!({
            "start": {"dateTime": "2026-11-24T10:30:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-24T11:30:00Z", "timeZone": "UTC"}
        });
        let was = json!({
            "start": {"dateTime": "2026-11-23T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-11-23T11:00:00Z", "timeZone": "UTC"}
        });

        assert_eq!(
            plan(Scope::This),
            (
                Edit::Occurrence {
                    series_id: "m".into(),
                    original_start: instant("2026-11-23T10:00:00Z"),
                    known_id: None,
                    etag: None,
                    changes: moved.clone(),
                },
                Some(UndoKind::Occurrence(was)),
            )
        );

        // All: the series' own start moves by as much, and its weekdays with it.
        assert_eq!(
            plan(Scope::All),
            (
                Edit::Event {
                    event_id: "m".into(),
                    etag: Some("\"s\"".into()),
                    changes: json!({
                        "start": {"dateTime": "2026-11-17T10:30:00Z", "timeZone": "UTC"},
                        "end": {"dateTime": "2026-11-17T11:30:00Z", "timeZone": "UTC"},
                        "recurrence": ["RRULE:FREQ=WEEKLY;COUNT=5;BYDAY=TU,TH"]
                    }),
                },
                Some(UndoKind::Patch {
                    event_id: "m".into(),
                    changes: json!({
                        "start": series["start"], "end": series["end"],
                        "recurrence": series["recurrence"]
                    }),
                }),
            )
        );

        // Following: the old series ends before the occurrence; a copy goes on from
        // its new time with the three occurrences that were left.
        let (edit, undo) = plan(Scope::Following);
        assert_eq!(undo, None);
        let Edit::Following {
            series_id,
            etag,
            recurrence,
            resource,
            ..
        } = edit
        else {
            panic!("not a split");
        };
        assert_eq!((series_id.as_str(), etag.as_deref()), ("m", Some("\"s\"")));
        assert_eq!(
            recurrence,
            ["RRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261123T095959Z"]
        );
        assert_eq!(resource["summary"], "Standup");
        assert_eq!(resource["start"], moved["start"]);
        assert_eq!(resource["end"], moved["end"]);
        assert_eq!(
            resource["recurrence"],
            json!(["RRULE:FREQ=WEEKLY;COUNT=3;BYDAY=TU,TH"])
        );

        // Resizing one occurrence only moves its end.
        let grow = Drag {
            instance: 0,
            days: 0,
            minutes: 45,
            resize: true,
        };
        let (edit, _) =
            plan_drag(&target, Scope::This, grow, None, Some(series.clone()), zone).unwrap();
        let Edit::Occurrence { changes, .. } = edit else {
            panic!("not an occurrence")
        };
        assert_eq!(changes["start"]["dateTime"], "2026-11-23T10:00:00Z");
        assert_eq!(changes["end"]["dateTime"], "2026-11-23T11:45:00Z");
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
            guests: false,
            can_answer: false,
            start: instant("2026-11-18T10:00:00Z"),
            end: instant("2026-11-18T11:00:00Z"),
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
                    etag: None,
                },
                UndoKind::Occurrence(json!({"status": "confirmed"})),
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
        let (start, end) = draft_times(day, 9 * 60 + 30, false, 60, zone);
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

        let (start, end) = draft_times(day, 0, true, 60, zone);
        assert_eq!(
            event_resource("Trip", start, end, zone),
            serde_json::json!({
                "summary": "Trip",
                "start": {"date": "2026-11-17"},
                "end": {"date": "2026-11-18"},
            })
        );

        // 02:30 does not exist on the night clocks go forward.
        let (start, _) = draft_times(date("2027-03-28"), 150, false, 60, zone);
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
