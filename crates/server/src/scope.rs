//! Subscription **scoping** (protocol.html §4).
//!
//! A subscription is a [`Scope`] plus a starting [`Cursor`](crate::stream::Cursor):
//! the scope is how a client asks for exactly the projections it needs — "a phone
//! fetches one heat, not the whole event" — and bounds both payload size and what a
//! client is allowed to see. The same [`Scope`] addresses a snapshot read (§2) and a
//! stream subscription (§3).
//!
//! protocol.html §9.6 resolves the *grammar* (a scope grammar with multi-scope
//! subscribe and cursor pagination) but defers the exact addressing scheme, URL
//! shapes, and filter expressiveness to implementation. This enum is the **four
//! addressable resources** §4 names (event / class / heat / pilot); the richer filter
//! grammar and composition ("a pilot within a class") layer on when the snapshot
//! endpoint lands (#42).

use gridfpv_events::HeatId;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Identifies an **event** — the whole competition (protocol.html §4 "Event scope").
///
/// A transparent string newtype like the event-model ids. The cross-event registry
/// and account model are a Cloud concern (protocol.html callout); this is just the
/// stable handle a scope addresses, sufficient for the wire contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct EventId(pub String);

/// Identifies a **class** within an event (protocol.html §4 "Class scope") — one
/// class's phases, schedule, and standings, which may run in parallel with others.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct ClassId(pub String);

/// Identifies a **pilot** across an event (protocol.html §4 "Pilot scope") — a racer
/// following their own laps, results, and next heat.
///
/// This is the event-scoped pilot handle a scope addresses, distinct from the
/// per-source [`CompetitorRef`](gridfpv_events::CompetitorRef) the timer reports: a
/// pilot is bound to source competitors by a registration action (Architecture §9),
/// which is out of scope here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct PilotId(pub String);

/// What a subscription addresses (protocol.html §4): one of the four resources a
/// client can scope to. Externally tagged like the event model, so it maps to a TS
/// discriminated union.
///
/// Scopes compose and a client may hold several at once over one connection (§4); a
/// multi-scope subscribe is a list of these. The richer filter grammar (§9.6) is
/// deferred — this fixes the addressable *resources*, which is what later issues build
/// the grammar over.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum Scope {
    /// Everything for one event — the broad default for the "whole venue screen" and
    /// the Cloud live surface.
    Event {
        /// The event addressed.
        event: EventId,
    },
    /// One class's phases, schedule, and standings.
    Class {
        /// The event the class belongs to.
        event: EventId,
        /// The class addressed.
        class: ClassId,
    },
    /// One heat's live race-state and lap lists — the tightest, lowest-latency scope.
    Heat {
        /// The heat addressed (the id it carries in the log).
        heat: HeatId,
    },
    /// One pilot across the event — their laps, results, and next heat.
    Pilot {
        /// The event the pilot is racing in.
        event: EventId,
        /// The pilot addressed.
        pilot: PilotId,
    },
}

/// A subscription request (protocol.html §3, §4): a [`Scope`] plus where to resume
/// the stream from.
///
/// `from` is the last-applied [`Cursor`](crate::stream::Cursor) the client presents on
/// (re)connect — `Some(cursor)` to resume after a drop, `None` for a fresh
/// subscription that begins from the snapshot's cursor (protocol.html §3 "resume by
/// cursor, fall back to re-snapshot"). If the gap is too old to replay the server
/// answers with a [`StaleCursor`](crate::error::ErrorCode::StaleCursor) error and the
/// client re-snapshots.
/// The subscribe frame doubles as the **connect message** for the WS read path
/// (protocol.html §7): it carries the client's [`ContractVersion`](crate::ContractVersion)
/// (`contract_version`) so the server can negotiate the contract band before streaming,
/// and an optional read **token** (`token`) — a bearer or read-only join-token — so a
/// gated deployment can authenticate the read on the same frame. Both are optional and
/// default-absent so an existing fresh subscribe (just a `scope`) is unchanged on the
/// wire; a missing `contract_version` is treated as this build's [`CONTRACT_VERSION`]
/// (the legacy/unspecified client), and a missing `token` is an anonymous LAN read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SubscribeRequest {
    /// The resource this subscription is scoped to.
    pub scope: Scope,
    /// The cursor to resume after, or `None` to start fresh from the snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from: Option<crate::stream::Cursor>,
    /// The contract version the client was built against (protocol.html §7). Checked
    /// against the server's supported band before the stream starts; out of band gets the
    /// "please refresh" [`VersionMismatch`](crate::error::ErrorCode::VersionMismatch)
    /// signal. Absent means "unspecified" — treated as the server's own
    /// [`CONTRACT_VERSION`](crate::CONTRACT_VERSION).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub contract_version: Option<crate::ContractVersion>,
    /// An optional read credential (protocol.html §5): a bearer or read-only join-token.
    /// Reads are open on the LAN, so this is omitted by an anonymous viewer; when present
    /// it must be valid (an unknown/revoked token is rejected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::Cursor;

    #[test]
    fn every_scope_variant_round_trips() {
        let scopes = vec![
            Scope::Event {
                event: EventId("spring-cup".into()),
            },
            Scope::Class {
                event: EventId("spring-cup".into()),
                class: ClassId("open".into()),
            },
            Scope::Heat {
                heat: HeatId("q-1".into()),
            },
            Scope::Pilot {
                event: EventId("spring-cup".into()),
                pilot: PilotId("acroace".into()),
            },
        ];
        for scope in scopes {
            let json = serde_json::to_string(&scope).unwrap();
            let back: Scope = serde_json::from_str(&json).unwrap();
            assert_eq!(scope, back);
        }
    }

    #[test]
    fn subscribe_request_round_trips_with_and_without_cursor() {
        let fresh = SubscribeRequest {
            scope: Scope::Heat {
                heat: HeatId("q-1".into()),
            },
            from: None,
            contract_version: None,
            token: None,
        };
        let json = serde_json::to_string(&fresh).unwrap();
        // A fresh subscribe omits `from`, `contract_version`, and `token` entirely
        // (skip_serializing_if), so it is byte-identical to the pre-#46 shape.
        assert!(
            !json.contains("from") && !json.contains("contract_version") && !json.contains("token"),
            "fresh subscribe omits optional fields: {json}"
        );
        let back: SubscribeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(fresh, back);

        let resume = SubscribeRequest {
            scope: Scope::Event {
                event: EventId("spring-cup".into()),
            },
            from: Some(Cursor::new(42)),
            contract_version: Some(crate::CONTRACT_VERSION),
            token: Some("read-token".into()),
        };
        let json = serde_json::to_string(&resume).unwrap();
        let back: SubscribeRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(resume, back);
    }
}
