//! GridFPV protocol server — the one read/realtime contract (snapshot + WS change
//! stream) plus the RD control path, served over axum on the Director (and reused
//! by the Cloud). The wire types are defined here once and generated to TypeScript
//! (ts-rs), so clients never hand-write a wire type. See docs/protocol.html.
//!
//! # What this crate is, as of #41 / #42 / #43
//!
//! The wire-type surface — the Rust types that *are* the protocol (protocol.html §6:
//! "the contract defined once, in Rust") — plus the **read transport** over them: the
//! live race-state projection ([`live_state`], #41), the axum snapshot endpoints
//! ([`app`], #42), and the WebSocket change stream ([`ws`], #43) that keeps a client's
//! scoped projection current after the snapshot. Auth (#44) and control endpoints (#45)
//! build on the same [`app::AppState`] and [`app::router`] — the control path reusing
//! [`app::AppState::append`], the one log-write-plus-stream-wakeup the change stream
//! observes. Defining the types first means every one of those issues — and the frontend
//! protocol client (#49) — builds against a single generated contract that already exists.
//!
//! Every public wire type derives [`serde`] (its JSON encoding *is* the wire format)
//! and [`ts_rs::TS`] with `#[ts(export, export_to = "bindings/")]`, exactly mirroring
//! the event model in `gridfpv-events`. `cargo xtask gen` runs the ts-rs export tests
//! across every crate that exports (events, projection, engine, server) with
//! `TS_RS_EXPORT_DIR` pinned to the repo root, so all `.ts` files land in
//! `<repo>/bindings/`; `cargo xtask ci` drift-checks them.
//!
//! # The shape of the contract (protocol.html §2–§7)
//!
//! 1. A client connects and announces the [`ContractVersion`] it was built against in
//!    a [`Hello`] (§7); the server replies with the band it serves ([`ServerHello`]).
//! 2. It fetches a [`Snapshot`](snapshot::Snapshot) of a [`Scope`](scope::Scope) — the
//!    current materialized projection plus the [`Cursor`](stream::Cursor) the stream
//!    resumes from (§2).
//! 3. It subscribes ([`SubscribeRequest`](scope::SubscribeRequest)) and receives an
//!    ordered run of [`ChangeEnvelope`](stream::ChangeEnvelope)s, each a per-projection
//!    delta-or-fresh-value carrying the monotonic [`Cursor`](stream::Cursor) (§3).
//! 4. The RD additionally drives the control path: [`Command`](control::Command)s up,
//!    [`CommandAck`](control::CommandAck)s down (§5).
//!
//! Any failure on any of those paths is the single shared
//! [`ProtocolError`](error::ProtocolError) (§9.8).
//!
//! # Deferred (later issues + the doc-reconciliation pass)
//!
//! - **Exact delta encodings.** [`Change::Delta`](stream::Change::Delta) is a
//!   `serde_json::Value` placeholder; the change stream ([`ws`], #43) emits only
//!   [`Change::FreshValue`](stream::Change::FreshValue) envelopes for now, while wiring
//!   the per-projection delta-vs-fresh *distinction* (§9.2) so the precise delta shapes
//!   (a single appended lap, a heat-state transition, …) can be pinned in #59 without
//!   reshaping the stream or its sequencing.
//! - **Scope addressing grammar.** [`app::router`] pins a concrete REST addressing over
//!   the four resources (event/class/heat/pilot, §4); the richer filter grammar (§9.6),
//!   and the log-level *class* filter (the log carries no class tag yet), are refined in
//!   the doc-reconciliation pass and when the schedule model grows class tags.
//! - **Split-level live progress.** [`live_state::LiveRaceState`] carries lap-count and
//!   last-lap progress; per-gate split progress is a later refinement.
//! - **Auth tokens / sessions.** The credential format (§9.4) is a #44 concern; no
//!   token types live here yet.
#![forbid(unsafe_code)]

pub mod app;
pub mod control;
pub mod control_handler;
pub mod error;
pub mod live_state;
pub mod scope;
pub mod snapshot;
pub mod stream;
pub mod ws;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The protocol **contract version** — axis 1 of the three version axes
/// (protocol.html §7): the wire shapes owned by this crate, kept distinct from the
/// log/schema version and the projection version.
///
/// A single monotonic integer. A breaking change to any wire type bumps it; additive
/// changes (new projections, new fields an older client ignores) do not. The version
/// is negotiated at connect time ([`Hello`] / [`ServerHello`]) and is independent of
/// the transport (LAN or Cloud) — see [`CONTRACT_VERSION`] for the version this build
/// speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/", as = "u32")]
pub struct ContractVersion {
    /// The monotonic contract version number.
    pub version: u32,
}

impl ContractVersion {
    /// Construct from a raw version number.
    pub const fn new(version: u32) -> Self {
        Self { version }
    }
}

/// The contract version this server build speaks. The first wire contract is `1`.
pub const CONTRACT_VERSION: ContractVersion = ContractVersion::new(1);

/// The client's **connect message** (protocol.html §7): announced before anything
/// else, it carries the [`ContractVersion`] the client was built against so the
/// server can decide whether it can serve it.
///
/// "The contract version is negotiated, not assumed" (§7): the client states its
/// version, the server replies with the band it serves ([`ServerHello`]). The version
/// is the only thing negotiated here; auth (a bearer token, §9.4) is a separate #44
/// concern layered in front, not part of this message yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Hello {
    /// The contract version the client was built against.
    pub contract_version: ContractVersion,
}

/// The server's reply to a [`Hello`] (protocol.html §7).
///
/// The server "states the version(s) it serves" as an inclusive band
/// `[min_contract_version, max_contract_version]`. A client whose
/// [`Hello::contract_version`] falls inside the band is served; one that falls
/// outside gets `compatible = false` — the explicit "too old / too new, please
/// refresh" signal — rather than malformed data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ServerHello {
    /// The oldest contract version this server still serves (inclusive).
    pub min_contract_version: ContractVersion,
    /// The newest contract version this server serves (inclusive).
    pub max_contract_version: ContractVersion,
    /// Whether the client's announced version falls within the served band. `false`
    /// is the "please refresh" signal (the client is too old or too new to be served).
    pub compatible: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_version_is_a_bare_integer_on_the_wire() {
        // `#[serde(transparent)]`: the version serialises as the raw integer, and
        // `#[ts(as = "u32")]` mirrors that as a `number` in the generated TS.
        assert_eq!(
            serde_json::to_string(&ContractVersion::new(7)).unwrap(),
            "7"
        );
    }

    #[test]
    fn hello_round_trips() {
        let hello = Hello {
            contract_version: CONTRACT_VERSION,
        };
        let json = serde_json::to_string(&hello).unwrap();
        let back: Hello = serde_json::from_str(&json).unwrap();
        assert_eq!(hello, back);
    }

    #[test]
    fn server_hello_round_trips() {
        let reply = ServerHello {
            min_contract_version: ContractVersion::new(1),
            max_contract_version: ContractVersion::new(3),
            compatible: true,
        };
        let json = serde_json::to_string(&reply).unwrap();
        let back: ServerHello = serde_json::from_str(&json).unwrap();
        assert_eq!(reply, back);
    }
}
