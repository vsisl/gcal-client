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

/// How many notifications Google lets an event have.
pub const MOST_NOTIFICATIONS: usize = 5;

/// One notification before an event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Notification {
    pub minutes: i64,
    /// By e-mail rather than on the screen.
    pub email: bool,
}

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
    /// Whether the event has the notifications usual in its calendar; if not, its own
    /// ones follow (none at all is possible).
    pub usual_notifications: bool,
    pub notifications: Vec<Notification>,
    pub visibility: usize,
    pub busy: bool,
    /// The guests' addresses, separated by commas, spaces or new lines.
    pub guests: String,
    /// The addresses among them whose coming is optional.
    pub optional: Vec<String>,
    /// Whether the event has a Google Meet video meeting.
    pub meet: bool,
    /// What the guests may do: change the event, invite others, see who else is
    /// invited. Google's own defaults are no, yes, yes.
    pub may_modify: bool,
    pub may_invite: bool,
    pub may_see: bool,
}

/// What the form cannot show but must not lose.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Kept {
    pub recurrence: Vec<String>,
    pub start_zone: Option<String>,
    pub end_zone: Option<String>,
    /// The guests as the event has them, with their answers and whatever else Google
    /// keeps about each; a guest who stays in the form stays as it is here.
    pub attendees: Vec<Value>,
    /// The address of the account, when the event is in that account's own calendar
    /// and so organised by it; empty otherwise. It is added to the guests when the
    /// first ones are invited, as the web does, so that the organiser is among them.
    /// In a calendar shared by someone else the organiser is not this account, and it
    /// must not invite itself.
    pub own_email: String,
    /// The event's video meeting as Google describes it; null if it has none.
    pub conference: Value,
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
        usual_notifications: true,
        may_invite: true,
        may_see: true,
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
        start_zone: resource["start"]["timeZone"].as_str().map(str::to_owned),
        end_zone: resource["end"]["timeZone"].as_str().map(str::to_owned),
        attendees: resource["attendees"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        own_email: String::new(),
        conference: resource["conferenceData"].clone(),
    };
    let guests: Vec<&str> = kept
        .attendees
        .iter()
        .filter_map(|guest| guest["email"].as_str())
        .collect();
    let mut form = Form {
        meet: !kept.conference.is_null(),
        guests: guests.join(", "),
        optional: kept
            .attendees
            .iter()
            .filter(|guest| guest["optional"].as_bool() == Some(true))
            .filter_map(|guest| guest["email"].as_str().map(str::to_owned))
            .collect(),
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
        usual_notifications: resource["reminders"]["useDefault"].as_bool() != Some(false),
        notifications: resource["reminders"]["overrides"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|one| {
                Some(Notification {
                    minutes: one["minutes"].as_i64()?,
                    email: one["method"] == "email",
                })
            })
            .collect(),
        // Google leaves out what is as it is by default.
        may_modify: resource["guestsCanModify"].as_bool().unwrap_or(false),
        may_invite: resource["guestsCanInviteOthers"].as_bool().unwrap_or(true),
        may_see: resource["guestsCanSeeOtherGuests"]
            .as_bool()
            .unwrap_or(true),
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

/// How long the form's event is: in whole days if it is all day (0 for one day), else
/// by the clock. None while a date or time cannot be read, or if it ends before it
/// starts.
pub fn length(form: &Form) -> Option<Duration> {
    let first = parse_date(&form.start_date, "start").ok()?;
    let last = parse_date(&form.end_date, "end").ok()?;
    let length = if form.all_day {
        last - first
    } else {
        last.and_time(parse_time(&form.end_time, "end").ok()?)
            - first.and_time(parse_time(&form.start_time, "start").ok()?)
    };
    (length >= Duration::zero()).then_some(length)
}

/// The end (date, time) that makes the form's event `length` long from its start, for
/// moving the end along when the start is changed, as the web does. Reckoned on the
/// clock, so an event moved across a clock change keeps its times of day. None while
/// the start cannot be read.
pub fn end_for_length(form: &Form, length: Duration) -> Option<(String, String)> {
    let first = parse_date(&form.start_date, "start").ok()?;
    if form.all_day {
        let last = first.checked_add_signed(length)?;
        return Some((last.to_string(), form.end_time.clone()));
    }
    let end = first
        .and_time(parse_time(&form.start_time, "start").ok()?)
        .checked_add_signed(length)?;
    Some((end.date().to_string(), end.format("%-H:%M").to_string()))
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
    if form.notifications.len() > MOST_NOTIFICATIONS {
        return Err(format!(
            "An event can have {MOST_NOTIFICATIONS} notifications at most."
        ));
    }
    let reminders = if form.usual_notifications {
        json!({ "useDefault": true })
    } else {
        let overrides: Vec<Value> = form
            .notifications
            .iter()
            .map(|one| {
                json!({
                    "method": if one.email { "email" } else { "popup" },
                    "minutes": one.minutes,
                })
            })
            .collect();
        json!({ "useDefault": false, "overrides": overrides })
    };
    // A meeting the event has stays as it is; a new one is asked of Google, which
    // needs a text of our choosing to tell a repeated request from a second one.
    let conference = match (form.meet, kept.conference.is_null()) {
        (true, false) => kept.conference.clone(),
        (true, true) => json!({
            "createRequest": {
                "requestId": request_id(),
                "conferenceSolutionKey": { "type": "hangoutsMeet" },
            }
        }),
        (false, _) => Value::Null,
    };
    Ok(json!({
        "conferenceData": conference,
        "attendees": guest_list(form, kept)?,
        // Who may change the event may do the rest as well, as on the web.
        "guestsCanModify": form.may_modify,
        "guestsCanInviteOthers": form.may_modify || form.may_invite,
        "guestsCanSeeOtherGuests": form.may_modify || form.may_see,
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

/// A text that differs for every request made by this run of the app.
fn request_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let now = Utc::now().timestamp_nanos_opt().unwrap_or_default();
    format!(
        "gcal-client-{now}-{}",
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

/// The e-mail addresses in a text, separated by commas, semicolons, spaces or new
/// lines, each once. Fails with a sentence when a part is not an address.
pub fn addresses(text: &str) -> Result<Vec<&str>, String> {
    let mut addresses: Vec<&str> = Vec::new();
    for address in text
        .split([',', ';', ' ', '\n', '\t'])
        .filter(|part| !part.is_empty())
    {
        let (name, domain) = address.split_once('@').unwrap_or(("", ""));
        if name.is_empty() || !domain.contains('.') || domain.contains('@') {
            return Err(format!("“{address}” is not an e-mail address."));
        }
        if !addresses
            .iter()
            .any(|known| known.eq_ignore_ascii_case(address))
        {
            addresses.push(address);
        }
    }
    Ok(addresses)
}

/// The guests the form names, as Google's list of attendees. A guest the event had
/// keeps its entry; a new one is its address alone. Fails with a sentence when
/// something in the field is not an e-mail address.
fn guest_list(form: &Form, kept: &Kept) -> Result<Vec<Value>, String> {
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let addresses = addresses(&form.guests)?;
    let optional = |address: &str| form.optional.iter().any(|known| same(known, address));
    let named = |guest: &Value, address: &str| {
        guest["email"]
            .as_str()
            .is_some_and(|known| same(known, address))
    };
    // The guests the event had, in the order it had them: those still named, and those
    // the field cannot name at all (a guest without an address), which stay. Then the
    // new ones.
    let mut guests: Vec<Value> = kept
        .attendees
        .iter()
        .filter(|guest| {
            guest["email"].as_str().is_none()
                || addresses.iter().any(|address| named(guest, address))
        })
        .cloned()
        .collect();
    // Whether a guest's coming is optional is the form's to say; an entry that says
    // nothing about it and need not is left as it is.
    for guest in &mut guests {
        let Some(address) = guest["email"].as_str() else {
            continue;
        };
        let (wanted, has) = (optional(address), guest["optional"].as_bool());
        if has.unwrap_or(false) != wanted {
            guest["optional"] = wanted.into();
        }
    }
    for address in &addresses {
        if !kept.attendees.iter().any(|guest| named(guest, address)) {
            guests.push(if optional(address) {
                json!({ "email": address, "optional": true })
            } else {
                json!({ "email": address })
            });
        }
    }
    // The first guests of an event: the organiser joins them, as going.
    let own_among = addresses
        .iter()
        .any(|address| same(address, &kept.own_email));
    if kept.attendees.is_empty() && !guests.is_empty() && !kept.own_email.is_empty() && !own_among {
        guests.insert(
            0,
            json!({ "email": kept.own_email, "responseStatus": "accepted" }),
        );
    }
    Ok(guests)
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
                ("recurrence" | "attendees", None) => json!([]),
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
        "colorId" | "conferenceData" => value.is_null(),
        "recurrence" | "attendees" => value == &json!([]),
        "reminders" => value == &json!({ "useDefault": true }),
        "visibility" => value == "default",
        "transparency" => value == "opaque",
        "guestsCanModify" => value == false,
        "guestsCanInviteOthers" | "guestsCanSeeOtherGuests" => value == true,
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

    #[test]
    fn the_end_follows_a_changed_start() {
        let mut form = Form {
            start_date: "2026-11-17".into(),
            start_time: "9:00".into(),
            end_date: "2026-11-17".into(),
            end_time: "10:30".into(),
            ..Form::default()
        };
        let kept = length(&form).unwrap();
        assert_eq!(kept, Duration::minutes(90));
        // Later the same day, and past midnight.
        form.start_time = "14:15".into();
        assert_eq!(
            end_for_length(&form, kept),
            Some(("2026-11-17".into(), "15:45".into()))
        );
        form.start_time = "23".into();
        assert_eq!(
            end_for_length(&form, kept),
            Some(("2026-11-18".into(), "0:30".into()))
        );
        // Another day.
        form.start_date = "2026-12-31".into();
        assert_eq!(
            end_for_length(&form, kept),
            Some(("2027-01-01".into(), "0:30".into()))
        );
        // Half typed: nothing to go by.
        form.start_time = "1x".into();
        assert_eq!(end_for_length(&form, kept), None);
        assert_eq!(length(&form), None);
    }

    #[test]
    fn an_all_day_event_keeps_its_days() {
        let mut form = Form {
            all_day: true,
            start_date: "2026-11-17".into(),
            end_date: "2026-11-19".into(),
            end_time: "10:00".into(),
            ..Form::default()
        };
        let kept = length(&form).unwrap();
        assert_eq!(kept, Duration::days(2));
        form.start_date = "2026-11-30".into();
        assert_eq!(
            end_for_length(&form, kept),
            Some(("2026-12-02".into(), "10:00".into()))
        );
    }

    #[test]
    fn an_event_that_ends_before_it_starts_has_no_length() {
        let form = Form {
            start_date: "2026-11-17".into(),
            start_time: "9:00".into(),
            end_date: "2026-11-17".into(),
            end_time: "8:00".into(),
            ..Form::default()
        };
        assert_eq!(length(&form), None);
    }

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
        form.usual_notifications = false;
        form.notifications = vec![
            Notification {
                minutes: 10,
                email: false,
            },
            Notification {
                minutes: 1440,
                email: true,
            },
        ];
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
                "reminders": {"useDefault": false, "overrides": [
                    {"method": "popup", "minutes": 10}, {"method": "email", "minutes": 1440}
                ]},
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
                usual_notifications: false,
                notifications: vec![
                    Notification {
                        minutes: 30,
                        email: true
                    },
                    Notification {
                        minutes: 7,
                        email: false
                    },
                ],
                visibility: 2,
                busy: false,
                guests: String::new(),
                optional: Vec::new(),
                meet: false,
                may_modify: false,
                may_invite: true,
                may_see: true,
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
        assert!(event(json!({})).usual_notifications);
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
        // Its own notifications: none, or several, each on the screen or by e-mail.
        let own = |reminders: Value| {
            let form = event(json!({ "reminders": reminders }));
            assert!(!form.usual_notifications);
            form.notifications
        };
        assert_eq!(own(json!({"useDefault": false})), vec![]);
        assert_eq!(
            own(json!({"useDefault": false, "overrides": [
                {"method": "popup", "minutes": 30}, {"method": "email", "minutes": 45}
            ]})),
            vec![
                Notification {
                    minutes: 30,
                    email: false
                },
                Notification {
                    minutes: 45,
                    email: true
                }
            ]
        );
    }

    #[test]
    fn an_event_can_have_several_notifications_but_not_more_than_google_takes() {
        let (start, end) = (timed("2026-11-17T11:00:00Z"), timed("2026-11-17T12:00:00Z"));
        let mut form = new_form(start, end, zone());
        // A new event has its calendar's usual ones, and says nothing about them.
        assert!(form.usual_notifications);
        let new = creation(fields(&form, &Kept::default(), zone()).unwrap());
        assert!(new.get("reminders").is_none());

        // None at all is said.
        form.usual_notifications = false;
        assert_eq!(
            fields(&form, &Kept::default(), zone()).unwrap()["reminders"],
            json!({"useDefault": false, "overrides": []})
        );

        let one = |minutes| Notification {
            minutes,
            email: false,
        };
        form.notifications = vec![one(10), one(60), one(1440), one(2880), one(10080)];
        assert_eq!(
            fields(&form, &Kept::default(), zone()).unwrap()["reminders"]["overrides"]
                .as_array()
                .unwrap()
                .len(),
            5
        );
        form.notifications.push(one(5));
        assert_eq!(
            fields(&form, &Kept::default(), zone()),
            Err("An event can have 5 notifications at most.".into())
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
    fn guests_are_added_and_removed_by_their_addresses() {
        let (start, end) = (timed("2026-11-17T11:00:00Z"), timed("2026-11-17T12:00:00Z"));
        // A new event with its first guests: the organiser joins them.
        let mut form = new_form(start, end, zone());
        form.title = "Review".into();
        form.guests = "ann@x.org, Bob@x.org\nann@x.org".into();
        let kept = Kept {
            own_email: "me@x.org".into(),
            ..Kept::default()
        };
        let created = creation(fields(&form, &kept, zone()).unwrap());
        assert_eq!(
            created["attendees"],
            json!([
                {"email": "me@x.org", "responseStatus": "accepted"},
                {"email": "ann@x.org"},
                {"email": "Bob@x.org"}
            ])
        );
        // Without guests the field is not sent at all.
        form.guests = " ".into();
        assert!(
            creation(fields(&form, &kept, zone()).unwrap())
                .get("attendees")
                .is_none()
        );
        form.guests = "ann@x.org, not-an-address".into();
        assert_eq!(
            fields(&form, &kept, zone()).unwrap_err(),
            "“not-an-address” is not an e-mail address."
        );

        // An event that has guests: untouched, nothing is sent; one removed and one
        // added, the others keep their answers.
        let event = json!({
            "id": "e",
            "start": {"dateTime": "2026-11-17T12:00:00+01:00", "timeZone": "Europe/Prague"},
            "end": {"dateTime": "2026-11-17T13:00:00+01:00", "timeZone": "Europe/Prague"},
            "attendees": [
                {"email": "me@x.org", "self": true, "organizer": true, "responseStatus": "accepted"},
                {"email": "ann@x.org", "responseStatus": "declined", "comment": "away"},
                {"email": "bob@x.org", "responseStatus": "needsAction"}
            ]
        });
        let (opened, mut kept) = form_of(&event, start, end, zone());
        kept.own_email = "me@x.org".into();
        assert_eq!(opened.guests, "me@x.org, ann@x.org, bob@x.org");
        let before = fields(&opened, &kept, zone()).unwrap();
        assert_eq!(before["attendees"], event["attendees"]);
        let mut edited = opened.clone();
        edited.guests = "me@x.org, ANN@x.org, cy@x.org".into();
        let changes = diff(&before, &fields(&edited, &kept, zone()).unwrap());
        assert_eq!(
            changes,
            json!({"attendees": [
                {"email": "me@x.org", "self": true, "organizer": true, "responseStatus": "accepted"},
                {"email": "ann@x.org", "responseStatus": "declined", "comment": "away"},
                {"email": "cy@x.org"}
            ]})
        );
        assert_eq!(
            inverse(&changes, &event),
            json!({"attendees": event["attendees"]})
        );
        assert_eq!(
            inverse(&changes, &json!({"id": "e"})),
            json!({"attendees": []})
        );

        // The text's order does not matter; the event's does.
        let mut shuffled = opened.clone();
        shuffled.guests = "bob@x.org ann@x.org me@x.org".into();
        assert_eq!(
            fields(&shuffled, &kept, zone()).unwrap()["attendees"],
            event["attendees"]
        );

        // A guest without an address cannot be named in the field, and stays when
        // others are added or removed.
        let odd = json!({
            "id": "e",
            "start": {"dateTime": "2026-11-17T12:00:00+01:00"},
            "end": {"dateTime": "2026-11-17T13:00:00+01:00"},
            "attendees": [
                {"id": "1234", "displayName": "Someone", "responseStatus": "accepted"},
                {"email": "ann@x.org", "responseStatus": "declined"}
            ]
        });
        let (opened, kept) = form_of(&odd, start, end, zone());
        assert_eq!(opened.guests, "ann@x.org");
        let mut edited = opened.clone();
        edited.guests = "cy@x.org".into();
        assert_eq!(
            fields(&edited, &kept, zone()).unwrap()["attendees"],
            json!([
                {"id": "1234", "displayName": "Someone", "responseStatus": "accepted"},
                {"email": "cy@x.org"}
            ])
        );

        // In a calendar that is not the account's own (no own address is handed in),
        // the first guests are not joined by the account.
        let mut form = new_form(start, end, zone());
        form.guests = "ann@x.org".into();
        assert_eq!(
            fields(&form, &Kept::default(), zone()).unwrap()["attendees"],
            json!([{"email": "ann@x.org"}])
        );
    }

    #[test]
    fn a_guests_coming_can_be_optional() {
        let (start, end) = (timed("2026-11-17T11:00:00Z"), timed("2026-11-17T12:00:00Z"));
        // New guests: only the optional one says so.
        let mut form = new_form(start, end, zone());
        form.guests = "ann@x.org, bob@x.org".into();
        form.optional = vec!["BOB@x.org".into()];
        assert_eq!(
            fields(&form, &Kept::default(), zone()).unwrap()["attendees"],
            json!([{"email": "ann@x.org"}, {"email": "bob@x.org", "optional": true}])
        );

        // An existing event: the form shows who is optional; untouched, nothing is
        // sent; a change touches that guest's entry alone and keeps the rest of it.
        let resource = json!({
            "summary": "Review",
            "start": {"dateTime": "2026-11-17T11:00:00Z"},
            "end": {"dateTime": "2026-11-17T12:00:00Z"},
            "attendees": [
                {"email": "ann@x.org", "responseStatus": "accepted", "comment": "ok"},
                {"email": "bob@x.org", "optional": true, "responseStatus": "declined"},
                {"email": "cy@x.org", "optional": false}
            ]
        });
        let (opened, kept) = form_of(&resource, start, end, zone());
        assert_eq!(opened.optional, vec!["bob@x.org".to_owned()]);
        let before = fields(&opened, &kept, zone()).unwrap();
        assert_eq!(before["attendees"], resource["attendees"]);
        let mut edited = opened.clone();
        edited.optional = vec!["ann@x.org".into()];
        assert_eq!(
            diff(&before, &fields(&edited, &kept, zone()).unwrap()),
            json!({"attendees": [
                {"email": "ann@x.org", "responseStatus": "accepted", "comment": "ok", "optional": true},
                {"email": "bob@x.org", "optional": false, "responseStatus": "declined"},
                {"email": "cy@x.org", "optional": false}
            ]})
        );
    }

    #[test]
    fn what_guests_may_do_is_sent_only_when_it_differs() {
        let start = timed("2026-11-17T09:00:00Z");
        let end = timed("2026-11-17T10:00:00Z");
        let may = |form: &Form| {
            let fields = fields(form, &Kept::default(), zone()).unwrap();
            [
                "guestsCanModify",
                "guestsCanInviteOthers",
                "guestsCanSeeOtherGuests",
            ]
            .map(|name| fields[name].as_bool().unwrap())
        };

        // A new event has Google's defaults, and says nothing about them.
        let mut form = new_form(start, end, zone());
        assert_eq!(may(&form), [false, true, true]);
        let new = creation(fields(&form, &Kept::default(), zone()).unwrap());
        assert!(new.get("guestsCanModify").is_none());
        assert!(new.get("guestsCanInviteOthers").is_none());
        assert!(new.get("guestsCanSeeOtherGuests").is_none());

        // What differs is said.
        form.may_see = false;
        let new = creation(fields(&form, &Kept::default(), zone()).unwrap());
        assert_eq!(new["guestsCanSeeOtherGuests"], json!(false));
        assert!(new.get("guestsCanInviteOthers").is_none());

        // Who may change the event may do the rest, whatever the other boxes say.
        form.may_modify = true;
        form.may_invite = false;
        assert_eq!(may(&form), [true, true, true]);

        // An existing event: what it does not say is the default; one box changed
        // sends that field alone, and taking it back removes it again.
        let resource = json!({
            "summary": "Review",
            "start": {"dateTime": "2026-11-17T09:00:00Z"},
            "end": {"dateTime": "2026-11-17T10:00:00Z"},
            "guestsCanInviteOthers": false
        });
        let (opened, kept) = form_of(&resource, start, end, zone());
        assert_eq!(
            (opened.may_modify, opened.may_invite, opened.may_see),
            (false, false, true)
        );
        let mut edited = opened.clone();
        edited.may_modify = true;
        let changes = diff(
            &fields(&opened, &kept, zone()).unwrap(),
            &fields(&edited, &kept, zone()).unwrap(),
        );
        assert_eq!(
            changes,
            json!({"guestsCanModify": true, "guestsCanInviteOthers": true})
        );
        assert_eq!(
            inverse(&changes, &resource),
            json!({"guestsCanModify": null, "guestsCanInviteOthers": false})
        );
    }

    #[test]
    fn a_video_meeting_is_asked_for_kept_or_removed() {
        let (start, end) = (timed("2026-11-17T11:00:00Z"), timed("2026-11-17T12:00:00Z"));
        // New event with a meeting: a request for one; without: nothing.
        let mut form = new_form(start, end, zone());
        assert!(
            creation(fields(&form, &Kept::default(), zone()).unwrap())
                .get("conferenceData")
                .is_none()
        );
        form.meet = true;
        let created = creation(fields(&form, &Kept::default(), zone()).unwrap());
        let request = &created["conferenceData"]["createRequest"];
        assert_eq!(request["conferenceSolutionKey"]["type"], "hangoutsMeet");
        assert!(
            request["requestId"]
                .as_str()
                .unwrap()
                .starts_with("gcal-client-")
        );
        // Two requests never share their text.
        let again = creation(fields(&form, &Kept::default(), zone()).unwrap());
        assert_ne!(
            again["conferenceData"]["createRequest"]["requestId"],
            request["requestId"]
        );

        // An event that has one: untouched nothing is sent; unticked it is removed.
        let event = json!({
            "id": "e",
            "start": {"dateTime": "2026-11-17T12:00:00+01:00"},
            "end": {"dateTime": "2026-11-17T13:00:00+01:00"},
            "hangoutLink": "https://meet.google.com/abc-defg-hij",
            "conferenceData": {"conferenceId": "abc-defg-hij"}
        });
        let (opened, kept) = form_of(&event, start, end, zone());
        assert!(opened.meet);
        let before = fields(&opened, &kept, zone()).unwrap();
        assert_eq!(diff(&before, &before), json!({}));
        let mut without = opened.clone();
        without.meet = false;
        let changes = diff(&before, &fields(&without, &kept, zone()).unwrap());
        assert_eq!(changes, json!({"conferenceData": null}));
        assert_eq!(
            inverse(&changes, &event),
            json!({"conferenceData": {"conferenceId": "abc-defg-hij"}})
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
