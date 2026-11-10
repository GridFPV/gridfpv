//! GridFPV server — the Director's HTTP API, served over axum. The wire types are defined
//! here once and generated to TypeScript (ts-rs), so clients never hand-write a wire type.
//!
//! # What this crate is right now: the skeleton
//!
//! The competition model is being rebuilt from the primitives in `docs/primitives.md`.
//! What is here is the part that does not change:
//!
//! - [`app`] — the one [`Router`](axum::Router): liveness, build identity, server time, and
//!   the routes over the modules below; plus the typed-404 / SPA fallback.
//! - [`events`] — the event registry: create / list / delete events and the active event.
//!   Each event owns its own append-only log.
//! - [`pilots`] and [`classes`] — the app-level pilot and class directories.
//! - [`control`] — the competition: commands in, facts logged, a snapshot ([`view`]) and a
//!   change feed out. The rules live in `gridfpv-engine`; this is the plumbing around them.
//! - [`auth`] — the token tiers and the one gate in front of every write.
//! - [`error`] — the single [`ProtocolError`](error::ProtocolError) every failure returns.
//! - The contract-version helpers below ([`ContractVersion`], [`Hello`], [`ServerHello`]).
//!
//! Every public wire type derives [`serde`] (its JSON encoding *is* the wire format)
//! and [`ts_rs::TS`] with `#[ts(export, export_to = "bindings/")]`, exactly mirroring
//! the event model in `gridfpv-events`. `cargo xtask gen` runs the ts-rs export tests
//! across every crate that exports with `TS_RS_EXPORT_DIR` pinned to the repo root, so
//! all `.ts` files land in `<repo>/bindings/`; `cargo xtask ci` drift-checks them.
#![forbid(unsafe_code)]

pub mod app;
pub mod auth;
pub mod classes;
pub mod control;
pub mod error;
pub mod events;
pub mod pilots;
pub mod view;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The protocol **contract version**: the version of the wire shapes owned by this
/// crate, kept distinct from the log/schema version.
///
/// A single monotonic integer. A breaking change to any wire type bumps it; additive
/// changes (new routes, new fields an older client ignores) do not. The version
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

/// The **product version this build reports** (#513), stamped by `build.rs`: a release build
/// (HEAD on a clean `v*` tag, or an explicit `GRIDFPV_RELEASE_VERSION`) names itself by its
/// standard alpha/beta/full version, and every other build names its commit —
/// `0.4.0-dev-<short hash>` (`-dirty` when the tree had uncommitted changes). This is what
/// `/about` serves and the console's brand stamp shows; use it in place of
/// `CARGO_PKG_VERSION` anywhere a human reads a version off a running build.
pub const BUILD_VERSION: &str = env!("GRIDFPV_BUILD_VERSION");

/// The **oldest** contract version this server still serves (protocol.html §7, §9.7) —
/// the bottom of the supported band `MIN_SUPPORTED_CONTRACT_VERSION..=CONTRACT_VERSION`.
///
/// A client whose [`Hello::contract_version`] is below this is "too old, please refresh";
/// one above [`CONTRACT_VERSION`] is "too new". Both fall outside the band and get the
/// refresh signal (a [`ServerHello`] with `compatible = false`, or a
/// [`VersionMismatch`](error::ErrorCode::VersionMismatch) error on a path that cannot
/// carry a `ServerHello`). At the first contract the band is the single version `1..=1`;
/// it widens as the server keeps serving older clients across additive changes (§7).
pub const MIN_SUPPORTED_CONTRACT_VERSION: ContractVersion = ContractVersion::new(1);

/// Whether `client` falls within this server's supported contract band
/// (protocol.html §7): `MIN_SUPPORTED_CONTRACT_VERSION..=CONTRACT_VERSION`, inclusive.
/// `false` is the "too old / too new, please refresh" condition.
pub fn is_supported_contract_version(client: ContractVersion) -> bool {
    MIN_SUPPORTED_CONTRACT_VERSION <= client && client <= CONTRACT_VERSION
}

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

impl ServerHello {
    /// The server's reply for a client announcing `client` (protocol.html §7): this
    /// build's supported band `[MIN_SUPPORTED_CONTRACT_VERSION, CONTRACT_VERSION]` with
    /// `compatible` set by [`is_supported_contract_version`]. The single place the band is
    /// turned into a handshake reply, so every transport reports the same range.
    pub fn for_client(client: ContractVersion) -> Self {
        Self {
            min_contract_version: MIN_SUPPORTED_CONTRACT_VERSION,
            max_contract_version: CONTRACT_VERSION,
            compatible: is_supported_contract_version(client),
        }
    }

    /// The "too old / too new, please refresh" error for an out-of-band `client`
    /// (protocol.html §7, §9.8) — the [`VersionMismatch`](error::ErrorCode::VersionMismatch)
    /// form of the refresh signal for a path that cannot carry a `ServerHello`. The message
    /// names the served band so the client knows whether to upgrade or downgrade.
    pub fn refresh_error(client: ContractVersion) -> error::ProtocolError {
        error::ProtocolError::new(
            error::ErrorCode::VersionMismatch,
            format!(
                "contract version {} is outside this server's supported band {}..={} — \
                 please refresh the client",
                client.version, MIN_SUPPORTED_CONTRACT_VERSION.version, CONTRACT_VERSION.version,
            ),
        )
    }
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
    fn in_band_version_is_supported() {
        assert!(is_supported_contract_version(CONTRACT_VERSION));
        assert!(is_supported_contract_version(
            MIN_SUPPORTED_CONTRACT_VERSION
        ));
        let reply = ServerHello::for_client(CONTRACT_VERSION);
        assert!(reply.compatible);
        assert_eq!(reply.min_contract_version, MIN_SUPPORTED_CONTRACT_VERSION);
        assert_eq!(reply.max_contract_version, CONTRACT_VERSION);
    }

    #[test]
    fn out_of_band_version_gets_the_refresh_signal() {
        // Too new (above the band).
        let too_new = ContractVersion::new(CONTRACT_VERSION.version + 1);
        assert!(!is_supported_contract_version(too_new));
        assert!(!ServerHello::for_client(too_new).compatible);
        assert_eq!(
            ServerHello::refresh_error(too_new).code,
            error::ErrorCode::VersionMismatch
        );

        // Too old (below the band), when one exists.
        if MIN_SUPPORTED_CONTRACT_VERSION.version > 0 {
            let too_old = ContractVersion::new(MIN_SUPPORTED_CONTRACT_VERSION.version - 1);
            assert!(!is_supported_contract_version(too_old));
            assert!(!ServerHello::for_client(too_old).compatible);
        }
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

    #[test]
    fn build_version_is_semver_shaped_whatever_named_this_build() {
        // The stamp (#513) resolves differently per build (a release tag, a dev commit hash, a
        // tarball fallback), but every spelling must satisfy the shape the /about contract test
        // pins: x.y.z with an optional -prerelease tail. This is the compile-time proof the
        // build.rs emitted SOMETHING sane for the build running these tests.
        let v = crate::BUILD_VERSION;
        let (core, _tail) = v.split_once('-').unwrap_or((v, ""));
        assert_eq!(core.split('.').count(), 3, "MAJOR.MINOR.PATCH core: {v:?}");
        assert!(
            core.split('.')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())),
            "numeric core: {v:?}"
        );
    }
}
