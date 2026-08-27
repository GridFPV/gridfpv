//! Timers as **application-level configuration** — the `TimerRegistry` and `Timer` (issue #73).
//!
//! A timer is the thing that *produces lap-gate passes* — the built-in synthetic
//! **Mock**, or a real **RotorHazard** server (connected since #65). The model parallels
//! the event model ([`EventRegistry`](crate::events::EventRegistry)): a Race Director configures
//! their timers **once** at the application level (a persisted registry) and each event simply
//! **selects** which of them to use (see [`EventMeta::timers`](crate::events::EventMeta::timers)).
//! Set up the RotorHazard once, and every new event just picks it.
//!
//! # Two pieces, mirroring events
//!
//! - **App-level registry (this module).** The [`TimerRegistry`] holds every configured
//!   [`Timer`] behind a lock and **persists** them to `<GRIDFPV_DATA_DIR>/timers.json`
//!   (restored on boot; in-memory only when no data dir is configured). A built-in
//!   **Mock** ([`MOCK_TIMER_ID`]) is always present — so an unconfigured Director can run a
//!   sim race out of the box — and cannot be deleted.
//! - **Per-event selection (`crate::events`).** Each [`EventMeta`](crate::events::EventMeta)
//!   carries a `timers: Vec<TimerId>` of the timers that event uses; new events (and Practice)
//!   default to `["sim"]`.
//!
//! # The kinds
//!
//! [`TimerKind::Mock`] is the synthetic source wired end-to-end here (its `laps`/`lap_ms` drive
//! the per-event sim bridge). [`TimerKind::Rotorhazard`] holds the RH server `url`, and **is
//! connected** (#65): the Director dials it, drives the
//! [`Connecting`](TimerStatus::Connecting) → [`Connected`](TimerStatus::Connected) →
//! [`Disconnected`](TimerStatus::Disconnected)/[`Error`](TimerStatus::Error) lifecycle, probes for
//! the GridFPV plugin (see [`PluginPresence`]), and feeds its passes into the event log. This
//! module stays purely the *configuration* half — the connector itself lives in the app crate,
//! behind its default `live` feature (a non-`live` build leaves an RH timer inert).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use gridfpv_events::CompetitorRef;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The reserved id of the always-present built-in **Mock** timer.
///
/// The Mock is seeded into every registry, draws its `laps`/`lap_ms` from the Director's
/// env defaults (`GRIDFPV_SIM_LAPS` / `GRIDFPV_SIM_LAP_MS`), and cannot be deleted — so a
/// Director with nothing configured can still run a sim race. New events default to selecting it.
pub const MOCK_TIMER_ID: &str = "mock";

/// The display name of the built-in Mock timer.
pub const MOCK_TIMER_NAME: &str = "Mock";

/// The default node/slot count for a timer that does not specify one (race redesign Slice 4a).
///
/// Eight is the ubiquitous FPV timer width (RotorHazard's default, the Raceband R1–R8 grid), so a
/// timer persisted before the channel model existed — or created without an explicit `node_count`
/// — reads back as an 8-node timer, the same heat-size cap a real 8-seat timer enforces.
pub const DEFAULT_NODE_COUNT: u32 = 8;

/// The file name (under the data dir) the timer registry is persisted to (issue #73).
pub const TIMERS_FILE: &str = "timers.json";

/// Identifies a **timer** in the application-level registry (issue #73).
///
/// A transparent string newtype like [`EventId`](crate::scope::EventId): the built-in Mock
/// has the reserved id [`MOCK_TIMER_ID`]; created timers get an auto-generated slug + suffix id,
/// never user-entered (names are display-only).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct TimerId(pub String);

/// The kind of a timer — *how* it produces passes (issue #73).
///
/// Externally tagged so it maps to a TS discriminated union. [`Mock`](TimerKind::Mock) is the
/// built-in synthetic source; [`Rotorhazard`](TimerKind::Rotorhazard) is a **real, connected**
/// timer (#65) — its `url` is stored here and round-trips on the wire and on disk, and the
/// Director dials it, probes for the GridFPV plugin, and streams its passes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerKind {
    /// The built-in synthetic source: emit a holeshot + `laps` laps per pilot at `lap_ms`
    /// real-time pace when a heat goes Running (mirrors the existing sim knobs).
    Mock {
        /// Laps each sim pilot flies beyond the holeshot.
        laps: u32,
        /// The nominal real-time pace of one sim lap, in milliseconds.
        #[ts(type = "number")]
        lap_ms: u64,
    },
    /// A **RotorHazard** server (#65): holds the base URL the connector dials.
    Rotorhazard {
        /// The RotorHazard server base URL — `http://<host>:5000`, e.g.
        /// `http://rotorhazard.local:5000`.
        ///
        /// Passed **verbatim** to the socket.io client: no trimming, no trailing-slash removal, no
        /// scheme defaulting. [`validate_timer_config`] only rejects empty/whitespace, so a
        /// trailing slash, a missing `http://`, or `https://` against a plain-HTTP RH all reach the
        /// dialer as-is and fail as a connection [`Error`](TimerStatus::Error). The console's URL
        /// field states that shape (#381).
        url: String,
    },
}

/// What channels a timer can be tuned to (race redesign Slice 4a) — its *channel capability*,
/// declared generically (NOT RotorHazard-specific).
///
/// A timer is one of two kinds:
///
/// - [`Fixed`](ChannelCapability::Fixed) — the timer supports only a **specific allowed set** of
///   built-in catalog frequencies (raw MHz). A limited timer (e.g. a fixed-band module) exposes
///   only what it physically supports; a console must pick a heat's channels from this set.
/// - [`Flexible`](ChannelCapability::Flexible) — the timer accepts **any** frequency: the whole
///   standard catalog *plus* arbitrary custom raw MHz (e.g. RotorHazard, whose nodes tune freely).
///
/// Externally tagged so it maps to a TS discriminated union. It is **additive** on the wire and on
/// disk: a timer persisted before the channel model existed deserializes with the
/// [`Default`] capability ([`Flexible`](ChannelCapability::Flexible)), so old `timers.json` files
/// round-trip and stay valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ChannelCapability {
    /// The timer supports only this explicit set of built-in catalog frequencies (raw MHz). A
    /// console offers exactly these; assignment allocates from (the timer's available subset of)
    /// these.
    Fixed {
        /// The allowed built-in channel centre frequencies, in raw MHz, in preference order.
        channels: Vec<u16>,
    },
    /// The timer accepts any frequency — the standard catalog plus arbitrary custom raw MHz.
    Flexible,
}

impl Default for ChannelCapability {
    /// A timer with no declared capability is [`Flexible`](ChannelCapability::Flexible) — the
    /// permissive default, so a pre-channel-model timer (and the free-tune Mock) stays usable.
    fn default() -> Self {
        ChannelCapability::Flexible
    }
}

impl ChannelCapability {
    /// Whether `mhz` is a frequency this timer can be tuned to: any value for a
    /// [`Flexible`](ChannelCapability::Flexible) timer, or one of the allowed set for a
    /// [`Fixed`](ChannelCapability::Fixed) one.
    pub fn allows(&self, mhz: u16) -> bool {
        match self {
            ChannelCapability::Flexible => true,
            ChannelCapability::Fixed { channels } => channels.contains(&mhz),
        }
    }
}

/// Whether a timer is currently usable, and — for a live source — the state of its connection
/// (issues #73, #65).
///
/// Two **static** states describe a timer's resting config: the Mock is always
/// [`Ready`](TimerStatus::Ready) (it needs nothing external), and a configured-but-not-yet-dialed
/// RotorHazard timer reports [`Configured`](TimerStatus::Configured). The remaining four are
/// **dynamic** connection states the Director drives on a live (`live`-feature) RotorHazard timer
/// as its connection comes and goes: [`Connecting`](TimerStatus::Connecting) while the socket is
/// being established, [`Connected`](TimerStatus::Connected) once it is up,
/// [`Disconnected`](TimerStatus::Disconnected) when it drops, and [`Error`](TimerStatus::Error)
/// when the connection attempt fails. They are **additive on the wire** — a console that only knows
/// `Ready`/`Configured` still parses the type; new variants surface richer status.
///
/// These dynamic states are **not persisted** (`timers.json` always restores a timer's resting
/// status from its kind — see [`Timer::status_for`]); they are live, in-memory, and reset to
/// `Configured` whenever the RH timer's kind/config **actually changes** (a no-op edit leaves the
/// live state alone — see [`TimerRegistry::update`] and #382).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum TimerStatus {
    /// Usable right now — the built-in Mock.
    Ready,
    /// Configured but not connected (a RotorHazard timer with a URL on file, not yet dialed).
    Configured,
    /// A live RotorHazard timer whose connection is being established.
    Connecting,
    /// A live RotorHazard timer with an established connection (passes are flowing in).
    Connected,
    /// A live RotorHazard timer whose connection has dropped (was up, now down).
    Disconnected,
    /// A live RotorHazard timer whose connection attempt failed (could not reach the server).
    Error,
}

/// Whether a connected RotorHazard timer carries the **GridFPV plugin** (RH plugin design D16,
/// Slice 1) — the in-process integration the Director probes for over the existing socket.io
/// connection (`gridfpv_hello` → `gridfpv_hello_ack`). Carried as an `Option` on [`Timer`]:
/// `None` is "not probed" (a Mock timer, or an RH not yet connected). Like [`TimerStatus`] it is a
/// **live, in-memory** value, not persisted (reset to `None` on load and on reconfigure) and
/// **additive on the wire** (`#[ts(optional)]` — an older console still parses a `Timer`). The
/// Director uses it to drive the required-with-guided-install UX (§5): `Missing` and `Incompatible`
/// surface the one-step install; `Present` surfaces a healthy timer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum PluginPresence {
    /// Probed, but no GridFPV plugin answered the handshake (a stock RH, or one whose plugin
    /// failed to load) — the guided-install prompt applies. An RH older than v4.3.0 also lands
    /// here (it can't run the plugin), and the install guidance says to update RH first.
    Missing,
    /// The plugin answered and its `gridfpv_*` protocol is compatible with this Director.
    Present {
        /// The plugin build version (e.g. `"0.1.0"`).
        plugin_version: String,
        /// The RHAPI version the plugin reported (e.g. `"1.4"`).
        rhapi_version: String,
        /// The capabilities the plugin declared (e.g. `["hello"]`).
        capabilities: Vec<String>,
    },
    /// The plugin answered but its protocol version is outside the Director's supported range —
    /// the guided install offers the matching plugin build.
    Incompatible {
        /// The plugin build version, so the UI can name the mismatch.
        plugin_version: String,
        /// The plugin's `gridfpv_*` protocol version (the field that didn't match).
        protocol_version: u32,
        /// A short plain-language reason for the mismatch.
        reason: String,
    },
}

/// Why an event may **not select** a RotorHazard timer (#405) — the reason, so every surface can
/// phrase it its own way without re-deriving the rule.
///
/// The GridFPV plugin is **required** for Grid to race a RotorHazard timer (D16 / #405): without
/// it Grid cannot own its RH-side race objects, and every race-conduct decision falls back to
/// mutating the RD's own format and hoping (the #403 failure). The gate is at **event timer
/// selection** — not at connecting, and not at the plugin probe: connect / disconnect (#383),
/// restart (#386) and the presence probe stay open, because they are exactly how the RD *gets* to
/// a working plugin. Gating them would deadlock the setup flow.
///
/// The three variants are three genuinely different problems with three different fixes, which is
/// why they are not collapsed into one "plugin not ok":
///
/// - [`NotConnected`](Self::NotConnected) — presence is `None`, i.e. **never probed**. Probing
///   needs a live socket, so this is the *normal* state of a freshly added timer, and the fix is
///   "connect it", not "install something".
/// - [`PluginMissing`](Self::PluginMissing) — probed, nothing answered: install the plugin.
/// - [`PluginIncompatible`](Self::PluginIncompatible) — probed, answered, wrong protocol: update it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionRefusal {
    /// The timer has never been probed (`plugin: None`) — it has not been connected yet, so Grid
    /// has never had a chance to ask whether the plugin is there.
    NotConnected,
    /// Probed, and no GridFPV plugin answered ([`PluginPresence::Missing`]).
    PluginMissing,
    /// Probed, and the plugin that answered speaks a protocol this Director does not
    /// ([`PluginPresence::Incompatible`]).
    PluginIncompatible,
}

impl SelectionRefusal {
    /// The **selection-time** refusal, naming the timer by its friendly name (repo display rule).
    ///
    /// Each variant points at the next action the RD should take, because "no" without "do this
    /// instead" is what stranded the RD in #385.
    pub fn selection_message(self, name: &str) -> String {
        match self {
            Self::NotConnected => format!(
                "cannot select {name:?} for this event: it has not been connected yet, so Grid \
                 cannot tell whether it is running the GridFPV plugin. Connect it from the Timers \
                 menu first, then select it."
            ),
            Self::PluginMissing => format!(
                "cannot select {name:?} for this event: it is not running the GridFPV plugin, \
                 which Grid requires to race a RotorHazard timer. Install it from the guided \
                 install in the Timers menu, restart RotorHazard, then select it."
            ),
            Self::PluginIncompatible => format!(
                "cannot select {name:?} for this event: its GridFPV plugin speaks a protocol this \
                 Director does not. Update it from the guided install in the Timers menu, restart \
                 RotorHazard, then select it."
            ),
        }
    }

    /// The **arm-time** backstop refusal (#405), naming the timer by its friendly name.
    ///
    /// Distinct copy from [`selection_message`](Self::selection_message) because the situation is
    /// distinct: the selection was *valid when it was made* and the plugin has since gone away (RH
    /// restarted without it, or it failed to load), so the RD is being told something changed
    /// under them mid-event — not that they picked wrong.
    pub fn arm_message(self, name: &str) -> String {
        match self {
            Self::NotConnected => format!(
                "cannot arm this heat: this event races {name:?}, which is not connected, so Grid \
                 cannot confirm the GridFPV plugin is loaded. Connect it from the Timers menu."
            ),
            Self::PluginMissing => format!(
                "cannot arm this heat: the GridFPV plugin is no longer answering on {name:?}, and \
                 Grid requires it to race a RotorHazard timer. Reinstall it and restart \
                 RotorHazard, or select a different timer for this event."
            ),
            Self::PluginIncompatible => format!(
                "cannot arm this heat: the GridFPV plugin on {name:?} now speaks a protocol this \
                 Director does not. Update it and restart RotorHazard, or select a different timer \
                 for this event."
            ),
        }
    }
}

/// The lowest enter/exit threshold a calibration write may carry (#355).
///
/// **Not `0`.** RotorHazard's `calibration.py` tests the incoming level for *truthiness*
/// (`if not enter_at_level:`), so a `0` means "ignore me and read the level back off the node"
/// rather than "set the level to zero" — identical on v4.3.0 and v4.4.0. A typed `0` would
/// therefore be accepted, answered with a success, and silently change nothing: the #403 failure
/// class this page exists to diagnose. The console clamps to this too; the Director clamps again
/// because a value that reaches timing hardware is never the client's to decide.
pub const RSSI_MIN: u32 = 1;

/// The highest enter/exit threshold a calibration write may carry (#355) — RSSI on RotorHazard is
/// a filtered 8-bit ADC count, so `255` is the top of the domain.
///
/// ⚠️ **RotorHazard's own hardware gate is one count tighter.** `Node.is_valid_rssi` is
/// `value > 0 and value < max_rssi_value`, and `max_rssi_value` is `255` on any node at API level
/// ≥ 18 — so a literal `255` writes the *profile* row and is then dropped by
/// `RHInterface.set_enter_at_level` without ever reaching the node. The readback
/// (`enter_and_exit_at_levels`) is served from that profile row, so it would confirm a value the
/// detector does not hold. `255` is a useless threshold in practice (nothing ever reaches full
/// scale), so this matches the console's agreed domain rather than silently disagreeing with it —
/// but the console's own ceiling wants lowering to `254`.
pub const RSSI_MAX: u32 = 255;

/// One node's **GridFPV-owned** enter/exit calibration (#355, D27).
///
/// D27: *"GridFPV is the sole system of record for configuration"* — a threshold the RD sets is
/// GridFPV's value, and the timer is merely where it takes effect. So an accepted calibration write
/// is recorded here, on the [`Timer`], and persisted with it; the level that comes back from the
/// timer on `GET /timers/{id}/signal` is **evidence about the timer**, never the store.
///
/// Each threshold is independently optional because the console writes only the one the RD actually
/// moved: a node whose exit has been tuned and whose enter has not holds `exit_at: Some, enter_at:
/// None`, which is the truth rather than a fabricated pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NodeCalibration {
    /// The node's index on the timer, `0`-based (RotorHazard's `seat_index`).
    pub node: u32,
    /// The enter threshold GridFPV has set on this node, or `None` if it never has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enter_at: Option<u32>,
    /// The exit threshold GridFPV has set on this node, or `None` if it never has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_at: Option<u32>,
}

/// One node's **GridFPV-owned** channel (#413, D27) — the twin of [`NodeCalibration`].
///
/// D27 again: *"GridFPV is the sole system of record for configuration"*, and *"writing to a timer
/// is applying, not storing"*. A channel the RD picks on the Tune page is GridFPV's value; the
/// node's receiver is merely where it takes effect. So an accepted channel write is recorded here,
/// on the [`Timer`], and persisted with it — while the `frequency_mhz` RotorHazard reports back on
/// `GET /timers/{id}/signal` is **evidence about the timer**, never the store.
///
/// The `band`/`channel` pair is the **friendly name** ([`crate::channels::ChannelCatalogEntry`]),
/// resolved server-side from the shared catalog at accept time and carried onto the emit so
/// RotorHazard's own UI shows `Raceband R7` rather than a bare number. `None` for a **custom** raw
/// MHz the catalog does not know — that is an honest absence, not a name worth inventing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NodeChannel {
    /// The node's index on the timer, `0`-based (RotorHazard's `seat_index`).
    pub node: u32,
    /// The channel's centre frequency in raw MHz — the value the receiver actually tunes.
    pub mhz: u16,
    /// The catalog band this channel was picked from (`"Raceband"`), or `None` for a custom MHz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub band: Option<String>,
    /// The catalog channel label within that band (`"R7"`), or `None` for a custom MHz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel: Option<String>,
}

/// The **friendly name** for a raw centre frequency (`5880` → `"Raceband R7"`), resolved through
/// the shared channel catalog — the repo display rule's `frequency → band+channel` resolver on the
/// Director side, so a refusal message never puts a bare number in front of an RD.
///
/// The **first** catalog entry whose MHz matches wins (the catalog is ordered Raceband first), and
/// a frequency the catalog does not know falls back to `"5885 MHz"` — a custom channel has no name
/// to resolve, and inventing one would be worse than the number.
pub fn channel_label(mhz: u16) -> String {
    crate::channels::label_of(mhz)
        .map(|(band, channel)| format!("{band} {channel}"))
        .unwrap_or_else(|| format!("{mhz} MHz"))
}

/// Resolve the `(band, channel)` a channel write should carry, against the shared catalog (#413).
///
/// GridFPV owns the vocabulary (D27), so a client-supplied label is **validated**, never trusted:
/// a `(band, channel, mhz)` triple the catalog actually holds is honoured — which is what lets the
/// caller name `Fatshark F8` for the frequency the console leads as `Raceband R7` — and anything
/// else falls back to
/// the first catalog entry for that frequency. A frequency the catalog does not know resolves to
/// `None`: a **custom** channel travels as a bare frequency, because it has no label to send.
fn resolve_channel_label(
    mhz: u16,
    band: Option<&str>,
    channel: Option<&str>,
) -> Option<(String, String)> {
    if let (Some(band), Some(channel)) = (band, channel) {
        if let Some(entry) = crate::channels::catalog()
            .into_iter()
            .find(|e| e.mhz == mhz && e.band == band && e.channel == channel)
        {
            return Some((entry.band, entry.channel));
        }
    }
    crate::channels::label_of(mhz)
}

/// One configured timer in the application-level registry (issue #73).
///
/// The wire shape `GET /timers` returns and the on-disk shape `timers.json` persists: a stable
/// [`TimerId`], a human display `name`, the [`TimerKind`] (its config), and a derived
/// [`TimerStatus`]. Derives serde (its JSON *is* both the wire and the persisted form) and
/// `ts_rs::TS` so the frontend reads a generated `Timer` type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Timer {
    /// The stable handle an event selects by and the API addresses (`PUT /timers/{id}`).
    pub id: TimerId,
    /// The human-readable display name (display-only; the id is authoritative).
    pub name: String,
    /// The kind + config: a [`TimerKind::Mock`] or a [`TimerKind::Rotorhazard`].
    pub kind: TimerKind,
    /// The derived usability of the timer (see [`TimerStatus`]).
    pub status: TimerStatus,
    /// The timer's **channel capability** (race redesign Slice 4a): the set of frequencies it can
    /// tune to ([`Fixed`](ChannelCapability::Fixed)) or that it tunes freely
    /// ([`Flexible`](ChannelCapability::Flexible)). Additive — defaults to
    /// [`Flexible`](ChannelCapability::Flexible) for a pre-channel-model timer.
    #[serde(default)]
    pub channel_capability: ChannelCapability,
    /// The Race Director's **explicit width override** — how many nodes GridFPV should treat this
    /// timer as having, regardless of what the hardware says (#412).
    ///
    /// `None` — the default for a newly created timer — means **follow the timer**: the width comes
    /// from [`reported_nodes`](Timer::reported_nodes), discovered on connect. `Some(n)` pins it,
    /// which is what a Mock (nothing to ask) and a timer whose RD deliberately set a width use.
    /// [`node_width`](Timer::node_width) resolves the two.
    ///
    /// **Migration (#412).** This was a plain `u32` defaulting to [`DEFAULT_NODE_COUNT`], and the
    /// registry always wrote it — so every pre-#412 `timers.json` carries an explicit number and
    /// reads back as `Some(n)`, keeping exactly the width it had. Only timers created *after* this
    /// change start out following the hardware. A disagreement between this and what the timer
    /// reports is surfaced as [`NodeDrift`], never silently resolved.
    #[serde(default)]
    #[ts(optional)]
    pub node_count: Option<u32>,
    /// How many nodes the timer **said it has**, learned on connect (#412) — an *observation*, not
    /// config.
    ///
    /// RotorHazard publishes no `num_nodes` scalar on the socket, but the count is knowable three
    /// ways, and the Director takes the most direct one available on each (re)connect: the GridFPV
    /// plugin's `gridfpv_hello_ack` (`len(rhapi.interface.seats)`), else the length of
    /// `frequency_data.fdata`, else the `[:num_nodes]`-sliced `enter_and_exit_at_levels`. `None`
    /// means the timer has never been asked — a Mock, an adapter that cannot report, or an RH not
    /// yet dialed.
    ///
    /// **Live, in-memory, never persisted as config** (D27: *"a value read from a timer is evidence
    /// about the timer, not an input to a decision"*). Like [`plugin`](Timer::plugin) it round-trips
    /// on the wire but is reset to `None` on load, so a restart re-observes rather than remembering.
    #[serde(default)]
    #[ts(optional)]
    pub reported_nodes: Option<u32>,
    /// The node indices (**0-based**) the Race Director has **disabled** on this timer (#412) — a
    /// dead receiver, a gate that will not tune, a seat that must not be flown.
    ///
    /// **This is a decision, so it is persisted and it survives a reconnect.** A timer that keeps
    /// reporting four nodes does not re-enable node 3; the RD turned it off on purpose. Stored as
    /// the *complement* of the enabled set precisely so it stays meaningful when the reported width
    /// changes: "node 2 is busted" is true whether the timer reports 4 nodes or 8.
    ///
    /// Sorted ascending and de-duplicated on write. Entries at or beyond
    /// [`node_width`](Timer::node_width) are inert (there is no such node to disable) but are kept
    /// rather than pruned — a timer that comes back wider must not silently un-disable a node.
    /// Additive on the wire and on disk: an older `timers.json` restores with none disabled, which
    /// is every node enabled — exactly the pre-#412 behaviour.
    ///
    /// **0-based on the wire, 1-based on screen.** Index `2` is the node the RD calls "Node 3"; see
    /// [`Timer::node_label`].
    #[serde(default)]
    pub disabled_nodes: Vec<u32>,
    /// The timer's **allowed channel set** (race redesign Slice 4a; #117 S1): the raw-MHz channels
    /// the Race Director has said this timer **may ever use** — what per-heat assignment draws
    /// from, in preference order. Edited by the checkbox picker on the Timers page.
    ///
    /// **"Allowed", not "assigned", and not "capable".** Three different questions live nearby and
    /// conflating them has been a recurring bug class (#402, #413, #416):
    ///
    /// - *What can the hardware tune?* → [`channel_capability`](Timer::channel_capability).
    ///   [`Fixed`](ChannelCapability::Fixed) restricts; [`Flexible`](ChannelCapability::Flexible)
    ///   does not. Assignment filters this set by it.
    /// - *What may this timer be used on?* → **here**.
    /// - *What is node N tuned to right now?* → [`node_channels`](Timer::node_channels) for what
    ///   GridFPV last wrote from the bench, the heat's own `frequencies` for what a race allocated,
    ///   `NodeSignal::frequency_mhz` for what the hardware reports. **Never this field indexed by
    ///   node** — an allowed set carries no per-node mapping (that is S2's event *layouts*).
    ///
    /// ⚠️ **Empty means "the RD has not configured this timer", and what follows differs by
    /// context.** Every RotorHazard on the bench reports `Flexible` with an empty set.
    ///
    /// - **Offering a human a choice** (the #413 Tune-page dropdown): empty ⇒ offer the whole
    ///   catalog. The RD is actively picking; show them everything the timer can do.
    /// - **Assigning automatically** ([`assign_frequencies`](crate::round_engine::assign_frequencies)):
    ///   empty ⇒ **refuse**, naming the timer. Inventing channels from the catalog would scatter a
    ///   heat across the band with no RD intent behind it — "no channels" becoming "arbitrary
    ///   channels", which is worse for looking deliberate.
    ///
    /// Additive — defaults empty for a pre-channel-model timer.
    #[serde(default)]
    pub available_channels: Vec<u16>,
    /// Whether this (RotorHazard) timer carries the **GridFPV plugin** (D16, S1). `None` until
    /// probed (Mock timers, or an RH not yet connected). Live, in-memory, not persisted — reset to
    /// `None` on load/reconfigure and driven by the connect-time handshake probe. Additive on the
    /// wire (`#[ts(optional)]`): an older console (or a test fixture) omits it.
    #[serde(default)]
    #[ts(optional)]
    pub plugin: Option<PluginPresence>,
    /// Whether the Race Director is **manually holding a connection** to this (RotorHazard) timer,
    /// independent of any event (issue #383).
    ///
    /// A timer only ever connected when the *active event* selected it, so "is this timer even
    /// reachable?" — the question the Timers menu exists to answer — could not be asked without
    /// first creating and activating an event. `POST /timers/{id}/connect` sets this hold and
    /// `POST /timers/{id}/disconnect` clears it; the connection reconciler unions the held timers
    /// with the active event's selection, so a held timer dials and publishes the same
    /// [`TimerStatus`] and [`PluginPresence`] the event-driven path does.
    ///
    /// **Lifetime: explicit.** The hold persists until the RD disconnects — this is a diagnostic
    /// control, and a "test connection" that silently expired would be worse than useless at a
    /// venue. It is deliberately *not* dropped when an active event takes the timer over: the event
    /// connection simply supersedes it, and when the event lets go the manual hold takes the timer
    /// back. Live and **in-memory only**, like [`status`](Timer::status) — it round-trips on the
    /// wire but a restart comes back with no holds. Additive: defaults to `false`.
    #[serde(default)]
    pub manual_connect: bool,
    /// The per-node enter/exit thresholds **GridFPV has set** on this timer (#355, D27), in node
    /// order, one entry per node that has ever been calibrated from the Tune page.
    ///
    /// **This is the config; the timer is where it is applied.** D27's test — *"delete everything
    /// GridFPV put on the timer, and it must rebuild identically from GridFPV's own state"* — is
    /// why this is a persisted field on `Timer` rather than something read back off RotorHazard.
    /// A threshold read from the timer is evidence about the timer, never an input to a decision.
    ///
    /// Written by [`TimerRegistry::request_calibration`] at the moment a write is **accepted**,
    /// which is deliberately before the emit has landed: the record is of what GridFPV decided,
    /// not of what the hardware was observed to do (that is what `GET /timers/{id}/signal` is
    /// for). Additive on the wire and on disk — an older `timers.json` restores with none.
    ///
    /// ⚠️ **Not yet re-applied on reconnect.** D27 also asks that a timer coming back with
    /// different values be pushed back to GridFPV's, with a drift notice rather than a silent
    /// overwrite; that half is not built (see [`TimerRegistry::request_calibration`]).
    #[serde(default)]
    pub calibration: Vec<NodeCalibration>,
    /// The per-node channels **GridFPV has set** on this timer (#413, D27), in node order — one
    /// entry per node whose channel has ever been set from the Tune page.
    ///
    /// The twin of [`calibration`](Timer::calibration), and for the same D27 reason: a channel is
    /// Grid-owned config *applied* to the timer, exactly like a threshold. Written by
    /// [`TimerRegistry::request_channel`] at the moment a write is accepted.
    ///
    /// ⚠️ **Distinct from [`available_channels`](Timer::available_channels).** That is the
    /// *allowed set* per-heat assignment draws from — a set, carrying no per-node mapping; this is
    /// what an individual node was last told to tune to from the bench. A heat legitimately
    /// overwrites the latter (it re-tunes every node to its assigned channel) and this record is
    /// not re-applied afterwards — the Tune page says so rather than pretending the bench value
    /// wins.
    ///
    /// ⚠️ **Not re-applied on reconnect** either — same gap, and same reason, as `calibration`'s.
    ///
    /// **Additive on the wire and on disk**, and `#[ts(optional)]` like [`plugin`](Timer::plugin):
    /// an empty record is omitted entirely, so a pre-#413 `timers.json` restores with none and a
    /// console (or a test fixture) written before this existed still parses a `Timer`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(as = "Option<Vec<NodeChannel>>", optional)]
    pub node_channels: Vec<NodeChannel>,
}

impl Timer {
    /// The timer's **effective width** in nodes (#412) — how many node indices exist at all.
    ///
    /// Resolves the two values the model deliberately keeps apart, in priority order:
    ///
    /// 1. [`node_count`](Timer::node_count) — the RD's explicit override, if they set one;
    /// 2. [`reported_nodes`](Timer::reported_nodes) — what the timer said on connect;
    /// 3. [`DEFAULT_NODE_COUNT`] — nothing configured and nothing observed (a Mock, or an RH that
    ///    has never been dialed).
    ///
    /// Config wins over the observation on purpose: an observation that disagrees is a
    /// [`NodeDrift`] notice for the RD, never a silent edit of what GridFPV decided (D27).
    ///
    /// This is the width, **not** the heat-size cap — a disabled node still occupies an index. The
    /// cap is [`enabled_nodes`](Timer::enabled_nodes)`.len()`.
    pub fn node_width(&self) -> u32 {
        self.node_count
            .or(self.reported_nodes)
            .unwrap_or(DEFAULT_NODE_COUNT)
    }

    /// The node indices (**0-based, ascending**) a heat may actually be seated on (#412): every
    /// index below [`node_width`](Timer::node_width) that the RD has not disabled.
    ///
    /// **A set, not a count — and not necessarily a prefix.** With node index `2` disabled on a
    /// 4-node timer this is `[0, 1, 3]`, and a 3-pilot heat occupies exactly those nodes. Callers
    /// must walk this list rather than `0..len()`: the *n*-th pilot of a heat sits on
    /// `enabled_nodes()[n]`, which is the index RotorHazard's `seat_index`, the `node-{i}`
    /// competitor ref and [`NodeSignal::node`] all mean. Renumbering to close the gap would seat a
    /// pilot on a dead gate — the exact failure #412 exists to prevent.
    pub fn enabled_nodes(&self) -> Vec<u32> {
        (0..self.node_width())
            .filter(|node| !self.disabled_nodes.contains(node))
            .collect()
    }

    /// How many pilots this timer can seat in one heat (#412) — the size of the enabled set.
    pub fn seat_capacity(&self) -> usize {
        self.enabled_nodes().len()
    }

    /// Whether `node` (0-based) exists on this timer **and** the RD has left it enabled (#412).
    /// The single rule the calibration route and the seat mapping both ask.
    pub fn node_enabled(&self, node: u32) -> bool {
        node < self.node_width() && !self.disabled_nodes.contains(&node)
    }

    /// The display name of node `node` (0-based) — **1-based**, per the repo display rule: index
    /// `2` renders as `"Node 3"`, which is the node the RD means when they say "node 3 is busted".
    ///
    /// The one place the 0-based wire and the 1-based screen meet. Everything that reaches a person
    /// goes through here; everything that reaches a wire keeps the raw index.
    pub fn node_label(node: u32) -> String {
        format!("Node {}", node + 1)
    }

    /// The disagreement between what the timer **reported** and what GridFPV is **configured** for
    /// (#412), or `None` when they agree (or nothing has been observed yet).
    ///
    /// Surfaced, never acted on. Same rule as #355's calibration drift: an observation that
    /// contradicts config is information for the RD, not a licence to overwrite a decision.
    pub fn node_drift(&self) -> Option<NodeDrift> {
        let reported = self.reported_nodes?;
        let configured = self.node_width();
        // Enabled seats the hardware does not have: these silently seat nobody, so they are the
        // half of the drift that actually loses laps.
        let enabled_beyond_reported: Vec<u32> = self
            .enabled_nodes()
            .into_iter()
            .filter(|node| *node >= reported)
            .collect();
        if reported == configured && enabled_beyond_reported.is_empty() {
            return None;
        }
        Some(NodeDrift {
            reported,
            configured,
            enabled_beyond_reported,
        })
    }

    /// Lay a heat's `lineup` onto this timer's **real node indices** (#412) — the one rule that
    /// decides which gate each pilot flies.
    ///
    /// Returns `(node_index, competitor)` in lineup order, where `node_index` is the **0-based
    /// index RotorHazard's `seat_index` means**: the *n*-th pilot of the heat sits on
    /// [`enabled_nodes`](Timer::enabled_nodes)`[n]`, **not** on `n`. With node index `2` disabled on
    /// a 4-node timer, a 3-pilot heat comes back as nodes `0, 1, 3`.
    ///
    /// This is the correctness heart of #412. Every caller that pushes a heat at a timer — the
    /// `set_frequency` tune plan, the `alter_heat` pilot seating — must go through here, because
    /// getting it wrong seats a pilot on the dead node the feature exists to avoid, one layer down
    /// from the bug it fixes. The indices are **never renumbered** to close the gap: `node-{i}`
    /// competitor refs, [`NodeSignal::node`] and the signal trace all mean the same physical gate,
    /// and a compacted index would make marshaling and the trace disagree about where a pass came
    /// from.
    ///
    /// Two kinds of lineup entry, handled together because a heat may in principle mix them:
    ///
    /// - A **`node-{i}` seat ref** (an open-practice channel lineup) already *names* its node, so it
    ///   keeps index `i` verbatim. That is the whole point of the handle.
    /// - Any other ref (a pilot id) takes the next enabled index not already claimed by one of
    ///   those explicit seats.
    ///
    /// Entries that cannot be placed are **dropped, not squeezed in**: a `node-{i}` ref naming a
    /// disabled or non-existent node, and any pilot beyond the enabled set (which the heat-size cap
    /// should already have refused). Dropping seats nobody there, which records nothing; squeezing
    /// would seat somebody on the wrong gate, which records *the wrong pilot*.
    pub fn seat_nodes(&self, lineup: &[CompetitorRef]) -> Vec<(u32, CompetitorRef)> {
        let enabled = self.enabled_nodes();
        // Indices the lineup names outright — they are spoken for, so the positional walk below
        // must not hand one of them to a different competitor as well.
        let claimed: Vec<u32> = lineup
            .iter()
            .filter_map(node_seat_index)
            .filter(|node| enabled.contains(node))
            .collect();
        let mut free = enabled.iter().copied().filter(|n| !claimed.contains(n));
        let mut seats = Vec::with_capacity(lineup.len());
        for competitor in lineup {
            match node_seat_index(competitor) {
                // An explicit seat ref: it names its own gate. Skipped when that gate is disabled
                // or does not exist — a practice round must not fly a node the RD switched off.
                Some(node) => {
                    if enabled.contains(&node) {
                        seats.push((node, competitor.clone()));
                    }
                }
                // A pilot: the next enabled gate that nothing else has claimed.
                None => match free.next() {
                    Some(node) => seats.push((node, competitor.clone())),
                    None => break,
                },
            }
        }
        seats
    }

    /// The whole node picture for this timer — the body of `GET /timers/{id}/nodes` (#412).
    ///
    /// One place builds it so the console, the seat mapping and the calibration guard cannot drift
    /// apart on what "enabled" means (CLAUDE.md: go through the shared resolver).
    pub fn node_view(&self) -> TimerNodes {
        let width = self.node_width();
        let enabled = self.enabled_nodes();
        TimerNodes {
            timer: self.id.clone(),
            reported: self.reported_nodes,
            configured: self.node_count,
            width,
            nodes: (0..width)
                .map(|node| TimerNode {
                    node,
                    label: Timer::node_label(node),
                    seat: CompetitorRef(format!("node-{node}")),
                    enabled: !self.disabled_nodes.contains(&node),
                    reported: self.reported_nodes.is_some_and(|r| node < r),
                })
                .collect(),
            enabled,
            drift: self.node_drift(),
        }
    }

    /// Derive the [`TimerStatus`] from a [`TimerKind`]: the Mock is [`Ready`](TimerStatus::Ready);
    /// a RotorHazard timer starts [`Configured`](TimerStatus::Configured) (a URL on file, not yet
    /// dialed) — the connector then drives it through the live statuses.
    fn status_for(kind: &TimerKind) -> TimerStatus {
        match kind {
            TimerKind::Mock { .. } => TimerStatus::Ready,
            TimerKind::Rotorhazard { .. } => TimerStatus::Configured,
        }
    }

    /// Why this timer cannot be **selected by an event** (#405), or `None` when it can.
    ///
    /// The single source of the rule, so the API route, the arm-time backstop and any future
    /// surface cannot drift. **Mock timers are always selectable** — the requirement is
    /// RotorHazard-specific, and the built-in Mock is what an unconfigured Director races out of
    /// the box. A RotorHazard timer is selectable only when its GridFPV plugin has been probed and
    /// found [`Present`](PluginPresence::Present); every other presence maps to the matching
    /// [`SelectionRefusal`].
    pub fn selection_refusal(&self) -> Option<SelectionRefusal> {
        match self.kind {
            // Mock is unaffected: it produces its own passes, there is no plugin to require.
            TimerKind::Mock { .. } => None,
            TimerKind::Rotorhazard { .. } => match &self.plugin {
                Some(PluginPresence::Present { .. }) => None,
                Some(PluginPresence::Missing) => Some(SelectionRefusal::PluginMissing),
                Some(PluginPresence::Incompatible { .. }) => {
                    Some(SelectionRefusal::PluginIncompatible)
                }
                // Never probed. Presence is only knowable over a live socket, so this is the
                // resting state of a freshly added (or freshly restarted-into) timer.
                None => Some(SelectionRefusal::NotConnected),
            },
        }
    }
}

/// The node index a `node-{i}` competitor ref names, or `None` for any other ref (a pilot id, a
/// sim free-text name).
///
/// The **wire** side of the seat handle: 0-based, verbatim, never renumbered. The display side is
/// [`Timer::node_label`].
pub fn node_seat_index(competitor: &CompetitorRef) -> Option<u32> {
    competitor.0.strip_prefix("node-")?.parse::<u32>().ok()
}

/// One node of a timer, as the console lays the node picker out (#412).
///
/// Carries both halves of the 0-based/1-based boundary explicitly — the raw wire index and the
/// resolved display label — so no surface has to do the `+ 1` itself. Every off-by-one here is a
/// pilot on a dead gate, so the boundary is data rather than convention.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerNode {
    /// The node's index on the timer, **0-based** — RotorHazard's `seat_index`, and the same index
    /// [`NodeSignal::node`] and [`CalibrationRequest::node`] carry.
    pub node: u32,
    /// The node's **display name**, 1-based: node `0` is `"Node 1"` (the repo display rule). Use
    /// this wherever a person reads it; never print the raw index.
    pub label: String,
    /// The stable per-node competitor handle (`node-0`, `node-1`, …) — a wire handle, not a label,
    /// and always the **real** node index even when the enabled set has holes in it.
    pub seat: CompetitorRef,
    /// Whether the Race Director has this node **enabled**. A disabled node seats no pilot, is
    /// offered no channel, and refuses calibration.
    pub enabled: bool,
    /// Whether the timer itself reported this node on its last connect. `false` on a node that
    /// exists only because GridFPV is configured wider than the hardware — the drift that seats a
    /// pilot on nothing.
    pub reported: bool,
}

/// A disagreement between what a timer **reported** and what GridFPV is **configured** for (#412).
///
/// **A notice, never an edit.** D27 and #355's calibration drift settle the rule: a value read back
/// from a timer is evidence about the timer, not an input to a decision. GridFPV shows this and
/// keeps racing on its own config; resolving it is the RD's call.
///
/// The two directions mean different things. `reported < configured` is the bench bug #412 was
/// filed for — a real 4-node timer configured as 8, which builds an 8-pilot heat that can only time
/// four. `reported > configured` is a timer with capacity GridFPV is not using, which costs
/// nothing but is worth showing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NodeDrift {
    /// How many nodes the timer said it has.
    pub reported: u32,
    /// The width GridFPV is using ([`Timer::node_width`]).
    pub configured: u32,
    /// The **enabled** node indices (0-based) at or beyond `reported` — seats GridFPV would fill
    /// that the hardware does not have. These are the ones that lose laps: a pilot seated here
    /// flies a heat that records nothing.
    pub enabled_beyond_reported: Vec<u32>,
}

/// A timer's node configuration and the observation behind it — the body of
/// `GET /timers/{id}/nodes` and `PUT /timers/{id}/nodes` (#412).
///
/// The shape the console lays the per-node enable/disable picker out from, and the one answer to
/// "how many pilots fit in a heat on this timer?" (`enabled.len()`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerNodes {
    /// The timer this view describes.
    pub timer: TimerId,
    /// How many nodes the timer **reported** on its last connect, or `None` if it has never been
    /// asked (a Mock, an adapter that cannot report, an RH not yet dialed). An observation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reported: Option<u32>,
    /// The RD's explicit width **override**, or `None` to follow the timer. A decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub configured: Option<u32>,
    /// The **effective width** — how many node indices exist ([`Timer::node_width`]). `nodes` is
    /// exactly this long.
    pub width: u32,
    /// Every node index `0..width`, in order, each with its display label and enabled state.
    pub nodes: Vec<TimerNode>,
    /// The **enabled node indices** (0-based, ascending) — the seats a heat is laid onto, in order:
    /// the *n*-th pilot of a heat sits on `enabled[n]`.
    ///
    /// **Not necessarily `0..n`.** With node index `2` disabled on a 4-node timer this is
    /// `[0, 1, 3]`, and a 3-pilot heat occupies nodes 0, 1 and 3 — a set with a hole, not a prefix.
    pub enabled: Vec<u32>,
    /// The reported-vs-configured disagreement, when there is one (see [`NodeDrift`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub drift: Option<NodeDrift>,
}

/// The body of `PUT /timers/{id}/nodes` — set a timer's node configuration (#412).
///
/// Both fields are independently optional, so the console can send exactly the thing the RD
/// changed: flipping one node's checkbox carries `enabled`, and pinning the width carries
/// `node_count`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetTimerNodesRequest {
    /// The RD's explicit width override.
    ///
    /// **Three-valued on purpose**: absent leaves it unchanged, `null` **clears** it (follow
    /// whatever the timer reports), and a number pins it. "Go back to trusting the hardware" is a
    /// real thing an RD does after a drift notice, and a two-valued field cannot say it.
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional, type = "number | null")]
    pub node_count: Option<Option<u32>>,
    /// The node indices (**0-based**) to leave **enabled**; every other index below the effective
    /// width becomes disabled. Absent leaves the enabled set unchanged.
    ///
    /// Sent as the set the RD wants rather than as a delta so a stale console cannot half-apply an
    /// edit. Indices at or beyond the width, and duplicates, are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enabled: Option<Vec<u32>>,
}

/// serde's three-valued `Option<Option<T>>`: absent → `None`, `null` → `Some(None)`, value →
/// `Some(Some(v))`.
///
/// Needed because serde's *default* handling of a nested `Option` collapses `null` and absent into
/// the same `None`, which is exactly the distinction [`SetTimerNodesRequest::node_count`] depends
/// on to tell "leave it alone" from "clear it".
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Deserialize::deserialize(deserializer).map(Some)
}

/// The body of `POST /timers` — the config a caller supplies to create a timer (issue #73).
///
/// A display `name` plus the [`TimerKind`]; the **id is auto-generated** server-side (a slug of
/// the name + a short random suffix), never user-entered, mirroring `POST /events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateTimerRequest {
    /// The display name for the new timer.
    pub name: String,
    /// The kind + config of the new timer.
    pub kind: TimerKind,
    /// The new timer's **channel capability** (race redesign Slice 4a). Optional and additive —
    /// omit it for the permissive [`Flexible`](ChannelCapability::Flexible) default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel_capability: Option<ChannelCapability>,
    /// The new timer's **node-count override** (race redesign Slice 4a; #412). Optional — omit it
    /// (the normal case now) and the timer's width follows what the hardware reports on connect,
    /// falling back to [`DEFAULT_NODE_COUNT`] until it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node_count: Option<u32>,
    /// The new timer's **available channels** in raw MHz (race redesign Slice 4a). Optional;
    /// defaults to empty (none configured).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub available_channels: Option<Vec<u16>>,
}

/// The body of `PUT /timers/{id}` — the editable fields of a timer (issue #73).
///
/// Edits the display `name` and/or the [`TimerKind`] config (e.g. retune the sim's `lap_ms`, or
/// point a RotorHazard timer at a new URL). Both optional so a partial edit is a one-field body;
/// the id is fixed (it is in the path) and the built-in Mock may be retuned but not removed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UpdateTimerRequest {
    /// A new display name, or `None` to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// A new kind + config, or `None` to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub kind: Option<TimerKind>,
    /// A new **channel capability** (race redesign Slice 4a), or `None` to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel_capability: Option<ChannelCapability>,
    /// A new **node-count override** (race redesign Slice 4a; #412), or `None` to leave it
    /// unchanged. Use `PUT /timers/{id}/nodes` ([`SetTimerNodesRequest`]) to *clear* the override
    /// (follow the timer) or to enable/disable individual nodes — this field can only set one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node_count: Option<u32>,
    /// A new **available-channels** set in raw MHz (race redesign Slice 4a), or `None` to leave it
    /// unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub available_channels: Option<Vec<u16>>,
}

/// The body of `PUT /events/{id}/timers` — the timer ids an event selects (issue #73), and
/// optionally which of them is the **primary** (issue #112).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetEventTimersRequest {
    /// The timers this event uses, in selection order. Each must name a known timer.
    pub ids: Vec<TimerId>,
    /// The **primary** timer among `ids` (issue #112): the timer whose passes feed the log while
    /// healthy, the rest being hot-standby alternates. Optional and additive — omit it to leave the
    /// primary defaulting to the first selected timer. When given, it must be one of `ids`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub primary: Option<TimerId>,
}

/// The body of `PUT /events/{id}/primary-timer` — designate which selected timer is the
/// **primary** (issue #112), the rest being alternates. The `id` must be one of the event's
/// currently-selected timers; `null` clears the override (the first selected timer becomes primary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetPrimaryTimerRequest {
    /// The timer to make primary, or `null` to clear the override (default to the first selected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id: Option<TimerId>,
}

/// The body of `POST /timers/{timer_id}/calibration` (#355) — set one node's detection thresholds.
///
/// **Only the threshold that changed is sent.** The Tune page writes on interaction end, per
/// threshold, so a slider release carries exactly one of the two; both absent is a refusal rather
/// than a no-op success, because "I asked for nothing and it worked" is the shape of every silent
/// calibration failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CalibrationRequest {
    /// The node to calibrate, `0`-based — RotorHazard's `seat_index`, and the same index
    /// [`NodeSignal::node`] carries.
    pub node: u32,
    /// The new **enter** threshold, or absent to leave it alone. Clamped to
    /// [`RSSI_MIN`]..=[`RSSI_MAX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enter_at: Option<u32>,
    /// The new **exit** threshold, or absent to leave it alone. Clamped to
    /// [`RSSI_MIN`]..=[`RSSI_MAX`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_at: Option<u32>,
}

/// The answer to `POST /timers/{timer_id}/calibration` (#355): **what was dispatched**, after
/// clamping — never a readback.
///
/// RotorHazard does not echo `set_enter_at_level` / `set_exit_at_level` (verified on v4.3.0 and
/// v4.4.0: neither handler emits, and `calibration.py` only triggers an internal `Evt`), so there
/// is nothing synchronous to answer with. Reporting the requested value under a readback's name
/// would claim success for a write that may never have reached the detector — the exact failure
/// this page exists to catch — so the field names deliberately match the **request**, not
/// [`NodeSignal`].
///
/// **Confirmation is by poll.** The Director asks RotorHazard to re-broadcast
/// `enter_and_exit_at_levels` right after the write, which arrives on the same socket that feeds
/// `GET /timers/{id}/signal`; the console confirms by seeing [`NodeSignal::enter_at`] /
/// [`NodeSignal::exit_at`] come back holding the value it sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CalibrationDispatch {
    /// The timer the write was dispatched to.
    pub timer: TimerId,
    /// The node it addresses, `0`-based.
    pub node: u32,
    /// The clamped **enter** threshold that was queued, or absent if the request did not carry one.
    /// May differ from what was asked for — that is the clamp, and it is worth showing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enter_at: Option<u32>,
    /// The clamped **exit** threshold that was queued, or absent if the request did not carry one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_at: Option<u32>,
}

/// One queued calibration write, as the connection reconciler drains it (#355).
///
/// The internal twin of [`CalibrationDispatch`] with the timer it belongs to: the queue is a
/// hand-off across the crate boundary (the live sockets live in `gridfpv-app`, *above* this
/// crate), exactly like every other variant of [`PendingTimerWrite`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCalibration {
    /// Which timer to write to.
    pub timer: TimerId,
    /// The node, `0`-based.
    pub node: u32,
    /// The clamped enter threshold to emit, if any.
    pub enter_at: Option<u32>,
    /// The clamped exit threshold to emit, if any.
    pub exit_at: Option<u32>,
    /// Whether the route accepted this write **with an open-practice heat racing on the timer**
    /// (#355, #398).
    ///
    /// The driver keeps its own armed-heat backstop for the window between the route's phase check
    /// and the emit; this tells it that this particular write was already cleared against a heat
    /// that is exempt, so it must not drop it. Without the flag the route would accept a practice
    /// write the driver then silently discarded — dispatched, never landed, which is the failure
    /// mode the whole confirmation design exists to make impossible.
    pub during_open_practice: bool,
}

/// Which of a node's two detection thresholds a **capture** is for (#355).
///
/// A capture is per threshold because RotorHazard's are: `cap_enter_at_btn` and `cap_exit_at_btn`
/// are separate handlers arming separate sampling windows, and the enter branch applies a peak
/// margin the exit branch does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "bindings/")]
pub enum CaptureThreshold {
    /// The **enter** threshold — the level a rising signal must pass for the gate to open.
    Enter,
    /// The **exit** threshold — the level a falling signal must drop back past to close it.
    Exit,
}

impl CaptureThreshold {
    /// The threshold's name as the console and every refusal message say it — never a raw variant.
    pub fn label(self) -> &'static str {
        match self {
            CaptureThreshold::Enter => "Enter at",
            CaptureThreshold::Exit => "Exit at",
        }
    }
}

/// How long RotorHazard samples once a capture starts, in milliseconds (#355).
///
/// `BaseHardwareInterface::CAP_ENTER_EXIT_AT_MILLIS`, verified `3000` on **v4.3.0 and v4.4.0**
/// (the capture path is byte-identical on both). The window opens the moment the emit lands, not
/// when the pass happens — so this is the interval the RD has to fly through the gate, and the
/// Director hands it to the console rather than letting the console hardcode a number that could
/// drift from RotorHazard's.
pub const CAPTURE_WINDOW_MS: u32 = 3_000;

/// How long after the sampling window closes GridFPV keeps waiting for the captured level to come
/// back before calling the capture **not landed** (#355).
///
/// The capture has to survive RotorHazard's own `gevent.sleep(0.025)`, its profile write, the
/// `node_enter_at_level` broadcast (or the driver's readback behind it), the Director's 5 Hz
/// decimation and the console's poll. Generous enough for a slow LAN; short enough that an RD
/// standing at the gate is not left watching a spinner over a capture RotorHazard silently refused.
pub const CAPTURE_SETTLE_MS: u32 = 4_000;

/// The body of `POST /timers/{timer_id}/capture` (#355) — have the timer **measure** one node's
/// threshold instead of being told it.
///
/// The answer to the gap #411 names: a fresh RD with no saved profile and a badly-tuned timer has
/// no starting point, and GridFPV deliberately ships no fabricated default because the right level
/// depends on craft, VTX power, antenna and gate geometry — none of which GridFPV knows. A capture
/// measures the RD's actual craft on their actual gate, which is the only honest way to bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CaptureRequest {
    /// The node to capture on, `0`-based — RotorHazard's `seat_index`, and the same index
    /// [`NodeSignal::node`] and [`CalibrationRequest::node`] carry.
    pub node: u32,
    /// Which threshold to capture.
    pub threshold: CaptureThreshold,
}

/// The answer to `POST /timers/{timer_id}/capture` (#355): **what was dispatched**, never a
/// captured level — the level does not exist yet when this returns.
///
/// A capture is dispatched, then measured for [`window_ms`](CaptureDispatch::window_ms), and only
/// then does RotorHazard have a value. So this is a `200` meaning *the capture was started*, and
/// the fields exist to let the console count the window down and say what it is waiting for.
///
/// **Confirmation is by poll**, exactly as it is for [`CalibrationDispatch`]: the level arrives as
/// [`NodeSignal::enter_at`] / [`NodeSignal::exit_at`] on a later `GET /timers/{id}/signal`, fed
/// both by RotorHazard's end-of-capture `node_enter_at_level` broadcast and by the readback the
/// driver fires once the window has elapsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CaptureDispatch {
    /// The timer the capture was started on.
    pub timer: TimerId,
    /// The node it addresses, `0`-based.
    pub node: u32,
    /// Which threshold is being captured.
    pub threshold: CaptureThreshold,
    /// How long RotorHazard will sample for, in milliseconds — [`CAPTURE_WINDOW_MS`]. The RD has to
    /// fly the pass **inside** this window, which starts now.
    pub window_ms: u32,
    /// How long after the window GridFPV waits for the level before reporting the capture did not
    /// land — [`CAPTURE_SETTLE_MS`].
    pub settle_ms: u32,
    /// The level the timer was reporting for this threshold when the capture started, if it was
    /// reporting one. The console shows it as what the capture is replacing, and GridFPV uses it to
    /// tell "a new level arrived" from "nothing happened".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub previous: Option<u32>,
}

/// One queued **capture** the connection reconciler drains (#355) — the internal twin of
/// [`CaptureDispatch`], and the exact shape [`PendingCalibration`] is for a typed level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCapture {
    /// Which timer to capture on.
    pub timer: TimerId,
    /// The node, `0`-based.
    pub node: u32,
    /// Which threshold.
    pub threshold: CaptureThreshold,
    /// Whether the route accepted this capture **with an open-practice heat racing on the timer**
    /// (#355, #398) — the same flag [`PendingCalibration::during_open_practice`] carries, and for
    /// the same reason: a capture *ends by setting a threshold*, so the driver's armed-heat backstop
    /// would otherwise drop one the route deliberately allowed.
    pub during_open_practice: bool,
}

/// A capture GridFPV has started and is still waiting on (#355) — held in memory beside the signal
/// rings, never persisted.
///
/// It exists so that a captured level becomes **GridFPV's value** (D27) rather than something read
/// back off the timer. Nobody knows the level at request time, so the record of it cannot be
/// written at accept time the way [`TimerRegistry::request_calibration`] writes a typed one. This
/// is the outstanding half of that write: when the level lands on the signal feed it is copied onto
/// [`Timer::calibration`], and when it does not, the capture is dropped and nothing is recorded —
/// because recording a level GridFPV never saw arrive is exactly the fabricated-success failure
/// this whole page exists to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingCapture {
    /// The node being captured, `0`-based.
    pub node: u32,
    /// Which threshold.
    pub threshold: CaptureThreshold,
    /// The level the timer reported when the capture started, if any — what "changed" is measured
    /// against.
    pub previous: Option<u32>,
    /// When the capture was accepted. The sampling window runs from here.
    started: Instant,
}

impl OutstandingCapture {
    /// Whether this capture has had its window **and** its settle grace and is out of time.
    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.started)
            >= Duration::from_millis(u64::from(CAPTURE_WINDOW_MS) + u64::from(CAPTURE_SETTLE_MS))
    }
}

/// How one capture ended (#355) — what [`TimerRegistry::resolve_captures`] settled.
///
/// `level: Some` is a capture that **landed**: the timer came back reporting a threshold it was not
/// reporting before, and that level is now recorded on [`Timer::calibration`] as GridFPV's own
/// (D27). `level: None` is a capture that did **not** land, and nothing was recorded — which is the
/// honest reading of RotorHazard's silence, since it refuses a capture (a node not answering, or one
/// already capturing) without emitting anything at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOutcome {
    /// The timer it ran on.
    pub timer: TimerId,
    /// The node, `0`-based.
    pub node: u32,
    /// Which threshold was being captured.
    pub threshold: CaptureThreshold,
    /// The level that came back, or `None` if none ever did.
    pub level: Option<u32>,
}

/// The lowest raw centre frequency a channel write may carry (#413) — the bottom of the 5.8 GHz
/// band the catalog lives in, matching the console's own custom-MHz guard.
///
/// **Not `0`.** RotorHazard reads `frequency: 0` as *"tune this node to nothing"* — a real command
/// that silently switches a gate off, and one no dropdown should be able to send by accident. A
/// Flexible timer tunes freely, but "freely" means any channel, not any integer.
pub const CHANNEL_MHZ_MIN: u16 = 5300;

/// The highest raw centre frequency a channel write may carry (#413) — the top of the same band.
pub const CHANNEL_MHZ_MAX: u16 = 6000;

/// The body of `POST /timers/{timer_id}/channel` (#413) — set one node's channel while tuning it.
///
/// Tuning a gate is meaningless until the node is listening on the channel it will race, so the
/// Tune page makes the frequency it already *shows* settable. The `band`/`channel` pair is the
/// **catalog entry the RD picked**; it is validated against the shared catalog server-side (D27
/// owns the vocabulary) and carried onto RotorHazard's `set_frequency`, whose handler stores both
/// on the active profile — without them RotorHazard's own UI shows a bare number with no `R7`-style
/// label, and the RD validates this work by refreshing that page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ChannelRequest {
    /// The node to tune, `0`-based — RotorHazard's `seat_index`, and the same index
    /// [`NodeSignal::node`] and [`CalibrationRequest::node`] carry.
    pub node: u32,
    /// The channel's centre frequency in raw MHz. Must be within the timer's
    /// [`channel_capability`](Timer::channel_capability) and inside
    /// [`CHANNEL_MHZ_MIN`]..=[`CHANNEL_MHZ_MAX`].
    pub mhz: u16,
    /// The catalog band the RD picked (`"Raceband"`), if any. Honoured only when the
    /// `(band, channel, mhz)` triple is a real catalog entry; otherwise the label is resolved from
    /// the catalog instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub band: Option<String>,
    /// The catalog channel label the RD picked (`"R7"`), if any. See [`band`](ChannelRequest::band).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel: Option<String>,
}

/// The answer to `POST /timers/{timer_id}/channel` (#413): **what was dispatched** — never a
/// readback, exactly like [`CalibrationDispatch`].
///
/// RotorHazard's `on_set_frequency` does not answer the caller; it re-broadcasts its frequency data
/// and, more usefully, every heartbeat carries each node's current frequency. So the console
/// confirms a channel change the same way it confirms a threshold: by seeing
/// [`NodeSignal::frequency_mhz`] come back holding what it sent on a later
/// `GET /timers/{id}/signal`.
///
/// `previous_mhz` is what the node was **last told to tune to by GridFPV**, when GridFPV had told
/// it anything — the console pairs it with its own live reading to say plainly that the node's
/// enter/exit thresholds were tuned on a different channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ChannelDispatch {
    /// The timer the write was dispatched to.
    pub timer: TimerId,
    /// The node it addresses, `0`-based.
    pub node: u32,
    /// The centre frequency that was queued, in raw MHz.
    pub mhz: u16,
    /// The resolved catalog band, or absent for a custom raw MHz the catalog does not know.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub band: Option<String>,
    /// The resolved catalog channel label, or absent for a custom raw MHz.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel: Option<String>,
    /// The channel GridFPV had this node on before this write, if it had set one. Absent the first
    /// time a node's channel is set from GridFPV.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub previous_mhz: Option<u16>,
    /// Whether GridFPV holds **enter/exit thresholds** for this node ([`Timer::calibration`]) that
    /// were set while it was on a *different* channel — i.e. whether the levels the Tune page is
    /// showing were tuned for the frequency this write just moved away from.
    ///
    /// Reported, never acted on: the thresholds are deliberately left exactly where they are (D27 —
    /// GridFPV changed one thing, so one thing changed). Recalling per-channel levels is #411.
    pub thresholds_tuned_on_another_channel: bool,
}

/// One queued channel write, as the connection reconciler drains it (#413) — the internal twin of
/// [`ChannelDispatch`], and the exact shape [`PendingCalibration`] is for a threshold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingChannel {
    /// Which timer to write to.
    pub timer: TimerId,
    /// The node, `0`-based.
    pub node: u32,
    /// The centre frequency to tune to, in raw MHz.
    pub mhz: u16,
    /// The resolved catalog band to send alongside it, if the catalog knows one.
    pub band: Option<String>,
    /// The resolved catalog channel label to send alongside it, if the catalog knows one.
    pub channel: Option<String>,
    /// Whether the route accepted this write with an **open-practice** heat racing on the timer
    /// (#355's rule, applied to #413) — so the driver's armed-heat backstop lets it through.
    pub during_open_practice: bool,
}

/// **One pending timer write** the connection reconciler drains (#457) — the single queue every
/// RD-initiated write to a live timer travels on.
///
/// # Why one queue
///
/// Restart (#386), calibration (#355), capture (#355) and channel (#413) each grew their own `Vec`
/// field, their own `take_*_requests()` drain and their own copy-pasted "no live connection →
/// warn, drop" paragraph in the reconciler. They are the *same* hand-off: a route in this crate
/// accepts a write, and the live socket that has to carry it lives in `gridfpv-app`, **above** this
/// crate, so the registry is the one seam both sides already share. Four copies of one pipe meant
/// any policy fix — #436's clear-on-reconnect, #437's not-landed-while-dialling — had to be made
/// four times, in four places that could silently diverge.
///
/// So: one enum, one queue, one drain ([`TimerRegistry::take_pending_writes`]), and one `match` in
/// the reconciler. The per-variant *differences* that are real are stated once, as
/// [`coalesces_with`](PendingTimerWrite::coalesces_with) / [`fold_into`](PendingTimerWrite::fold_into),
/// instead of being implied by which of four functions a caller happened to reach.
///
/// The payloads keep their own types ([`PendingCalibration`], [`PendingCapture`],
/// [`PendingChannel`]) — those are the shapes the emit needs, and each carries the timer it is for.
///
/// **In-memory only, never persisted.** A Director restart must not replay a previous session's
/// restart, tuning, capture or retune onto whatever timer happens to be plugged in now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingTimerWrite {
    /// **Restart the RotorHazard server** behind this timer (#386) — the guided plugin install's
    /// last step, so RH re-imports its `plugins/` directory.
    Restart {
        /// Which timer to restart.
        timer: TimerId,
    },
    /// **Set a node's enter/exit detection thresholds** (#355) — the Tune page's typed level.
    Calibrate(PendingCalibration),
    /// **Measure** one of a node's thresholds (#355) — the Tune page's Capture button.
    Capture(PendingCapture),
    /// **Set a node's channel** (#413) — the Tune page's other write.
    SetChannel(PendingChannel),
}

impl PendingTimerWrite {
    /// Which timer this write is addressed to — the only thing every variant has in common, and
    /// what the reconciler matches a live connection on.
    pub fn timer(&self) -> &TimerId {
        match self {
            PendingTimerWrite::Restart { timer } => timer,
            PendingTimerWrite::Calibrate(w) => &w.timer,
            PendingTimerWrite::Capture(w) => &w.timer,
            PendingTimerWrite::SetChannel(w) => &w.timer,
        }
    }

    /// **Queue this write**, applying its variant's coalescing policy — the one enqueue used by
    /// *both* queues a write crosses: the registry's hand-off queue
    /// ([`TimerRegistry::take_pending_writes`]) and the per-connection queue the live driver in
    /// `gridfpv-app` holds. One function, so the two can never disagree about whether a second
    /// press replaces the first (#457).
    ///
    /// A write that [supersedes](Self::coalesces_with) one already queued is
    /// [folded into](Self::fold_into) it **in place**, so the queue keeps request order — a
    /// coalesced write must not jump ahead of writes queued after the one it replaces. A write
    /// that supersedes nothing (every capture, and the first of anything) is appended.
    pub fn queue_into(self, queue: &mut Vec<PendingTimerWrite>) {
        match queue.iter_mut().find(|queued| self.coalesces_with(queued)) {
            Some(queued) => self.fold_into(queued),
            None => queue.push(self),
        }
    }

    /// Whether this write **supersedes** `queued` — the per-variant coalescing policy, in one
    /// place (#457). Each rule below is a deliberate decision, and they are deliberately not the
    /// same rule:
    ///
    /// * **Restart** coalesces per **timer**. Pressing Restart twice before a drain is one
    ///   restart; firing two would take the timing hardware down, bring it up, and take it down
    ///   again.
    /// * **Calibrate** coalesces per **(timer, node)**, last value wins per threshold (see
    ///   [`fold_into`](Self::fold_into)). A slider dragged twice before a drain must apply the
    ///   *latest* level once — replaying a stale one after a fresh one would leave the detector on
    ///   a value the page is no longer showing.
    /// * **SetChannel** coalesces per **(timer, node)**, latest pick wins, for exactly the same
    ///   reason: a dropdown changed twice before a drain retunes the node once.
    /// * **Capture NEVER coalesces.** This is the one that is easy to get wrong by symmetry. Two
    ///   writes of a *value* to one node are one intent; two captures are two **measurements** the
    ///   RD asked for, each of which needs a pass flown through the gate — collapsing them would
    ///   silently drop a pass they flew. The single case where two would genuinely collide (a
    ///   second capture of a threshold already capturing, which RotorHazard refuses in complete
    ///   silence) is refused by [`TimerRegistry::request_capture`] up front, so the queue never
    ///   has to.
    fn coalesces_with(&self, queued: &PendingTimerWrite) -> bool {
        match (self, queued) {
            (PendingTimerWrite::Restart { timer: a }, PendingTimerWrite::Restart { timer: b }) => {
                a == b
            }
            (PendingTimerWrite::Calibrate(a), PendingTimerWrite::Calibrate(b)) => {
                a.timer == b.timer && a.node == b.node
            }
            (PendingTimerWrite::SetChannel(a), PendingTimerWrite::SetChannel(b)) => {
                a.timer == b.timer && a.node == b.node
            }
            // A capture is a measurement, not a value. It never folds into anything.
            _ => false,
        }
    }

    /// Fold this write into the `queued` one it [supersedes](Self::coalesces_with). Only ever
    /// called for a pair `coalesces_with` accepted, so the non-matching arms are unreachable.
    fn fold_into(self, queued: &mut PendingTimerWrite) {
        match (self, queued) {
            // Nothing to carry: one restart is indistinguishable from another.
            (PendingTimerWrite::Restart { .. }, PendingTimerWrite::Restart { .. }) => {}
            (PendingTimerWrite::Calibrate(fresh), PendingTimerWrite::Calibrate(queued)) => {
                // Per threshold: a write that carries only an enter level must not blank the exit
                // level a previous write in the same tick set.
                if fresh.enter_at.is_some() {
                    queued.enter_at = fresh.enter_at;
                }
                if fresh.exit_at.is_some() {
                    queued.exit_at = fresh.exit_at;
                }
                // The freshest phase reading wins: a heat that has just gone racing (or just
                // stopped) must not be judged by a check made several writes ago.
                queued.during_open_practice = fresh.during_open_practice;
            }
            // Wholesale: a channel pick has no independent halves.
            (PendingTimerWrite::SetChannel(fresh), PendingTimerWrite::SetChannel(queued)) => {
                *queued = fresh;
            }
            (fresh, queued) => {
                debug_assert!(false, "fold_into called on writes that do not coalesce");
                *queued = fresh;
            }
        }
    }
}

/// The application-level registry of all configured timers (issue #73).
///
/// Maps each [`TimerId`] to its [`Timer`]. A built-in **Mock** ([`MOCK_TIMER_ID`]) is always
/// present. The set is **persisted** to `<data_dir>/timers.json` (restored on boot) so the RD's
/// timers survive a Director restart; with no data dir configured it is in-memory only. Cloning
/// shares the one registry (`Arc<RwLock<…>>`), so it is the axum router state cloned into every
/// handler, exactly like the [`EventRegistry`](crate::events::EventRegistry).
#[derive(Clone)]
pub struct TimerRegistry {
    inner: Arc<RwLock<Registry>>,
    /// Live **tune telemetry** (#355 S2a), keyed by timer — a sibling map beside the timer set,
    /// exactly like `pending_writes`, and for the same layering reason: the live socket lives in
    /// `gridfpv-app`, above this crate, so the registry is the one seam the route and the
    /// connection driver already share.
    ///
    /// Its **own** lock rather than a field inside [`Registry`]: pushes land at 5 Hz per watched
    /// timer, and they must never contend with — or worse, be tempted to ride along with — the
    /// timer set's write path, which persists `timers.json`. Nothing in here is ever written to
    /// disk, restored on boot, or turned into an [`Event`](gridfpv_events::Event); the whole map
    /// evaporates when the Director exits, which is the point.
    signal: Arc<Mutex<HashMap<TimerId, TimerSignalState>>>,
    /// **Captures in flight** (#355), keyed by timer — the RD pressed Capture and RotorHazard is
    /// sampling; GridFPV is waiting to see what level comes back.
    ///
    /// Its own lock, beside `signal` and for the same reason: nothing in here is ever written to
    /// disk or restored on boot. A capture is a three-second intent, and a Director restart must
    /// not resume one — the RD is not standing at the gate any more.
    ///
    /// **Never held across another lock.** Every method here takes this, the signal store and the
    /// timer set in separate, non-overlapping critical sections, so there is no ordering to get
    /// wrong. The cost is that a capture requested between two of those sections is simply handled
    /// on the next pass, which at 5 Hz is not a cost at all.
    captures: Arc<Mutex<HashMap<TimerId, Vec<OutstandingCapture>>>>,
}

/// The guarded interior: the timer map and where `timers.json` lives.
struct Registry {
    /// `TimerId → Timer`. A `BTreeMap` so listing is deterministic (the Mock is listed
    /// first explicitly regardless).
    timers: BTreeMap<TimerId, Timer>,
    /// Directory `timers.json` is persisted under; `None` ⇒ in-memory only (no data dir).
    data_dir: Option<PathBuf>,
    /// **Every pending write to a live timer** (#457), in request order — restarts (#386),
    /// calibration writes (#355), captures (#355) and channel writes (#413), on one queue.
    ///
    /// A hand-off queue, not state: the connection layer that owns the live sockets lives in
    /// `gridfpv-app`, *above* this crate, so a route here cannot call it. The manual connection
    /// hold solves the same layering problem with a flag ([`Timer::manual_connect`]); a write is an
    /// **edge** rather than a level, so it is a drained queue instead — the reconciler takes each
    /// write exactly once ([`TimerRegistry::take_pending_writes`]) and dispatches it onto the live
    /// connection.
    ///
    /// **Enqueued through [`Registry::queue_write`]**, which applies the per-variant coalescing
    /// policy stated once on [`PendingTimerWrite::coalesces_with`] — so "calibration coalesces per
    /// node, a capture never does" is a documented rule rather than a difference between two
    /// copy-pasted `push` sites.
    ///
    /// In-memory only, and never persisted: a Director restart must not re-fire an RD's restart,
    /// replay their tuning, or retune whatever timer happens to be plugged in now. The durable
    /// records of what GridFPV *decided* are [`Timer::calibration`] and [`Timer::node_channels`],
    /// written at accept time (D27); this is only the in-flight buffer.
    pending_writes: Vec<PendingTimerWrite>,
}

// -------------------------------------------------------------------------------------------
// Tune telemetry (#355, slice 2a) — the per-timer live signal snapshot.
// -------------------------------------------------------------------------------------------

/// How long a tune-telemetry subscription survives without being renewed.
///
/// **A lease, not a boolean.** Every `GET /timers/{id}/signal` renews it; nothing else does. A
/// closed tab, a crashed browser, a laptop that walked out of Wi-Fi range and a Director the RD
/// forgot about all stop the stream by simply not asking again — which a bare "streaming: true"
/// flag would leave running until the process died. Sized at roughly ten polls of a 4–5 Hz page,
/// so an ordinary hiccup does not tear the stream down mid-tune.
pub const SIGNAL_LEASE: Duration = Duration::from_secs(5);

/// The Director's own decimation cadence for tune telemetry — 5 Hz.
///
/// Deliberately **not** RotorHazard's. `HEARTBEAT_DATA_RATE_FACTOR` is 5 (10 Hz) on a stock timer
/// and jumps to 50 (100 Hz) the moment RH's frequency scanner is switched on, so a ring driven by
/// arrival would silently change what a "30 second window" means. The Director samples the
/// transport's last-value-wins store on this fixed schedule instead, which makes the ring's time
/// base exact by construction and caps the work per timer regardless of what the wire is doing.
pub const SIGNAL_SAMPLE_INTERVAL: Duration = Duration::from_millis(200);

/// How many samples each node's ring holds — [`SIGNAL_SAMPLE_INTERVAL`] × this is the rolling
/// window the Tune graph can draw (30 s at 5 Hz).
///
/// The ring is the *only* thing here that grows with time, and it does not: it is a fixed-capacity
/// [`VecDeque`] per node, so a Tune page left open all weekend costs exactly what it cost after the
/// first thirty seconds.
pub const SIGNAL_RING: usize = 150;

/// A snapshot is reported as **streaming** while a live connection has pushed into it this
/// recently. Comfortably longer than [`SIGNAL_SAMPLE_INTERVAL`], so ordinary jitter does not make
/// the page flicker between "live" and "stalled".
const SIGNAL_STREAMING_WITHIN: Duration = Duration::from_millis(1500);

/// One node's live signal, as a Tune page reads it (#355).
///
/// Everything is `Option` because everything is genuinely optional: a node RotorHazard has not
/// reported yet, a timer whose thresholds have not arrived, a build that omits a readout. A
/// missing value renders as "—", which is information; a zero would be a lie.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NodeSignal {
    /// The node's index on the timer, `0`-based. The **display** name is built from this plus the
    /// channel (`Node 1 · Raceband R7`) — see the repo display rule.
    pub node: u32,
    /// The stable per-node competitor handle (`node-0`, `node-1`, …) the rest of GridFPV uses for
    /// a seat. A wire handle, not a label.
    pub seat: CompetitorRef,
    /// Whether RotorHazard has reported anything at all for this node.
    ///
    /// **Unseated nodes are included, and this is why.** "Is this node even alive?" is half the
    /// diagnostic a mistuned timer needs, and filtering the snapshot to a heat's lineup would
    /// answer it with silence for exactly the nodes an RD is most likely to be checking.
    pub seen: bool,
    /// Live RSSI (filtered ADC counts) at the newest sample.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rssi: Option<f32>,
    /// The node's tuned frequency in MHz; `None` when the node is not tuned to anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub frequency_mhz: Option<u16>,
    /// The detector's loop time in microseconds — a timer falling behind shows up here first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub loop_time_micros: Option<u32>,
    /// Whether the node is inside a crossing right now.
    pub crossing: bool,
    /// Whether a crossing was seen at any point in the interval this sample covers — the sticky
    /// flag that survives the Director's decimation, so a fast pass still lights the lamp.
    pub crossed_recently: bool,
    /// The node's running peak RSSI (`node_data.node_peak_rssi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node_peak_rssi: Option<f32>,
    /// The node's running nadir RSSI (`node_data.node_nadir_rssi`) — the noise floor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node_nadir_rssi: Option<f32>,
    /// Peak RSSI of the most recent pass (`node_data.pass_peak_rssi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_peak_rssi: Option<f32>,
    /// Nadir RSSI of the most recent pass (`node_data.pass_nadir_rssi`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_nadir_rssi: Option<f32>,
    /// How many passes this node has detected (`node_data.debug_pass_count`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pass_count: Option<u32>,
    /// The enter threshold the timer is detecting against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub enter_at: Option<f32>,
    /// The exit threshold the timer is detecting against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_at: Option<f32>,
    /// The rolling RSSI window, **oldest first**, one value per entry in
    /// [`TimerSignal::sample_micros`]. At most [`SIGNAL_RING`] long, always.
    pub samples: Vec<f32>,
}

/// A timer's live tuning signal — the whole of `GET /timers/{id}/signal` (#355).
///
/// **Never an [`Event`](gridfpv_events::Event), never a log.** This is a read of a bounded
/// in-memory buffer that exists only while an RD is looking at it; it is not derived from a log,
/// it is not written to one, and it has no `SignalChunk` / `SignalHistory` in its lineage. That is
/// why it is timer-scoped and polled rather than a scoped subscription on the event change-stream:
/// it must work before an event exists at all, which is the state an untuned timer is in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerSignal {
    /// The timer this snapshot came from.
    pub timer: TimerId,
    /// Whether a live connection is actually feeding this snapshot right now. `false` with a valid
    /// lease means the timer is not connected (or has just dropped) — the difference between "no
    /// signal" and "no link", which an RD chasing a dead gate needs to be able to tell.
    pub streaming: bool,
    /// Milliseconds left on the subscription lease before the stream stops by itself. Each `GET`
    /// resets it to [`SIGNAL_LEASE`].
    pub lease_ms_remaining: u32,
    /// Microseconds between consecutive samples — the Director's own decimation cadence, not the
    /// timer's heartbeat rate.
    pub period_micros: u32,
    /// The **shared** sample time base (microseconds since this subscription started), oldest
    /// first: `sample_micros[i]` is when `nodes[*].samples[i]` was taken. One axis for every node
    /// because every node is sampled in the same pass, so this stays O(1) per tick rather than
    /// O(nodes) copies of the same numbers.
    ///
    /// Rendered as plain TS `number`s (`#[ts(as = …)]`), the same choice
    /// [`SourceTime`](gridfpv_events::SourceTime) makes and for the same reason: a rolling window's
    /// microsecond offsets sit far below 2^53, so `number` is exact — and a `bigint` here would be
    /// a needless conversion between this axis and the one every other trace on screen uses.
    #[ts(as = "Vec<f64>")]
    pub sample_micros: Vec<i64>,
    /// Every node the timer reports, **including unseated ones** (see [`NodeSignal::seen`]), in
    /// node order.
    pub nodes: Vec<NodeSignal>,
}

/// One node's latest readings as the connection layer hands them over — the crate-boundary twin of
/// the adapter's `NodeTick`.
///
/// It exists because the live socket lives in `gridfpv-app`, *above* this crate, so the registry
/// cannot name the adapter's type. Restating the fields here keeps the dependency arrow pointing
/// the right way; the app crate does the (trivial) mapping.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeReading {
    /// Whether the timer has reported this node at all.
    pub seen: bool,
    /// Live RSSI.
    pub rssi: Option<f32>,
    /// Tuned frequency in MHz, `None` when untuned.
    pub frequency_mhz: Option<u16>,
    /// Detector loop time in microseconds.
    pub loop_time_micros: Option<u32>,
    /// Crossing state as of the newest frame.
    pub crossing: bool,
    /// Any crossing seen since the previous reading was taken.
    pub crossed: bool,
    /// Running peak RSSI.
    pub node_peak_rssi: Option<f32>,
    /// Running nadir RSSI.
    pub node_nadir_rssi: Option<f32>,
    /// Most recent pass's peak RSSI.
    pub pass_peak_rssi: Option<f32>,
    /// Most recent pass's nadir RSSI.
    pub pass_nadir_rssi: Option<f32>,
    /// Passes detected.
    pub pass_count: Option<u32>,
    /// Enter threshold.
    pub enter_at: Option<f32>,
    /// Exit threshold.
    pub exit_at: Option<f32>,
}

/// The live tune-telemetry state for **one** timer: the lease, the shared time base and the
/// per-node rings.
///
/// Held in a map beside the timer set — never inside [`Timer`] — because `Timer`'s JSON *is* both
/// its wire form and its persisted form: a sample ring on it would be written to `timers.json` on
/// every CRUD, and read back on boot as configuration, which it emphatically is not.
struct TimerSignalState {
    /// When the subscription lapses unless renewed.
    lease_until: Instant,
    /// The origin the shared sample times are measured from (this subscription's start).
    origin: Instant,
    /// When a live connection last pushed — drives [`TimerSignal::streaming`].
    last_push: Option<Instant>,
    /// The shared sample time base, bounded to [`SIGNAL_RING`].
    times: VecDeque<i64>,
    /// Per-node latest readings + rolling window, bounded to [`SIGNAL_RING`] each.
    nodes: Vec<NodeRing>,
}

/// One node's latest reading plus its bounded rolling window.
#[derive(Default)]
struct NodeRing {
    /// The newest reading, last-value-wins.
    latest: NodeReading,
    /// The rolling RSSI window, oldest first, at most [`SIGNAL_RING`] long.
    samples: VecDeque<f32>,
}

impl TimerSignalState {
    /// A fresh subscription: leased from `now`, no samples yet.
    fn new(now: Instant) -> Self {
        Self {
            lease_until: now + SIGNAL_LEASE,
            origin: now,
            last_push: None,
            times: VecDeque::with_capacity(SIGNAL_RING),
            nodes: Vec::new(),
        }
    }

    /// Whether the lease is still open at `now`.
    fn leased(&self, now: Instant) -> bool {
        now < self.lease_until
    }

    /// Append one decimated tick. `readings` is every node the timer reports, in node order.
    ///
    /// A **width change** (the timer came back with a different node count) resets the rings: a
    /// window whose columns silently shifted under it is worse than a window that restarts.
    fn push(&mut self, readings: &[NodeReading], now: Instant) {
        if self.nodes.len() != readings.len() {
            self.nodes = (0..readings.len()).map(|_| NodeRing::default()).collect();
            self.times.clear();
        }
        self.last_push = Some(now);
        if self.times.len() == SIGNAL_RING {
            self.times.pop_front();
        }
        self.times
            .push_back(now.duration_since(self.origin).as_micros() as i64);
        for (ring, reading) in self.nodes.iter_mut().zip(readings) {
            ring.latest = reading.clone();
            if ring.samples.len() == SIGNAL_RING {
                ring.samples.pop_front();
            }
            ring.samples.push_back(reading.rssi.unwrap_or(0.0));
        }
    }

    /// Render the snapshot for `timer` at `now`.
    fn snapshot(&self, timer: &TimerId, now: Instant) -> TimerSignal {
        TimerSignal {
            timer: timer.clone(),
            streaming: self
                .last_push
                .is_some_and(|at| now.duration_since(at) < SIGNAL_STREAMING_WITHIN),
            lease_ms_remaining: self
                .lease_until
                .saturating_duration_since(now)
                .as_millis()
                .min(u32::MAX as u128) as u32,
            period_micros: SIGNAL_SAMPLE_INTERVAL.as_micros() as u32,
            sample_micros: self.times.iter().copied().collect(),
            nodes: self
                .nodes
                .iter()
                .enumerate()
                .map(|(index, ring)| NodeSignal {
                    node: index as u32,
                    seat: CompetitorRef(format!("node-{index}")),
                    seen: ring.latest.seen,
                    rssi: ring.latest.rssi,
                    frequency_mhz: ring.latest.frequency_mhz,
                    loop_time_micros: ring.latest.loop_time_micros,
                    crossing: ring.latest.crossing,
                    crossed_recently: ring.latest.crossed,
                    node_peak_rssi: ring.latest.node_peak_rssi,
                    node_nadir_rssi: ring.latest.node_nadir_rssi,
                    pass_peak_rssi: ring.latest.pass_peak_rssi,
                    pass_nadir_rssi: ring.latest.pass_nadir_rssi,
                    pass_count: ring.latest.pass_count,
                    enter_at: ring.latest.enter_at,
                    exit_at: ring.latest.exit_at,
                    samples: ring.samples.iter().copied().collect(),
                })
                .collect(),
        }
    }
}

impl TimerRegistry {
    /// Build a registry seeded with the built-in Mock, persisting to `data_dir` when given.
    ///
    /// The Mock's `laps`/`lap_ms` default from `sim_laps`/`sim_lap_ms` (the Director passes
    /// the env defaults). When `data_dir` is `Some` and a `timers.json` already exists, the saved
    /// timers are restored over the top (an unreadable/corrupt file degrades to just the
    /// Mock rather than failing to boot); a restored Mock's config wins so a retune
    /// survives a restart. When `data_dir` is `None` the registry is in-memory only.
    pub fn new(
        data_dir: Option<PathBuf>,
        sim_laps: u32,
        sim_lap_ms: u64,
    ) -> Result<Self, TimerError> {
        let mut timers = BTreeMap::new();

        // Always seed the built-in Mock first. Sensible channel defaults (race redesign Slice 4a):
        // flexible, 8 nodes, seeded from Raceband R1–R8 — so an out-of-the-box sim race has an
        // 8-channel pool and an 8-seat cap, matching a typical real timer.
        let sim = Timer {
            id: TimerId(MOCK_TIMER_ID.to_string()),
            name: MOCK_TIMER_NAME.to_string(),
            kind: TimerKind::Mock {
                laps: sim_laps,
                lap_ms: sim_lap_ms,
            },
            status: TimerStatus::Ready,
            channel_capability: ChannelCapability::Flexible,
            // The Mock has no hardware to ask, so its width is pinned rather than discovered.
            node_count: Some(DEFAULT_NODE_COUNT),
            reported_nodes: None,
            disabled_nodes: Vec::new(),
            available_channels: crate::channels::RACEBAND_MHZ.to_vec(),
            plugin: None,
            manual_connect: false,
            calibration: Vec::new(),
            node_channels: Vec::new(),
        };
        timers.insert(sim.id.clone(), sim);

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                TimerError(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            // Restore persisted timers over the seed (a missing/corrupt file is ignored — the
            // Director still boots with at least the Mock).
            if let Some(restored) = read_persisted_timers(dir) {
                for mut timer in restored {
                    // Keep the derived status authoritative (never trust a persisted status), and
                    // reset the live plugin-presence — it is re-probed on connect, never restored.
                    // The manual-connection hold (#383) is live too: a restart comes back holding
                    // nothing, so booting never silently dials a timer the RD last poked at.
                    timer.status = Timer::status_for(&timer.kind);
                    timer.plugin = None;
                    timer.manual_connect = false;
                    // The reported node count is an observation about the hardware (#412, D27), so
                    // it is re-read on the next connect rather than restored. The RD's *decisions*
                    // — `node_count` and `disabled_nodes` — come back exactly as they were: a
                    // disabled node survives a restart as surely as it survives a reconnect.
                    timer.reported_nodes = None;
                    timer.disabled_nodes.sort_unstable();
                    timer.disabled_nodes.dedup();
                    timers.insert(timer.id.clone(), timer);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Registry {
                timers,
                data_dir,
                pending_writes: Vec::new(),
            })),
            signal: Arc::new(Mutex::new(HashMap::new())),
            captures: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// Every timer, **Mock first**, then the rest in id order — the `GET /timers` body.
    pub fn list(&self) -> Vec<Timer> {
        let reg = self.read();
        let mut out = Vec::with_capacity(reg.timers.len());
        let sim = TimerId(MOCK_TIMER_ID.to_string());
        if let Some(s) = reg.timers.get(&sim) {
            out.push(s.clone());
        }
        for (id, timer) in &reg.timers {
            if *id != sim {
                out.push(timer.clone());
            }
        }
        out
    }

    /// Whether a timer with `id` exists — the per-event selection validates each id through this.
    pub fn exists(&self, id: &TimerId) -> bool {
        self.read().timers.contains_key(id)
    }

    /// The [`Timer`] for `id`, or `None` — the source bridge resolves a selected id's config here.
    pub fn get(&self, id: &TimerId) -> Option<Timer> {
        self.read().timers.get(id).cloned()
    }

    /// Create a timer from a [`CreateTimerRequest`], returning it (issue #73).
    ///
    /// The **id is auto-generated** — a slug of the `name` + a short random suffix — so it is
    /// unique and never the reserved `sim`. The derived [`TimerStatus`] is set from the kind, and
    /// the registry is **persisted** on success.
    pub fn create(&self, request: &CreateTimerRequest) -> Result<Timer, TimerError> {
        let mut reg = self.write();
        let id = loop {
            let candidate = TimerId(format!("{}-{}", slugify(&request.name), short_suffix()));
            if candidate.0 != MOCK_TIMER_ID && !reg.timers.contains_key(&candidate) {
                break candidate;
            }
        };
        let timer = Timer {
            id: id.clone(),
            name: request.name.trim().to_string(),
            status: Timer::status_for(&request.kind),
            kind: request.kind.clone(),
            channel_capability: request.channel_capability.clone().unwrap_or_default(),
            // No override unless the caller asked for one (#412): a new timer follows the hardware.
            node_count: request.node_count,
            reported_nodes: None,
            disabled_nodes: Vec::new(),
            available_channels: request.available_channels.clone().unwrap_or_default(),
            plugin: None,
            manual_connect: false,
            calibration: Vec::new(),
            node_channels: Vec::new(),
        };
        reg.timers.insert(id, timer.clone());
        reg.persist()?;
        Ok(timer)
    }

    /// Edit a timer's name and/or kind (issue #73), returning the updated [`Timer`].
    ///
    /// The built-in Mock may be retuned (e.g. a new `lap_ms`) but not renamed away — any
    /// timer's name/kind is editable. An unknown id is a [`TimerError`]. The registry is
    /// **persisted** on success.
    pub fn update(&self, id: &TimerId, request: &UpdateTimerRequest) -> Result<Timer, TimerError> {
        let mut reg = self.write();
        let timer = reg
            .timers
            .get_mut(id)
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if let Some(name) = &request.name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                timer.name = trimmed.to_string();
            }
        }
        if let Some(kind) = &request.kind {
            // Only a **real** kind/config change resets the live state (#382). A reconfigured timer
            // (new URL/kind) must be re-probed — the reconciler notices the change and supersedes +
            // reopens the connection, which republishes `Connecting → Connected` and re-runs the
            // plugin probe. A *no-op* edit (the same kind resubmitted, e.g. a rename PUT that
            // echoes the kind back) changes nothing for the reconciler, so nothing would ever
            // republish: wiping here would strand a live `Connected`+`Present` timer at the resting
            // `Configured` with no plugin, permanently, until a restart.
            if timer.kind != *kind {
                timer.kind = kind.clone();
                timer.status = Timer::status_for(kind);
                timer.plugin = None;
            }
        }
        if let Some(capability) = &request.channel_capability {
            timer.channel_capability = capability.clone();
        }
        if let Some(node_count) = request.node_count {
            timer.node_count = Some(node_count);
        }
        if let Some(available) = &request.available_channels {
            timer.available_channels = available.clone();
        }
        let updated = timer.clone();
        reg.persist()?;
        Ok(updated)
    }

    /// Set a timer's **live connection status** (issues #73, #65) — the Director drives an RH
    /// timer's [`TimerStatus`] as its connection comes and goes (connecting → connected →
    /// disconnected/error). A no-op for an unknown id. This is an **in-memory only** update: the
    /// dynamic states are not persisted (a `persist` always re-derives the resting status from the
    /// kind), so a restart starts a configured RH timer back at [`Configured`](TimerStatus::Configured).
    pub fn set_status(&self, id: &TimerId, status: TimerStatus) {
        let mut reg = self.write();
        if let Some(timer) = reg.timers.get_mut(id) {
            timer.status = status;
        }
    }

    /// Set a timer's **live GridFPV-plugin presence** (D16, S1) — the Director drives this from the
    /// connect-time `gridfpv_hello` handshake (present/compatible, missing, or incompatible). A
    /// no-op for an unknown id. **In-memory only**, like [`set_status`](Self::set_status): it is
    /// re-probed on every (re)connect and never persisted.
    pub fn set_plugin(&self, id: &TimerId, plugin: PluginPresence) {
        let mut reg = self.write();
        if let Some(timer) = reg.timers.get_mut(id) {
            timer.plugin = Some(plugin);
        }
    }

    /// Record how many nodes a timer **reported** (#412) — the Director drives this from the
    /// connect-time discovery (`frequency_data.fdata`, falling back to
    /// `enter_and_exit_at_levels`). A no-op for an unknown id.
    ///
    /// **An observation, never config.** Like [`set_plugin`](Self::set_plugin) it is in-memory only
    /// and re-read on every (re)connect: it is not persisted, and it does **not** touch
    /// [`Timer::node_count`] or [`Timer::disabled_nodes`]. A timer that comes back reporting a
    /// different width shows up as [`Timer::node_drift`] for the RD to act on — D27's rule that
    /// drift is surfaced and never silently adopted. That is also what makes a disabled node
    /// survive a reconnect: nothing on this path can re-enable one.
    pub fn set_reported_nodes(&self, id: &TimerId, reported: u32) {
        let mut reg = self.write();
        if let Some(timer) = reg.timers.get_mut(id) {
            timer.reported_nodes = Some(reported);
        }
    }

    /// **Set a timer's node configuration** (#412) — the width override and/or the enabled node
    /// set — returning the resulting [`TimerNodes`] view. `PUT /timers/{id}/nodes`.
    ///
    /// This is the RD's *decision* half of the model, so it is **persisted**: a node disabled here
    /// stays disabled across a reconnect, a Director restart, and a timer that keeps insisting it
    /// has four working nodes.
    ///
    /// `enabled` is the set to keep on (0-based); every other index below the effective width is
    /// disabled. Indices at or beyond the width are ignored rather than rejected — a console racing
    /// a width change should not lose the edit — and the stored disabled set keeps any index the RD
    /// turned off earlier, so a timer that comes back *wider* does not silently un-disable a node.
    ///
    /// Refused (a [`TimerError`], reported as a `400`) for an unknown id, and for an edit that would
    /// leave the timer with **no enabled node at all** — that caps every heat to no pilots, which is
    /// the same refusal [`validate_timer_config`] makes for a zero `node_count`.
    pub fn set_nodes(
        &self,
        id: &TimerId,
        request: &SetTimerNodesRequest,
    ) -> Result<TimerNodes, TimerError> {
        let mut reg = self.write();
        let timer = reg
            .timers
            .get_mut(id)
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if let Some(node_count) = request.node_count {
            if node_count == Some(0) {
                return Err(TimerError(
                    "node_count must be at least 1 (a 0-node timer caps every heat to no pilots)"
                        .to_string(),
                ));
            }
            timer.node_count = node_count;
        }
        if let Some(enabled) = &request.enabled {
            let width = timer.node_width();
            // Only indices that exist are turned off here; anything the RD already disabled above
            // the current width is preserved, so a timer that widens does not resurrect a dead node.
            let mut disabled: Vec<u32> = timer
                .disabled_nodes
                .iter()
                .copied()
                .filter(|node| *node >= width)
                .collect();
            disabled.extend((0..width).filter(|node| !enabled.contains(node)));
            disabled.sort_unstable();
            disabled.dedup();
            if (0..width).all(|node| disabled.contains(&node)) {
                return Err(TimerError(
                    "at least one node must stay enabled (a timer with none caps every heat to no pilots)"
                        .to_string(),
                ));
            }
            timer.disabled_nodes = disabled;
        }
        let view = timer.node_view();
        reg.persist()?;
        Ok(view)
    }

    /// The [`TimerNodes`] view for `id` (#412) — `GET /timers/{id}/nodes`, and the one shared
    /// answer to "which seats does this timer have, and which of them may a heat use?".
    pub fn nodes(&self, id: &TimerId) -> Option<TimerNodes> {
        self.read().timers.get(id).map(Timer::node_view)
    }

    /// Set (or clear) a timer's **manual connection hold** (issue #383), returning the updated
    /// [`Timer`].
    ///
    /// The Timers menu's Connect / Disconnect: it asks the connection reconciler to hold a live
    /// link to this RotorHazard timer **independent of any event**, so "is this timer reachable,
    /// and does it have the GridFPV plugin?" can be answered where the timer is configured — before
    /// any event exists. The reconciler unions the held timers with the active event's selection on
    /// its next tick, and the timer then publishes the same [`TimerStatus`] / [`PluginPresence`] the
    /// event-driven path does.
    ///
    /// Only a [`Rotorhazard`](TimerKind::Rotorhazard) timer can be held — a Mock has nothing to
    /// dial (that is a [`TimerError`], which the route reports as a `400`), as is an unknown id.
    /// The hold is **in-memory only**, like [`set_status`](Self::set_status): nothing is persisted,
    /// so it does not survive a restart and does not dirty `timers.json`.
    pub fn set_manual_connect(&self, id: &TimerId, held: bool) -> Result<Timer, TimerError> {
        let mut reg = self.write();
        let timer = reg
            .timers
            .get_mut(id)
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if held && !matches!(timer.kind, TimerKind::Rotorhazard { .. }) {
            return Err(TimerError(format!(
                "{:?} is not a RotorHazard timer — there is nothing to connect to",
                timer.name
            )));
        }
        timer.manual_connect = held;
        Ok(timer.clone())
    }

    /// The RotorHazard timers the RD is **manually holding a connection to** (issue #383) — the
    /// connection reconciler's second input, unioned with the active event's selection.
    ///
    /// Filtered to `Rotorhazard` kinds: a hold set before the timer's kind was edited to a Mock
    /// goes dormant rather than asking the reconciler to dial something that cannot be dialled.
    pub fn manual_connections(&self) -> Vec<TimerId> {
        self.read()
            .timers
            .values()
            .filter(|t| t.manual_connect && matches!(t.kind, TimerKind::Rotorhazard { .. }))
            .map(|t| t.id.clone())
            .collect()
    }

    /// **Request a RotorHazard restart** for `id` (issue #386), returning the [`Timer`] unchanged.
    ///
    /// The guided plugin install's last step: RotorHazard imports plugins **once at startup**, so a
    /// freshly-dropped-in `plugins/gridfpv/` stays inert until RH re-executes. Rather than sending
    /// the RD off to RotorHazard's own web UI, the Director emits RH's unauthenticated
    /// `restart_server` on the socket it is already holding.
    ///
    /// This only **parks the request**: the sockets live in `gridfpv-app`, above this crate, so the
    /// connection reconciler drains the queue on its next tick
    /// ([`take_pending_writes`](Self::take_pending_writes)) and fires the emit. The queue is
    /// in-memory and never persisted.
    ///
    /// Refused (a [`TimerError`], which the route reports as a `400`) for an unknown id, for a
    /// non-RotorHazard timer (a Mock has no server to restart), and for a timer that is **not
    /// connected** — there is no socket to emit on, and a request is deliberately not held over for
    /// a future connection. Requests **coalesce**: asking twice before the reconciler drains queues
    /// one restart, not two.
    ///
    /// The **race-phase refusal** — a restart must never land on a running or armed heat — is not
    /// here: it needs the event log, so it lives in the route
    /// (`EventRegistry::heat_in_progress_on_timer`). This layer knows only about the timer.
    pub fn request_restart(&self, id: &TimerId) -> Result<Timer, TimerError> {
        let mut reg = self.write();
        let timer = reg
            .timers
            .get(id)
            .cloned()
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if !matches!(timer.kind, TimerKind::Rotorhazard { .. }) {
            return Err(TimerError(format!(
                "{:?} is not a RotorHazard timer — there is no timing server to restart",
                timer.name
            )));
        }
        if timer.status != TimerStatus::Connected {
            return Err(TimerError(format!(
                "{:?} is not connected — connect it before restarting it",
                timer.name
            )));
        }
        reg.queue_write(PendingTimerWrite::Restart { timer: id.clone() });
        Ok(timer)
    }

    /// Take every pending timer write (#457), leaving the queue empty — the connection
    /// reconciler's one drain, for restarts (#386), calibration writes (#355), captures (#355) and
    /// channel writes (#413) alike.
    ///
    /// Each write is handed out **exactly once**: if no live connection is found for it the write
    /// is dropped (and logged), never re-queued for a later connection. That is a policy, not an
    /// omission, and it is the same one for every variant — a restart, a threshold, a capture or a
    /// retune arriving minutes later on a reconnect would each act on hardware nobody asked to
    /// touch, on a timer that may not even be the same physical box. The RD sees it as a value
    /// that never comes back confirmed; the durable record of what GridFPV decided
    /// ([`Timer::calibration`], [`Timer::node_channels`]) is unaffected.
    pub fn take_pending_writes(&self) -> Vec<PendingTimerWrite> {
        std::mem::take(&mut self.write().pending_writes)
    }

    /// **Set a node's enter/exit detection thresholds** on `id` (#355), returning what was
    /// dispatched.
    ///
    /// The write half of the Tune page. The RD moves a slider, releases it, and the value goes to
    /// the timer — there is no Apply button, so this runs per adjustment rather than once per
    /// session.
    ///
    /// # D27: this is GridFPV's value, applied to the timer
    ///
    /// *"GridFPV owns every config and every record; a timer is controlled, never consulted."* The
    /// accepted (clamped) thresholds are recorded on [`Timer::calibration`] and **persisted** here,
    /// at accept time — that record is the system of record. What RotorHazard later reports on
    /// `GET /timers/{id}/signal` is evidence about the timer, and is never adopted as truth.
    ///
    /// ⚠️ **The re-apply half of D27 is not built.** A timer that comes back holding different
    /// levels (the RD tuned in RH's own UI, a profile switch, a restore) is not pushed back to
    /// GridFPV's values on reconnect, because doing that silently would overwrite deliberate
    /// RH-side work with no way for the RD to see it happen — D27 asks for a **drift notice**, and
    /// there is no surface for one yet. Until then `Timer::calibration` is a faithful record of
    /// what GridFPV set, and rebuilding a wiped timer from it is a manual re-tune.
    ///
    /// # Clamping
    ///
    /// Each supplied level is clamped to [`RSSI_MIN`]..=[`RSSI_MAX`] — **not** validated and
    /// rejected. The console clamps at its own state already, so anything out of range here is a
    /// bug or a hand-rolled client, and the dangerous value is `0`: RotorHazard reads it as falsy
    /// and re-reads the level off the node instead of setting it, which looks exactly like success.
    /// The returned [`CalibrationDispatch`] carries the clamped values, so a caller can see what
    /// actually went out.
    ///
    /// # Refused
    ///
    /// A [`TimerError`] (which the route reports as a `400`) for an unknown id, a non-RotorHazard
    /// timer (a Mock has no radio to calibrate), a timer that is **not connected** (there is no
    /// socket to emit on, and a threshold is not held over for a future connection), a `node`
    /// beyond the timer's width, and a request carrying **neither** threshold.
    ///
    /// The **race-phase refusal** — never move a detection threshold under a *scored* race — is not
    /// here: it needs the event log, so it lives in the route
    /// (`EventRegistry::scored_heat_in_progress_on_timer`), as the restart's does in
    /// [`request_restart`](Self::request_restart). `during_open_practice` is that route's answer to
    /// the *other* half of the question — whether an (exempt) practice heat is racing right now —
    /// carried through to the driver so its own armed-heat backstop does not drop a write the route
    /// deliberately allowed.
    pub fn request_calibration(
        &self,
        id: &TimerId,
        request: &CalibrationRequest,
        during_open_practice: bool,
    ) -> Result<CalibrationDispatch, TimerError> {
        let enter_at = request.enter_at.map(clamp_level);
        let exit_at = request.exit_at.map(clamp_level);
        if enter_at.is_none() && exit_at.is_none() {
            return Err(TimerError(
                "no threshold given — a calibration write must carry an enter or an exit level"
                    .to_string(),
            ));
        }

        let mut reg = self.write();
        let timer = reg
            .timers
            .get_mut(id)
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if !matches!(timer.kind, TimerKind::Rotorhazard { .. }) {
            return Err(TimerError(format!(
                "{:?} is not a RotorHazard timer — there is no detector to calibrate",
                timer.name
            )));
        }
        if timer.status != TimerStatus::Connected {
            return Err(TimerError(format!(
                "{:?} is not connected — connect it before setting its thresholds",
                timer.name
            )));
        }
        // The node must exist AND be enabled (#412). Tuning a node the RD has switched off is
        // pointless at best and misleading at worst: the threshold would be applied to hardware no
        // heat is ever seated on, and the page would show a confirmed write on a dead gate.
        if request.node >= timer.node_width() {
            return Err(TimerError(format!(
                "{:?} has {} nodes — there is no {} to calibrate",
                timer.name,
                timer.node_width(),
                // Display the node the way the page labels it (1-based), per the repo display rule.
                Timer::node_label(request.node)
            )));
        }
        if !timer.node_enabled(request.node) {
            return Err(TimerError(format!(
                "{} is disabled on {:?} — enable it before setting its thresholds",
                Timer::node_label(request.node),
                timer.name
            )));
        }

        // D27: record GridFPV's value first — the store, not the timer, is where this lives.
        match timer
            .calibration
            .iter_mut()
            .find(|c| c.node == request.node)
        {
            Some(existing) => {
                if enter_at.is_some() {
                    existing.enter_at = enter_at;
                }
                if exit_at.is_some() {
                    existing.exit_at = exit_at;
                }
            }
            None => {
                timer.calibration.push(NodeCalibration {
                    node: request.node,
                    enter_at,
                    exit_at,
                });
                timer.calibration.sort_by_key(|c| c.node);
            }
        }

        // Then queue the *application* of it (#457, one queue). Coalesced per (timer, node) by
        // `PendingTimerWrite`'s own policy: a drag that lands twice before a drain applies the
        // latest value once, rather than replaying a stale one after it.
        reg.queue_write(PendingTimerWrite::Calibrate(PendingCalibration {
            timer: id.clone(),
            node: request.node,
            enter_at,
            exit_at,
            during_open_practice,
        }));
        reg.persist()?;

        Ok(CalibrationDispatch {
            timer: id.clone(),
            node: request.node,
            enter_at,
            exit_at,
        })
    }

    /// **Start a capture** on one node's threshold (#355), returning what was dispatched.
    ///
    /// The Tune page's third write, and the only one that does not carry a number. The RD presses
    /// Capture, RotorHazard samples this node for [`CAPTURE_WINDOW_MS`] and sets the threshold from
    /// what it saw — so this is GridFPV asking the timer to *measure* a level rather than telling it
    /// one.
    ///
    /// # Why this exists at all
    ///
    /// A fresh RD with no saved profile (#411) and a badly-tuned timer has no starting point, and
    /// GridFPV deliberately ships no fabricated default: the right threshold depends on craft, VTX
    /// power, antenna and gate geometry, and a made-up number would also *change the hardware on
    /// first connect*, which is the surprise D27's drift rule exists to prevent. A capture measures
    /// the RD's actual craft on their actual gate. It is the only non-guessing bootstrap there is.
    ///
    /// # D27: the captured level becomes GridFPV's value — but only once it exists
    ///
    /// [`request_calibration`](Self::request_calibration) records its level on
    /// [`Timer::calibration`] at accept time, because it *has* one. A capture does not: the value
    /// is three seconds in the future and nobody, GridFPV included, knows what it will be. So the
    /// record is deferred to [`resolve_captures`](Self::resolve_captures), which writes it the
    /// moment the level is actually observed — and writes **nothing** if it never is. Recording a
    /// level GridFPV never saw arrive would be the fabricated success this page exists to remove.
    ///
    /// # Refused
    ///
    /// Everything [`request_calibration`](Self::request_calibration) refuses — an unknown id, a
    /// non-RotorHazard timer, a timer that is not connected, a `node` beyond the timer's width, and
    /// a node the RD has **disabled** (#412) — plus one more:
    ///
    /// * **A capture of this threshold is already running on this node.** RotorHazard's
    ///   `start_capture_enter_at_level` returns `False` in exactly that case and emits nothing at
    ///   all, so a second press would be accepted here, ignored there, and shown as started. That is
    ///   a fourth silent write (#423), and it is refused instead.
    ///
    /// As with the calibration write, the **race-phase refusal** is not here — it needs the event
    /// log, so it lives in the route — and `during_open_practice` is that route's answer carried
    /// through to the driver's own backstop.
    pub fn request_capture(
        &self,
        id: &TimerId,
        request: &CaptureRequest,
        during_open_practice: bool,
    ) -> Result<CaptureDispatch, TimerError> {
        // 1. Validate against the timer set. Read-only, and the guard is dropped before anything
        //    else is locked — see `captures`' note: no lock here is ever held across another.
        {
            let reg = self.read();
            let timer = reg
                .timers
                .get(id)
                .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
            if !matches!(timer.kind, TimerKind::Rotorhazard { .. }) {
                return Err(TimerError(format!(
                    "{:?} is not a RotorHazard timer — there is no detector to capture from",
                    timer.name
                )));
            }
            if timer.status != TimerStatus::Connected {
                return Err(TimerError(format!(
                    "{:?} is not connected — connect it before capturing a level",
                    timer.name
                )));
            }
            if request.node >= timer.node_width() {
                return Err(TimerError(format!(
                    "{:?} has {} nodes — there is no {} to capture on",
                    timer.name,
                    timer.node_width(),
                    Timer::node_label(request.node)
                )));
            }
            if !timer.node_enabled(request.node) {
                return Err(TimerError(format!(
                    "{} is disabled on {:?} — enable it before capturing a level",
                    Timer::node_label(request.node),
                    timer.name
                )));
            }
        }

        // 2. What the timer is reporting for this threshold right now. Read from the signal feed —
        //    evidence about the timer, used only to tell "a new level arrived" from "nothing
        //    happened", never adopted as GridFPV's value.
        let previous = self.reported_level(id, request.node, request.threshold);

        // 3. Claim the capture. Check-and-insert under one lock so two presses a millisecond apart
        //    cannot both start a capture RotorHazard would refuse the second of.
        let now = Instant::now();
        {
            let mut captures = self.capture_store();
            let outstanding = captures.entry(id.clone()).or_default();
            // Drop anything that has already run out of time — the resolver prunes these too, but a
            // press arriving between two resolver passes must not be refused by a ghost.
            outstanding.retain(|c| !c.expired(now));
            if outstanding
                .iter()
                .any(|c| c.node == request.node && c.threshold == request.threshold)
            {
                return Err(TimerError(format!(
                    "{} is already capturing its {} level — wait for that capture to finish",
                    Timer::node_label(request.node),
                    request.threshold.label()
                )));
            }
            outstanding.push(OutstandingCapture {
                node: request.node,
                threshold: request.threshold,
                previous,
                started: now,
            });
        }

        // 4. Queue the emit (#457, one queue). **Never coalesced** — see
        //    `PendingTimerWrite::coalesces_with`, where that is stated as the policy it is: a
        //    second capture is a second *measurement*, not a restatement of a value, and step 3
        //    has already refused the one case where two would collide.
        {
            let mut reg = self.write();
            reg.queue_write(PendingTimerWrite::Capture(PendingCapture {
                timer: id.clone(),
                node: request.node,
                threshold: request.threshold,
                during_open_practice,
            }));
        }

        Ok(CaptureDispatch {
            timer: id.clone(),
            node: request.node,
            threshold: request.threshold,
            window_ms: CAPTURE_WINDOW_MS,
            settle_ms: CAPTURE_SETTLE_MS,
            previous,
        })
    }

    /// Whether a capture is running on `id` right now (#355) — read by the driver so it knows to
    /// fire the post-window threshold readback.
    pub fn capture_in_flight(&self, id: &TimerId) -> bool {
        let now = Instant::now();
        self.capture_store()
            .get(id)
            .is_some_and(|list| list.iter().any(|c| !c.expired(now)))
    }

    /// **Settle every capture that has run its course** (#355) — called on the reconciler's tick.
    ///
    /// This is the half of a capture that makes it a *GridFPV* value rather than an observation.
    /// For each outstanding capture whose sampling window has closed:
    ///
    /// * the timer is reporting a level, and it **differs** from what it was reporting when the
    ///   capture started ⇒ the capture landed. The level is recorded on [`Timer::calibration`]
    ///   (D27 — written the same way a typed one is, and persisted), and the capture is done.
    /// * the settle window has also elapsed and the level has **not** changed ⇒ the capture did not
    ///   land. Nothing is recorded, and the capture is dropped. RotorHazard refuses a capture
    ///   silently — a node that is not answering, or one already capturing — so "no new level" is
    ///   the only evidence of that refusal there is, and inventing a success here would be the
    ///   fourth silently-ignored write (#423).
    ///
    /// A capture is deliberately **not** resolved before its window closes, even if a level changes
    /// during it: a threshold that moved in those three seconds moved for some other reason, and
    /// crediting it to the capture would report a number RotorHazard had not measured yet.
    ///
    /// Returns what was settled, so the caller can log it. Takes each of its three locks in a
    /// separate critical section (see [`TimerRegistry::captures`]).
    pub fn resolve_captures(&self) -> Vec<CaptureOutcome> {
        let now = Instant::now();

        // 1. What is outstanding, per timer. Cloned out; the lock is not held past this block.
        let pending: Vec<(TimerId, OutstandingCapture)> = {
            let captures = self.capture_store();
            captures
                .iter()
                .flat_map(|(timer, list)| list.iter().map(|c| (timer.clone(), c.clone())))
                .collect()
        };
        if pending.is_empty() {
            return Vec::new();
        }

        // 2. Decide each one against the level the timer is reporting now.
        let mut settled: Vec<CaptureOutcome> = Vec::new();
        for (timer, capture) in pending {
            if now.duration_since(capture.started)
                < Duration::from_millis(u64::from(CAPTURE_WINDOW_MS))
            {
                continue; // still sampling — the RD is mid-pass.
            }
            let reported = self.reported_level(&timer, capture.node, capture.threshold);
            match reported {
                Some(level) if Some(level) != capture.previous => settled.push(CaptureOutcome {
                    timer,
                    node: capture.node,
                    threshold: capture.threshold,
                    level: Some(level),
                }),
                _ if capture.expired(now) => settled.push(CaptureOutcome {
                    timer,
                    node: capture.node,
                    threshold: capture.threshold,
                    level: None,
                }),
                _ => {}
            }
        }
        if settled.is_empty() {
            return Vec::new();
        }

        // 3. Record the ones that landed (D27), then persist once for the whole batch.
        let landed: Vec<&CaptureOutcome> = settled.iter().filter(|o| o.level.is_some()).collect();
        if !landed.is_empty() {
            let mut reg = self.write();
            for outcome in &landed {
                let Some(timer) = reg.timers.get_mut(&outcome.timer) else {
                    continue;
                };
                let level = outcome.level;
                match timer
                    .calibration
                    .iter_mut()
                    .find(|c| c.node == outcome.node)
                {
                    Some(existing) => match outcome.threshold {
                        CaptureThreshold::Enter => existing.enter_at = level,
                        CaptureThreshold::Exit => existing.exit_at = level,
                    },
                    None => {
                        timer.calibration.push(match outcome.threshold {
                            CaptureThreshold::Enter => NodeCalibration {
                                node: outcome.node,
                                enter_at: level,
                                exit_at: None,
                            },
                            CaptureThreshold::Exit => NodeCalibration {
                                node: outcome.node,
                                enter_at: None,
                                exit_at: level,
                            },
                        });
                        timer.calibration.sort_by_key(|c| c.node);
                    }
                }
            }
            // Best-effort, exactly as the calibration write's persist is: the in-memory record is
            // already correct, and a disk failure must not lose the level the RD just flew for.
            let _ = reg.persist();
        }

        // 4. Retire them. Re-locked rather than held: a capture started while step 3 was writing is
        //    simply left alone, which is what we want.
        {
            let mut captures = self.capture_store();
            for outcome in &settled {
                if let Some(list) = captures.get_mut(&outcome.timer) {
                    list.retain(|c| !(c.node == outcome.node && c.threshold == outcome.threshold));
                }
            }
            captures.retain(|_, list| !list.is_empty());
        }

        settled
    }

    /// The level the timer is **reporting** for one node/threshold on the live signal feed, rounded
    /// to the integer domain a threshold actually lives in.
    ///
    /// Evidence about the timer, never GridFPV's store (D27) — it is used to tell whether a capture
    /// produced a new level, and for nothing else.
    fn reported_level(&self, id: &TimerId, node: u32, threshold: CaptureThreshold) -> Option<u32> {
        let store = self.signal_store();
        let ring = store.get(id)?.nodes.get(node as usize)?;
        let level = match threshold {
            CaptureThreshold::Enter => ring.latest.enter_at,
            CaptureThreshold::Exit => ring.latest.exit_at,
        }?;
        (level.is_finite() && level >= 0.0).then(|| level.round() as u32)
    }

    fn capture_store(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<TimerId, Vec<OutstandingCapture>>> {
        self.captures.lock().expect("timer capture lock poisoned")
    }

    /// **Set a node's channel** on `id` (#413), returning what was dispatched.
    ///
    /// The Tune page's other write, and the one that makes the page's readings mean anything:
    /// tuning a gate is pointless until the node is listening on the channel it will race. Modelled
    /// on [`request_calibration`](Self::request_calibration) line for line, because it is the same
    /// kind of thing.
    ///
    /// # D27: this is GridFPV's value, applied to the timer
    ///
    /// The accepted channel is recorded on [`Timer::node_channels`] and persisted **here**, at
    /// accept time; the `frequency_mhz` RotorHazard later reports is evidence about the timer, never
    /// adopted as truth. The `(band, channel)` label is resolved **server-side** from the shared
    /// catalog ([`resolve_channel_label`]) rather than trusted from the client — GridFPV owns the
    /// vocabulary — and travels with the emit so RotorHazard's own UI shows `Raceband R7` instead
    /// of a bare number.
    ///
    /// ⚠️ **A heat will overwrite this, and that is correct.** Heat setup re-tunes every node to
    /// its assigned channel; this record is a bench setting, not a claim on the node, and is
    /// deliberately not re-applied afterwards.
    ///
    /// # Refused
    ///
    /// A [`TimerError`] (a `400` at the route) for an unknown id, a non-RotorHazard timer (a Mock
    /// has no receiver to tune), a timer that is **not connected** (there is no socket to emit on,
    /// and a channel is not held over for a future connection), a `node` beyond the timer's width
    /// or one the RD has **disabled** (#412 — RotorHazard validates `0 <= node < num_nodes` and
    /// otherwise just logs, so an out-of-range write would vanish silently), a frequency outside
    /// [`CHANNEL_MHZ_MIN`]..=[`CHANNEL_MHZ_MAX`], and a frequency the timer's
    /// [`channel_capability`](Timer::channel_capability) does not allow.
    ///
    /// **Two nodes on one channel is NOT refused.** It is a real mistake worth flagging, and the
    /// console flags it — but it is also exactly what a bench swap looks like halfway through, and
    /// blocking it would block the legitimate case to prevent a recoverable one.
    ///
    /// The **race-phase refusal** — never retune a receiver under a *scored* race — is not here for
    /// the same reason the calibration one is not: it needs the event log, so it lives in the route
    /// (`EventRegistry::scored_heat_in_progress_on_timer`). `during_open_practice` is that route's
    /// answer to the other half, carried through so the driver's armed-heat backstop does not drop
    /// a write the route deliberately allowed.
    pub fn request_channel(
        &self,
        id: &TimerId,
        request: &ChannelRequest,
        during_open_practice: bool,
    ) -> Result<ChannelDispatch, TimerError> {
        let mut reg = self.write();
        let timer = reg
            .timers
            .get_mut(id)
            .ok_or_else(|| TimerError(format!("no timer with id {:?}", id.0)))?;
        if !matches!(timer.kind, TimerKind::Rotorhazard { .. }) {
            return Err(TimerError(format!(
                "{:?} is not a RotorHazard timer — there is no receiver to tune",
                timer.name
            )));
        }
        if timer.status != TimerStatus::Connected {
            return Err(TimerError(format!(
                "{:?} is not connected — connect it before setting a node's channel",
                timer.name
            )));
        }
        // The node must exist AND be enabled (#412). RotorHazard validates
        // `0 <= node_index < num_nodes` and otherwise only writes a log line, so an out-of-range
        // write would look accepted here and land nowhere at all.
        if request.node >= timer.node_width() {
            return Err(TimerError(format!(
                "{:?} has {} nodes — there is no {} to tune",
                timer.name,
                timer.node_width(),
                Timer::node_label(request.node)
            )));
        }
        if !timer.node_enabled(request.node) {
            return Err(TimerError(format!(
                "{} is disabled on {:?} — enable it before setting its channel",
                Timer::node_label(request.node),
                timer.name
            )));
        }
        if !(CHANNEL_MHZ_MIN..=CHANNEL_MHZ_MAX).contains(&request.mhz) {
            return Err(TimerError(format!(
                "{} MHz is not a 5.8 GHz channel — a node's channel must be between {} and {} MHz",
                request.mhz, CHANNEL_MHZ_MIN, CHANNEL_MHZ_MAX
            )));
        }
        // A Fixed timer supports only its declared set. (A Flexible one allows anything the range
        // check above let through — that is what Flexible means, and an EMPTY `available_channels`
        // on a Flexible timer is "no restriction", never "no channels": every RotorHazard on the
        // bench reports exactly that, and reading it as a restriction would leave the RD's dropdown
        // empty on precisely the timers this exists for.)
        if !timer.channel_capability.allows(request.mhz) {
            return Err(TimerError(format!(
                "{:?} cannot tune to {} — it supports only the channels it was configured with",
                timer.name,
                channel_label(request.mhz)
            )));
        }

        // The label GridFPV will send, resolved from ITS catalog rather than trusted from the wire.
        let label = resolve_channel_label(
            request.mhz,
            request.band.as_deref(),
            request.channel.as_deref(),
        );
        let (band, channel) = match &label {
            Some((band, channel)) => (Some(band.clone()), Some(channel.clone())),
            None => (None, None),
        };

        // What the console needs to tell the RD their thresholds are now stale: the channel GridFPV
        // had this node on, and whether GridFPV holds levels for it at all. Read BEFORE the record
        // is updated — afterwards "previous" is gone.
        let previous_mhz = timer
            .node_channels
            .iter()
            .find(|c| c.node == request.node)
            .map(|c| c.mhz);
        let has_thresholds = timer
            .calibration
            .iter()
            .any(|c| c.node == request.node && (c.enter_at.is_some() || c.exit_at.is_some()));
        let thresholds_tuned_on_another_channel =
            has_thresholds && previous_mhz != Some(request.mhz);

        // D27: record GridFPV's value first — the store, not the timer, is where this lives.
        match timer
            .node_channels
            .iter_mut()
            .find(|c| c.node == request.node)
        {
            Some(existing) => {
                existing.mhz = request.mhz;
                existing.band = band.clone();
                existing.channel = channel.clone();
            }
            None => {
                timer.node_channels.push(NodeChannel {
                    node: request.node,
                    mhz: request.mhz,
                    band: band.clone(),
                    channel: channel.clone(),
                });
                timer.node_channels.sort_by_key(|c| c.node);
            }
        }

        // Then queue the *application* of it (#457, one queue), coalesced per (timer, node) by
        // `PendingTimerWrite`'s own policy: a dropdown changed twice before a drain tunes the node
        // once, to the latest value.
        reg.queue_write(PendingTimerWrite::SetChannel(PendingChannel {
            timer: id.clone(),
            node: request.node,
            mhz: request.mhz,
            band: band.clone(),
            channel: channel.clone(),
            during_open_practice,
        }));
        reg.persist()?;

        Ok(ChannelDispatch {
            timer: id.clone(),
            node: request.node,
            mhz: request.mhz,
            band,
            channel,
            previous_mhz,
            thresholds_tuned_on_another_channel,
        })
    }

    /// The per-node channels GridFPV holds for `id` (#413, D27) — its own record, never a readback.
    /// Empty for an unknown timer.
    pub fn node_channels(&self, id: &TimerId) -> Vec<NodeChannel> {
        self.read()
            .timers
            .get(id)
            .map(|t| t.node_channels.clone())
            .unwrap_or_default()
    }

    /// The per-node thresholds GridFPV holds for `id` (#355, D27) — its own record, never a
    /// readback. Empty for an unknown timer.
    pub fn calibration(&self, id: &TimerId) -> Vec<NodeCalibration> {
        self.read()
            .timers
            .get(id)
            .map(|t| t.calibration.clone())
            .unwrap_or_default()
    }

    /// **Read the timer's live tuning signal and renew its lease** (#355 S2a) — the whole of
    /// `GET /timers/{id}/signal`, and the only thing that keeps the stream alive.
    ///
    /// The first call *starts* the subscription: it creates the state, which the connection driver
    /// notices on its next tick and turns into an open transport gate. Every later call pushes the
    /// expiry out by [`SIGNAL_LEASE`]. Stop calling — because the tab closed, the browser died, or
    /// the Wi-Fi went — and the stream stops on its own within five seconds, with nothing to clean
    /// up and no client cooperation required.
    pub fn signal(&self, id: &TimerId) -> TimerSignal {
        let now = Instant::now();
        let mut store = self.signal_store();
        // An expired entry is a *new* subscription, not a resumed one: its ring belongs to a
        // window that has since gone stale, and its sample origin to a session that has ended.
        let state = match store.entry(id.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) if entry.get().leased(now) => {
                entry.into_mut()
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                entry.insert(TimerSignalState::new(now));
                entry.into_mut()
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(TimerSignalState::new(now))
            }
        };
        state.lease_until = now + SIGNAL_LEASE;
        state.snapshot(id, now)
    }

    /// **Stop the timer's tuning stream now** (#355 S2a) — `POST /timers/{id}/signal/stop`.
    ///
    /// The lease alone is enough for correctness; this exists for *promptness*. Closing the Tune
    /// view should quiet the socket immediately rather than after the lease runs out, so the
    /// heartbeat gate shuts on the driver's next tick instead of five seconds later.
    pub fn stop_signal(&self, id: &TimerId) {
        self.signal_store().remove(id);
    }

    /// Whether a tune-telemetry subscription is currently open for `timer` — what the connection
    /// driver reads on every tick to decide whether the transport's pre-parse gate is open.
    ///
    /// **Prunes as it reads**: a lapsed subscription's state is dropped here, so a Tune page that
    /// went away leaves nothing behind at all.
    pub fn signal_wanted(&self, id: &TimerId) -> bool {
        let now = Instant::now();
        let mut store = self.signal_store();
        match store.get(id) {
            Some(state) if state.leased(now) => true,
            Some(_) => {
                store.remove(id);
                false
            }
            None => false,
        }
    }

    /// Append one **decimated** tick of per-node readings to `timer`'s rolling window (#355 S2a).
    ///
    /// Called by the connection driver on its own fixed cadence ([`SIGNAL_SAMPLE_INTERVAL`]) —
    /// never on frame arrival, because RotorHazard's heartbeat rate is not something to trust
    /// (`HEARTBEAT_DATA_RATE_FACTOR` jumps 5 → 50 when its frequency scanner is on).
    ///
    /// A push with **no live lease is dropped**, and takes the state with it. The gate should
    /// already be shut by then; this is the second lock on the same door, so a driver that is a
    /// tick behind can never resurrect a stream nobody is watching.
    pub fn push_signal(&self, id: &TimerId, readings: &[NodeReading]) {
        let now = Instant::now();
        let mut store = self.signal_store();
        let Some(state) = store.get_mut(id) else {
            return;
        };
        if !state.leased(now) {
            store.remove(id);
            return;
        }
        state.push(readings, now);
    }

    fn signal_store(&self) -> std::sync::MutexGuard<'_, HashMap<TimerId, TimerSignalState>> {
        self.signal.lock().expect("timer signal lock poisoned")
    }

    /// Delete a timer (issue #73). The built-in **Mock cannot be deleted** (it is always
    /// present); attempting to is a [`TimerError`]. An unknown id is also an error. The registry
    /// is **persisted** on success. A manual connection hold (#383) dies with the timer — the
    /// reconciler stops seeing it in [`manual_connections`](Self::manual_connections) and drops the
    /// link on its next tick.
    pub fn delete(&self, id: &TimerId) -> Result<(), TimerError> {
        if id.0 == MOCK_TIMER_ID {
            return Err(TimerError(
                "the built-in Mock timer cannot be deleted".to_string(),
            ));
        }
        let mut reg = self.write();
        if reg.timers.remove(id).is_none() {
            return Err(TimerError(format!("no timer with id {:?}", id.0)));
        }
        reg.persist()?;
        Ok(())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.inner.read().expect("timer registry lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.inner.write().expect("timer registry lock poisoned")
    }
}

impl Registry {
    /// Queue one write for the connection reconciler (#457) through
    /// [`PendingTimerWrite::queue_into`], so the registry's queue and the live driver's own queue
    /// apply the *same* coalescing policy.
    fn queue_write(&mut self, write: PendingTimerWrite) {
        write.queue_into(&mut self.pending_writes);
    }

    /// Persist the timer set to `<data_dir>/timers.json` (issue #73), a no-op with no data dir.
    /// The Mock is persisted too so a retune survives a restart.
    fn persist(&self) -> Result<(), TimerError> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let timers: Vec<&Timer> = self.timers.values().collect();
        let json = serde_json::to_string_pretty(&timers)
            .map_err(|e| TimerError(format!("could not serialize timers: {e}")))?;
        std::fs::write(timers_path(dir), json)
            .map_err(|e| TimerError(format!("could not persist timers: {e}")))
    }
}

/// Clamp one calibration level into [`RSSI_MIN`]..=[`RSSI_MAX`] (#355).
///
/// The **server-side** half of the console's `clampLevel`. It exists even though the page already
/// clamps because this value reaches timing hardware: `0` is the one that matters, since
/// RotorHazard reads a falsy level as "re-read it off the node" and silently keeps the old
/// threshold while answering as though the write succeeded.
fn clamp_level(level: u32) -> u32 {
    level.clamp(RSSI_MIN, RSSI_MAX)
}

/// The file the timer set is persisted to under `dir`: `<dir>/timers.json`.
fn timers_path(dir: &Path) -> PathBuf {
    dir.join(TIMERS_FILE)
}

/// Read the persisted timers from `<dir>/timers.json`, or `None` if absent/unreadable/corrupt.
/// A bad file degrades to "no persisted timers" so the Director still boots with the Mock.
fn read_persisted_timers(dir: &Path) -> Option<Vec<Timer>> {
    let raw = std::fs::read_to_string(timers_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The largest Mock `laps` count we accept (release-hardening P2): a sane ceiling so a fat-fingered
/// or hostile value can't ask the sim to generate a runaway number of laps per pilot.
pub const MAX_MOCK_LAPS: u32 = 1000;

/// Validate a timer's effective configuration (release-hardening P2), returning a human-readable
/// message on the first problem (the caller maps it to a `400`).
///
/// Rejects a `node_count` **override** of `0` (it caps every heat to **no** pilots — nothing could
/// ever race), an empty/whitespace RotorHazard URL (nothing to dial), and a Mock `laps` count beyond
/// [`MAX_MOCK_LAPS`] (a runaway sim). `node_count` is passed in so the merged value can be checked
/// on a partial edit; `None` (#412 — follow whatever the timer reports) is always fine, since a
/// discovered width can never be zero.
pub fn validate_timer_config(kind: &TimerKind, node_count: Option<u32>) -> Result<(), String> {
    if node_count == Some(0) {
        return Err(
            "node_count must be at least 1 (a 0-node timer caps every heat to no pilots)"
                .to_string(),
        );
    }
    match kind {
        TimerKind::Rotorhazard { url } if url.trim().is_empty() => {
            Err("a RotorHazard timer requires a non-empty server URL".to_string())
        }
        TimerKind::Mock { laps, .. } if *laps > MAX_MOCK_LAPS => {
            Err(format!("laps must be at most {MAX_MOCK_LAPS}"))
        }
        _ => Ok(()),
    }
}

/// An error mutating the timer registry (a persistence failure, an unknown id, a protected delete).
#[derive(Debug, Clone)]
pub struct TimerError(pub String);

impl std::fmt::Display for TimerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "timer registry error: {}", self.0)
    }
}

impl std::error::Error for TimerError {}

/// Slugify a display name into an id-friendly stem (same rule as the event registry): lowercase
/// ASCII alphanumerics kept, every other run collapsed to a single `-`, trimmed of dashes; an
/// empty/symbol-only name yields `timer`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "timer".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A short random lowercase-alphanumeric suffix making an auto-generated id unique (same source
/// as the event registry — the OS CSPRNG).
fn short_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("OS CSPRNG available");
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory registry plus a RotorHazard timer to hang tune telemetry off.
    fn registry_with_rh() -> (TimerRegistry, TimerId) {
        let timers = TimerRegistry::new(None, 5, 2500).expect("in-memory registry");
        let id = timers
            .create(&CreateTimerRequest {
                name: "Field RH".to_string(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".to_string(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .expect("timer created")
            .id;
        (timers, id)
    }

    /// One tick of readings for `nodes` nodes, node 0 carrying `rssi`.
    fn tick(nodes: usize, rssi: f32) -> Vec<NodeReading> {
        (0..nodes)
            .map(|index| NodeReading {
                seen: true,
                rssi: Some(if index == 0 { rssi } else { 10.0 }),
                ..Default::default()
            })
            .collect()
    }

    /// Back-date the lease so expiry is testable without sleeping through [`SIGNAL_LEASE`].
    fn expire(timers: &TimerRegistry, id: &TimerId) {
        timers
            .signal_store()
            .get_mut(id)
            .expect("a live subscription")
            .lease_until = Instant::now() - Duration::from_millis(1);
    }

    /// **The first GET starts the stream; nothing else does.** Before anyone asks, the connection
    /// driver is told not to capture — an idle Director must not be paying for a heartbeat parse.
    #[test]
    fn the_first_read_opens_the_subscription() {
        let (timers, rh) = registry_with_rh();
        assert!(
            !timers.signal_wanted(&rh),
            "no one has asked, so nothing streams"
        );
        let snapshot = timers.signal(&rh);
        assert_eq!(snapshot.timer, rh);
        assert!(!snapshot.streaming, "leased, but nothing has pushed yet");
        assert!(snapshot.nodes.is_empty());
        assert!(timers.signal_wanted(&rh), "the read is the subscription");
    }

    /// **A lease, not a boolean.** A Tune page that stops asking — closed tab, dead browser, lost
    /// network — stops the stream by itself, and takes its buffer with it. A bare flag would leave
    /// the timer streaming until the Director exited.
    #[test]
    fn a_lapsed_lease_stops_the_stream_and_drops_its_buffer() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        timers.push_signal(&rh, &tick(4, 48.0));
        assert_eq!(timers.signal(&rh).nodes.len(), 4);

        expire(&timers, &rh);
        assert!(
            !timers.signal_wanted(&rh),
            "the gate shuts with no client cooperation at all"
        );
        assert!(
            !timers.signal_store().contains_key(&rh),
            "and the lapsed subscription's buffer is pruned, not left to rot"
        );

        // A driver a tick behind cannot resurrect it either — the push is the second lock.
        timers.push_signal(&rh, &tick(4, 48.0));
        assert!(!timers.signal_store().contains_key(&rh));
    }

    /// A renewed lease keeps the window; a *lapsed* one starts a new session rather than resuming
    /// a stale buffer whose samples belong to a window that has since scrolled away.
    #[test]
    fn renewing_keeps_the_window_but_relapsing_starts_a_new_one() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        timers.push_signal(&rh, &tick(4, 48.0));
        timers.push_signal(&rh, &tick(4, 49.0));
        assert_eq!(timers.signal(&rh).nodes[0].samples.len(), 2);

        expire(&timers, &rh);
        assert_eq!(
            timers.signal(&rh).nodes.len(),
            0,
            "a re-opened subscription starts from nothing"
        );
    }

    /// **The ring is bounded.** Cost per tick is O(nodes) and the buffer's size is fixed, so a Tune
    /// page left open all afternoon costs exactly what it cost after the first thirty seconds.
    #[test]
    fn the_rolling_window_is_bounded() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        for i in 0..(SIGNAL_RING * 3) {
            timers.push_signal(&rh, &tick(8, i as f32));
        }
        let snapshot = timers.signal(&rh);
        assert_eq!(snapshot.sample_micros.len(), SIGNAL_RING);
        for node in &snapshot.nodes {
            assert_eq!(node.samples.len(), SIGNAL_RING);
        }
        // Oldest-first, last-value-wins: the window holds the MOST RECENT `SIGNAL_RING` samples.
        let first = &snapshot.nodes[0];
        assert_eq!(first.samples[SIGNAL_RING - 1], (SIGNAL_RING * 3 - 1) as f32);
        assert_eq!(first.rssi, Some((SIGNAL_RING * 3 - 1) as f32));
        // And the shared time base stays parallel to every node's window.
        assert!(snapshot.sample_micros.windows(2).all(|w| w[0] <= w[1]));
    }

    /// **Every node, including unseated ones.** Tune telemetry never passes through the app layer's
    /// lineup remap, so a node no heat has seated still reports — which is the whole point, since
    /// "is this node even alive?" is the question an RD with a dead gate cannot otherwise answer.
    #[test]
    fn unseated_nodes_are_in_the_snapshot() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        // Eight nodes; node 0 is flying, the rest are unseated and idle.
        let mut readings = tick(8, 120.0);
        for reading in readings.iter_mut().skip(1) {
            reading.seen = true;
            reading.rssi = Some(9.0);
        }
        timers.push_signal(&rh, &readings);

        let snapshot = timers.signal(&rh);
        assert_eq!(snapshot.nodes.len(), 8);
        assert_eq!(snapshot.nodes[7].seat, CompetitorRef("node-7".to_string()));
        assert!(snapshot.nodes[7].seen);
        assert_eq!(snapshot.nodes[7].rssi, Some(9.0));
    }

    /// A timer that comes back a different width restarts the window rather than shifting every
    /// node's history sideways under the graph.
    #[test]
    fn a_node_count_change_restarts_the_window() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        timers.push_signal(&rh, &tick(8, 48.0));
        timers.push_signal(&rh, &tick(8, 49.0));
        timers.push_signal(&rh, &tick(4, 50.0));

        let snapshot = timers.signal(&rh);
        assert_eq!(snapshot.nodes.len(), 4);
        assert_eq!(snapshot.sample_micros.len(), 1);
        assert_eq!(snapshot.nodes[0].samples, vec![50.0]);
    }

    /// The explicit stop is about **promptness**, not correctness: closing the Tune view should
    /// quiet the socket now rather than when the lease runs out.
    #[test]
    fn stopping_ends_the_subscription_immediately() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        timers.push_signal(&rh, &tick(4, 48.0));
        assert!(timers.signal_wanted(&rh));

        timers.stop_signal(&rh);
        assert!(!timers.signal_wanted(&rh));
        assert!(timers.signal_store().is_empty());
        // Idempotent, and harmless on a timer that never streamed.
        timers.stop_signal(&rh);
    }

    /// Tune telemetry is **never persisted**. `Timer`'s JSON is both its wire form and its
    /// on-disk form, so a sample ring living on it would hit `timers.json` on every CRUD and come
    /// back on boot as configuration. It lives in a sibling map for exactly that reason.
    #[test]
    fn telemetry_never_reaches_the_persisted_timer() {
        let (timers, rh) = registry_with_rh();
        timers.signal(&rh);
        timers.push_signal(&rh, &tick(8, 120.0));
        let json = serde_json::to_string(&timers.get(&rh).expect("the timer")).expect("serializes");
        for leaked in ["samples", "rssi", "sample_micros", "lease"] {
            assert!(
                !json.contains(leaked),
                "a persisted Timer must carry no telemetry ({leaked} leaked into {json})"
            );
        }
    }

    #[test]
    fn validate_timer_config_rejects_bad_configs() {
        // A 0-node timer caps every heat to no pilots — rejected (P2).
        assert!(
            validate_timer_config(
                &TimerKind::Mock {
                    laps: 5,
                    lap_ms: 100
                },
                Some(0)
            )
            .is_err()
        );
        // An empty / whitespace RotorHazard URL can never be dialed — rejected.
        assert!(
            validate_timer_config(&TimerKind::Rotorhazard { url: "   ".into() }, Some(8)).is_err()
        );
        // A runaway Mock laps count is rejected.
        assert!(
            validate_timer_config(
                &TimerKind::Mock {
                    laps: MAX_MOCK_LAPS + 1,
                    lap_ms: 100
                },
                Some(8)
            )
            .is_err()
        );
        // A sane config passes.
        assert!(
            validate_timer_config(
                &TimerKind::Mock {
                    laps: 5,
                    lap_ms: 2500
                },
                Some(8)
            )
            .is_ok()
        );
        assert!(
            validate_timer_config(
                &TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into()
                },
                Some(8)
            )
            .is_ok()
        );
    }

    fn sim_req(name: &str) -> CreateTimerRequest {
        CreateTimerRequest {
            name: name.to_string(),
            kind: TimerKind::Mock {
                laps: 3,
                lap_ms: 2000,
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        }
    }

    fn rh_req(name: &str, url: &str) -> CreateTimerRequest {
        CreateTimerRequest {
            name: name.to_string(),
            kind: TimerKind::Rotorhazard {
                url: url.to_string(),
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        }
    }

    #[test]
    fn mock_is_always_present_and_first() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let list = reg.list();
        let first = list.first().unwrap();
        assert_eq!(first.id.0, MOCK_TIMER_ID);
        assert_eq!(first.name, MOCK_TIMER_NAME);
        assert_eq!(first.status, TimerStatus::Ready);
        // The Mock draws its config from the Director's env defaults.
        assert_eq!(
            first.kind,
            TimerKind::Mock {
                laps: 5,
                lap_ms: 2500
            }
        );
    }

    #[test]
    fn create_auto_generates_a_unique_slug_id_and_lists_after_mock() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let a = reg
            .create(&rh_req("Track RH!", "http://rh.local:5000"))
            .unwrap();
        let b = reg
            .create(&rh_req("Track RH!", "http://rh.local:5000"))
            .unwrap();
        assert!(a.id.0.starts_with("track-rh-"));
        assert_ne!(a.id, b.id);
        assert_eq!(a.status, TimerStatus::Configured);
        let ids: Vec<_> = reg.list().into_iter().map(|t| t.id).collect();
        assert_eq!(ids[0].0, MOCK_TIMER_ID);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn update_edits_name_and_kind() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let created = reg.create(&sim_req("My Sim")).unwrap();
        let updated = reg
            .update(
                &created.id,
                &UpdateTimerRequest {
                    name: Some("Renamed".into()),
                    kind: Some(TimerKind::Mock {
                        laps: 9,
                        lap_ms: 1000,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.name, "Renamed");
        assert_eq!(
            updated.kind,
            TimerKind::Mock {
                laps: 9,
                lap_ms: 1000
            }
        );
    }

    #[test]
    fn retuning_the_mock_is_allowed_but_deleting_it_is_not() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let sim = TimerId(MOCK_TIMER_ID.into());
        // Retune is fine.
        reg.update(
            &sim,
            &UpdateTimerRequest {
                name: None,
                kind: Some(TimerKind::Mock {
                    laps: 1,
                    lap_ms: 50,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            reg.get(&sim).unwrap().kind,
            TimerKind::Mock {
                laps: 1,
                lap_ms: 50
            }
        );
        // Delete is rejected.
        assert!(reg.delete(&sim).is_err());
        assert!(reg.exists(&sim));
    }

    #[test]
    fn delete_removes_a_created_timer() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let created = reg.create(&sim_req("Temp")).unwrap();
        assert!(reg.exists(&created.id));
        reg.delete(&created.id).unwrap();
        assert!(!reg.exists(&created.id));
        assert!(reg.delete(&created.id).is_err());
    }

    #[test]
    fn timers_persist_across_a_restart_with_a_data_dir() {
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-test-{}", short_suffix()));
        {
            let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let created = reg
                .create(&rh_req("Field RH", "http://rh.local:5000"))
                .unwrap();
            // Retune the Mock too, to prove its config also survives.
            reg.update(
                &TimerId(MOCK_TIMER_ID.into()),
                &UpdateTimerRequest {
                    name: None,
                    kind: Some(TimerKind::Mock {
                        laps: 7,
                        lap_ms: 1234,
                    }),
                    ..Default::default()
                },
            )
            .unwrap();

            let reopened = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            // The created RH timer survived…
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(
                got.kind,
                TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into()
                }
            );
            assert_eq!(got.status, TimerStatus::Configured);
            // …and so did the retuned Mock config.
            assert_eq!(
                reopened.get(&TimerId(MOCK_TIMER_ID.into())).unwrap().kind,
                TimerKind::Mock {
                    laps: 7,
                    lap_ms: 1234
                }
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn set_status_drives_an_rh_timers_live_connection_state() {
        // The Director publishes an RH timer's connection lifecycle through `set_status` (#65):
        // Configured (resting) → Connecting → Connected → Disconnected, all live in `GET /timers`.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = reg
            .create(&rh_req("Field RH", "http://rh.local:5000"))
            .unwrap();
        assert_eq!(reg.get(&rh.id).unwrap().status, TimerStatus::Configured);

        for status in [
            TimerStatus::Connecting,
            TimerStatus::Connected,
            TimerStatus::Disconnected,
            TimerStatus::Error,
        ] {
            reg.set_status(&rh.id, status);
            assert_eq!(reg.get(&rh.id).unwrap().status, status);
            // The live status is reflected in the `GET /timers` listing too.
            let listed = reg.list().into_iter().find(|t| t.id == rh.id).unwrap();
            assert_eq!(listed.status, status);
        }

        // An unknown id is a no-op (no panic).
        reg.set_status(&TimerId("nope".into()), TimerStatus::Connected);
    }

    #[test]
    fn live_status_is_not_persisted_and_resets_to_configured_on_reopen() {
        // Dynamic connection states are in-memory only: a reopen restores the RH timer at its
        // resting `Configured`, never a stale `Connected`/`Error`.
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-status-{}", short_suffix()));
        {
            let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let rh = reg
                .create(&rh_req("Field RH", "http://rh.local:5000"))
                .unwrap();
            reg.set_status(&rh.id, TimerStatus::Connected);
            assert_eq!(reg.get(&rh.id).unwrap().status, TimerStatus::Connected);

            let reopened = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            assert_eq!(
                reopened.get(&rh.id).unwrap().status,
                TimerStatus::Configured,
                "a restored RH timer rests at Configured, not a persisted live state"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_real_kind_change_resets_live_state_but_a_no_op_edit_does_not() {
        // #382: the reset exists so a reconfigured timer is re-dialled + re-probed. It must fire
        // ONLY on a genuine change — the reconciler is what republishes the live values, and it
        // sees nothing to do when the kind is unchanged, so wiping on a no-op edit strands the
        // timer at `Configured` with no plugin **permanently**.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = reg
            .create(&rh_req("Field RH", "http://rh.local:5000"))
            .unwrap();
        reg.set_status(&rh.id, TimerStatus::Connected);
        reg.set_plugin(
            &rh.id,
            PluginPresence::Present {
                plugin_version: "0.1.0".into(),
                rhapi_version: "1.4".into(),
                capabilities: vec!["hello".into()],
            },
        );

        // A rename that echoes the SAME kind back leaves the live status + plugin alone.
        reg.update(
            &rh.id,
            &UpdateTimerRequest {
                name: Some("Field RH (north)".into()),
                kind: Some(TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let got = reg.get(&rh.id).unwrap();
        assert_eq!(got.name, "Field RH (north)");
        assert_eq!(got.status, TimerStatus::Connected);
        assert!(got.plugin.is_some(), "a no-op edit must not drop the probe");

        // A real URL edit DOES reset both — the connection is about to be superseded and re-probed.
        reg.update(
            &rh.id,
            &UpdateTimerRequest {
                kind: Some(TimerKind::Rotorhazard {
                    url: "http://rh-new.local:5000".into(),
                }),
                ..Default::default()
            },
        )
        .unwrap();
        let got = reg.get(&rh.id).unwrap();
        assert_eq!(got.status, TimerStatus::Configured);
        assert!(got.plugin.is_none());

        // A kind change to Mock rests at `Ready`, likewise re-probed from scratch.
        reg.set_status(&rh.id, TimerStatus::Connected);
        reg.update(
            &rh.id,
            &UpdateTimerRequest {
                kind: Some(TimerKind::Mock {
                    laps: 3,
                    lap_ms: 2000,
                }),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(reg.get(&rh.id).unwrap().status, TimerStatus::Ready);
    }

    #[test]
    fn the_seeded_mock_has_channel_defaults() {
        // Race redesign Slice 4a: the built-in Mock is flexible, 8 nodes, seeded from Raceband.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let mock = reg.get(&TimerId(MOCK_TIMER_ID.into())).unwrap();
        assert_eq!(mock.channel_capability, ChannelCapability::Flexible);
        // The Mock's width is PINNED (#412): there is no hardware to ask, so it carries the
        // override rather than waiting for a report that never comes.
        assert_eq!(mock.node_count, Some(DEFAULT_NODE_COUNT));
        assert_eq!(mock.node_width(), DEFAULT_NODE_COUNT);
        assert_eq!(
            mock.available_channels,
            crate::channels::RACEBAND_MHZ.to_vec()
        );
    }

    #[test]
    fn channel_capability_node_count_and_available_persist_across_restart() {
        // Race redesign Slice 4a: a timer's Fixed capability + node count + available channels
        // survive a Director restart, and an old `timers.json` (no channel fields) reads back valid.
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-chan-{}", short_suffix()));
        let fixed = ChannelCapability::Fixed {
            channels: vec![5658, 5695, 5732, 5769],
        };
        {
            let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let created = reg
                .create(&CreateTimerRequest {
                    name: "Field RH".into(),
                    kind: TimerKind::Rotorhazard {
                        url: "http://rh.local:5000".into(),
                    },
                    channel_capability: Some(fixed.clone()),
                    node_count: Some(4),
                    available_channels: Some(vec![5658, 5695, 5732, 5769]),
                })
                .unwrap();
            assert_eq!(created.channel_capability, fixed);
            assert_eq!(created.node_count, Some(4));

            let reopened = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.channel_capability, fixed);
            assert_eq!(got.node_count, Some(4));
            assert_eq!(got.available_channels, vec![5658, 5695, 5732, 5769]);
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_pre_channel_model_timers_file_deserializes_with_defaults() {
        // An old `timers.json` written before the channel fields existed must still load — the new
        // fields default (Flexible, 8 nodes, no available channels).
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-legacy-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        // A minimal legacy timer entry: id/name/kind/status only.
        let legacy = r#"[{"id":"mock","name":"Mock","kind":{"Mock":{"laps":3,"lap_ms":2000}},"status":"Ready"},
                         {"id":"old-rh","name":"Old RH","kind":{"Rotorhazard":{"url":"http://x:5000"}},"status":"Configured"}]"#;
        std::fs::write(timers_path(&dir), legacy).unwrap();
        let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
        let old = reg.get(&TimerId("old-rh".into())).unwrap();
        assert_eq!(old.channel_capability, ChannelCapability::Flexible);
        // No `node_count` on disk ⇒ no override (#412): the width follows whatever the timer
        // reports, and falls back to the 8-node default until it does.
        assert_eq!(old.node_count, None);
        assert_eq!(old.node_width(), DEFAULT_NODE_COUNT);
        assert!(old.disabled_nodes.is_empty());
        assert_eq!(
            old.enabled_nodes(),
            (0..DEFAULT_NODE_COUNT).collect::<Vec<_>>()
        );
        assert!(old.available_channels.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn update_edits_channel_capability_and_node_count() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let created = reg.create(&sim_req("Tunable")).unwrap();
        let updated = reg
            .update(
                &created.id,
                &UpdateTimerRequest {
                    node_count: Some(6),
                    available_channels: Some(vec![5800, 5820, 5840]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.node_count, Some(6));
        assert_eq!(updated.available_channels, vec![5800, 5820, 5840]);
    }

    // ── #412: the node set — reported, configured, enabled ────────────────────

    /// An RH timer in `reg` with no width override (the post-#412 default: follow the hardware).
    fn discoverable_rh(reg: &TimerRegistry, name: &str) -> Timer {
        reg.create(&CreateTimerRequest {
            name: name.into(),
            kind: TimerKind::Rotorhazard {
                url: "http://rh.local:5000".into(),
            },
            channel_capability: None,
            node_count: None,
            available_channels: None,
        })
        .unwrap()
    }

    #[test]
    fn a_new_timer_follows_what_the_hardware_reports() {
        // The bench bug: a real 4-node NuclearHazard read as 8 because nothing ever asked it.
        // A timer created with no explicit width now takes the reported one.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        assert_eq!(rh.node_count, None, "no override on a fresh timer");
        assert_eq!(
            rh.node_width(),
            DEFAULT_NODE_COUNT,
            "…so it sits on the default until the timer says otherwise"
        );

        reg.set_reported_nodes(&rh.id, 4);
        let rh = reg.get(&rh.id).unwrap();
        assert_eq!(rh.reported_nodes, Some(4));
        assert_eq!(rh.node_width(), 4, "the observation now drives the width");
        assert_eq!(rh.enabled_nodes(), vec![0, 1, 2, 3]);
        assert_eq!(rh.seat_capacity(), 4, "a heat seats four pilots, not eight");
        assert_eq!(rh.node_drift(), None, "nothing to disagree about");
    }

    #[test]
    fn an_explicit_width_wins_over_the_report_and_the_disagreement_is_surfaced() {
        // D27 / #355's rule: an observation that contradicts config is a NOTICE, never an edit.
        // The RD who typed 8 keeps 8 — and is told the timer only has 4.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = reg
            .create(&CreateTimerRequest {
                name: "Field RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: Some(8),
                available_channels: None,
            })
            .unwrap();
        reg.set_reported_nodes(&rh.id, 4);
        let rh = reg.get(&rh.id).unwrap();

        assert_eq!(rh.node_count, Some(8), "the decision is untouched");
        assert_eq!(rh.reported_nodes, Some(4), "the observation is recorded");
        assert_eq!(rh.node_width(), 8, "config wins");
        let drift = rh.node_drift().expect("a disagreement is surfaced");
        assert_eq!(drift.reported, 4);
        assert_eq!(drift.configured, 8);
        assert_eq!(
            drift.enabled_beyond_reported,
            vec![4, 5, 6, 7],
            "the four seats that would record nothing are named"
        );
    }

    #[test]
    fn a_disabled_node_leaves_a_hole_in_the_enabled_set() {
        // The RD's words: "reported is 4 but node 3 is busted, I need to use nodes 1, 2 and 4."
        // Display "Node 3" is wire index 2, so the enabled set is {0, 1, 3} — NOT a prefix, which
        // is the whole reason this is a set and not a count.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);
        let view = reg
            .set_nodes(
                &rh.id,
                &SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some(vec![0, 1, 3]),
                },
            )
            .unwrap();

        assert_eq!(view.enabled, vec![0, 1, 3]);
        assert_eq!(view.width, 4, "the disabled node still occupies an index");
        let rh = reg.get(&rh.id).unwrap();
        assert_eq!(rh.disabled_nodes, vec![2]);
        assert_eq!(rh.seat_capacity(), 3, "three pilots fit, not four");
        assert!(!rh.node_enabled(2));
        assert!(rh.node_enabled(3), "node 3 is NOT renumbered away");
    }

    #[test]
    fn a_heat_seats_onto_the_real_node_indices_not_the_lineup_positions() {
        // THE #412 correctness case. With node index 2 disabled on a 4-node timer, a 3-pilot heat
        // occupies nodes 0, 1 and 3. Seating the third pilot on node 2 (their lineup position)
        // would put them on the dead gate this feature exists to keep them off.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);
        reg.set_nodes(
            &rh.id,
            &SetTimerNodesRequest {
                node_count: None,
                enabled: Some(vec![0, 1, 3]),
            },
        )
        .unwrap();
        let rh = reg.get(&rh.id).unwrap();

        let lineup = vec![
            CompetitorRef("ace".into()),
            CompetitorRef("bolt".into()),
            CompetitorRef("cyan".into()),
        ];
        let seats = rh.seat_nodes(&lineup);
        assert_eq!(
            seats.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![0, 1, 3],
            "the third pilot sits on node 3, not node 2"
        );
        assert_eq!(seats[2].1, CompetitorRef("cyan".into()));

        // A fourth pilot has nowhere to go: dropped, never squeezed onto the dead node. (The
        // heat-size cap refuses this upstream; this is the backstop.)
        let mut oversized = lineup.clone();
        oversized.push(CompetitorRef("dart".into()));
        assert_eq!(rh.seat_nodes(&oversized).len(), 3);
    }

    #[test]
    fn a_node_seat_ref_keeps_its_own_index_and_a_disabled_one_is_dropped() {
        // `node-{i}` is a WIRE HANDLE for a physical gate (the open-practice channel lineup). It
        // names its own node — renumbering it would make marshaling and the signal trace disagree
        // about which gate a pass came from.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);
        reg.set_nodes(
            &rh.id,
            &SetTimerNodesRequest {
                node_count: None,
                enabled: Some(vec![0, 1, 3]),
            },
        )
        .unwrap();
        let rh = reg.get(&rh.id).unwrap();

        let practice = vec![
            CompetitorRef("node-0".into()),
            CompetitorRef("node-3".into()),
        ];
        assert_eq!(
            rh.seat_nodes(&practice)
                .iter()
                .map(|(n, _)| *n)
                .collect::<Vec<_>>(),
            vec![0, 3]
        );
        // A practice round naming the disabled gate flies nothing there rather than being
        // silently slid onto a working one.
        let stale = vec![CompetitorRef("node-2".into())];
        assert!(rh.seat_nodes(&stale).is_empty());
    }

    #[test]
    fn a_disabled_node_survives_a_reconnect_and_a_restart() {
        // A disable is a DECISION, not an observation: the timer insisting it still has four
        // working nodes does not switch one back on, and neither does a Director restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-nodes-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = {
            let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let rh = discoverable_rh(&reg, "Field RH");
            reg.set_reported_nodes(&rh.id, 4);
            reg.set_nodes(
                &rh.id,
                &SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some(vec![0, 1, 3]),
                },
            )
            .unwrap();

            // …the link drops and comes back, and the timer reports its four nodes again.
            reg.set_reported_nodes(&rh.id, 4);
            let after = reg.get(&rh.id).unwrap();
            assert_eq!(
                after.enabled_nodes(),
                vec![0, 1, 3],
                "a reconnect must not re-enable a node the RD switched off"
            );
            assert_eq!(after.disabled_nodes, vec![2]);
            rh.id
        };

        // …and a restart: the decision is persisted, the observation is not.
        let reopened = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
        let restored = reopened.get(&id).unwrap();
        assert_eq!(restored.disabled_nodes, vec![2], "the decision persists");
        assert_eq!(
            restored.reported_nodes, None,
            "the observation is re-read on the next connect, never restored"
        );
        assert_eq!(restored.node_count, None, "no override was ever set");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_timer_that_widens_does_not_resurrect_a_disabled_node() {
        // The disabled set is stored as node INDICES, so it stays meaningful when the width moves.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);
        reg.set_nodes(
            &rh.id,
            &SetTimerNodesRequest {
                node_count: None,
                enabled: Some(vec![0, 1, 3]),
            },
        )
        .unwrap();
        // The RD swaps in an 8-node timer at the same URL.
        reg.set_reported_nodes(&rh.id, 8);
        assert_eq!(
            reg.get(&rh.id).unwrap().enabled_nodes(),
            vec![0, 1, 3, 4, 5, 6, 7],
            "the new nodes come up enabled; node 2 stays off"
        );
    }

    #[test]
    fn the_wire_is_zero_based_and_the_display_is_one_based() {
        // Every off-by-one here is a pilot on a dead gate, so the boundary is explicit and checked
        // in both directions.
        assert_eq!(Timer::node_label(0), "Node 1");
        assert_eq!(
            Timer::node_label(2),
            "Node 3",
            "the RD's \"node 3\" is index 2"
        );
        assert_eq!(node_seat_index(&CompetitorRef("node-0".into())), Some(0));
        assert_eq!(node_seat_index(&CompetitorRef("node-2".into())), Some(2));
        assert_eq!(node_seat_index(&CompetitorRef("ace".into())), None);
        assert_eq!(node_seat_index(&CompetitorRef("node-x".into())), None);

        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);
        let view = reg.nodes(&rh.id).unwrap();
        assert_eq!(
            view.nodes
                .iter()
                .map(|n| n.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Node 1", "Node 2", "Node 3", "Node 4"]
        );
        assert_eq!(
            view.nodes.iter().map(|n| n.node).collect::<Vec<_>>(),
            vec![0, 1, 2, 3],
            "…while the wire index stays 0-based"
        );
        assert_eq!(
            view.nodes[2].seat,
            CompetitorRef("node-2".into()),
            "the seat handle is the wire index, not the label"
        );
    }

    #[test]
    fn setting_nodes_is_three_valued_on_the_width_override() {
        // "Go back to trusting the hardware" is a real thing an RD does after a drift notice, so
        // `null` must be distinguishable from absent.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);

        let pin: SetTimerNodesRequest = serde_json::from_str(r#"{"node_count": 6}"#).unwrap();
        assert_eq!(reg.set_nodes(&rh.id, &pin).unwrap().width, 6);

        let untouched: SetTimerNodesRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(untouched.node_count, None);
        assert_eq!(reg.set_nodes(&rh.id, &untouched).unwrap().width, 6);

        let clear: SetTimerNodesRequest = serde_json::from_str(r#"{"node_count": null}"#).unwrap();
        assert_eq!(clear.node_count, Some(None), "null is not absent");
        let view = reg.set_nodes(&rh.id, &clear).unwrap();
        assert_eq!(view.configured, None);
        assert_eq!(view.width, 4, "back to what the timer reports");
    }

    #[test]
    fn a_timer_with_no_enabled_node_is_refused() {
        // Same rule as a zero `node_count`: it caps every heat to no pilots, so nothing could race.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = discoverable_rh(&reg, "Field RH");
        reg.set_reported_nodes(&rh.id, 4);
        assert!(
            reg.set_nodes(
                &rh.id,
                &SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some(vec![]),
                },
            )
            .is_err()
        );
        assert!(
            reg.set_nodes(
                &rh.id,
                &SetTimerNodesRequest {
                    node_count: Some(Some(0)),
                    enabled: None,
                },
            )
            .is_err()
        );
        assert_eq!(
            reg.get(&rh.id).unwrap().enabled_nodes(),
            vec![0, 1, 2, 3],
            "a refused edit changes nothing"
        );
    }

    #[test]
    fn a_pre_412_timers_file_keeps_its_explicit_node_count() {
        // MIGRATION: the registry always wrote `node_count`, so every pre-#412 file carries an
        // explicit number — and an RD who set one meant it. It is kept, and the disagreement with
        // what the timer reports is representable rather than silently resolved.
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-412-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = r#"[{"id":"old-rh","name":"Old RH","kind":{"Rotorhazard":{"url":"http://x:5000"}},
                          "status":"Configured","channel_capability":"Flexible","node_count":8,
                          "available_channels":[5658]}]"#;
        std::fs::write(timers_path(&dir), legacy).unwrap();

        let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
        let id = TimerId("old-rh".into());
        let old = reg.get(&id).unwrap();
        assert_eq!(old.node_count, Some(8), "the RD's explicit width is kept");
        assert!(old.disabled_nodes.is_empty(), "every node starts enabled");

        reg.set_reported_nodes(&id, 4);
        let old = reg.get(&id).unwrap();
        assert_eq!(old.node_width(), 8, "still 8 — never silently overwritten");
        assert_eq!(
            old.node_drift().map(|d| (d.reported, d.configured)),
            Some((4, 8)),
            "…but the console can flag it"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ── The GridFPV-plugin selection gate (#405) ──────────────────────────────

    /// The `Present` presence a healthy probe records.
    fn present() -> PluginPresence {
        PluginPresence::Present {
            plugin_version: "0.1.0".into(),
            rhapi_version: "1.4".into(),
            capabilities: vec!["hello".into()],
        }
    }

    #[test]
    fn a_mock_timer_is_always_selectable() {
        // #405 is RotorHazard-specific: the built-in Mock (and any created Mock) has no plugin to
        // require, and is what an unconfigured Director races out of the box.
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let mock = reg.get(&TimerId(MOCK_TIMER_ID.into())).unwrap();
        assert!(mock.selection_refusal().is_none());
        let created = reg.create(&sim_req("Extra Sim")).unwrap();
        assert!(created.selection_refusal().is_none());
    }

    #[test]
    fn an_rh_timer_is_selectable_only_once_its_plugin_probes_present() {
        let reg = TimerRegistry::new(None, 5, 2500).unwrap();
        let rh = reg
            .create(&rh_req("Field RH", "http://rh.local:5000"))
            .unwrap();

        // Never probed: a different problem ("connect it first"), not a missing plugin.
        assert_eq!(
            reg.get(&rh.id).unwrap().selection_refusal(),
            Some(SelectionRefusal::NotConnected)
        );

        reg.set_plugin(&rh.id, PluginPresence::Missing);
        assert_eq!(
            reg.get(&rh.id).unwrap().selection_refusal(),
            Some(SelectionRefusal::PluginMissing)
        );

        reg.set_plugin(
            &rh.id,
            PluginPresence::Incompatible {
                plugin_version: "0.0.1".into(),
                protocol_version: 99,
                reason: "protocol 99 is newer than this Director".into(),
            },
        );
        assert_eq!(
            reg.get(&rh.id).unwrap().selection_refusal(),
            Some(SelectionRefusal::PluginIncompatible)
        );

        // Only a Present plugin unlocks selection.
        reg.set_plugin(&rh.id, present());
        assert!(reg.get(&rh.id).unwrap().selection_refusal().is_none());

        // …and the presence can go away again (RH restarted without the plugin) — the timer
        // becomes unselectable, which is what the arm-time backstop keys off.
        reg.set_plugin(&rh.id, PluginPresence::Missing);
        assert_eq!(
            reg.get(&rh.id).unwrap().selection_refusal(),
            Some(SelectionRefusal::PluginMissing)
        );
    }

    #[test]
    fn each_refusal_names_the_timer_and_says_something_different() {
        // Three problems, three fixes: "connect it", "install it", "update it". Collapsing them
        // into one message is the bug this test exists to prevent. Every message names the timer
        // by its friendly name (repo display rule) and never by its id.
        let name = "Field RH";
        let id = "field-rh-ab12";
        let messages: Vec<String> = [
            SelectionRefusal::NotConnected,
            SelectionRefusal::PluginMissing,
            SelectionRefusal::PluginIncompatible,
        ]
        .into_iter()
        .map(|r| r.selection_message(name))
        .collect();

        for message in &messages {
            assert!(message.contains(name), "{message:?} must name the timer");
            assert!(
                !message.contains(id),
                "{message:?} must not leak the raw id"
            );
        }
        // Distinct copy, and each points at its own next action.
        assert!(messages[0].contains("Connect it"));
        assert!(messages[1].contains("Install it"));
        assert!(messages[2].contains("Update it"));
        let unique: std::collections::BTreeSet<&String> = messages.iter().collect();
        assert_eq!(unique.len(), 3, "the three refusals must not share copy");
    }

    #[test]
    fn the_arm_message_differs_from_the_selection_message() {
        // The arm-time backstop is a different situation — the selection was valid when it was
        // made and the plugin went away underneath it — so it gets its own copy.
        for refusal in [
            SelectionRefusal::NotConnected,
            SelectionRefusal::PluginMissing,
            SelectionRefusal::PluginIncompatible,
        ] {
            let arm = refusal.arm_message("Field RH");
            assert_ne!(arm, refusal.selection_message("Field RH"));
            assert!(arm.contains("Field RH"));
            assert!(arm.contains("arm"));
        }
    }

    #[test]
    fn a_corrupt_timers_file_degrades_to_just_the_mock() {
        let dir = std::env::temp_dir().join(format!("gridfpv-timers-bad-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(timers_path(&dir), b"not json at all").unwrap();
        let reg = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
        let list = reg.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id.0, MOCK_TIMER_ID);
        std::fs::remove_dir_all(&dir).ok();
    }
}
