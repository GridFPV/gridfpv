//! The live **RotorHazard connection layer** (#65, #105) — only compiled under `--features live`.
//!
//! A RotorHazard timer **connects when it is selected for the active event and stays connected**,
//! so the Director monitors its live link continuously and a drop-off is visible *before and
//! between* races — not only while a heat runs (#105). The earlier slice (#65) connected an RH
//! timer only for the duration of a `Running` heat and tore the socket down when the heat left
//! `Running`; that meant a selected-but-idle timer read `Configured` and a drop could not surface
//! until a race was underway.
//!
//! This module splits that responsibility into two pieces:
//!
//! 1. **The persistent connection** ([`RhConnection`]). One per *(active event, RH timer)* pair.
//!    A dedicated [`spawn_blocking`](tokio::task::spawn_blocking) driver thread connects to the RH
//!    server, drives the timer's [`TimerStatus`](gridfpv_server::timers::TimerStatus) through its
//!    lifecycle (`Connecting → Connected`), then **loops maintaining and monitoring** the link:
//!    on a drop it sets `Disconnected`/`Error` and **reconnects with backoff**, updating the status
//!    across attempts. While connected it continuously drains the translated event stream — when a
//!    heat is "armed" on the connection (see below) it appends the translated passes (remapped onto
//!    the heat lineup) into the event log; otherwise it discards them (idle monitoring). On cancel
//!    (the timer is deselected, the active event changes, or the Director shuts down) it stops any
//!    running race, disconnects, and leaves the timer [`Disconnected`].
//!
//! 2. **Race driving, decoupled from the connection.** A running heat does **not** open a socket
//!    of its own — it *uses the already-live connection*. The lifecycle splits across two bridge
//!    hooks so that **GridFPV owns all start/stop timing and RH is only a start/stop/get-data
//!    device** (no RH-side staging countdown or tone competing with Grid's start procedure):
//!    * at **Stage** the bridge [prepares](RhConnection::prepare) each selected RH connection —
//!      zero RH's current-format staging (no staging hold/tones) and reset RH to READY, well ahead
//!      of Grid's go;
//!    * at **Running** (the `Armed → Running` instant, when Grid's tone fires) the bridge
//!      [arms](RhConnection::arm_heat) the heat — the driver emits a single `stage_race` so RH
//!      begins recording **immediately** (no reset, no settle, no RH staging) and remaps its node
//!      seats onto the heat lineup; the driver thread then routes drained passes into the event log.
//!
//!    When the heat leaves `Running` the bridge [disarms](RhConnection::disarm) it — the race is
//!    stopped/cleared but the **connection stays alive** (and keeps reporting status).
//!
//! # Why a dedicated driver thread
//!
//! The RotorHazard transport's emit/poll are **blocking** — they `block_on` an internal runtime —
//! so they must never run on a tokio worker thread (that panics). The entire connection lifecycle
//! (connect → monitor → reconnect → stage → drain → stop → disconnect) therefore runs on one
//! dedicated `spawn_blocking` thread; the async side only holds a handle that flips shared atomics
//! (a cancel flag, and an "armed heat" slot). The driver checks those each loop and reacts on its
//! own thread, so nothing ever emits on a tokio worker.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::rotorhazard::transport::{
    DIRECTOR_PROTOCOL_VERSION, NodeTick, PluginHello, RotorHazardConnection,
};
use gridfpv_events::{AdapterId, CompetitorRef, Event};
use gridfpv_server::timers::{
    NodeReading, PluginPresence, SIGNAL_SAMPLE_INTERVAL, TimerId, TimerRegistry, TimerStatus,
};
use tokio::task::JoinHandle;

use super::PassSink;

/// How often the driver thread drains the RotorHazard connection's translated-event queue.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait, after staging an armed heat, for the RH race to reach RACING before giving up
/// on the wait (the drain loop still runs regardless — this only bounds the staging settle).
const STAGE_SETTLE: Duration = Duration::from_secs(15);

/// How often to RE-EMIT `stage_race` while staged-but-not-RACING inside the settle window — a
/// busy RH silently drops a stage ("status is not 'ready'"); without the retry the race never
/// starts on the RH side and no passes are ever recorded. Paced ABOVE RH's stock staging
/// sequence (~3-4s with unzeroed delays) so a retry can never land mid-staging and restart the
/// countdown it is waiting on; with the instant-start prepare applied, staging is sub-second
/// and the retry only ever fires on a genuinely dropped stage.
const STAGE_RETRY_INTERVAL: Duration = Duration::from_secs(5);

/// How long the driver keeps routing drained events into a **finishing** heat's sink after it
/// stopped the RH race (marshaling path-2). Stopping the race drives RH to `DONE`, which
/// auto-triggers the dense `current_marshal_data` / `save_laps` → `race_list` → `get_pilotrace`
/// marshal pull; those round-trips take a moment on RH's gevent loop, so we keep the heat's sink
/// armed this long to capture the resulting [`Event::SignalHistory`] into the right heat's log
/// before clearing the slot. Generous enough for the per-pilotrace pull chain, still brief.
const FINISH_DRAIN_SETTLE: Duration = Duration::from_secs(3);

/// How long the driver waits, at heat-end, for RotorHazard's `heat_data` response (after
/// `ensure_savable_heat`'s `add_heat`/`load_data`) before giving up on selecting a savable heat and
/// stopping the race anyway. Bounds the case where an older/quirky RH never answers `heat_data` —
/// the finish proceeds and the dense pull simply no-ops (the coarse trace still stands).
const ENSURE_HEAT_TIMEOUT: Duration = Duration::from_secs(3);

/// The minimum backoff between reconnect attempts after a dropped/failed connection.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(500);

/// The maximum backoff between reconnect attempts (the backoff doubles up to this ceiling).
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(10);

/// How long the connection can drain no events before it probes liveness with a fresh `load_data`.
/// RH pushes asynchronously, so a healthy idle link is silent; the probe distinguishes "idle" from
/// "dropped" without depending on transport-level disconnect callbacks.
const IDLE_PROBE_INTERVAL: Duration = Duration::from_secs(5);

/// How long to wait, after connecting, for the GridFPV plugin's `gridfpv_hello_ack` (D16, S1). A
/// plugin-equipped RH replies near-instantly (`wait_for_plugin` returns as soon as it lands); a
/// stock RH never answers, so this bounds how long we wait before declaring the plugin *missing*
/// and entering the maintain loop. Kept short — the only cost is a one-time per-connect delay
/// against a stock RH, which then gets the guided-install prompt anyway.
const PLUGIN_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// How long to wait, after connecting, for a **stock** RotorHazard to say how many nodes it has
/// (#412) — the fallback path, when no GridFPV plugin answered the handshake with a seat count.
///
/// `connect` asks for `frequency_data` in its warm-up `load_data`, so a healthy RotorHazard answers
/// within a frame or two and [`wait_for_reported_nodes`] returns as soon as it lands. The timeout
/// only bounds the case where nothing answers, which leaves GridFPV on its configured width — where
/// it was before #412 — so it is deliberately short.
///
/// [`wait_for_reported_nodes`]: gridfpv_adapters::rotorhazard::transport::RotorHazardConnection::wait_for_reported_nodes
const NODE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);

/// A timer's **friendly name** for an operator log line, falling back to its raw id only if the
/// registry no longer holds it (CLAUDE.md: friendly names everywhere, raw ids as a last resort).
///
/// These lines are what an RD reads when a link drops or a filter could not be cleared, and
/// `"bench-rotorhazard-xvb27q"` does not tell them which box on the bench to go and look at.
fn timer_name(timers: &TimerRegistry, id: &TimerId) -> String {
    timers
        .get(id)
        .map(|t| t.name)
        .unwrap_or_else(|| id.0.clone())
}

/// One node of a **heat's channel plan** (race redesign Slice 4a, #421): the node the engine
/// allocated a frequency to, that frequency, and the catalog label to put on RotorHazard's own
/// screen beside it.
///
/// The twin of [`ChannelWrite`] for the *heat* write path rather than the Tune page's bench one,
/// and it carries a label for exactly the same reason: without one RotorHazard's UI shows a bare
/// `5880` — or worse, keeps the *previous* channel's label against a changed frequency — and the
/// RD who cross-checks GridFPV against RH's screen cannot tell a display quirk from a node that
/// never retuned.
///
/// `band`/`channel` are `None` for a **custom** raw MHz the catalog has no name for. The emit then
/// carries the frequency alone (see [`ChannelWrite`]): an honest absence, never an invented label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuneNode {
    /// The node to tune, 0-based (RotorHazard's `seat_index`) — the pilot's real seat, not their
    /// lineup position (#412).
    pub node: u64,
    /// The centre frequency to tune it to, in raw MHz — what the engine allocated.
    pub mhz: u16,
    /// The catalog band to label it with on RotorHazard (`"Raceband"`), if the catalog knows one.
    pub band: Option<String>,
    /// The catalog channel label (`"R7"`), if the catalog knows one.
    pub channel: Option<String>,
}

/// A **pending tune** the driver applies on its next loop (race redesign Slice 4a): the per-node
/// channel plan the engine allocated for the staging heat, shared from the async
/// [`RhConnection::tune`] caller to the blocking driver thread. `None` ⇒ nothing pending.
type TuneSlot = Arc<Mutex<Option<Vec<TuneNode>>>>;

/// A **pending prepare** the driver applies on its next loop: when a heat is **Staged** the bridge
/// asks the connection to ready RH for an instant start — zero the current format's staging delays
/// (so `stage_race` has no RH-side hold/tones) and reset RH to a clean READY state. Shared from the
/// async [`RhConnection::prepare`] caller to the blocking driver thread; `true` ⇒ a prepare is due.
type PrepareSlot = Arc<AtomicBool>;

/// A **pending seat assignment** the driver applies on its next loop: when a heat is **Staged** the
/// bridge hands the connection the heat's `(node_index, callsign)` bind so the driver seats each
/// bound pilot onto its RH node (`seat_heat`) before racing — so RH records *and* attributes passes
/// (the laps-attribute fix). Shared from the async [`RhConnection::seat`] caller to the blocking
/// driver thread; `None` ⇒ nothing pending.
type SeatSlot = Arc<Mutex<Option<Vec<(u64, String)>>>>;

/// A **pending timer restart** the driver fires on its next loop (#386): the RD asked, from the
/// guided plugin install, that RotorHazard re-execute itself so it re-imports its plugins. Shared
/// from the async [`RhConnection::restart`] caller to the blocking driver thread; `true` ⇒ a
/// restart is due. The driver emits `restart_server` and lets the ordinary drop → backoff →
/// reconnect → re-probe path do the rest.
type RestartSlot = Arc<AtomicBool>;

/// One queued **calibration write** the driver applies on its next loop (#355): a node and whichever
/// of its enter/exit thresholds the RD moved, already clamped by the server route.
///
/// The app-crate twin of `gridfpv_server::timers::PendingCalibration`, restated here so this crate's
/// slot type does not leak a `TimerId` it does not need — the connection already knows which timer
/// it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CalibrationWrite {
    /// The node to calibrate, 0-based (RotorHazard's `seat_index`).
    pub node: u64,
    /// The enter threshold to set, or `None` to leave it alone.
    pub enter_at: Option<u32>,
    /// The exit threshold to set, or `None` to leave it alone.
    pub exit_at: Option<u32>,
    /// Whether the route accepted this write with an **open-practice** heat racing on the timer
    /// (#355, #398) — so the driver's armed-heat backstop below must let it through.
    pub during_open_practice: bool,
}

/// **Pending calibration writes** the driver applies on its next loop (#355): the enter/exit
/// thresholds the RD set on the Tune page, shared from the async
/// [`RhConnection::calibrate`] caller to the blocking driver thread.
///
/// A **queue**, not a single slot like [`TuneSlot`]: the page writes per threshold on interaction
/// end, so several nodes can be pending in one reconcile tick, and each is a distinct emit. Pushes
/// coalesce per node (last value wins per threshold) so a drag that lands twice before a drain
/// applies the latest value once rather than replaying a stale one after it.
type CalibrationSlot = Arc<Mutex<Vec<CalibrationWrite>>>;

/// One queued **capture** the driver fires on its next loop (#355): the RD pressed Capture on one
/// of a node's two thresholds, and RotorHazard is being asked to *measure* the level rather than
/// being told it.
///
/// The app-crate twin of `gridfpv_server::timers::PendingCapture`, restated here for the same
/// reason [`CalibrationWrite`] is — the connection already knows which timer it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureWrite {
    /// The node to capture on, 0-based (RotorHazard's `seat_index`).
    pub node: u64,
    /// Whether this is the **enter** threshold (`cap_enter_at_btn`) or the exit one
    /// (`cap_exit_at_btn`). A `bool` rather than the server crate's enum because this crate sits
    /// *above* that one and the driver's only question is which of two emits to make.
    pub enter: bool,
    /// Whether the route accepted this capture with an **open-practice** heat racing on the timer
    /// (#355, #398) — so the driver's armed-heat backstop lets it through. A capture ends by
    /// *setting* a threshold, so it needs exactly the gate a typed level does.
    pub during_open_practice: bool,
}

/// **Pending captures** the driver fires on its next loop (#355), shared from the async
/// [`RhConnection::capture`] caller to the blocking driver thread.
///
/// A queue like [`CalibrationSlot`], and **deliberately not coalesced**: a second press is a second
/// measurement the RD asked for, not a restatement of a value. The server registry has already
/// refused the one case where two would collide (a capture of that threshold already running on
/// that node), which RotorHazard itself rejects in silence.
type CaptureSlot = Arc<Mutex<Vec<CaptureWrite>>>;

/// How long after a capture emit the driver fires the `enter_and_exit_at_levels` readback (#355).
///
/// RotorHazard samples for `CAP_ENTER_EXIT_AT_MILLIS` (3000 ms, identical on v4.3.0 and v4.4.0),
/// then sleeps 25 ms, writes its profile and pushes the level to the hardware. Asking before that is
/// asking for the old value and would report the capture as not landed while it was still running.
/// The slack covers the sleep and the write.
const CAPTURE_READBACK_DELAY: Duration = Duration::from_millis(3_400);

/// One queued **channel write** the driver applies on its next loop (#413): a node, the channel it
/// should be listening on, and the catalog label to put on RotorHazard's own screen alongside it.
///
/// The app-crate twin of `gridfpv_server::timers::PendingChannel`, restated here for the same
/// reason [`CalibrationWrite`] is — the connection already knows which timer it is.
///
/// `band`/`channel` are owned `String`s rather than `&str` because the write crosses from the async
/// reconciler onto the blocking driver thread and outlives the registry read that produced it. They
/// are `None` for a **custom** raw MHz the catalog has no name for: the emit then carries the
/// frequency alone, which is honest, rather than a label GridFPV invented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelWrite {
    /// The node to tune, 0-based (RotorHazard's `seat_index`).
    pub node: u64,
    /// The centre frequency to tune it to, in raw MHz.
    pub mhz: u16,
    /// The catalog band to label it with on RotorHazard (`"Raceband"`), if the catalog knows one.
    pub band: Option<String>,
    /// The catalog channel label (`"R7"`), if the catalog knows one.
    pub channel: Option<String>,
    /// Whether the route accepted this write with an **open-practice** heat racing on the timer
    /// (#398) — so the driver's armed-heat backstop below must let it through.
    pub during_open_practice: bool,
}

/// **Pending channel writes** the driver applies on its next loop (#413): the channels the RD
/// picked on the Tune page, shared from the async [`RhConnection::set_channel`] caller to the
/// blocking driver thread.
///
/// A **queue** for the same reason [`CalibrationSlot`] is, and distinct from [`TuneSlot`] on
/// purpose: `TuneSlot` is the *heat's* whole-timer channel plan, pushed at Stage and legitimately
/// overwriting everything; this is one node at a time from the bench. Pushes coalesce per node.
type ChannelSlot = Arc<Mutex<Vec<ChannelWrite>>>;

/// The RH heat id **seated** for the current arming, if seating succeeded (the laps-attribute fix):
/// a fresh RH heat built at Stage with the bound pilots assigned + made current, so RH records +
/// attributes passes. Held by [`drive`] **outside** its reconnect loop (not a `maintain`-local) so it
/// **survives a mid-race reconnect**, and shared into [`maintain`] so the finish-time dense save reads
/// it to reuse the seated heat (already current + savable) rather than adding a separate empty heat.
type SeatedHeatSlot = Arc<Mutex<Option<u64>>>;

/// A heat armed onto a live RH connection: the lineup its node seats remap onto, and a flag the
/// driver flips once it has staged the RH race for this arming (so a re-drain doesn't re-stage).
struct ArmedHeat {
    /// The running heat's lineup, in seeding order; node `n`'s passes attribute to `lineup[n]`.
    lineup: Vec<CompetitorRef>,
    /// The sink (the event's log) translated passes are appended through while armed.
    sink: PassSink,
    /// Set by the driver once it has staged the RH race for this arming.
    staged: bool,
    /// Set once RH confirmed **RACING for this arming** (its READY/DONE → RACING transition —
    /// the adapter's `SessionStarted`). Lap records drained BEFORE this are a stale replay:
    /// a busy RH ("stage while status is not 'ready'") re-broadcasts the PREVIOUS race's
    /// `current_laps` snapshot, and remapping those into the new heat recorded the last
    /// race's laps as this race's passes (observed live: a fresh heat opening with lap 6+).
    /// Lives in the shared slot so a mid-race reconnect (where RH re-sends the LIVE race's
    /// snapshot for the carried adapter to dedup) keeps flowing.
    started: bool,
    /// Set by [`disarm`](RhConnection::disarm) when the heat left `Running`: the driver finishes the
    /// heat by **stopping the RH race** (driving it to `DONE`, which auto-triggers the dense
    /// `current_marshal_data`/`get_pilotrace` marshal pull — marshaling path-2), keeps routing the
    /// resulting [`Event::SignalHistory`] into this heat's sink for a short settle, then clears the
    /// slot. Without the stop, RH stays `RACING` between heats and the dense history is never pulled
    /// into the finishing heat's log — only the coarse streamed samples survive.
    finishing: bool,
    /// Set by the driver the instant it has **fired the heat-end dense save** (the
    /// `ensure_savable_heat → set_current_heat → stop_race` dance) for this arming, so it runs
    /// **exactly once**. This guard lives in the *shared* slot (not a `maintain`-local) deliberately:
    /// the dense pull's burst of socket emits can itself drop the link, and on reconnect the driver
    /// re-enters [`maintain`] with the same still-`finishing` slot — without this flag it would
    /// re-run the whole add_heat/set_current_heat/stop_race dance every reconnect, looping heat after
    /// heat, re-flooding+resetting the socket and killing the live race (the #250 regression). Once
    /// `done` is set, the finish is never re-triggered by a re-sent `DONE`, a reconnect, or a
    /// maintain re-entry; only the (local) settle drain remains, after which the slot is cleared.
    done: bool,
}

/// Render an error together with its full `source()` chain as `"top: cause: root-cause"`.
///
/// `rust_socketio::Error`'s Display is lossy for the connect path: its
/// `IncompleteResponseFromEngineIo(rust_engineio::Error)` variant carries no `{0}`, so it prints the
/// bare string "EngineIO Error" and drops the wrapped engine.io/reqwest cause — a refused TCP
/// connect, a handshake reject, a TLS failure, and a timeout all collapse to that one opaque line.
/// Walking `std::error::Error::source()` recovers the real reason so a connect-failure log is
/// actionable (e.g. distinguishes "RH not running on :5000 (connection refused)" from a genuine
/// handshake regression).
fn error_chain(err: &dyn std::error::Error) -> String {
    let mut out = err.to_string();
    let mut source = err.source();
    while let Some(cause) = source {
        out.push_str(": ");
        out.push_str(&cause.to_string());
        source = cause.source();
    }
    out
}

/// One persistent live RotorHazard connection for a *(active event, RH timer)* pair (#105).
///
/// Owns a dedicated `spawn_blocking` driver thread that connects on construction, maintains and
/// monitors the link (reconnecting with backoff on a drop), and drives a race onto the *existing*
/// connection when a heat is armed. The async handle here only flips shared atomics the driver
/// observes; dropping/[`cancel`](Self::cancel)ling it tears the connection down on the driver
/// thread.
pub struct RhConnection {
    /// The cancel flag the driver polls; flipped on [`cancel`](Self::cancel) / drop.
    cancel: Arc<AtomicBool>,
    /// Set when this connection is being SUPERSEDED by a new one for the same timer (an
    /// active-event switch): the exiting driver must then leave the shared timer status alone —
    /// its async teardown used to stomp `Disconnected` over the successor's `Connecting`/
    /// `Connected`, and the failover logic read the healthy new primary as down.
    yield_status: Arc<AtomicBool>,
    /// The armed-heat slot: `Some` while a heat is racing on this connection, else `None`.
    armed: Arc<Mutex<Option<ArmedHeat>>>,
    /// A **pending tune** the driver applies on its next loop (race redesign Slice 4a): the per-node
    /// `(node_index, frequency_mhz)` assignment the engine allocated for the staging heat. Set by
    /// [`tune`](Self::tune) (called when a heat is Staged), drained + emitted on the driver thread
    /// (`set_frequency` per node) so the device tunes its nodes to the assigned channels.
    tune: TuneSlot,
    /// A **pending prepare** the driver applies on its next loop (Grid owns all timing): set by
    /// [`prepare`](Self::prepare) when a heat is **Staged**, the driver zeroes RH's current-format
    /// staging and resets RH to READY so the eventual `stage_race` (at Grid's go) starts RH recording
    /// instantly, with no RH-side staging hold/tones.
    prepare: PrepareSlot,
    /// A **pending seat assignment** the driver applies on its next loop (the laps-attribute fix):
    /// set by [`seat`](Self::seat) when a heat is **Staged**, the driver seats each bound pilot
    /// (`(node_index, callsign)`) onto its RH node (`seat_heat`) so RH records + attributes passes
    /// — without it RH races an empty-pilot heat and rejects every crossing ("Pilot not defined").
    seat: SeatSlot,
    /// A **pending restart** the driver fires on its next loop (#386): set by
    /// [`restart`](Self::restart) from the guided plugin install, the driver emits RotorHazard's
    /// `restart_server` so RH re-executes and re-imports its `plugins/` directory. The socket then
    /// drops, this driver reconnects with backoff, and the reconnect's plugin probe republishes the
    /// timer's `PluginPresence` — `Missing → Present` with no extra plumbing.
    restart: RestartSlot,
    /// **Pending calibration writes** the driver applies on its next loop (#355): set by
    /// [`calibrate`](Self::calibrate) from the Tune page, the driver emits RotorHazard's
    /// `set_enter_at_level` / `set_exit_at_level` and then asks for the `enter_and_exit_at_levels`
    /// readback — RH echoes neither write, so the readback is the only confirmation there is.
    calibration: CalibrationSlot,
    /// **Pending channel writes** the driver applies on its next loop (#413): set by
    /// [`set_channel`](Self::set_channel) from the Tune page, the driver emits RotorHazard's
    /// `set_frequency` carrying the band/channel label as well as the frequency. No readback is
    /// needed here — unlike a threshold, every RotorHazard heartbeat already reports each node's
    /// current frequency, so the confirming value is on the feed the Tune page is polling anyway.
    channel: ChannelSlot,
    /// **Pending captures** the driver fires on its next loop (#355): set by
    /// [`capture`](Self::capture) from the Tune page, the driver emits RotorHazard's
    /// `cap_enter_at_btn` / `cap_exit_at_btn` and then, once the sampling window has closed, asks
    /// for the `enter_and_exit_at_levels` readback. RotorHazard *does* broadcast the captured level
    /// on its own (`node_enter_at_level`) — the readback is the second witness, because a gate's
    /// calibration is not a thing to stake on one unsolicited frame.
    capture: CaptureSlot,
    /// The driver thread's join handle, held so the spawned task is owned by this connection;
    /// teardown is cooperative via the `cancel` flag (the thread is blocking, so it cannot be
    /// aborted) — dropping the connection flips `cancel` and lets the thread exit on its own.
    _driver: JoinHandle<()>,
}

impl RhConnection {
    /// Open a persistent connection for `timer_id` at `url`, publishing status through `timers`.
    ///
    /// Spawns the driver thread immediately: it sets the timer `Connecting`, connects, then
    /// `Connected`, and loops maintaining the link. The connection is idle (monitoring only) until
    /// a heat is [armed](Self::arm_heat) onto it.
    pub fn open(timer_id: TimerId, url: String, timers: TimerRegistry) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let yield_status = Arc::new(AtomicBool::new(false));
        let armed: Arc<Mutex<Option<ArmedHeat>>> = Arc::new(Mutex::new(None));
        let tune: TuneSlot = Arc::new(Mutex::new(None));
        let prepare: PrepareSlot = Arc::new(AtomicBool::new(false));
        let seat: SeatSlot = Arc::new(Mutex::new(None));
        let restart: RestartSlot = Arc::new(AtomicBool::new(false));
        let channel: ChannelSlot = Arc::new(Mutex::new(Vec::new()));
        let calibration: CalibrationSlot = Arc::new(Mutex::new(Vec::new()));
        let capture: CaptureSlot = Arc::new(Mutex::new(Vec::new()));
        let driver = {
            let cancel = cancel.clone();
            let yield_status = yield_status.clone();
            let armed = armed.clone();
            let tune = tune.clone();
            let prepare = prepare.clone();
            let seat = seat.clone();
            let restart = restart.clone();
            let calibration = calibration.clone();
            let capture = capture.clone();
            let channel = channel.clone();
            tokio::task::spawn_blocking(move || {
                drive(
                    url,
                    timer_id,
                    timers,
                    cancel,
                    yield_status,
                    armed,
                    tune,
                    prepare,
                    seat,
                    restart,
                    calibration,
                    capture,
                    channel,
                );
            })
        };
        Self {
            cancel,
            yield_status,
            armed,
            tune,
            prepare,
            seat,
            restart,
            calibration,
            capture,
            channel,
            _driver: driver,
        }
    }

    /// **Prepare** this connection for an instant start (Grid owns all timing): the driver zeroes
    /// RH's current-format staging delays (no RH-side staging hold/tones) and resets RH to a clean
    /// READY state, so the eventual [`arm_heat`](Self::arm_heat) at Grid's go starts RH recording
    /// immediately. The bridge calls this when a heat is **Staged** — before the Armed hold and the
    /// start tone — so all the reset/format work happens well ahead of go and never races RH's own
    /// staging at the start instant (which is what the retired `STAGE_RESET_SETTLE` band-aid fought).
    pub fn prepare(&self) {
        self.prepare.store(true, Ordering::Relaxed);
    }

    /// **Tune** this connection's nodes to an assigned channel plan (race redesign Slice 4a): the
    /// engine allocates the channels, the adapter applies them (RE §7.3). `assignment` is the
    /// per-node [`TuneNode`] set for the staging heat — frequency **and** its catalog label (#421)
    /// — and the driver thread emits a `set_frequency` per node on its next loop (best-effort — a
    /// failed emit on a dropped link is logged, not fatal). The bridge calls this when a heat is
    /// **Staged**, before it arms/runs.
    pub fn tune(&self, assignment: Vec<TuneNode>) {
        let mut slot = self.tune.lock().expect("tune lock poisoned");
        *slot = Some(assignment);
    }

    /// **Seat** this connection's heat: hand the driver the heat's `(node_index, callsign)` bind so
    /// it seats each bound pilot onto its RH node before racing (the laps-attribute fix). The driver
    /// builds a fresh RH heat with these pilots assigned and makes it current, so RH records *and*
    /// attributes passes on the bound nodes (its pass gate dismisses a crossing on a node with no
    /// seated pilot). The bridge calls this when a heat is **Staged**, alongside `prepare`/`tune`,
    /// before it arms/runs. `seats` carries one entry per **bound** node; unbound nodes are omitted
    /// (left unseated — RH won't record there). An empty `seats` is a no-op (nothing to seat).
    pub fn seat(&self, seats: Vec<(u64, String)>) {
        let mut slot = self.seat.lock().expect("seat lock poisoned");
        *slot = Some(seats);
    }

    /// Arm a running heat onto this live connection: called at Grid's go (the `Armed → Running`
    /// instant). The driver emits a single `stage_race` so RH **starts recording immediately** — the
    /// connection was already reset to READY with zeroed staging by the Stage-time
    /// [`prepare`](Self::prepare), so there is no reset or staging hold here — then routes its
    /// translated passes (remapped onto `lineup`) into `sink`'s log. Replaces any previously armed
    /// heat (a newer running heat supersedes the prior one).
    pub fn arm_heat(&self, lineup: Vec<CompetitorRef>, sink: PassSink) {
        let mut slot = self.armed.lock().expect("armed-heat lock poisoned");
        *slot = Some(ArmedHeat {
            lineup,
            sink,
            staged: false,
            started: false,
            finishing: false,
            done: false,
        });
    }

    /// Disarm the current heat (it left `Running`): the driver **stops the RH race** so it reaches
    /// `DONE` — which auto-triggers RotorHazard's dense marshal-data pull (marshaling path-2) — keeps
    /// routing the resulting [`Event::SignalHistory`] into the finishing heat's log for a short
    /// settle, then clears the slot. The **connection stays alive** (and keeps reporting status)
    /// throughout. A no-op if nothing is armed. Marking `finishing` (rather than nulling the slot
    /// outright) is what lets the dense history land in the right heat's log before the slot clears.
    pub fn disarm(&self) {
        let mut slot = self.armed.lock().expect("armed-heat lock poisoned");
        if let Some(heat) = slot.as_mut() {
            heat.finishing = true;
        }
    }

    /// **Restart the RotorHazard server** behind this connection (#386) — the guided plugin
    /// install's last step, so the RD never has to open RotorHazard's own web UI.
    ///
    /// RotorHazard imports plugins **once at startup**, so a freshly-installed `plugins/gridfpv/`
    /// is inert until RH re-executes. The driver emits RH's unauthenticated `restart_server` on
    /// its next loop; from there the ordinary reconnect path does everything else — the socket
    /// drops, [`drive`] marks the timer `Disconnected` and retries with backoff (10s cap), and the
    /// reconnect **re-probes the plugin**, so [`PluginPresence`] flips `Missing → Present` by
    /// itself. That expected drop → reconnect is *not* a fault, and the console presents it as
    /// such.
    ///
    /// **Refused mid-race by the driver as well as by the route.** Restarting RH with a race on it
    /// takes the timing hardware down under the running heat; the server route gates the request on
    /// heat phase, and the driver additionally drops a request that arrives while a heat is armed on
    /// this connection (below) so a request racing an arm can never land on a live race.
    pub fn restart(&self) {
        self.restart.store(true, Ordering::Relaxed);
    }

    /// **Set a node's enter/exit detection thresholds** on this connection (#355) — the Tune page's
    /// write.
    ///
    /// Queues the write; the driver emits RotorHazard's `set_enter_at_level` /
    /// `set_exit_at_level` on its next loop and then fires the `enter_and_exit_at_levels` readback,
    /// which flows back through the same signal tap the Tune page polls. RH echoes neither write, so
    /// that readback is the *only* evidence a level landed — there is nothing synchronous to return
    /// here, and this deliberately returns nothing at all.
    ///
    /// Pushes **coalesce per node**: a slider dragged twice before the driver's next loop applies the
    /// latest value once rather than replaying a stale one after it. A write carrying neither
    /// threshold is dropped (the route already refuses one, so this is only a backstop).
    ///
    /// **Refused mid-race by the driver as well as by the route** — but only for a *scored* heat.
    /// The route decides that (it is the layer that can see the event log) and records its answer in
    /// [`CalibrationWrite::during_open_practice`]; this backstop only covers the window between that
    /// check and the emit. An open-practice write is passed through, because #398 excludes practice
    /// from scoring and tuning with pilots in the air is the page's whole workflow.
    pub fn calibrate(&self, write: CalibrationWrite) {
        if write.enter_at.is_none() && write.exit_at.is_none() {
            return;
        }
        let mut pending = self.calibration.lock().expect("calibration lock poisoned");
        match pending.iter_mut().find(|p| p.node == write.node) {
            Some(existing) => {
                if write.enter_at.is_some() {
                    existing.enter_at = write.enter_at;
                }
                if write.exit_at.is_some() {
                    existing.exit_at = write.exit_at;
                }
                // The freshest phase reading wins (see the registry's coalesce).
                existing.during_open_practice = write.during_open_practice;
            }
            None => pending.push(write),
        }
    }

    /// **Capture** one of a node's thresholds on this connection (#355) — the Tune page's third
    /// write, and the only one that does not carry a number.
    ///
    /// Queues it; the driver emits RotorHazard's `cap_enter_at_btn` / `cap_exit_at_btn` on its next
    /// loop. RotorHazard then opens a **three-second sampling window starting at that emit** and
    /// sets the threshold to the mean RSSI it saw across it — so the RD's pass has to happen inside
    /// the window, not before it.
    ///
    /// Nothing is returned, and nothing could be: the level does not exist yet. Confirmation is the
    /// captured level arriving on the tune feed — RotorHazard broadcasts it (`node_enter_at_level`)
    /// when the window closes, and the driver asks for the readback behind it.
    ///
    /// Pushes are **not coalesced**, unlike [`calibrate`](Self::calibrate): two presses are two
    /// measurements, and the server registry has already refused the only pair that would collide.
    ///
    /// **Refused mid-race by the driver as well as by the route**, and by the same rule the
    /// calibration write uses: a *scored* heat blocks it (a capture ends by setting a threshold, so
    /// it changes what counts as a lap just as surely), open practice does not.
    pub fn capture(&self, write: CaptureWrite) {
        self.capture
            .lock()
            .expect("capture lock poisoned")
            .push(write);
    }

    /// **Set a node's channel** on this connection (#413) — the Tune page's other write.
    ///
    /// Queues the write; the driver emits RotorHazard's `set_frequency` on its next loop, carrying
    /// the catalog band/channel alongside the frequency so RotorHazard's own screen reads
    /// `Raceband R7` rather than a bare number (the RD validates this by refreshing that page).
    ///
    /// **No readback is fired, and none is needed.** Unlike a threshold — which RotorHazard never
    /// echoes, and which therefore had to be *asked* for — every RH heartbeat already reports each
    /// node's current frequency, so the confirming value arrives on the very feed the Tune page is
    /// polling. The page confirms a channel exactly as it confirms a level: by seeing it come back.
    ///
    /// Pushes **coalesce per node**: a dropdown changed twice before the driver's next loop tunes
    /// the node once, to the latest value.
    ///
    /// **Refused mid-race by the driver as well as by the route**, and by the same rule the
    /// calibration write uses: only a *scored* heat blocks it. The route owns that judgement (it is
    /// the layer that can see the event log) and stamps its answer in
    /// [`ChannelWrite::during_open_practice`]; this backstop only covers the window between that
    /// check and the emit.
    pub fn set_channel(&self, write: ChannelWrite) {
        let mut pending = self.channel.lock().expect("channel lock poisoned");
        match pending.iter_mut().find(|p| p.node == write.node) {
            Some(existing) => *existing = write,
            None => pending.push(write),
        }
    }

    /// Tear the connection down: stop any race, disconnect, leave the timer `Disconnected`. Called
    /// when the timer is deselected, the active event changes, or the Director shuts down.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Cancel this connection because a NEW connection for the same timer is replacing it (an
    /// active-event switch): the exiting driver yields the shared timer status to its successor
    /// (see [`yield_status`](Self::yield_status)).
    pub fn cancel_superseded(&self) {
        self.yield_status.store(true, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Drop for RhConnection {
    fn drop(&mut self) {
        // A dropped connection (the reconcile map removed it) must still tear down on its thread.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Map the transport's latest per-node readings onto the registry's crate-boundary twin (#355 S2a).
///
/// A field-for-field copy, and deliberately so: the live socket lives here in `gridfpv-app` and the
/// registry lives in `gridfpv-server` *below* it, so the two cannot share one type without pointing
/// the dependency arrow the wrong way. The mapping is the seam, and it is the only place the two
/// shapes meet.
fn readings(ticks: Vec<NodeTick>) -> Vec<NodeReading> {
    ticks
        .into_iter()
        .map(|tick| NodeReading {
            seen: tick.seen,
            rssi: tick.rssi,
            frequency_mhz: tick.frequency_mhz,
            loop_time_micros: tick.loop_time_micros,
            crossing: tick.crossing,
            crossed: tick.crossed,
            node_peak_rssi: tick.node_peak_rssi,
            node_nadir_rssi: tick.node_nadir_rssi,
            pass_peak_rssi: tick.pass_peak_rssi,
            pass_nadir_rssi: tick.pass_nadir_rssi,
            pass_count: tick.pass_count,
            enter_at: tick.enter_at,
            exit_at: tick.exit_at,
        })
        .collect()
}

/// The RH node index `node-{n}` encodes, if any. Passes from the adapter carry the stable node seat
/// handle; we remap it onto the running heat's lineup by this index.
fn node_index(competitor: &CompetitorRef) -> Option<usize> {
    competitor.0.strip_prefix("node-")?.parse().ok()
}

/// Remap one canonical RH [`Event`] onto the heat's lineup and the source adapter id, or `None` to
/// drop it. [`Event::Pass`]es feed the lap projection and the signal facts
/// ([`Event::SignalChunk`]/[`Event::SignalThresholds`], marshaling Slice 1) feed the signal-trace
/// projection; each is keyed on a node seat (`node-{n}`), attributed to `lineup[n]` and re-stamped
/// with `adapter`. Facts for a node outside the lineup (an idle seat) are dropped, as are the
/// adapter's lifecycle / `CompetitorSeen` events (the heat lineup is already established by the
/// control path).
fn remap(event: Event, lineup: &[CompetitorRef], adapter: &AdapterId) -> Option<Event> {
    match event {
        Event::Pass(mut pass) => {
            let index = node_index(&pass.competitor)?;
            let competitor = lineup.get(index)?.clone();
            pass.adapter = adapter.clone();
            pass.competitor = competitor;
            Some(Event::Pass(pass))
        }
        Event::SignalChunk(mut chunk) => {
            let index = node_index(&chunk.competitor)?;
            chunk.competitor = lineup.get(index)?.clone();
            chunk.adapter = adapter.clone();
            Some(Event::SignalChunk(chunk))
        }
        Event::SignalThresholds(mut t) => {
            let index = node_index(&t.competitor)?;
            t.competitor = lineup.get(index)?.clone();
            t.adapter = adapter.clone();
            Some(Event::SignalThresholds(t))
        }
        Event::SignalHistory(mut h) => {
            // The dense post-race history (RH `current_marshal_data`) is keyed on a node seat exactly
            // like a chunk; remap it onto the heat's lineup pilot so the signal-trace projection's
            // prefer-dense rule supersedes the coarse streamed chunks for the right competitor.
            let index = node_index(&h.competitor)?;
            h.competitor = lineup.get(index)?.clone();
            h.adapter = adapter.clone();
            Some(Event::SignalHistory(h))
        }
        _ => None,
    }
}

/// Decide whether to fire the heat-end dense save **right now**, and atomically claim it so it can
/// fire **exactly once per arming**. Returns `true` (and flips the heat's shared `done` flag) only
/// when: a heat is armed, it is `finishing`, it has not already fired (`!done`), and no settle is in
/// flight (`!settle_pending`). On every other call it returns `false`.
///
/// The once-only guarantee lives in the *shared* `done` flag (on [`ArmedHeat`], persisted across
/// reconnects) rather than a `maintain`-local: the dense pull's burst of emits can drop the link, so
/// the driver re-enters [`maintain`] (fresh local `finish_deadline = None`) with the same still-
/// `finishing` slot. Were the guard local, that re-entry would re-run the whole add_heat /
/// set_current_heat / stop_race dance — looping heat after heat, re-flooding+resetting the socket and
/// stopping the live race so no laps land (the #250 regression). Claiming on the shared flag makes a
/// re-sent `DONE`, a reconnect, and a maintain re-entry all no-ops. `settle_pending` blocks a second
/// claim within the *same* invocation while the post-save drain settle is still running.
fn claim_finish(heat: Option<&mut ArmedHeat>, settle_pending: bool) -> bool {
    match heat {
        Some(h) => claim_finish_flags(h.finishing, &mut h.done, settle_pending),
        None => false,
    }
}

/// The pure once-only decision behind [`claim_finish`], over the raw flags so it is unit-testable
/// without a live `ArmedHeat` (which needs a `PassSink`/log). Flips `done` and returns `true` iff
/// the save should fire now: `finishing && !done && !settle_pending`.
fn claim_finish_flags(finishing: bool, done: &mut bool, settle_pending: bool) -> bool {
    if finishing && !*done && !settle_pending {
        *done = true;
        true
    } else {
        false
    }
}

/// The persistent driver: connect → `Connected` → maintain/monitor → reconnect on drop, until
/// cancelled, then disconnect and leave `Disconnected` (#105). Runs on a dedicated blocking thread.
#[allow(clippy::too_many_arguments)]
fn drive(
    url: String,
    timer_id: TimerId,
    timers: TimerRegistry,
    cancel: Arc<AtomicBool>,
    yield_status: Arc<AtomicBool>,
    armed: Arc<Mutex<Option<ArmedHeat>>>,
    tune: TuneSlot,
    prepare: PrepareSlot,
    seat: SeatSlot,
    restart: RestartSlot,
    calibration: CalibrationSlot,
    capture: CaptureSlot,
    channel: ChannelSlot,
) {
    let mut backoff = RECONNECT_BACKOFF_MIN;
    // The RH heat id **seated** for the current arming (the laps-attribute fix), if seating
    // succeeded: a fresh RH heat built at Stage with the bound pilots assigned + made current. Lives
    // here in `drive` — **outside** the reconnect loop — so it **survives a mid-race reconnect**: the
    // finish-time dense save reads it (in `maintain`) to reuse the seated heat (already current +
    // savable) rather than adding a separate empty heat. A `maintain`-local would reset to `None` on
    // every reconnect, so the finish would then wrongly add an empty heat and clobber the still-
    // current seated one. Cleared when a new prepare begins (a fresh arming). `None` ⇒ no seated heat.
    let seated_heat: SeatedHeatSlot = Arc::new(Mutex::new(None));
    // The adapter is created **once** and reused across every (re)connection (#105). Its dedup +
    // `last_race_status` must be continuous across a reconnect: on a mid-race drop the running heat
    // stays `staged` (it lives in the shared `armed` Mutex, not the adapter), so the staging block
    // below does NOT reset RH, and RotorHazard re-sends the in-progress `current_laps` snapshot on
    // the new socket. A *fresh* adapter (empty dedup) would re-emit every replayed lap as a Pass,
    // and the lap projection — which does not dedup by sequence — would turn those into duplicate
    // laps (double-count). Reusing the adapter keeps that snapshot deduped.
    //
    // Combined invariant with #156 (the RACING-transition dedup reset):
    //   * Mid-race reconnect: the adapter persists, so `last_race_status == RACING`. RH's re-sent
    //     `race_status=RACING` is NOT a transition (`previous == Some(RACING)`) → no SessionStarted
    //     re-emit, no #156 reset → the re-sent `current_laps` are deduped (no double-count). ✓
    //   * New race / cross-heat: a real READY/DONE→RACING transition DOES fire #156, resetting dedup
    //     so the next heat (whose lap_number restarts at 0) ingests its own fresh laps. ✓
    let mut carry_adapter = Some(RotorHazardAdapter::new());
    while !cancel.load(Ordering::Relaxed) {
        timers.set_status(&timer_id, TimerStatus::Connecting);
        // Reuse the carried adapter (preserving dedup/last_race_status across reconnects); only on
        // the first attempt is it `Some` from above — every later iteration re-seeds it from the
        // adapter recovered out of the previous connection's `disconnect`.
        let adapter = carry_adapter.take().unwrap_or_default();
        let conn = match RotorHazardConnection::connect(&url, adapter) {
            Ok(conn) => conn,
            Err(e) => {
                // The connect attempt failed: surface Error, back off, and retry (unless cancelled).
                // The adapter was consumed by the failed `connect`; start the next attempt fresh.
                // (A connect failure means no socket and no replayed snapshot, so there is nothing
                // to dedup against — a fresh adapter is correct and #156 re-seeds on the next race.)
                //
                // Log the full error *chain*, not just `rust_socketio`'s top-level Display: its
                // `IncompleteResponseFromEngineIo` variant renders as the bare, useless string
                // "EngineIO Error" (no `{0}`), which hides the actual cause — a refused TCP connect
                // (RH not running / wrong port), an engine.io handshake reject, a TLS fault, or a
                // timeout all collapse to the same opaque line. `error_chain` walks `source()` so the
                // log tells a dead `:5000` apart from a genuine handshake failure at a glance.
                eprintln!(
                    "gridfpv: RotorHazard connect failed for {:?}: {}",
                    timer_name(&timers, &timer_id),
                    error_chain(&e)
                );
                timers.set_status(&timer_id, TimerStatus::Error);
                if sleep_unless_cancelled(backoff, &cancel) {
                    break;
                }
                backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
                continue;
            }
        };
        timers.set_status(&timer_id, TimerStatus::Connected);
        backoff = RECONNECT_BACKOFF_MIN;
        // Drop any restart request that outlived the previous connection. The route only accepts
        // one while the timer reads `Connected`, but the link can drop in the window between that
        // check and the reconciler's drain — and `maintain` only consumes the flag while a socket
        // is up. Without this the request would survive backoff and fire on a reconnect minutes
        // later, restarting the timer with no one asking: exactly the surprise `restart`'s own
        // contract says cannot happen.
        restart.store(false, Ordering::Relaxed);
        // …and any calibration write that outlived the previous connection, for the same reason.
        // The route only accepts one while the timer reads `Connected`, but the link can drop in the
        // window before the reconciler drains. A threshold is not held over for a future connection:
        // firing it minutes later, onto whatever RotorHazard came back, would move a detector nobody
        // asked to move — and the RD would have long since seen the value fail to come back and
        // re-set it by hand.
        calibration
            .lock()
            .expect("calibration lock poisoned")
            .clear();
        // …and any capture that outlived it, for a sharper version of the same reason. A capture
        // fired minutes later would sample a gate with nothing flying through it and set the
        // threshold off the noise floor — a *worse* outcome than not capturing, because the RD
        // would have no reason to suspect it.
        capture.lock().expect("capture lock poisoned").clear();

        // Probe for the GridFPV plugin (D16, S1): `connect` already emitted `gridfpv_hello`, so
        // wait briefly for the `gridfpv_hello_ack`. Present-&-compatible / incompatible / missing
        // drives the Director's required-with-guided-install UX. Re-probed on every (re)connect.
        let hello = conn.wait_for_plugin(PLUGIN_PROBE_TIMEOUT);
        // **Ask the timer how many nodes it has** (#412), preferring the most direct answer:
        //
        //   1. the GridFPV plugin's `gridfpv_hello_ack` — `len(rhapi.interface.seats)`, straight
        //      off the live interface, and it rides the handshake we already waited for;
        //   2. `frequency_data.fdata` — one entry per node on stock RotorHazard (identical on
        //      v4.3.0 and v4.4.0), requested in `connect`'s warm-up `load_data`;
        //   3. `enter_and_exit_at_levels` — explicitly sliced `[:num_nodes]`, the fallback.
        //
        // RotorHazard publishes no `num_nodes` scalar on the socket at all, which is why this is a
        // length rather than a field.
        //
        // It is an **observation**, recorded as one: it never touches `Timer::node_count` or
        // `Timer::disabled_nodes` (D27 — a value read from a timer is evidence about the timer, not
        // an input to a decision). A timer that comes back reporting a different width shows up as
        // `Timer::node_drift` for the RD; a node the RD disabled stays disabled, because nothing on
        // this path can re-enable one. Re-read on every (re)connect, like the plugin probe itself.
        let reported = hello
            .as_ref()
            .map(|h| h.node_count)
            .filter(|n| *n > 0)
            .or_else(|| conn.wait_for_reported_nodes(NODE_DISCOVERY_TIMEOUT));
        match reported {
            Some(nodes) => timers.set_reported_nodes(&timer_id, nodes),
            None => eprintln!(
                "gridfpv: RotorHazard {:?} did not report a node count; GridFPV keeps its \
                 configured width",
                timer_name(&timers, &timer_id)
            ),
        }

        // **Make RotorHazard stop refereeing lap length** (#407), before any heat can be armed.
        //
        // RH runs its own minimum-lap rule underneath GridFPV's, and its behaviour flag can
        // *discard* a sub-minimum crossing outright. A discarded crossing never arrives, so
        // GridFPV's per-round floor (D26/#409) never runs on it, marshaling has nothing to
        // restore, and #397's rejected-crossing tone stays silent for exactly the crossing the RD
        // most needs to hear about. D27: GridFPV owns this decision; the timer's copy of it is
        // neutralised and what GridFPV applied is recorded on GridFPV's side.
        //
        // The plugin does this in-process at load (and re-asserts it at every stage), in which
        // case this is a no-op that just reads the record back. Against a plugin older than
        // v0.4.0 — the field timer still runs v0.1.0 — the connection does it over the socket
        // instead. Either way a failure is announced through the adapter diagnostic sink, once,
        // naming the consequence; it is never a reason to refuse the connection, because a timer
        // whose filter could not be cleared is precisely one the RD needs to be *told* about.
        let min_lap = conn.ensure_min_lap_neutral();
        if min_lap.neutral {
            eprintln!(
                "gridfpv: RotorHazard {:?}: min-lap filter neutralised via {:?} (timer had \
                 MinLapSec={:?}, MinLapBehavior={:?}); GridFPV's per-round floor is the only \
                 min-lap rule in force",
                timer_name(&timers, &timer_id),
                min_lap.route,
                min_lap.found_secs,
                min_lap.found_behavior,
            );
        }

        let plugin = classify_plugin(hello);
        timers.set_plugin(&timer_id, plugin);

        // Maintain the live link until it drops or we are cancelled.
        let dropped = maintain(
            &conn,
            &cancel,
            ControlSlots {
                armed: &armed,
                tune: &tune,
                prepare: &prepare,
                seat: &seat,
                restart: &restart,
                calibration: &calibration,
                capture: &capture,
                channel: &channel,
                seated_heat: &seated_heat,
                timers: &timers,
                timer_id: &timer_id,
            },
        );

        // Stop any in-flight race and disconnect on the way out of this connection. `disconnect`
        // returns the adapter so the next reconnect reuses its dedup state (the #105 fix).
        conn.stop_race().ok();
        carry_adapter = Some(conn.disconnect());

        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // The link dropped (not a cancel): mark Disconnected and reconnect after a short backoff.
        if dropped {
            eprintln!(
                "gridfpv: RotorHazard connection lost for {:?}; reconnecting",
                timer_name(&timers, &timer_id)
            );
            timers.set_status(&timer_id, TimerStatus::Disconnected);
            if sleep_unless_cancelled(backoff, &cancel) {
                break;
            }
            backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
        }
    }
    // Cancelled: leave the timer Disconnected (deselected / shutdown) — UNLESS a successor
    // connection for this same timer already owns the status (an active-event switch): this
    // teardown runs async on the driver thread and used to land AFTER the successor's
    // `Connecting`/`Connected`, mislabeling a healthy timer and tripping failover.
    if !yield_status.load(Ordering::Relaxed) {
        timers.set_status(&timer_id, TimerStatus::Disconnected);
    }
}

/// Classify the GridFPV-plugin handshake result (D16, S1) into the [`PluginPresence`] the timer
/// surfaces: no answer → `Missing` (a stock RH — the guided install applies); an answer whose
/// `gridfpv_*` protocol matches the Director → `Present`; otherwise → `Incompatible` (the guided
/// install offers the matching build). Compatibility is the protocol version only — the plugin and
/// Director build versions can differ freely as long as the wire protocol agrees.
fn classify_plugin(hello: Option<PluginHello>) -> PluginPresence {
    match hello {
        None => PluginPresence::Missing,
        Some(h) if h.protocol_version == DIRECTOR_PROTOCOL_VERSION => PluginPresence::Present {
            plugin_version: h.plugin_version,
            rhapi_version: h.rhapi_version,
            capabilities: h.capabilities,
        },
        Some(h) => PluginPresence::Incompatible {
            reason: format!(
                "the timer's GridFPV plugin speaks protocol v{}, but this Director supports v{}",
                h.protocol_version, DIRECTOR_PROTOCOL_VERSION
            ),
            plugin_version: h.plugin_version,
            protocol_version: h.protocol_version,
        },
    }
}

/// Maintain one established connection: drain translated events each tick (routing passes into the
/// armed heat's log, or discarding them while idle), stage a freshly-armed heat, and probe liveness
/// when idle. Returns `true` if the link appears to have **dropped** (so the caller reconnects),
/// `false` if it exited because of cancellation.
/// The per-connection control slots the driver and the outside world share: everything the RD can
/// ask of a live link. Bundled rather than passed loose because they travel together and always
/// have — six of [`maintain`]'s arguments were these, which is both noisy and easy to transpose at
/// a call site (they are mostly the same two or three types).
struct ControlSlots<'a> {
    /// The heat currently armed on this connection, if any.
    armed: &'a Mutex<Option<ArmedHeat>>,
    /// A pending channel assignment to push to the timer.
    tune: &'a Mutex<Option<Vec<TuneNode>>>,
    /// Set when the timer should be prepared for a heat.
    prepare: &'a AtomicBool,
    /// A pending seat → pilot binding to push before racing.
    seat: &'a Mutex<Option<Vec<(u64, String)>>>,
    /// Set when the RD has asked RotorHazard to restart (#386).
    restart: &'a AtomicBool,
    /// Calibration writes the RD queued from the Tune page (#355), one entry per node.
    calibration: &'a Mutex<Vec<CalibrationWrite>>,
    /// Captures the RD queued from the Tune page (#355), one entry per press.
    capture: &'a Mutex<Vec<CaptureWrite>>,
    /// Channel writes the RD queued from the Tune page (#413), one entry per node. Distinct from
    /// [`tune`](ControlSlots::tune): that is the *heat's* whole-timer channel plan pushed at Stage,
    /// this is one node retuned from the bench.
    channel: &'a Mutex<Vec<ChannelWrite>>,
    /// The heat whose seats are currently bound on the timer.
    seated_heat: &'a Mutex<Option<u64>>,
    /// The timer registry — where the **tune-telemetry lease** lives (#355 S2a). The registry is
    /// the one seam this crate and the RD-gated route in `gridfpv-server` already share, exactly as
    /// it is for the manual connection hold and the restart queue.
    timers: &'a TimerRegistry,
    /// Which timer this connection is, for reading that lease and pushing snapshots back.
    timer_id: &'a TimerId,
}

fn maintain(conn: &RotorHazardConnection, cancel: &AtomicBool, slots: ControlSlots<'_>) -> bool {
    let ControlSlots {
        armed,
        tune,
        prepare,
        seat,
        restart,
        calibration,
        capture,
        channel,
        seated_heat,
        timers,
        timer_id,
    } = slots;
    let mut last_activity = Instant::now();
    let mut probed_since_activity = false;
    let mut stage_deadline: Option<Instant> = None;
    // Paces the busy-RH stage retry (see STAGE_RETRY_INTERVAL); seeded in the past so the
    // first retry fires as soon as it is needed.
    let mut last_stage_retry = Instant::now() - STAGE_RETRY_INTERVAL;
    // The settle window for a **finishing** heat (disarmed): the deadline by which the heat's sink
    // stays armed after the RH race is stopped, so the DONE-triggered dense marshal pull lands in
    // the right heat's log before the slot clears. `None` ⇒ no heat is finishing.
    let mut finish_deadline: Option<Instant> = None;
    // When this connection last sampled the tune-telemetry tap (#355 S2a). Seeded in the past so
    // the first sample lands on the tick after the subscription opens rather than 200 ms later.
    let mut last_signal_sample = Instant::now() - SIGNAL_SAMPLE_INTERVAL;
    // When a **capture's** threshold readback is due (#355). RotorHazard samples for three seconds
    // before it has a level at all, so unlike the calibration write the readback cannot be fired
    // beside the emit — asking early would read back the OLD level and report a capture that is
    // still running as one that did not land. Set to the latest outstanding capture's deadline, so
    // a batch of presses costs one `load_data` rather than one each. `None` ⇒ nothing outstanding.
    let mut capture_readback_at: Option<Instant> = None;

    while !cancel.load(Ordering::Relaxed) {
        // The source of truth for a drop (#105): `rust_socketio` runs with `.reconnect(false)`, so a
        // dropped socket fires the transport's `close`/`error` handlers, which flip `is_alive` to
        // false. (An emit alone can't be trusted — a buffering client returns `Ok` on a dead link.)
        if !conn.is_alive() {
            return true;
        }

        // Fire a pending **restart** (#386): the RD asked, from the guided plugin install, that
        // RotorHazard re-execute itself so it picks up a freshly-dropped-in `plugins/gridfpv/`.
        //
        // Belt-and-braces refusal while a heat is armed: the server route already gates the request
        // on heat phase (Staged/Armed/Running/Unofficial), but the request travels through the timer
        // registry to the reconciler to this thread, so an arm could in principle land in between.
        // Restarting RH under a live race takes the timing hardware down mid-heat, so the driver
        // drops the request rather than firing it — the RD can ask again once the heat is done.
        //
        // Otherwise it is one fire-and-forget emit: RH re-execs, the socket drops within a moment,
        // and the caller's reconnect loop takes over (marking `Disconnected`, retrying with backoff,
        // and re-probing the plugin on the new connection). We do NOT return `true` here — the drop
        // is detected by the same `is_alive` check as any other, so there is one drop path, not two.
        if restart.swap(false, Ordering::Relaxed) {
            if armed.lock().expect("armed-heat lock poisoned").is_some() {
                eprintln!(
                    "gridfpv: ignoring a RotorHazard restart request — a heat is armed on this \
                     connection; restarting mid-race would take the timer down with the race on it"
                );
            } else {
                eprintln!(
                    "gridfpv: restarting RotorHazard (restart_server) — the connection will drop \
                     and reconnect on its own, re-probing the GridFPV plugin"
                );
                if conn.restart_server().is_err() {
                    // A failed emit on a supposedly-live socket signals a drop; reconnect and let the
                    // RD retry (the restart may or may not have been taken — the reconnect tells us).
                    return true;
                }
            }
        }

        // Apply any pending **calibration** (#355): the RD moved an enter/exit threshold on the Tune
        // page and it goes to the timer now — there is no Apply button, so this is the whole write
        // path, once per adjustment.
        //
        // Belt-and-braces refusal while a heat is armed — but **only for a scored heat**, which is
        // the one place this differs from the restart above. The server route owns that judgement
        // (it is the layer that can see the event log) and stamps its answer on each write; this
        // backstop only covers the window between that check and this emit, since the write travels
        // route → registry → reconciler → this thread and an arm could land in between.
        //
        // Loosening it here is not optional: the route now ACCEPTS a write during open practice
        // (#398 excludes practice from scoring, and tuning with pilots in the air is the page's
        // whole point). A backstop that still dropped it would report a write as dispatched that
        // never landed — the exact failure the readback design exists to make impossible.
        let pending_calibration: Vec<CalibrationWrite> = {
            let mut slot = calibration.lock().expect("calibration lock poisoned");
            std::mem::take(&mut *slot)
        };
        if !pending_calibration.is_empty() {
            let heat_armed = armed.lock().expect("armed-heat lock poisoned").is_some();
            let mut emitted = 0usize;
            let mut refused = 0usize;
            for write in &pending_calibration {
                if heat_armed && !write.during_open_practice {
                    refused += 1;
                    continue;
                }
                if let Some(level) = write.enter_at {
                    if conn.set_enter_at_level(write.node, level).is_err() {
                        return true;
                    }
                }
                if let Some(level) = write.exit_at {
                    if conn.set_exit_at_level(write.node, level).is_err() {
                        return true;
                    }
                }
                emitted += 1;
            }
            if refused > 0 {
                eprintln!(
                    "gridfpv: ignoring {refused} calibration write(s) — a scored heat is armed on \
                     this connection; moving a detection threshold mid-race changes what counts as \
                     a lap"
                );
            }
            if emitted > 0 {
                // The readback, and the reason a write is confirmable at all: RotorHazard emits
                // NOTHING in reply to `set_enter_at_level` / `set_exit_at_level` (verified on
                // v4.3.0 and v4.4.0), so without this ask the Tune page would never see the level
                // it just sent come back — and "sent" would be indistinguishable from "landed",
                // which is the #403 failure class. One `load_data` covers every node in this batch.
                if conn.request_thresholds().is_err() {
                    return true;
                }
            }
        }

        // Fire any pending **captures** (#355): the RD pressed Capture on a node's threshold and
        // RotorHazard is being asked to measure the level rather than being told it.
        //
        // This is the same write path as the calibration block above — same queue seam, same
        // armed-heat backstop, same open-practice exemption — with one difference that shapes the
        // rest of the block: **the emit does not produce a value.** RotorHazard opens a three-second
        // sampling window at the emit, averages the node's RSSI across it, and only then sets the
        // threshold. So the readback is *scheduled*, not fired here.
        //
        // The armed-heat backstop applies for exactly the reason it does to a typed level: a
        // capture ends by setting a threshold, so it changes what counts as a lap under a scored
        // heat just as surely. Open practice is allowed — and is the natural moment to capture,
        // since the pass a capture needs is one a pilot is already flying (#398).
        let pending_captures: Vec<CaptureWrite> = {
            let mut slot = capture.lock().expect("capture lock poisoned");
            std::mem::take(&mut *slot)
        };
        if !pending_captures.is_empty() {
            let heat_armed = armed.lock().expect("armed-heat lock poisoned").is_some();
            let mut started = 0usize;
            let mut refused = 0usize;
            for write in &pending_captures {
                if heat_armed && !write.during_open_practice {
                    refused += 1;
                    continue;
                }
                let emitted = if write.enter {
                    conn.capture_enter_at_level(write.node)
                } else {
                    conn.capture_exit_at_level(write.node)
                };
                if emitted.is_err() {
                    return true;
                }
                started += 1;
            }
            if refused > 0 {
                eprintln!(
                    "gridfpv: ignoring {refused} capture(s) — a scored heat is armed on this \
                     connection; a capture sets a detection threshold when it finishes, which \
                     would change what that heat counts as a lap"
                );
            }
            if started > 0 {
                // Schedule the readback past the end of RotorHazard's sampling window. RH also
                // broadcasts the captured level itself (`node_enter_at_level`, folded by the
                // transport), so this is the second witness rather than the only one — but a gate's
                // calibration is not something to stake on one unsolicited frame, and a capture the
                // RD flew a pass for deserves both.
                let due = Instant::now() + CAPTURE_READBACK_DELAY;
                capture_readback_at = Some(match capture_readback_at {
                    Some(existing) if existing > due => existing,
                    _ => due,
                });
            }
        }
        // …and fire it when it comes due. Deliberately outside the block above: the emit and the
        // readback are separated by three seconds of RotorHazard sampling, and this loop turns over
        // every 20 ms in between.
        if let Some(due) = capture_readback_at {
            if Instant::now() >= due {
                capture_readback_at = None;
                if conn.request_thresholds().is_err() {
                    return true;
                }
            }
        }

        // Apply any pending **channel writes** (#413): the RD picked a channel for a node on the
        // Tune page. Same shape, same backstop and same reasoning as the calibration block above —
        // a *scored* heat blocks it (a receiver retuned mid-race takes the gate off the channel the
        // pilot is flying), open practice does not.
        //
        // The emit carries the catalog **band and channel** as well as the frequency: RotorHazard's
        // `on_set_frequency` stores them on the active profile, and without them its own UI shows a
        // bare number — which is what the RD is looking at when they refresh RH to check this
        // worked. There is deliberately no readback: every heartbeat already carries each node's
        // frequency, so the confirmation is already on the feed the Tune page polls.
        let pending_channels: Vec<ChannelWrite> = {
            let mut slot = channel.lock().expect("channel lock poisoned");
            std::mem::take(&mut *slot)
        };
        if !pending_channels.is_empty() {
            let heat_armed = armed.lock().expect("armed-heat lock poisoned").is_some();
            let mut refused = 0usize;
            for write in &pending_channels {
                if heat_armed && !write.during_open_practice {
                    refused += 1;
                    continue;
                }
                let label = write.band.as_deref().zip(write.channel.as_deref());
                if conn.set_frequency(write.node, write.mhz, label).is_err() {
                    return true;
                }
            }
            if refused > 0 {
                eprintln!(
                    "gridfpv: ignoring {refused} channel write(s) — a scored heat is armed on this \
                     connection; retuning a node mid-race takes the gate off the channel the pilot \
                     is flying"
                );
            }
        }

        // Apply a pending tune (race redesign Slice 4a): the bridge requested the device tune its
        // nodes to the staging heat's assigned channels. Emit a `set_frequency` per node; this is
        // best-effort (the engine has already allocated — applying is the adapter's half), so a
        // failed emit on a supposedly-live socket signals a drop the caller reconnects from.
        //
        // **This legitimately overwrites anything the Tune page set** (#413): a heat's channel
        // assignment is the race's, and the bench value does not get to win. The Tune page says so
        // rather than fighting it.
        //
        // The emit carries the catalog **band and channel** alongside the frequency, exactly as the
        // Tune page's write does (#421). It is resolved once, upstream, through the catalog's single
        // resolver (`gridfpv_server::channels::label_of`) and threaded here on the plan — nothing is
        // re-derived at the emit. `set_frequency` translates the catalog code into RotorHazard's own
        // vocabulary (`"R7"` → `{"b": "R", "c": 7}`); sending the code itself raised `ValueError` in
        // `on_set_frequency` and aborted the handler, so the frequency was never set at all. A
        // custom MHz the catalog cannot name travels as a bare frequency, never an invented label.
        let pending_tune = tune.lock().expect("tune lock poisoned").take();
        if let Some(assignment) = pending_tune {
            for node in assignment {
                let label = node.band.as_deref().zip(node.channel.as_deref());
                if conn.set_frequency(node.node, node.mhz, label).is_err() {
                    return true;
                }
            }
        }

        // Apply a pending **prepare** (Grid owns all timing): the bridge marked this connection at
        // the heat's **Stage** transition — well before Grid's go — so RH can be readied for an
        // *instant* start with no RH-side staging hold/tones. Two things, in order:
        //   1. zero the current format's staging delays (`prepare_instant_start`) so the eventual
        //      `stage_race` transitions straight to RACING — no staging tones, no fixed/random start
        //      delay, and `unlimited_time` so RH never auto-stops (Grid owns the stop);
        //   2. reset RH to a clean READY state (`stop_race` + `discard_laps`) so the start emit lands
        //      from a known-idle device.
        // Doing this at Stage (not at go) is what retires the `STAGE_RESET_SETTLE` band-aid: the
        // reset and the `stage_race` are now separated by the whole Armed hold (seconds), never the
        // same gevent tick, so there is no reset-vs-staging race to settle against. RH also no longer
        // runs its own staging sequence on top of Grid's start procedure — Grid's tone is the only go.
        if prepare.swap(false, Ordering::Relaxed) {
            if conn.prepare_instant_start().is_err() {
                return true;
            }
            conn.stop_race().ok();
            conn.discard_laps().ok();
            // A fresh prepare begins a new arming: drop any prior seated heat so this Stage's seat
            // (below) — or the finish-time fallback — applies cleanly.
            *seated_heat.lock().expect("seated-heat lock poisoned") = None;
            // Drop the reset-era event churn so it isn't remapped as race passes when a heat arms.
            let _ = conn.events();
        }

        // Apply a pending **seat** (the laps-attribute fix): the bridge handed this connection the
        // heat's `(node_index, callsign)` bind at Stage. Build a fresh RH heat with those pilots
        // seated and make it current, so RH **records and attributes** passes on the bound nodes —
        // without this RH races an empty-pilot heat and its pass gate dismisses every crossing
        // ("Pilot not defined"), the zero-laps bug. We seat AFTER the prepare reset (the reset's
        // `stop_race`/`discard_laps` don't touch heat rows, but ordering keeps RH idle while we set
        // the current heat). The seated heat is remembered so the finish-time dense save reuses it
        // (it is already current + savable) rather than adding a separate empty heat. Best-effort: a
        // seating that can't complete (a slow RH) leaves `seated_heat = None` and the flow falls back
        // to practice mode, which still records via RH's `current_heat is HEAT_ID_NONE` gate branch.
        let pending_seat = seat.lock().expect("seat lock poisoned").take();
        if let Some(seats) = pending_seat {
            match conn.seat_heat(&seats) {
                Ok(heat_id) => *seated_heat.lock().expect("seated-heat lock poisoned") = heat_id,
                // A failed emit on a supposedly-live socket signals a drop.
                Err(_) => return true,
            }
            // Drop the seating churn (heat_data/pilot_data/heat re-emits) so none is remapped as a
            // race pass when the heat arms.
            let _ = conn.events();
        }

        // Stage a freshly-armed heat once — **exactly at Grid's go** (the bridge arms on the
        // `Armed → Running` instant, when Grid's tone fires). The connection was already reset to
        // READY with zeroed staging by the Stage-time prepare above, so this is a single `stage_race`
        // emit with **no reset and no settle**: RH transitions straight to RACING with no RH-side hold
        // or tones. RH's race-start aligns with Grid's go, so each pass's `lap_time_stamp` (relative
        // to RH's start) maps onto Grid's race clock — and because Grid derives lap times as
        // pass-to-pass deltas, even RH's fixed `RACE_START_DELAY_EXTRA_SECS` prestage (a constant,
        // not socket-settable) cancels out and lap times stay correct.
        //
        // RH's pass gate (`server.py`'s `do_pass_record_callback`) records a crossing only when the
        // node has a *seated pilot* on the current heat, OR no heat is current (practice mode):
        // `(pilot_id is not None and pilot_id != PILOT_ID_NONE) or current_heat is HEAT_ID_NONE`.
        // The Stage-time **seat** above built a fresh heat with the bound pilots seated and made it
        // current, so each bound node records AND attributes its passes (and RH's "Racing heat …
        // pilots: …" log names the callsigns) — the laps-attribute fix. If seating could not complete
        // (`seated_heat` is `None`), the heat stays in practice mode (no current heat), which still
        // records via the `current_heat is HEAT_ID_NONE` branch (just unattributed on the RH side —
        // GridFPV remaps node→pilot itself). Either way the dense per-tick RSSI history accumulates on
        // the node interface during the race; the finish block below persists it (marshaling path-2),
        // reusing the seated heat when there is one rather than adding a separate empty heat.
        let mut just_staged = false;
        let do_stage = {
            let slot = armed.lock().expect("armed-heat lock poisoned");
            matches!(slot.as_ref(), Some(heat) if !heat.staged)
        };
        if do_stage {
            // Re-zero the staging delays of the format RH will ACTUALLY race, right before the
            // stage. The Stage-time prepare targeted the format that was current THEN — but
            // seating the heat can switch RH's effective format (a heat with a class races the
            // CLASS's format, RHRace's class_format_id override), whose stock multi-second
            // staging sequence then ran on top of Grid's start procedure: every race began
            // ~5-7s after Grid's go (lap stamps stayed self-consistent, so the skew was
            // invisible in results — but live laps, tones, and callouts all arrived that much
            // late; a DB-bloated RH stretched it past 15s). By now the seat's race_status has
            // long since folded, so `prepare_instant_start` targets the effective format —
            // and RH is READY here, which `alter_race_format` requires. Idempotent.
            if conn.prepare_instant_start().is_err() {
                return true;
            }
            // Drop any churn accumulated since the prepare so it isn't remapped as race passes.
            let _ = conn.events();
            if conn.stage_race().is_err() {
                // A failed emit on a supposedly-live socket signals a drop.
                return true;
            }
            // Mark the (still-armed) heat staged. A concurrent disarm/re-arm between the check above
            // and here is benign: a re-arm reset `staged` to false (we re-stage next loop), a disarm
            // cleared the slot (nothing to mark).
            let mut slot = armed.lock().expect("armed-heat lock poisoned");
            if let Some(heat) = slot.as_mut() {
                heat.staged = true;
            }
            just_staged = true;
            stage_deadline = Some(Instant::now() + STAGE_SETTLE);
            // A fresh heat staged over a still-finishing previous one (back-to-back heats): cancel
            // any pending finish settle so the new heat's slot is not cleared out from under it.
            finish_deadline = None;
        } else if armed.lock().expect("armed-heat lock poisoned").is_none() {
            // Nothing armed: clear any stale stage wait.
            stage_deadline = None;
        } else if stage_deadline.is_some() {
            // Staged but RH has not confirmed RACING yet (no SessionStarted this arming): a
            // busy RH — the previous race's dense save still settling — logs "Attempted to
            // stage race while status is not 'ready'" and DROPS the stage on the floor. The
            // race would then never start (and, before the pass gate above, the replayed old
            // snapshot contaminated the new heat). Re-emit the stage every couple of seconds
            // until RH takes it or the settle window gives up.
            let needs_restage = {
                let slot = armed.lock().expect("armed-heat lock poisoned");
                matches!(slot.as_ref(), Some(heat) if heat.staged && !heat.started)
            };
            if needs_restage && last_stage_retry.elapsed() >= STAGE_RETRY_INTERVAL {
                last_stage_retry = Instant::now();
                if conn.stage_race().is_err() {
                    return true;
                }
            }
        }
        if just_staged {
            last_activity = Instant::now();
            probed_since_activity = false;
        }

        // Finish a disarmed heat (marshaling path-2): the bridge marked the armed heat `finishing`
        // when it left `Running`. The race ran in practice mode (so live laps recorded), and the
        // node interface accumulated the dense per-tick RSSI history throughout. Now, at heat-END,
        // make a savable heat current and stop the race: stopping drives RH to DONE, and the
        // transport's DONE handler auto-emits `save_laps` -> `race_list` -> `get_pilotrace` (and the
        // aggregate `current_race_marshal` on newer RH) — which, with a current heat, persists and
        // returns that accumulated history. We keep the heat's sink armed through a settle window so
        // the resulting `SignalHistory` lands in THIS heat's log (the full-fidelity trace superseding
        // the coarse stream), then clear the slot (the connection stays alive).
        {
            // Fire the heat-end dense save **exactly once per arming**. The trigger is gated on three
            // independent conditions, all of which survive a reconnect: the heat is `finishing`, it
            // has not already fired (`!done`, the *shared* guard), and no settle is in flight locally.
            // We flip the shared `done` flag the instant we decide to fire — BEFORE any emit — so that
            // if the dense pull's emit burst drops the socket and the driver reconnects into a fresh
            // `maintain` with the same still-`finishing` slot, `done` is already set and the dance is
            // NOT re-run (the #250 looping/flapping regression). This makes the save idempotent: a
            // re-sent `DONE`, a reconnect, or a maintain re-entry can never re-create heats/rounds,
            // re-flood the socket, or re-stop the live race.
            let start_finish = {
                let mut slot = armed.lock().expect("armed-heat lock poisoned");
                claim_finish(slot.as_mut(), finish_deadline.is_some())
            };
            if start_finish {
                // The Stage-time seat already made a savable heat current (with the bound pilots
                // seated), so the DONE-triggered `save_laps` has a current heat to persist into — no
                // extra heat needed. Only when there is NO seated heat (seating couldn't complete, so
                // the race ran in practice mode) do we add+select a savable heat now, FIRST (while
                // still RACING), so the dense history still persists: request add_heat + the heat
                // list, wait for the `heat_data` response, then select synchronously on this thread
                // (keeping the heat-setup emits ordered and off the socket callback — an
                // emit-per-`heat_data` there floods + drops the link). Bounded so a quirky/older RH
                // that never answers doesn't stall the finish; the dense pull just no-ops then (the
                // coarse trace stands).
                let already_seated = seated_heat
                    .lock()
                    .expect("seated-heat lock poisoned")
                    .is_some();
                if !already_seated && conn.ensure_savable_heat().is_ok() {
                    let select_deadline = Instant::now() + ENSURE_HEAT_TIMEOUT;
                    loop {
                        // Keep draining so the `heat_data` handler runs; route any real passes that
                        // are still trickling in into the (still-armed) heat's log rather than drop
                        // them. (Use the same routing as the main drain below by deferring it — here
                        // we only need the handler to fire, so a discard of non-pass churn is fine;
                        // passes for the finishing heat are rare this late and the main drain catches
                        // any that remain on the next loop.)
                        let _ = conn.events();
                        if let Some(heat) = conn.take_savable_heat() {
                            conn.set_current_heat(heat).ok();
                            break;
                        }
                        if Instant::now() >= select_deadline {
                            break;
                        }
                        if sleep_unless_cancelled(Duration::from_millis(100), cancel) {
                            return false;
                        }
                    }
                }
                // Drive RH to DONE; the transport's DONE handler issues the dense marshal pull, now
                // with a current heat so `save_laps` persists the accumulated history.
                conn.stop_race().ok();
                finish_deadline = Some(Instant::now() + FINISH_DRAIN_SETTLE);
            } else if finish_deadline.is_none()
                && armed
                    .lock()
                    .expect("armed-heat lock poisoned")
                    .as_ref()
                    .is_some_and(|h| h.done)
            {
                // The save already fired (`done`) but this `maintain` invocation has no local settle —
                // i.e. the link dropped/reconnected mid-settle and we re-entered fresh. Do NOT re-fire
                // the dance (the guard above already prevents that); just restart the drain settle so
                // any dense `SignalHistory` re-pushed on the new socket still lands in this heat's log,
                // and the slot is eventually cleared rather than stranded `done`-but-armed forever.
                finish_deadline = Some(Instant::now() + FINISH_DRAIN_SETTLE);
            }
            if let Some(deadline) = finish_deadline {
                if Instant::now() >= deadline {
                    // Settle elapsed: the dense history has been drained into the heat's log. Clear
                    // the slot (heat fully disarmed) — the connection stays alive and idle-monitors.
                    *armed.lock().expect("armed-heat lock poisoned") = None;
                    finish_deadline = None;
                    stage_deadline = None;
                }
            }
        }

        // ---------------------------------------------------------------------------------------
        // Tune telemetry (#355 S2a). Three lines of policy, all of it here on the Director:
        //
        //  1. **The lease is the subscription.** Re-read every tick, so a Tune page that stopped
        //     polling — closed tab, dead browser, lost Wi-Fi — shuts the transport's pre-parse gate
        //     by itself within `SIGNAL_LEASE`. There is no state a client has to remember to clear.
        //  2. **Decimate here, not on arrival.** RotorHazard's heartbeat is 10 Hz on a stock timer
        //     and 100 Hz with its frequency scanner on (`HEARTBEAT_DATA_RATE_FACTOR` 5 → 50), so
        //     sampling the transport's last-value-wins store on our own fixed cadence is what makes
        //     the ring's time base mean something.
        //  3. **All nodes, unfiltered.** These readings go nowhere near `remap` — which drops every
        //     node outside the armed heat — because an unseated node's signal is exactly what an RD
        //     is looking for when a gate has stopped detecting.
        //
        // Nothing here touches `heat.sink`, and the readings are not `Event`s. There is no code
        // path from this block to a log.
        let wanted = timers.signal_wanted(timer_id);
        conn.set_signal_capture(wanted);
        if wanted && last_signal_sample.elapsed() >= SIGNAL_SAMPLE_INTERVAL {
            timers.push_signal(timer_id, &readings(conn.take_signal()));
            last_signal_sample = Instant::now();
        }

        // Drain whatever the transport has translated since the last tick.
        let drained = conn.events();
        if !drained.is_empty() {
            last_activity = Instant::now();
            probed_since_activity = false;
            let mut slot = armed.lock().expect("armed-heat lock poisoned");
            if let Some(heat) = slot.as_mut() {
                let adapter = heat.sink.adapter().clone();
                let mut saw_start = false;
                for event in drained {
                    if matches!(event, Event::SessionStarted { .. }) {
                        saw_start = true;
                        heat.started = true;
                    }
                    // Gate LAP RECORDS on RH having gone RACING for THIS arming: anything
                    // earlier is the previous race's snapshot replayed by a still-busy RH —
                    // remapping it used to contaminate the fresh heat with the last race's
                    // laps. (Processed in drain order, so passes in the same batch AFTER the
                    // RACING transition flow normally; signal facts always flow — a pre-start
                    // trace baseline is harmless and useful.)
                    if matches!(event, Event::Pass(_)) && !heat.started {
                        continue;
                    }
                    if let Some(remapped) = remap(event, &heat.lineup, &adapter) {
                        if heat.sink.append_event(remapped).is_err() {
                            // The log went away (event dropped at shutdown): stop draining.
                            return false;
                        }
                    }
                }
                if saw_start {
                    stage_deadline = None;
                }
            }
            // While idle (nothing armed) the drained events are monitoring-only and discarded.
        } else if stage_deadline.map(|d| Instant::now() >= d).unwrap_or(false) {
            // Gave up waiting for RACING after staging; keep draining steady-state.
            stage_deadline = None;
        } else if last_activity.elapsed() >= IDLE_PROBE_INTERVAL && !probed_since_activity {
            // A quiet link: probe liveness once. A failed emit means the socket has dropped.
            if conn.probe_liveness().is_err() {
                return true;
            }
            probed_since_activity = true;
        }

        if sleep_unless_cancelled(DRAIN_INTERVAL, cancel) {
            return false;
        }
    }
    false
}

/// Sleep `dur`, but wake early (in short slices) if `cancel` flips. Returns `true` if it woke
/// because of cancellation (so callers can stop promptly), `false` if it slept the full duration.
fn sleep_unless_cancelled(dur: Duration, cancel: &AtomicBool) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cancel.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{SignalChunk, SignalThresholds, SourceTime};

    fn lineup() -> Vec<CompetitorRef> {
        vec![CompetitorRef("Ace".into()), CompetitorRef("Bee".into())]
    }

    /// #380: an `eprintln!` **from this module** must land in the Director's log file.
    ///
    /// This is the regression guard for the whole issue. The connect-failure diagnostic a few
    /// hundred lines above — the `error_chain` line that tells a refused TCP connect apart
    /// from a TLS fault — is a plain `eprintln!`, and on the shipped Windows GUI-subsystem
    /// build stderr goes nowhere. It only reaches an RD because `crate::logging` shadows
    /// `eprintln!` for every module declared after it in `lib.rs`, and `macro_rules!` scope is
    /// *textual*: move that declaration below `pub mod source;`, or add a `use` that shadows
    /// it back, and this test fails instead of the field session.
    ///
    /// It asserts against the real resolved log file (no env mutation — `std::env::set_var` is
    /// unsafe in edition 2024 and this crate forbids unsafe), reading only the bytes appended
    /// after the marker is written, so it is safe under a parallel test runner.
    #[test]
    fn an_eprintln_from_this_module_reaches_the_log_file() {
        use std::io::{Seek, SeekFrom};

        let path = crate::logging::init().expect("a log file always resolves");
        let before = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);

        let marker = format!("gridfpv-380-marker-{}", std::process::id());
        eprintln!("gridfpv: RotorHazard connect failed for {marker:?}: <error chain>");

        let mut file = std::fs::File::open(path).expect("the log file is readable");
        file.seek(SeekFrom::Start(before)).expect("seekable");
        let mut tail = String::new();
        std::io::Read::read_to_string(&mut file, &mut tail).expect("readable tail");

        assert!(
            tail.contains(&marker),
            "the eprintln! did not reach {}; tail was {tail:?}",
            path.display()
        );
    }

    #[test]
    fn remap_attributes_signal_chunk_to_the_lineup_pilot() {
        // A trace chunk on node-1 is re-attributed to lineup[1] and re-stamped with the adapter id,
        // exactly like a pass — so the signal-trace projection groups it under the right pilot.
        let adapter = AdapterId("timer-7".into());
        let chunk = Event::SignalChunk(SignalChunk {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("node-1".into()),
            from: SourceTime::from_micros(0),
            period_micros: 100_000,
            rssi: vec![70, 150],
        });
        match remap(chunk, &lineup(), &adapter) {
            Some(Event::SignalChunk(c)) => {
                assert_eq!(c.competitor, CompetitorRef("Bee".into()));
                assert_eq!(c.adapter, adapter);
                assert_eq!(c.rssi, vec![70, 150]);
            }
            other => panic!("expected a remapped SignalChunk, got {other:?}"),
        }
    }

    #[test]
    fn remap_attributes_thresholds_and_drops_off_lineup_nodes() {
        let adapter = AdapterId("timer-7".into());
        let t = Event::SignalThresholds(SignalThresholds {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("node-0".into()),
            enter: 90,
            exit: 80,
        });
        match remap(t, &lineup(), &adapter) {
            Some(Event::SignalThresholds(t)) => {
                assert_eq!(t.competitor, CompetitorRef("Ace".into()));
                assert_eq!(t.adapter, adapter);
            }
            other => panic!("expected remapped SignalThresholds, got {other:?}"),
        }
        // A node beyond the (2-seat) lineup is dropped, like an idle-seat pass.
        let off = Event::SignalChunk(SignalChunk {
            adapter: AdapterId("rotorhazard".into()),
            competitor: CompetitorRef("node-5".into()),
            from: SourceTime::from_micros(0),
            period_micros: 100_000,
            rssi: vec![0],
        });
        assert!(remap(off, &lineup(), &adapter).is_none());
    }

    /// The heat-end dense save fires **exactly once per arming**, even across a reconnect (#250
    /// regression). The shared `done` flag — not a `maintain`-local — is what guarantees it: a
    /// finishing heat claims the save once; a second poll in the same invocation is gated by the
    /// settle; and crucially, after a simulated reconnect (the local settle resets to "not pending")
    /// the still-`finishing` heat must NOT re-fire because `done` persists in the shared slot. Pre-fix
    /// the local-only guard re-ran the add_heat/set_current_heat/stop_race dance on every reconnect,
    /// looping heats, flapping the socket, and stopping the live race so no laps landed.
    #[test]
    fn finish_fires_exactly_once_even_across_a_reconnect() {
        // Not finishing yet (heat still Running): never fires.
        let mut done = false;
        assert!(!claim_finish_flags(false, &mut done, false));
        assert!(!done);

        // Disarm → finishing. The first poll (no settle pending) claims the save and marks it done.
        assert!(
            claim_finish_flags(true, &mut done, false),
            "the first finish poll must fire the save"
        );
        assert!(done, "claiming the save must flip the shared `done` flag");

        // A second poll in the SAME maintain invocation, while the post-save settle is pending, does
        // not re-fire (settle_pending = true).
        assert!(
            !claim_finish_flags(true, &mut done, true),
            "the save must not re-fire while the post-save settle is still in flight"
        );

        // Simulate a RECONNECT: the dense pull dropped the link, the driver re-enters `maintain` with
        // a fresh local `finish_deadline = None` (settle_pending = false) but the SAME still-
        // `finishing` shared slot (`done` already true). It must NOT re-run the dance — this is the
        // exact #250 loop the fix closes.
        assert!(
            !claim_finish_flags(true, &mut done, false),
            "a reconnect must NOT re-fire the heat-end save once it has already fired (the #250 loop)"
        );

        // A FRESH arming (a new heat) resets `done` to false in `arm_heat`, so the next real finish
        // fires again — the once-only guard is per-arming, not per-connection.
        let mut next = false;
        assert!(
            claim_finish_flags(true, &mut next, false),
            "a brand-new arming's finish must fire (the guard is per-arming)"
        );
    }

    /// A failed connect to a dead port must produce an *actionable* log: the bare top-level
    /// `rust_socketio` Display is "EngineIO Error" (hides the cause), but `error_chain` walks the
    /// `source()` chain down to the real reason (here: the refused TCP connect). This is the
    /// regression-diagnosis fix — a dead `:5000` no longer looks identical to a handshake failure.
    #[test]
    fn error_chain_surfaces_the_real_connect_cause() {
        // Port 1 is reserved/unused on the loopback, so the connect is refused immediately.
        // (`RotorHazardConnection` isn't `Debug`, so match rather than `expect_err`.)
        let err =
            match RotorHazardConnection::connect("http://127.0.0.1:1", RotorHazardAdapter::new()) {
                Ok(_) => panic!("connecting to a dead port must fail"),
                Err(e) => e,
            };
        let chained = error_chain(&err);
        // The top-level Display alone is the useless opaque string...
        assert_eq!(err.to_string(), "EngineIO Error");
        // ...but the chain recovers the underlying cause (refused / no connection).
        assert!(
            chained.len() > "EngineIO Error".len(),
            "error_chain must add the underlying cause, got {chained:?}"
        );
        let lower = chained.to_lowercase();
        assert!(
            lower.contains("refused") || lower.contains("connect"),
            "error_chain should name the refused connect, got {chained:?}"
        );
    }
}
