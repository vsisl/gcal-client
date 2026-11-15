//! The event editor's form: what it shows for an event, and what to send to Google for
//! what the user made of it. No window and no network here, so all of it is tested.
//!
//! The form is compared with itself: the fields Google would get from the form as it
//! was opened, against those from the form as it was saved. Only what differs is sent,
//! so whatever the form cannot express (a formatted description, an unusual reminder or
//! repeat rule) is left alone unless the user changed that very field.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use gcal_core::recurrence::{remaining_series, shift_weekdays, split_dates};
use gcal_core::text::plain_text;
use gcal_core::{Event, EventTime};
use serde_json::{Value, json};

/// Choices of the "repeat" list, in the order the window shows them.
pub const REPEAT_NONE: usize = 0;
const REPEAT_DAILY: usize = 1;
const REPEAT_WEEKLY: usize = 2;
const REPEAT_MONTHLY: usize = 3;
const REPEAT_YEARLY: usize = 4;
const REPEAT_WEEKDAYS: usize = 5;
/// The event's own rule, which is none of the above; kept as it is.
pub const REPEAT_CUSTOM: usize = 6;

/// Choices of the "reminder" list: the calendar's default, none, then these minutes
/// before the event; the last choice keeps reminders that are none of these.
const REMINDER_DEFAULT: usize = 0;
const REMINDER_NONE: usize = 1;
const REMINDER_MINUTES: [i64; 7] = [0, 5, 10, 15, 30, 60, 24 * 60];
pub const REMINDER_CUSTOM: usize = 2 + REMINDER_MINUTES.len();

const VISIBILITY: [&str; 3] = ["default", "public", "private"];

/// The editor's fields, as text and choices.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Form {
    pub title: String,
    pub all_day: bool,
    /// Dates as `2026-11-17`; the end date is the last day of an all-day event.
    pub start_date: String,
    pub end_date: String,
    /// Times as `9:30`; ignored for all-day events.
    pub start_time: String,
    pub end_time: String,
    pub repeat: usize,
    pub location: String,
    pub description: String,
    /// 0 for the calendar's colour, else the number of one of Google's event colours.
    pub color: usize,
    pub reminder: usize,
    pub visibility: usize,
    pub busy: bool,
}

/// What the form cannot show but must not lose.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Kept {
    pub recurrence: Vec<String>,
    pub reminders: Value,
    pub start_zone: Option<String>,
    pub end_zone: Option<String>,
}

fn clock(instant: DateTime<Utc>, zone: Tz) -> String {
    instant.with_timezone(&zone).format("%-H:%M").to_string()
}

fn date(instant: DateTime<Utc>, zone: Tz) -> NaiveDate {
    instant.with_timezone(&zone).date_naive()
}

/// The form for a new event at the given time.
pub fn new_form(start: EventTime, end: EventTime, zone: Tz) -> Form {
    let mut form = Form {
        busy: true,
        ..Form::default()
    };
    set_times(&mut form, start, end, zone);
    form
}

fn set_times(form: &mut Form, start: EventTime, end: EventTime, zone: Tz) {
    match (start, end) {
        (EventTime::AllDay(first), EventTime::AllDay(after)) => {
            form.all_day = true;
            form.start_date = first.to_string();
            form.end_date = (after - Duration::days(1)).max(first).to_string();
            // Shown again if "all day" is unticked.
            form.start_time = "9:00".into();
            form.end_time = "10:00".into();
        }
        (EventTime::Timed(from), EventTime::Timed(to)) => {
            form.all_day = false;
            form.start_date = date(from, zone).to_string();
            form.end_date = date(to, zone).to_string();
            form.start_time = clock(from, zone);
            form.end_time = clock(to, zone);
        }
        _ => {}
    }
}

/// The repeat rules a choice stands for, for an event starting on `start`.
fn repeat_rules(choice: usize, start: NaiveDate, kept: &Kept) -> Vec<String> {
    let weekday = start.format("%a").to_string().to_uppercase();
    let rule = match choice {
        REPEAT_DAILY => "RRULE:FREQ=DAILY".to_owned(),
        REPEAT_WEEKLY => format!("RRULE:FREQ=WEEKLY;BYDAY={}", &weekday[..2]),
        REPEAT_MONTHLY => "RRULE:FREQ=MONTHLY".to_owned(),
        REPEAT_YEARLY => "RRULE:FREQ=YEARLY".to_owned(),
        REPEAT_WEEKDAYS => "RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR".to_owned(),
        REPEAT_CUSTOM => return kept.recurrence.clone(),
        _ => return Vec::new(),
    };
    vec![rule]
}

/// The form for an existing event. `resource` is the event (for an occurrence of a
/// series: the series, or the occurrence itself if it was changed) and `start`/`end`
/// are the times of the instance that was opened.
pub fn form_of(resource: &Value, start: EventTime, end: EventTime, zone: Tz) -> (Form, Kept) {
    let text = |name: &str| resource[name].as_str().unwrap_or("").to_owned();
    let recurrence: Vec<String> = resource["recurrence"]
        .as_array()
        .map(|lines| {
            lines
                .iter()
                .filter_map(|line| line.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let kept = Kept {
        recurrence,
        reminders: resource["reminders"].clone(),
        start_zone: resource["start"]["timeZone"].as_str().map(str::to_owned),
        end_zone: resource["end"]["timeZone"].as_str().map(str::to_owned),
    };
    let mut form = Form {
        title: text("summary"),
        location: text("location"),
        description: plain_text(&text("description")),
        color: text("colorId")
            .parse()
            .ok()
            .filter(|n| (1..=11).contains(n))
            .unwrap_or(0),
        visibility: match resource["visibility"].as_str() {
            Some("public") => 1,
            Some("private" | "confidential") => 2,
            _ => 0,
        },
        busy: resource["transparency"].as_str() != Some("transparent"),
        ..Form::default()
    };
    set_times(&mut form, start, end, zone);

    // The series' own first day decides what "weekly" means for it.
    let series_day = serde_json::from_value::<Event>(resource.clone())
        .ok()
        .and_then(|event| event.start_time())
        .map(|first| match first {
            EventTime::AllDay(day) => day,
            EventTime::Timed(instant) => date(instant, zone),
        });
    form.repeat = match series_day {
        _ if kept.recurrence.is_empty() => REPEAT_NONE,
        Some(day) => (REPEAT_DAILY..REPEAT_CUSTOM)
            .find(|choice| repeat_rules(*choice, day, &kept) == kept.recurrence)
            .unwrap_or(REPEAT_CUSTOM),
        None => REPEAT_CUSTOM,
    };
    let overrides = kept.reminders["overrides"].as_array();
    form.reminder = if kept.reminders["useDefault"].as_bool() != Some(false) {
        REMINDER_DEFAULT
    } else {
        match overrides.map(Vec::as_slice) {
            None | Some([]) => REMINDER_NONE,
            Some([one]) if one["method"] == "popup" => REMINDER_MINUTES
                .iter()
                .position(|minutes| one["minutes"].as_i64() == Some(*minutes))
                .map_or(REMINDER_CUSTOM, |position| position + 2),
            _ => REMINDER_CUSTOM,
        }
    };
    (form, kept)
}

fn parse_date(text: &str, what: &str) -> Result<NaiveDate, String> {
    NaiveDate::parse_from_str(text.trim(), "%Y-%m-%d")
        .map_err(|_| format!("The {what} date should look like 2026-11-17."))
}

fn parse_time(text: &str, what: &str) -> Result<NaiveTime, String> {
    let text = text.trim().replace('.', ":");
    NaiveTime::parse_from_str(&text, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(&format!("{text}:00"), "%H:%M"))
        .map_err(|_| format!("The {what} time should look like 9:30."))
}

/// The day the form's event starts on.
pub fn start_day(form: &Form) -> Result<NaiveDate, String> {
    parse_date(&form.start_date, "start")
}

/// Everything the form says about an event, as the fields of Google's event resource.
/// Fails with a sentence for the user when a date or time cannot be understood.
pub fn fields(form: &Form, kept: &Kept, zone: Tz) -> Result<Value, String> {
    let first = parse_date(&form.start_date, "start")?;
    let last = parse_date(&form.end_date, "end")?;
    let (start, end) = if form.all_day {
        if last < first {
            return Err("The event ends before it starts.".into());
        }
        (
            json!({ "date": first.to_string() }),
            json!({ "date": (last + Duration::days(1)).to_string() }),
        )
    } else {
        let instant = |day: NaiveDate, time: NaiveTime| {
            zone.from_local_datetime(&day.and_time(time))
                .earliest()
                .map(|local| local.with_timezone(&Utc))
                .ok_or_else(|| {
                    format!(
                        "{} does not exist on {day}: the clocks skip it.",
                        time.format("%-H:%M")
                    )
                })
        };
        let from = instant(first, parse_time(&form.start_time, "start")?)?;
        let to = instant(last, parse_time(&form.end_time, "end")?)?;
        if to < from {
            return Err("The event ends before it starts.".into());
        }
        let field = |instant: DateTime<Utc>, kept_zone: &Option<String>| {
            json!({
                "dateTime": instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                "timeZone": kept_zone.clone().unwrap_or_else(|| zone.name().to_owned()),
            })
        };
        (field(from, &kept.start_zone), field(to, &kept.end_zone))
    };
    let reminders = match form.reminder {
        REMINDER_DEFAULT => json!({ "useDefault": true }),
        REMINDER_NONE => json!({ "useDefault": false, "overrides": [] }),
        REMINDER_CUSTOM => kept.reminders.clone(),
        choice => json!({
            "useDefault": false,
            "overrides": [{
                "method": "popup",
                "minutes": REMINDER_MINUTES.get(choice - 2).copied().unwrap_or(10),
            }],
        }),
    };
    Ok(json!({
        "summary": form.title.trim(),
        "start": start,
        "end": end,
        "recurrence": repeat_rules(form.repeat, first, kept),
        "location": form.location.trim(),
        "description": form.description.trim(),
        "colorId": if form.color == 0 { Value::Null } else { json!(form.color.to_string()) },
        "reminders": reminders,
        "visibility": VISIBILITY.get(form.visibility).copied().unwrap_or("default"),
        "transparency": if form.busy { "opaque" } else { "transparent" },
    }))
}

/// A start or end as a change to send. Google merges a change into what the event
/// has, so a time that turns from all-day into timed (or back) must say that the other
/// kind's fields go; else the event would have both and be refused.
fn whole_time(mut time: Value) -> Value {
    if time.get("date").is_some_and(|date| !date.is_null()) {
        time["dateTime"] = Value::Null;
        time["timeZone"] = Value::Null;
    } else if time.get("dateTime").is_some() {
        time["date"] = Value::Null;
    }
    time
}

fn whole_times(changes: &mut Value) {
    for name in ["start", "end"] {
        if let Some(time) = changes.get(name).cloned() {
            changes[name] = whole_time(time);
        }
    }
}

/// The fields that differ between two sets, with their new values.
pub fn diff(before: &Value, after: &Value) -> Value {
    let mut changes = serde_json::Map::new();
    if let (Some(before), Some(after)) = (before.as_object(), after.as_object()) {
        for (name, value) in after {
            if before.get(name) != Some(value) {
                changes.insert(name.clone(), value.clone());
            }
        }
    }
    let mut changes = Value::Object(changes);
    whole_times(&mut changes);
    changes
}

/// What takes `changes` back: the values those fields have in `resource`. A field the
/// resource does not have is sent as null, which removes it again.
pub fn inverse(changes: &Value, resource: &Value) -> Value {
    let mut before = serde_json::Map::new();
    if let Some(changes) = changes.as_object() {
        for name in changes.keys() {
            let was = match (name.as_str(), resource.get(name)) {
                (_, Some(value)) => value.clone(),
                ("recurrence", None) => json!([]),
                (_, None) => Value::Null,
            };
            before.insert(name.clone(), was);
        }
    }
    let mut before = Value::Object(before);
    whole_times(&mut before);
    before
}

/// The resource to create a new event from: the fields without those that say nothing.
pub fn creation(fields: Value) -> Value {
    let Value::Object(mut fields) = fields else {
        return fields;
    };
    let says_nothing = |name: &str, value: &Value| match name {
        "location" | "description" => value == "",
        "colorId" => value.is_null(),
        "recurrence" => value == &json!([]),
        "reminders" => value == &json!({ "useDefault": true }),
        "visibility" => value == "default",
        "transparency" => value == "opaque",
        _ => false,
    };
    fields.retain(|name, value| !says_nothing(name, value));
    Value::Object(fields)
}

/// Shows a changed occurrence as repeating the way its series does. On its own it has
/// no rule, and would read "does not repeat"; saved for the whole series, that would
/// have ended the series.
pub fn adopt_series_rule(form: &mut Form, kept: &mut Kept, series: &Value, zone: Tz) {
    let (Some(start), Some(end)) = serde_json::from_value::<Event>(series.clone())
        .ok()
        .map(|event| (event.start_time(), event.end_time()))
        .unwrap_or_default()
    else {
        return;
    };
    let (of_series, kept_of_series) = form_of(series, start, end, zone);
    form.repeat = of_series.repeat;
    kept.recurrence = kept_of_series.recurrence;
}

fn instant_of(time: &Value) -> Option<DateTime<Utc>> {
    time["dateTime"].as_str()?.parse().ok()
}

fn day_of(time: &Value) -> Option<NaiveDate> {
    time["date"].as_str()?.parse().ok()
}

/// `changes` made in the form to one occurrence, turned into changes to its whole
/// series: other fields apply as they are; the series' first occurrence moves by as
/// much as the opened one did (`before` and `after` are all the form's fields as it
/// was opened and saved), and gets its new length.
///
/// The move is measured on the clock of the series' own time zone. Measured on the
/// viewer's clock and applied to a first occurrence in another season, it would be off
/// by an hour whenever one of the two zones changes its clocks and the other does not.
pub fn for_series(
    changes: &Value,
    before: &Value,
    after: &Value,
    kept: &Kept,
    series: &Event,
    zone: Tz,
) -> Result<Value, String> {
    let mut changes = changes.clone();
    let moved = changes.get("start").is_some() || changes.get("end").is_some();
    let (Some(first), true) = (series.start_time(), moved) else {
        return Ok(changes);
    };
    let series_zone: Tz = kept
        .start_zone
        .as_deref()
        .and_then(|name| name.parse().ok())
        .unwrap_or(zone);
    let on_clock = |instant: DateTime<Utc>| instant.with_timezone(&series_zone).naive_local();
    let field = |instant: DateTime<Utc>, kept_zone: &Option<String>| {
        json!({
            "dateTime": instant.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "timeZone": kept_zone.clone().unwrap_or_else(|| zone.name().to_owned()),
        })
    };
    let (was, now) = (&before["start"], &after["start"]);
    let days;
    match (
        first,
        instant_of(was),
        instant_of(now),
        day_of(was),
        day_of(now),
    ) {
        // Timed before and after.
        (EventTime::Timed(first), Some(was), Some(now), _, _) => {
            let Some(end) = instant_of(&after["end"]) else {
                return Ok(changes);
            };
            let by = on_clock(now) - on_clock(was);
            let moved_local = on_clock(first) + by;
            let start = series_zone
                .from_local_datetime(&moved_local)
                .earliest()
                .map(|start| start.with_timezone(&Utc))
                .unwrap_or(first + by);
            days = (moved_local.date() - on_clock(first).date()).num_days();
            changes["start"] = field(start, &kept.start_zone);
            changes["end"] = field(start + (end - now), &kept.end_zone);
        }
        // All-day before and after.
        (EventTime::AllDay(first), _, _, Some(was), Some(now)) => {
            let Some(end) = day_of(&after["end"]) else {
                return Ok(changes);
            };
            days = (now - was).num_days();
            let start = first + Duration::days(days);
            changes["start"] = json!({ "date": start.to_string() });
            changes["end"] = json!({ "date": (start + (end - now)).to_string() });
        }
        // Between all-day and timed: the new times as the form has them, on the
        // series' first day.
        (first, ..) => {
            let first_day = match first {
                EventTime::AllDay(day) => day,
                EventTime::Timed(instant) => date(instant, zone),
            };
            let (was_day, now_day) = match (day_of(was), day_of(now)) {
                (Some(was), _) => (was, instant_of(now).map(|now| date(now, zone))),
                (None, now) => (
                    instant_of(was)
                        .map(|was| date(was, zone))
                        .unwrap_or(first_day),
                    now,
                ),
            };
            days = now_day.map_or(0, |now| (now - was_day).num_days());
            let shift = |time: &Value| -> Value {
                let back = (first_day - was_day).num_days();
                match (day_of(time), instant_of(time)) {
                    (Some(day), _) => json!({ "date": (day + Duration::days(back)).to_string() }),
                    (None, Some(instant)) => {
                        let local =
                            instant.with_timezone(&zone).naive_local() + Duration::days(back);
                        let moved = zone
                            .from_local_datetime(&local)
                            .earliest()
                            .map(|moved| moved.with_timezone(&Utc))
                            .unwrap_or(instant + Duration::days(back));
                        field(moved, &None)
                    }
                    _ => time.clone(),
                }
            };
            changes["start"] = shift(&after["start"]);
            changes["end"] = shift(&after["end"]);
        }
    }
    whole_times(&mut changes);
    // A series moved to another weekday repeats on that one.
    match changes.get("recurrence").cloned() {
        Some(chosen) => {
            let chosen: Vec<String> = serde_json::from_value(chosen).unwrap_or_default();
            // A rule picked in the form was made for the opened occurrence's weekday,
            // which is the series' own as long as the form's rule is one of its lists.
            changes["recurrence"] = json!(chosen);
        }
        None if days != 0 => {
            changes["recurrence"] = json!(shift_weekdays(&kept.recurrence, days));
        }
        None => {}
    }
    Ok(changes)
}

/// The new series that "this and following" makes from an occurrence that would have
/// started at `boundary`: a copy of the old one with what the form changed, starting at
/// the occurrence's new time, with the form's rule if that was changed and else what
/// was left of the old one.
pub fn following_series(
    series: &Value,
    changes: &Value,
    all: &Value,
    earlier: u32,
    days: i64,
    boundary: EventTime,
) -> Value {
    let mut copy = series.clone();
    if let Some(changes) = changes.as_object() {
        for (name, value) in changes {
            copy[name] = value.clone();
        }
    }
    copy["start"] = all["start"].clone();
    copy["end"] = all["end"].clone();
    if changes.get("recurrence").is_none() {
        let old: Vec<String> = series["recurrence"]
            .as_array()
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let left = split_dates(&remaining_series(&old, earlier), boundary, true);
        copy["recurrence"] = json!(shift_weekdays(&left, days));
    }
    copy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone() -> Tz {
        "Europe/Prague".parse().unwrap()
    }

    fn timed(text: &str) -> EventTime {
        EventTime::Timed(text.parse().unwrap())
    }

    fn day(d: u32) -> EventTime {
        EventTime::AllDay(NaiveDate::from_ymd_opt(2026, 11, d).unwrap())
    }

    #[test]
    fn a_new_form_shows_the_time_that_was_clicked() {
        let form = new_form(
            timed("2026-11-17T09:00:00Z"),
            timed("2026-11-17T10:00:00Z"),
            zone(),
        );
        assert_eq!(
            (form.start_date.as_str(), form.start_time.as_str()),
            ("2026-11-17", "10:00")
        );
        assert_eq!(
            (form.end_date.as_str(), form.end_time.as_str()),
            ("2026-11-17", "11:00")
        );
        assert!(form.busy && !form.all_day);

        // An all-day event of two days shows its last day, not the day after.
        let form = new_form(day(16), day(18), zone());
        assert!(form.all_day);
        assert_eq!(
            (form.start_date.as_str(), form.end_date.as_str()),
            ("2026-11-16", "2026-11-17")
        );
    }

    #[test]
    fn a_new_event_is_created_from_what_was_filled_in_only() {
        let mut form = new_form(
            timed("2026-11-17T09:00:00Z"),
            timed("2026-11-17T10:00:00Z"),
            zone(),
        );
        form.title = " Dentist ".into();
        assert_eq!(
            creation(fields(&form, &Kept::default(), zone()).unwrap()),
            json!({
                "summary": "Dentist",
                "start": {"dateTime": "2026-11-17T09:00:00Z", "timeZone": "Europe/Prague"},
                "end": {"dateTime": "2026-11-17T10:00:00Z", "timeZone": "Europe/Prague"},
            })
        );

        form.all_day = true;
        form.end_date = "2026-11-18".into();
        form.repeat = REPEAT_WEEKLY;
        form.location = "Main street 1".into();
        form.color = 5;
        form.reminder = 4;
        form.visibility = 2;
        form.busy = false;
        assert_eq!(
            creation(fields(&form, &Kept::default(), zone()).unwrap()),
            json!({
                "summary": "Dentist",
                "start": {"date": "2026-11-17"},
                "end": {"date": "2026-11-19"},
                "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=TU"],
                "location": "Main street 1",
                "colorId": "5",
                "reminders": {"useDefault": false, "overrides": [{"method": "popup", "minutes": 10}]},
                "visibility": "private",
                "transparency": "transparent",
            })
        );
    }

    #[test]
    fn dates_and_times_that_make_no_sense_are_said_in_words() {
        let good = new_form(
            timed("2026-11-17T09:00:00Z"),
            timed("2026-11-17T10:00:00Z"),
            zone(),
        );
        let error = |change: &dyn Fn(&mut Form)| {
            let mut form = good.clone();
            change(&mut form);
            fields(&form, &Kept::default(), zone()).unwrap_err()
        };
        assert_eq!(
            error(&|f| f.start_date = "17.11.".into()),
            "The start date should look like 2026-11-17."
        );
        assert_eq!(
            error(&|f| f.end_time = "noon".into()),
            "The end time should look like 9:30."
        );
        assert_eq!(
            error(&|f| f.end_time = "9:00".into()),
            "The event ends before it starts."
        );
        // Forgiving about how a time is typed.
        let mut form = good.clone();
        form.start_time = "9.30".into();
        form.end_time = "11".into();
        let sent = fields(&form, &Kept::default(), zone()).unwrap();
        assert_eq!(sent["start"]["dateTime"], "2026-11-17T08:30:00Z");
        assert_eq!(sent["end"]["dateTime"], "2026-11-17T10:00:00Z");
    }

    fn lunch() -> Value {
        json!({
            "id": "e", "summary": "Lunch", "location": "Canteen",
            "description": "Bring <b>cake</b>",
            "start": {"dateTime": "2026-11-17T12:00:00+01:00", "timeZone": "Europe/Zurich"},
            "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Zurich"},
            "colorId": "7", "visibility": "confidential", "transparency": "transparent",
            "reminders": {"useDefault": false, "overrides": [
                {"method": "email", "minutes": 30}, {"method": "popup", "minutes": 7}
            ]},
            "recurrence": ["RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TU"]
        })
    }

    #[test]
    fn an_event_is_shown_and_saving_it_untouched_changes_nothing() {
        let (start, end) = (timed("2026-11-17T11:00:00Z"), timed("2026-11-17T12:00:00Z"));
        let (form, kept) = form_of(&lunch(), start, end, zone());
        assert_eq!(
            form,
            Form {
                title: "Lunch".into(),
                all_day: false,
                start_date: "2026-11-17".into(),
                end_date: "2026-11-17".into(),
                start_time: "12:00".into(),
                end_time: "13:00".into(),
                repeat: REPEAT_CUSTOM,
                location: "Canteen".into(),
                description: "Bring cake".into(),
                color: 7,
                reminder: REMINDER_CUSTOM,
                visibility: 2,
                busy: false,
            }
        );
        let opened = fields(&form, &kept, zone()).unwrap();
        assert_eq!(diff(&opened, &opened), json!({}));

        // Only the field that was touched is sent; the formatted description, the
        // unusual rule and reminders, "confidential" and the event's zone all stay.
        let mut edited = form.clone();
        edited.title = "Long lunch".into();
        edited.end_time = "13:30".into();
        let changes = diff(&opened, &fields(&edited, &kept, zone()).unwrap());
        assert_eq!(
            changes,
            json!({
                "summary": "Long lunch",
                "end": {"dateTime": "2026-11-17T12:30:00Z", "timeZone": "Europe/Zurich", "date": null},
            })
        );
        assert_eq!(
            inverse(&changes, &lunch()),
            json!({
                "summary": "Lunch",
                "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Zurich", "date": null},
            })
        );
        // A field the event did not have is removed again by null.
        let plain = json!({"id": "e", "summary": "x"});
        assert_eq!(
            inverse(
                &json!({"location": "There", "recurrence": ["RRULE:FREQ=DAILY"]}),
                &plain
            ),
            json!({"location": null, "recurrence": []})
        );
    }

    #[test]
    fn common_rules_and_reminders_are_recognised() {
        let event = |extra: Value| {
            let mut event = json!({
                "id": "e",
                "start": {"dateTime": "2026-11-17T12:00:00+01:00"},
                "end": {"dateTime": "2026-11-17T13:00:00+01:00"},
            });
            event
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            form_of(
                &event,
                timed("2026-11-17T11:00:00Z"),
                timed("2026-11-17T12:00:00Z"),
                zone(),
            )
            .0
        };
        assert_eq!(event(json!({})).repeat, REPEAT_NONE);
        assert_eq!(event(json!({})).reminder, REMINDER_DEFAULT);
        assert_eq!(event(json!({})).visibility, 0);
        assert!(event(json!({})).busy);
        let repeat = |rule: &str| event(json!({"recurrence": [rule]})).repeat;
        assert_eq!(repeat("RRULE:FREQ=DAILY"), REPEAT_DAILY);
        assert_eq!(repeat("RRULE:FREQ=WEEKLY;BYDAY=TU"), REPEAT_WEEKLY);
        assert_eq!(repeat("RRULE:FREQ=WEEKLY;BYDAY=WE"), REPEAT_CUSTOM);
        assert_eq!(repeat("RRULE:FREQ=MONTHLY"), REPEAT_MONTHLY);
        assert_eq!(repeat("RRULE:FREQ=YEARLY"), REPEAT_YEARLY);
        assert_eq!(
            repeat("RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR"),
            REPEAT_WEEKDAYS
        );
        let reminder = |reminders: Value| event(json!({"reminders": reminders})).reminder;
        assert_eq!(reminder(json!({"useDefault": false})), REMINDER_NONE);
        assert_eq!(
            reminder(
                json!({"useDefault": false, "overrides": [{"method": "popup", "minutes": 30}]})
            ),
            6
        );
        assert_eq!(
            reminder(
                json!({"useDefault": false, "overrides": [{"method": "email", "minutes": 30}]})
            ),
            REMINDER_CUSTOM
        );
    }

    #[test]
    fn turning_an_event_all_day_and_back_clears_the_other_kind_of_time() {
        let (start, end) = (timed("2026-11-17T11:00:00Z"), timed("2026-11-17T12:00:00Z"));
        let event = json!({
            "id": "e",
            "start": {"dateTime": "2026-11-17T12:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Prague"},
        });
        let (form, kept) = form_of(&event, start, end, zone());
        let before = fields(&form, &kept, zone()).unwrap();
        let mut all_day = form.clone();
        all_day.all_day = true;
        let changes = diff(&before, &fields(&all_day, &kept, zone()).unwrap());
        assert_eq!(
            changes,
            json!({
                "start": {"date": "2026-11-17", "dateTime": null, "timeZone": null},
                "end": {"date": "2026-11-18", "dateTime": null, "timeZone": null},
            })
        );
        // And the way back says that the date goes.
        assert_eq!(
            inverse(&changes, &event),
            json!({
                "start": {"dateTime": "2026-11-17T12:00:00+01:00", "timeZone": "Europe/Prague", "date": null},
                "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Prague", "date": null},
            })
        );
    }

    #[test]
    fn a_changed_occurrence_repeats_as_its_series_does() {
        let series = json!({
            "id": "m",
            "start": {"dateTime": "2026-11-16T10:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-16T10:30:00+01:00", "timeZone": "Europe/Prague"},
            "recurrence": ["RRULE:FREQ=WEEKLY;INTERVAL=2"]
        });
        let master: Event = serde_json::from_value(series.clone()).unwrap();
        // The occurrence of 30 November was moved to 11:00 before; it has no rule.
        let occurrence = json!({
            "id": "m_x", "recurringEventId": "m",
            "start": {"dateTime": "2026-11-30T11:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-30T11:30:00+01:00", "timeZone": "Europe/Prague"},
        });
        let (mut opened, mut kept) = form_of(
            &occurrence,
            timed("2026-11-30T10:00:00Z"),
            timed("2026-11-30T10:30:00Z"),
            zone(),
        );
        assert_eq!(opened.repeat, REPEAT_NONE);
        adopt_series_rule(&mut opened, &mut kept, &series, zone());
        assert_eq!(opened.repeat, REPEAT_CUSTOM);

        // Moved a day on and saved for the whole series: the series moves and goes on
        // repeating.
        let before = fields(&opened, &kept, zone()).unwrap();
        let mut moved = opened.clone();
        moved.start_date = "2026-12-01".into();
        moved.end_date = "2026-12-01".into();
        let after = fields(&moved, &kept, zone()).unwrap();
        let changes = for_series(
            &diff(&before, &after),
            &before,
            &after,
            &kept,
            &master,
            zone(),
        )
        .unwrap();
        assert_eq!(changes["start"]["dateTime"], "2026-11-17T09:00:00Z");
        assert_eq!(
            changes["recurrence"],
            json!(["RRULE:FREQ=WEEKLY;INTERVAL=2"])
        );
    }

    #[test]
    fn a_series_in_another_zone_moves_by_its_own_clock() {
        // Ten o'clock UTC every day from January; in Prague that is 11:00 in winter
        // and 12:00 in summer.
        let series = json!({
            "id": "m",
            "start": {"dateTime": "2026-01-05T10:00:00Z", "timeZone": "UTC"},
            "end": {"dateTime": "2026-01-05T11:00:00Z", "timeZone": "UTC"},
            "recurrence": ["RRULE:FREQ=DAILY"]
        });
        let master: Event = serde_json::from_value(series.clone()).unwrap();
        // Opened in July, where it shows as 12:00, and moved an hour later.
        let (opened, kept) = form_of(
            &series,
            timed("2026-07-06T10:00:00Z"),
            timed("2026-07-06T11:00:00Z"),
            zone(),
        );
        assert_eq!(opened.start_time, "12:00");
        let before = fields(&opened, &kept, zone()).unwrap();
        let mut later = opened.clone();
        later.start_time = "13:00".into();
        later.end_time = "14:00".into();
        let after = fields(&later, &kept, zone()).unwrap();
        let changes = for_series(
            &diff(&before, &after),
            &before,
            &after,
            &kept,
            &master,
            zone(),
        )
        .unwrap();
        // An hour later on the series' clock, not Prague's winter one.
        assert_eq!(changes["start"]["dateTime"], "2026-01-05T11:00:00Z");
        assert_eq!(changes["end"]["dateTime"], "2026-01-05T12:00:00Z");
        assert!(changes.get("recurrence").is_none());
    }

    #[test]
    fn a_change_to_one_occurrence_is_carried_over_to_the_series() {
        let series = json!({
            "id": "m", "summary": "Standup",
            "start": {"dateTime": "2026-11-16T10:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-16T10:30:00+01:00", "timeZone": "Europe/Prague"},
            "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO,WE"]
        });
        let master: Event = serde_json::from_value(series.clone()).unwrap();
        // Opened on Wednesday 25 November.
        let (opened, kept) = form_of(
            &series,
            timed("2026-11-25T09:00:00Z"),
            timed("2026-11-25T09:30:00Z"),
            zone(),
        );
        let before = fields(&opened, &kept, zone()).unwrap();

        // Only the title: nothing about times is sent.
        let mut renamed = opened.clone();
        renamed.title = "Daily".into();
        let changes = diff(&before, &fields(&renamed, &kept, zone()).unwrap());
        assert_eq!(
            for_series(&changes, &before, &before, &kept, &master, zone()).unwrap(),
            json!({"summary": "Daily"})
        );

        // A day later and an hour longer: the series moves from its own first day,
        // and its weekdays move along.
        let mut moved = opened.clone();
        moved.start_date = "2026-11-26".into();
        moved.end_date = "2026-11-26".into();
        moved.end_time = "11:30".into();
        let after = fields(&moved, &kept, zone()).unwrap();
        let changes = diff(&before, &after);
        assert_eq!(
            for_series(&changes, &before, &after, &kept, &master, zone()).unwrap(),
            json!({
                "start": {"dateTime": "2026-11-17T09:00:00Z", "timeZone": "Europe/Prague", "date": null},
                "end": {"dateTime": "2026-11-17T10:30:00Z", "timeZone": "Europe/Prague", "date": null},
                "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=TU,TH"],
            })
        );

        // This and following: a copy from the occurrence's new time on, with what was
        // left of the rule.
        let copy = following_series(
            &series,
            &changes,
            &after,
            3,
            1,
            timed("2026-11-25T09:00:00Z"),
        );
        assert_eq!(copy["summary"], "Standup");
        assert_eq!(copy["start"]["dateTime"], "2026-11-26T09:00:00Z");
        assert_eq!(copy["end"]["dateTime"], "2026-11-26T10:30:00Z");
        assert_eq!(copy["recurrence"], json!(["RRULE:FREQ=WEEKLY;BYDAY=TU,TH"]));
    }
}
