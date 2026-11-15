//! Pure calendar logic with no I/O: model, recurrence, event layout, shortcuts.

pub mod model;
pub mod month;
pub mod recurrence;
pub mod schedule;
pub mod text;
pub mod week;

pub use model::{CalendarListEntry, Event, EventDateTime, EventStatus, EventTime};
