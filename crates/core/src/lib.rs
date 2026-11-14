//! Pure calendar logic with no I/O: model, recurrence, event layout, shortcuts.

pub mod model;

pub use model::{CalendarListEntry, Event, EventDateTime, EventStatus, EventTime};
