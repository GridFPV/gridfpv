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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

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
}

/// The guarded interior: the timer map and where `timers.json` lives.
struct Registry {
    /// `TimerId → Timer`. A `BTreeMap` so listing is deterministic (the Mock is listed
    /// first explicitly regardless).
    timers: BTreeMap<TimerId, Timer>,
    /// Directory `timers.json` is persisted under; `None` ⇒ in-memory only (no data dir).
    data_dir: Option<PathBuf>,
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
            inner: Arc::new(RwLock::new(Registry { timers, data_dir })),
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
