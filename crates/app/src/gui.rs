//! The window: turns the cache into what the week view shows and wires up its controls.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration as StdDuration;

use anyhow::Result;
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, Timelike, Utc};
use chrono_tz::Tz;
use gcal_core::month::{MonthLayout, layout_month};
use gcal_core::recurrence::expand;
use gcal_core::text::plain_text;
use gcal_core::week::{WeekLayout, layout_days, local_day_start};
use gcal_core::{CalendarListEntry, Event, EventTime};
use gcal_store::{Account, AccountId, Store, StoredCalendar, StoredEvent};
use slint::{Color, ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::open_store;
use crate::worker::{self, Command};

slint::include_modules!();

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
}

impl Span {
    /// The order used by the interface's view switcher and the number keys, as on the web.
    const ALL: [Span; 4] = [Span::Day, Span::Week, Span::Month, Span::FourDays];

    /// The first day shown and the number of days, when the user is looking at `anchor`.
    /// The week view snaps to the start of the week, the month view shows the whole weeks
    /// that cover the month, and the others begin on the day itself.
    fn range(self, anchor: NaiveDate) -> (NaiveDate, i64) {
        match self {
            Span::Day => (anchor, 1),
            Span::FourDays => (anchor, 4),
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
    /// Series masters and their changed or deleted occurrences.
    recurring: Vec<StoredEvent>,
}

struct View {
    ui: slint::Weak<AppWindow>,
    store: Store,
    loaded: RefCell<Loaded>,
    zone: Tz,
    /// The day the user is looking at; which days are shown follows from it and `span`.
    anchor: Cell<NaiveDate>,
    span: Cell<Span>,
    /// The date the day headers were last drawn for, to notice midnight passing.
    drawn_today: Cell<Option<NaiveDate>>,
    days: Rc<VecModel<DayHeader>>,
    timed: Rc<VecModel<TimedItem>>,
    /// Details of every instance now on screen, by the index its block or bar carries.
    details: RefCell<Vec<EventDetail>>,
    all_day: Rc<VecModel<AllDayItem>>,
    month_cells: Rc<VecModel<MonthCell>>,
    month_chips: Rc<VecModel<MonthChip>>,
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
            Ok(Loaded {
                accounts: self.store.accounts()?,
                calendars: self.store.calendars()?,
                recurring: self.store.recurring_events()?,
            })
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
        let today = self.today();
        ui.set_view_index(Span::ALL.iter().position(|s| *s == span).unwrap_or(0) as i32);
        self.drawn_today.set(Some(today));
        ui.set_period_title(span.title(self.anchor.get()).into());
        if is_month {
            ui.set_month_weeks((days / 7) as i32);
        } else {
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

        let shown: HashMap<(AccountId, &str), &CalendarListEntry> = calendars
            .iter()
            .filter(|c| c.shown)
            .map(|c| ((c.account_id, c.entry.id.as_str()), &c.entry))
            .collect();

        let from = local_day_start(week_start, self.zone);
        let to = local_day_start(week_start + Duration::days(days), self.zone);
        let singles = self
            .store
            .single_events_for_range(from.timestamp() - 86_400, to.timestamp() + 86_400)?;
        let instances = expand(
            loaded
                .recurring
                .iter()
                .chain(&singles)
                .map(|e| ((e.account_id, e.calendar_id.as_str()), &e.event))
                .filter(|(key, _)| shown.contains_key(key)),
            from,
            to,
        );
        let spans: Vec<_> = instances.iter().map(|i| (i.start, i.end)).collect();
        // Only one of the two grids is on screen; the other one's models are emptied.
        let layout = if is_month {
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
            event_color(instance.event, shown[&instance.key])
        };
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
                let email = emails.get(&instance.key.0).copied().unwrap_or("");
                EventDetail {
                    title: title(index),
                    when: when_text(instance.start, instance.end, self.zone).into(),
                    location: event.location.as_deref().unwrap_or("").into(),
                    description: plain_text(event.description.as_deref().unwrap_or("")).into(),
                    calendar: shown[&instance.key].display_name().into(),
                    account: email.into(),
                    color: color(index),
                    link: web_link(event, email).into(),
                }
            })
            .collect();
        self.details.replace(details);
        // What it described may have moved or gone.
        ui.set_detail_visible(false);
        Ok(())
    }

    /// Moves the current-time line.
    fn update_now(&self, ui: &AppWindow) {
        let now = Utc::now().with_timezone(&self.zone);
        let (first, days) = self.range();
        let offset = (now.date_naive() - first).num_days();
        if self.span.get() != Span::Month && (0..days).contains(&offset) {
            ui.set_now_day(offset as i32);
            ui.set_now_min((now.hour() * 60 + now.minute()) as i32);
        } else {
            ui.set_now_day(-1);
        }
    }

    /// Shows or hides a calendar, remembers the choice, and redraws.
    fn set_calendar_shown(&self, account: AccountId, calendar_id: &str, shown: bool) {
        let saved = self
            .store
            .set_calendar_shown(account, calendar_id, shown)
            .and_then(|()| self.store.calendars());
        match saved {
            Ok(calendars) => {
                // Only the calendar list changed; the series need no re-reading.
                self.loaded.borrow_mut().calendars = calendars;
                self.refresh();
            }
            Err(error) => self.show_error(&error.into()),
        }
    }

    fn show(&self, anchor: NaiveDate) {
        self.anchor.set(anchor);
        self.refresh();
    }

    /// Moves one screenful back (-1) or forward (1).
    fn step(&self, direction: i64) {
        self.show(self.span.get().step(self.anchor.get(), direction));
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
    let view = Rc::new(View {
        ui: ui.as_weak(),
        store: open_store()?,
        loaded: RefCell::default(),
        zone,
        anchor: Cell::new(Utc::now().with_timezone(&zone).date_naive()),
        span: Cell::new(Span::Week),
        drawn_today: Cell::new(None),
        days: Rc::default(),
        timed: Rc::default(),
        details: RefCell::default(),
        all_day: Rc::default(),
        month_cells: Rc::default(),
        month_chips: Rc::default(),
        sidebar: Rc::default(),
    });
    ui.set_days(ModelRc::from(view.days.clone()));
    ui.set_timed(ModelRc::from(view.timed.clone()));
    ui.set_all_day(ModelRc::from(view.all_day.clone()));
    ui.set_month_cells(ModelRc::from(view.month_cells.clone()));
    ui.set_month_chips(ModelRc::from(view.month_chips.clone()));
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

    let commands = worker::spawn(ui.as_weak());

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
    fn patch_changes_only_what_differs() {
        let model = VecModel::from(vec![1, 2, 3, 4]);
        patch(&model, vec![1, 5, 3]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5, 3]);
        patch(&model, vec![1, 5, 3, 9]);
        assert_eq!(model.iter().collect::<Vec<_>>(), [1, 5, 3, 9]);
    }
}
