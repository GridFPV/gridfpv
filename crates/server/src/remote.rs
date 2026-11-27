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

/// One event a server knows of for a club (`GET /push/events`): what it holds, or that it
/// was removed there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushHeld {
    pub id: crate::events::EventId,
    #[ts(type = "number")]
    pub len: u64,
    pub chain: String,
    /// Someone who runs the club removed it from the server. Then `len` is 0.
    #[serde(default)]
    pub removed: bool,
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
    /// What the Director's clock said when it sent this, in microseconds since 1970. Every
    /// time in the log is on that clock. The server uses this to tell a page that is
    /// counting a race down what the Director's clock says now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub sent_at: Option<i64>,
}

/// An event a server holds, for a list of them (`GET /api/clubs/{club}/events`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ServerEvent {
    pub id: crate::events::EventId,
    pub name: String,
    /// The date the director gave it, as they typed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<String>,
    /// What the director wrote about the event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Who runs it, as the director wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organizer: Option<String>,
    /// When the Director made the event, in milliseconds since 1970.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Left out of the list for everyone but those who run the club (§13.9). Anyone with
    /// its address can still open it.
    #[serde(default)]
    pub hidden: bool,
}

/// An account on a server, as a Director needs to know it: enough to show who it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushAccount {
    pub id: String,
    /// What the person is called on the server.
    pub name: String,
}

/// What a pilot says of themselves on a server (§13.7): how they are shown, and the ids
/// they have elsewhere. It belongs to their account, so it is the same in every club.
/// Every part may be left out.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PilotProfile {
    /// The name they race under. Left out, the account's own name stands in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub callsign: Option<String>,
    /// Their colour, as `#RRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// Their MultiGP pilot id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub multigp_id: Option<String>,
    /// Their Velocidrone id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub velocidrone_id: Option<String>,
}

impl PilotProfile {
    /// Whether nothing is filled in.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// A pilot of a club, between a Director and a server (`/push/pilots`).
///
/// A Director sends the pilots it has, so the server knows them before any event names
/// them. A server sends the pilots the club has, each with the account that holds it, so
/// a Director can bring them in already tied to their accounts (§13.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushPilot {
    pub id: gridfpv_events::PilotId,
    pub callsign: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// The account that holds this pilot on the server. A server says; a Director does
    /// not, and what it sends here is not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub account: Option<PushAccount>,
}

/// Which account holds a pilot (`PUT /push/pilots/{pilot}/account`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushPilotAccount {
    pub account: String,
}

/// What a pilot asks for on a server, for the Director to decide (§13.4): the same things
/// a pilot asks for at the race, signing up for a heat (§11.5) and checking in to an open
/// seat (§11.9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum PilotAsk {
    /// Take a seat in a heat.
    Take {
        heat: gridfpv_events::PhaseId,
        seat: gridfpv_events::SignupSeat,
    },
    /// Take the first free spot on a seat.
    Next {
        phase: gridfpv_events::PhaseId,
        seat: gridfpv_events::SignupSeat,
    },
    /// Give a spot up.
    Leave { heat: gridfpv_events::PhaseId },
    /// Check in to an open seat of a race that is under way (§11.9). The Director takes
    /// it only with the event's race code, which shows the pilot is at the race (§13.13).
    TakeSeat {
        race: gridfpv_events::RaceId,
        adapter: gridfpv_events::AdapterId,
        seat: gridfpv_events::SeatRef,
    },
    /// Give up the seat the pilot checked in to.
    DropSeat { race: gridfpv_events::RaceId },
}

/// One ask waiting on a server for the Director (`GET /push/requests`).
///
/// **The server says who is asking.** It is the pilot the signed-in account holds in the
/// club (§13.7). The Director decides the ask as that pilot's own, under every rule of the
/// phase, and never as the director's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushRequest {
    /// Names this one ask, to answer it by.
    pub id: String,
    pub event: crate::events::EventId,
    pub pilot: gridfpv_events::PilotId,
    pub ask: PilotAsk,
    /// The race code the pilot typed, for an ask that needs one. The server passes it on
    /// as typed and does not know whether it is right: the Director checks it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub code: Option<String>,
}

/// The Director's answer to an ask (`POST /push/requests/{id}`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct PushAnswer {
    /// Why it was not done, in words for the pilot. Left out when it was done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub refused: Option<String>,
    /// The refusal is about the race code: none was sent, or it was not right (§13.13).
    /// The page then asks the pilot for the code, where any other refusal is only shown.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_code: bool,
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
    /// Someone who runs the club removed this event from the server. Nothing is taken
    /// until the director says to send it again.
    Removed,
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
