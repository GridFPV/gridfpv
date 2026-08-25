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
    /// How many nodes/slots the timer has (race redesign Slice 4a) — the **heat-size cap**: a
    /// heat's lineup must be ≤ this. Additive; defaults to [`DEFAULT_NODE_COUNT`].
    #[serde(default = "default_node_count")]
    pub node_count: u32,
    /// The timer's **defined available channels** (race redesign Slice 4a): the raw-MHz channels,
    /// within its [`channel_capability`](Timer::channel_capability), that the Race Director has
    /// made available on this timer — the pool per-heat assignment allocates from, in preference
    /// order. Empty means none configured (assignment then allocates nothing). Additive — defaults
    /// empty for a pre-channel-model timer.
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
}

/// The `serde(default)` provider for [`Timer::node_count`] (a function because serde defaults must
/// be callable): the ubiquitous 8-node width.
fn default_node_count() -> u32 {
    DEFAULT_NODE_COUNT
}

impl Timer {
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
    /// The new timer's **node/slot count** (race redesign Slice 4a) — the heat-size cap. Optional;
    /// defaults to [`DEFAULT_NODE_COUNT`].
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
    /// A new **node/slot count** (race redesign Slice 4a), or `None` to leave it unchanged.
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
/// crate), exactly like `restart_requests`.
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
    /// exactly like `restart_requests`, and for the same layering reason: the live socket lives in
    /// `gridfpv-app`, above this crate, so the registry is the one seam the route and the
    /// connection driver already share.
    ///
    /// Its **own** lock rather than a field inside [`Registry`]: pushes land at 5 Hz per watched
    /// timer, and they must never contend with — or worse, be tempted to ride along with — the
    /// timer set's write path, which persists `timers.json`. Nothing in here is ever written to
    /// disk, restored on boot, or turned into an [`Event`](gridfpv_events::Event); the whole map
    /// evaporates when the Director exits, which is the point.
    signal: Arc<Mutex<HashMap<TimerId, TimerSignalState>>>,
}

/// The guarded interior: the timer map and where `timers.json` lives.
struct Registry {
    /// `TimerId → Timer`. A `BTreeMap` so listing is deterministic (the Mock is listed
    /// first explicitly regardless).
    timers: BTreeMap<TimerId, Timer>,
    /// Directory `timers.json` is persisted under; `None` ⇒ in-memory only (no data dir).
    data_dir: Option<PathBuf>,
    /// **Pending RotorHazard restart requests** (issue #386), in request order — the RD asked, from
    /// the guided plugin install, that these timers re-execute their RotorHazard server so it
    /// re-imports its `plugins/` directory.
    ///
    /// A hand-off queue, not state: the connection layer that owns the live sockets lives in
    /// `gridfpv-app`, *above* this crate, so a route here cannot call it. The manual connection hold
    /// solves the same layering problem with a flag ([`Timer::manual_connect`]); a restart is an
    /// **edge** rather than a level, so it is a drained queue instead — the reconciler takes each
    /// request exactly once ([`TimerRegistry::take_restart_requests`]) and emits it onto the live
    /// connection. In-memory only, and never persisted: a Director restart must not re-fire an
    /// RD's restart from a previous session.
    restart_requests: Vec<TimerId>,
    /// **Pending calibration writes** (#355), in request order — enter/exit thresholds the RD set
    /// on the Tune page that have not yet been emitted onto a live socket.
    ///
    /// The same hand-off queue `restart_requests` is, for the same layering reason: the live
    /// sockets live in `gridfpv-app`, *above* this crate, so the RD-gated route here cannot emit.
    /// The reconciler drains it exactly once
    /// ([`TimerRegistry::take_calibration_requests`]) and fires the emits.
    ///
    /// **Coalesced per `(timer, node)`, last write wins per threshold.** A slider dragged twice
    /// before a drain should put the *latest* value on the timer, not replay a stale one after it —
    /// and a node whose enter and exit both moved in the same tick travels as one entry carrying
    /// both. This queue is only the in-flight buffer; the durable record of what GridFPV decided is
    /// [`Timer::calibration`], written at accept time (D27).
    ///
    /// In-memory only, and never persisted: a Director restart must not replay an RD's tuning from
    /// a previous session onto whatever timer happens to be plugged in now.
    calibration_requests: Vec<PendingCalibration>,
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
            node_count: DEFAULT_NODE_COUNT,
            available_channels: crate::channels::RACEBAND_MHZ.to_vec(),
            plugin: None,
            manual_connect: false,
            calibration: Vec::new(),
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
                    timers.insert(timer.id.clone(), timer);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Registry {
                timers,
                data_dir,
                restart_requests: Vec::new(),
                calibration_requests: Vec::new(),
            })),
            signal: Arc::new(Mutex::new(HashMap::new())),
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
            node_count: request.node_count.unwrap_or(DEFAULT_NODE_COUNT),
            available_channels: request.available_channels.clone().unwrap_or_default(),
            plugin: None,
            manual_connect: false,
            calibration: Vec::new(),
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
            timer.node_count = node_count;
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
    /// ([`take_restart_requests`](Self::take_restart_requests)) and fires the emit. The queue is
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
        if !reg.restart_requests.contains(id) {
            reg.restart_requests.push(id.clone());
        }
        Ok(timer)
    }

    /// Take every pending restart request (issue #386), leaving the queue empty — the connection
    /// reconciler's drain. Each request is handed out **exactly once**: if no live connection is
    /// found for it the request is dropped (and logged), never re-queued for a later connection.
    pub fn take_restart_requests(&self) -> Vec<TimerId> {
        std::mem::take(&mut self.write().restart_requests)
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
    /// The **race-phase refusal** — never move a detection threshold under a live race — is not
    /// here: it needs the event log, so it lives in the route
    /// (`EventRegistry::heat_in_progress_on_timer`), exactly as it does for
    /// [`request_restart`](Self::request_restart).
    pub fn request_calibration(
        &self,
        id: &TimerId,
        request: &CalibrationRequest,
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
        if request.node >= timer.node_count {
            return Err(TimerError(format!(
                "{:?} has {} nodes — there is no node {} to calibrate",
                timer.name,
                timer.node_count,
                // Display the node the way the page labels it (1-based), per the repo display rule.
                request.node + 1
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

        // Then queue the *application* of it. Coalesced per (timer, node): a drag that lands twice
        // before a drain applies the latest value once, rather than replaying a stale one after it.
        match reg
            .calibration_requests
            .iter_mut()
            .find(|p| &p.timer == id && p.node == request.node)
        {
            Some(pending) => {
                if enter_at.is_some() {
                    pending.enter_at = enter_at;
                }
                if exit_at.is_some() {
                    pending.exit_at = exit_at;
                }
            }
            None => reg.calibration_requests.push(PendingCalibration {
                timer: id.clone(),
                node: request.node,
                enter_at,
                exit_at,
            }),
        }
        reg.persist()?;

        Ok(CalibrationDispatch {
            timer: id.clone(),
            node: request.node,
            enter_at,
            exit_at,
        })
    }

    /// Take every pending calibration write (#355), leaving the queue empty — the connection
    /// reconciler's drain, and the twin of [`take_restart_requests`](Self::take_restart_requests).
    ///
    /// Each write is handed out **exactly once**: if no live connection is found for it the write
    /// is dropped (and logged), never re-queued. The RD sees that on the page as a threshold that
    /// never comes back confirmed, which is the honest outcome — the durable record of the value
    /// stays on [`Timer::calibration`] regardless.
    pub fn take_calibration_requests(&self) -> Vec<PendingCalibration> {
        std::mem::take(&mut self.write().calibration_requests)
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
/// Rejects a `node_count` of `0` (it caps every heat to **no** pilots — nothing could ever race),
/// an empty/whitespace RotorHazard URL (nothing to dial), and a Mock `laps` count beyond
/// [`MAX_MOCK_LAPS`] (a runaway sim). `node_count` is passed in so the merged value can be checked
/// on a partial edit.
pub fn validate_timer_config(kind: &TimerKind, node_count: u32) -> Result<(), String> {
    if node_count == 0 {
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
                0
            )
            .is_err()
        );
        // An empty / whitespace RotorHazard URL can never be dialed — rejected.
        assert!(validate_timer_config(&TimerKind::Rotorhazard { url: "   ".into() }, 8).is_err());
        // A runaway Mock laps count is rejected.
        assert!(
            validate_timer_config(
                &TimerKind::Mock {
                    laps: MAX_MOCK_LAPS + 1,
                    lap_ms: 100
                },
                8
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
                8
            )
            .is_ok()
        );
        assert!(
            validate_timer_config(
                &TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into()
                },
                8
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
        assert_eq!(mock.node_count, DEFAULT_NODE_COUNT);
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
            assert_eq!(created.node_count, 4);

            let reopened = TimerRegistry::new(Some(dir.clone()), 5, 2500).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.channel_capability, fixed);
            assert_eq!(got.node_count, 4);
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
        assert_eq!(old.node_count, DEFAULT_NODE_COUNT);
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
        assert_eq!(updated.node_count, 6);
        assert_eq!(updated.available_channels, vec![5800, 5820, 5840]);
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
