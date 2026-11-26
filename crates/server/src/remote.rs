//! What a Director and a remote server say to each other (docs/primitives.md §13).
//!
//! The Director sends; the server answers. These are the shapes on that wire, kept here so
//! both ends are built from one definition.
//!
//! An event is sent in two parts:
//!
//! - **Its header** ([`PushHeader`]): the event's name and date, and the pilots, classes and
//!   timers it refers to. These are not in the log. The log names a pilot by id, and the
//!   header says who that id is. It is sent whole each time it changes.
//! - **Its log** ([`PushLog`]): the entries, in order, **as the Director stored them**. The
//!   text of an entry is never rewritten on the way (see `gridfpv_storage::RawEntry`).
//!
//! The server only ever adds to the end of the log it holds. Each batch says where it starts
//! and carries the chain value of everything before it (`gridfpv_storage::Chain`), so the
//! server can tell a batch that continues its copy from one that belongs to some other log.

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use ts_rs::TS;

use crate::classes::Class;
use crate::events::EventMeta;
use crate::pilots::Pilot;
use crate::timers::{SeatChannel, TimerView};

/// The server's answer to "who are you, and who am I to you?" (`GET /push/hello`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushHello {
    /// The server's build.
    pub version: String,
    /// The club this Director's link token pushes into.
    pub club: PushClub,
}

/// A club on a server (§13.2), as a Director needs to know it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushClub {
    pub id: String,
    pub name: String,
}

/// How much of an event's log a server holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushStatus {
    /// How many entries. The next entry the server wants is this one.
    #[ts(type = "number")]
    pub len: u64,
    /// The chain value of those entries, as hex.
    pub chain: String,
}

/// An event's header: what the log refers to but does not hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushHeader {
    pub meta: EventMeta,
    #[serde(default)]
    pub pilots: Vec<Pilot>,
    #[serde(default)]
    pub classes: Vec<Class>,
    #[serde(default)]
    pub timers: Vec<TimerView>,
    #[serde(default)]
    pub channels: Vec<SeatChannel>,
}

/// One log entry on its way to a server.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushEntry {
    /// When the Director's log received it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub recorded_at: Option<i64>,
    /// The event, as the text the Director stored.
    #[ts(type = "unknown")]
    pub event: Box<RawValue>,
}

/// A run of log entries (`POST /push/events/{id}/log`).
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushLog {
    /// The offset of the first entry here.
    #[ts(type = "number")]
    pub from: u64,
    /// The chain value of every entry before `from`, as hex.
    pub before: String,
    pub entries: Vec<PushEntry>,
}

/// Why a server did not take a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum PushRefusal {
    /// The batch starts after the end of what the server holds. Send from `held.len`.
    Gap,
    /// The server holds a different log under this event's id. Nothing was changed. The
    /// director decides whether the server's copy is replaced.
    Different,
}

/// A batch the server did not take (`409`), and what it holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushRefused {
    pub reason: PushRefusal,
    pub held: PushStatus,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_keeps_its_text_through_the_wire() {
        // Odd spacing and a key nobody knows: both must come out as they went in.
        let text = r#"{"from":3,"before":"ab","entries":[{"recorded_at":7,"event":{ "X" : {"new":[1, 2]} }},{"event":"Y"}]}"#;
        let log: PushLog = serde_json::from_str(text).unwrap();
        assert_eq!(log.from, 3);
        assert_eq!(log.entries[0].recorded_at, Some(7));
        assert_eq!(log.entries[0].event.get(), r#"{ "X" : {"new":[1, 2]} }"#);
        assert_eq!(log.entries[1].recorded_at, None);
        assert_eq!(log.entries[1].event.get(), r#""Y""#);

        let again: PushLog = serde_json::from_str(&serde_json::to_string(&log).unwrap()).unwrap();
        assert_eq!(again.entries[0].event.get(), log.entries[0].event.get());
        assert_eq!(again.entries[1].recorded_at, None);
    }

    #[test]
    fn a_header_with_only_its_meta_reads() {
        let text = r#"{"meta":{"id":"e","name":"E","created_at":1,"persistent":true}}"#;
        let header: PushHeader = serde_json::from_str(text).unwrap();
        assert_eq!(header.meta.name, "E");
        assert!(header.pilots.is_empty() && header.timers.is_empty());
    }
}
