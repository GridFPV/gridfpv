//! The single shared error shape (protocol.html §9.8, "Error model").
//!
//! The contract uses **one** error type across every path — HTTP snapshot reads, the
//! WS change stream, and control acknowledgements — defined once in Rust like the
//! rest of the wire contract. A client therefore handles auth failure, an unknown
//! scope, a stale cursor, or a version mismatch through a single uniform shape rather
//! than a different envelope per surface.
//!
//! The exact field set is "pinned at implementation" (§9.8); this is the minimal
//! shape every surface needs — a machine-readable [`ErrorCode`] plus a human-readable
//! `message` — with room to grow additively (a new code, an optional detail field)
//! without breaking older clients.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A machine-readable error category — the cases protocol.html §9.8 enumerates
/// ("auth failure, unknown scope, stale-cursor, version-mismatch"), plus a generic
/// fallback. Externally tagged like the event model so it maps to a TS string union.
///
/// Adding a new variant is an additive change (§7): an older client that does not
/// understand a new code treats it as it would [`ErrorCode::Internal`] — an error it
/// surfaces but cannot specifically branch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ErrorCode {
    /// The caller is not authenticated, or not authorized for this scope / action
    /// (protocol.html §5). Covers a missing/invalid read token and an unprivileged
    /// client attempting the control path.
    Unauthorized,
    /// The requested [`Scope`](crate::scope::Scope) does not exist or addresses
    /// nothing the server can serve (an unknown heat id, a pilot not in the event).
    UnknownScope,
    /// The client presented a resume [`Cursor`](crate::stream::Cursor) older than the
    /// server's retained window (protocol.html §3): the gap cannot be replayed and the
    /// client must **re-snapshot**. The distinguished "go re-snapshot" signal.
    StaleCursor,
    /// The client's [`ContractVersion`](crate::ContractVersion) falls outside the band
    /// the server serves (protocol.html §7) — the "too old / too new, please refresh"
    /// signal as an error on a path that cannot carry a [`ServerHello`](crate::ServerHello).
    VersionMismatch,
    /// The request was malformed — an unparseable body, an illegal command for the
    /// current state, a bad scope expression.
    BadRequest,
    /// An unexpected server-side failure. The catch-all an older client also maps an
    /// unknown future code onto.
    Internal,
}

/// The one error shape every protocol surface returns (protocol.html §9.8).
///
/// Carried in an HTTP error body, a WS error frame, and a failed
/// [`CommandAck`](crate::control::CommandAck) alike. `code` is the branchable
/// category; `message` is the human-readable detail for logs and the RD console.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ProtocolError {
    /// The machine-readable error category.
    pub code: ErrorCode,
    /// A human-readable description of what went wrong.
    pub message: String,
}

impl ProtocolError {
    /// Build an error from a code and a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_error_round_trips() {
        let err = ProtocolError::new(ErrorCode::StaleCursor, "cursor 12 is below the window");
        let json = serde_json::to_string(&err).unwrap();
        let back: ProtocolError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn every_error_code_round_trips() {
        use ErrorCode::*;
        for code in [
            Unauthorized,
            UnknownScope,
            StaleCursor,
            VersionMismatch,
            BadRequest,
            Internal,
        ] {
            let json = serde_json::to_string(&code).unwrap();
            let back: ErrorCode = serde_json::from_str(&json).unwrap();
            assert_eq!(code, back);
        }
    }
}
