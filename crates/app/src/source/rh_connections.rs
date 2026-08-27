//! The **RotorHazard connection set** + its reconciler (#105, #383) — `live`-gated.
//!
//! A RotorHazard timer **connects when it is selected for the active event and stays connected**
//! (#105), so its live link is monitored continuously and a drop-off surfaces *before and between*
//! races. It also connects when the RD **manually holds it** from the Timers menu (#383), with no
//! event involved at all. This module owns the set of persistent [`RhConnection`]s and the
//! background task that keeps that set in sync with the Director's state:
//!
//! - [`RhConnections`] — a shared `(Option<EventId>, TimerId) → LiveConnection` map. The value
//!   carries the **URL the connection was dialled with**, so an edited URL is noticed (#382); the
//!   key's event is `None` for a manual, event-independent connection (#383). The per-event source
//!   bridge consults it to **arm/disarm** a running heat onto the *already-live* connection (race
//!   driving decoupled from connecting); the reconciler opens/closes entries.
//! - [`spawn_rh_reconciler`] — polls the two inputs ([`wanted_connections`]) and reconciles: open a
//!   connection for every selected RotorHazard timer of the active event **and** every manually-held
//!   one, and close any connection no longer wanted (deselected, event no longer active, hold
//!   released). On the active event changing, the previous event's connections are dropped (left
//!   `Disconnected`) and the new event's selected RH timers connect.
//!
//! # The two inputs, and why the event wins
//!
//! A timer that is *both* manually held and selected by the active event holds exactly **one**
//! connection, under the **event** key — that is the key a running heat arms/tunes/seats on, so the
//! event's claim is the one that must exist. The manual entry is `cancel_superseded()`d into it (no
//! `Disconnected` flash, no double connection), and when the event lets the timer go the manual hold
//! takes it back the same way. The hold itself is never cleared implicitly: it is a diagnostic
//! control, and it lasts until the RD disconnects (see `TimerRegistry::set_manual_connect`).
//!
//! Beyond those two, an idle non-active event does not tie up a timer. (A non-active event's heat
//! can still be driven through its bridge, but it just finds no live connection to arm; RH racing is
//! for the event the Director is running.)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gridfpv_events::CompetitorRef;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{
    CaptureResolution, PendingTimerWrite, Timer, TimerId, TimerKind, TimerRegistry,
};
use tokio::task::JoinHandle;

use super::PassSink;
use super::rotorhazard::{RhConnection, TuneNode};

/// How often the reconciler polls the active event + its selected timers to sync the live set.
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

/// The shared set of persistent RotorHazard connections, keyed by *(claimant, RH timer)* (#105,
/// #383).
///
/// Cloning shares the one map (`Arc<Mutex<…>>`), so the reconciler (which opens/closes connections)
/// and the per-event source bridges (which arm/disarm a running heat onto a live connection) act on
/// the same connections. A connection exists while its timer is selected for the active event, or
/// while the RD manually holds it from the Timers menu.
#[derive(Clone, Default)]
pub struct RhConnections {
    inner: Arc<Mutex<HashMap<ConnKey, LiveConnection>>>,
}

/// What identifies one live connection: **who wants it** plus the RH timer.
///
/// `Some(event)` is the active event that selects the timer (#105) — the key a running heat's
/// bridge arms, tunes and seats on. `None` is a **manual** hold from the Timers menu (#383), which
/// belongs to no event: it exists purely so the RD can see whether the timer answers and whether it
/// carries the GridFPV plugin. A timer never holds both at once — see the module docs.
type ConnKey = (Option<EventId>, TimerId);

/// One connection the Director **wants**: its [`ConnKey`] parts plus the timer's *current* URL. The
/// URL travels with it so the reconciler can compare it against the live connection's (#382).
type Wanted = (Option<EventId>, TimerId, String);

/// One live connection **plus the dial config it was opened with** (#382).
///
/// The map key is a [`ConnKey`], which does **not** change when the RD edits the timer's URL —
/// and [`RhConnection::open`] captures the URL *by value* at spawn, so an already-live entry would
/// keep retrying the **old** address forever (the driver's backoff loop never re-reads the
/// registry). Carrying the URL in the value is what lets the reconciler notice the edit and
/// supersede + reopen.
struct LiveConnection {
    /// The RotorHazard base URL this connection's driver thread dialled.
    url: String,
    /// The driver handle: arming/tuning/seating and teardown.
    conn: RhConnection,
}

impl RhConnections {
    /// An empty connection set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm a running heat onto the live connection for `(event, timer)`, if one exists: the driver
    /// stages the RH race and routes its passes (remapped onto `lineup`) into `sink`'s log. Returns
    /// whether a live connection was found to arm (a non-active event, or a timer not yet connected,
    /// finds none — the heat then drives no RH passes, which is correct).
    pub fn arm_heat(
        &self,
        event: &EventId,
        timer: &TimerId,
        lineup: Vec<CompetitorRef>,
        sink: PassSink,
    ) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        if let Some(live) = map.get(&(Some(event.clone()), timer.clone())) {
            live.conn.arm_heat(lineup, sink);
            true
        } else {
            false
        }
    }

    /// **Tune** `(event, timer)`'s connection to a per-node channel assignment (race redesign Slice
    /// 4a), if a live connection exists: the driver emits a `set_frequency` per node so the device
    /// tunes to the staging heat's assigned channels ("the engine allocates, the adapter applies").
    /// Each [`TuneNode`] carries the catalog band/channel too, so RotorHazard's own UI labels the
    /// heat's channels rather than showing a bare frequency (#421).
    /// Returns whether a live connection was found to tune. A no-op (returns `false`) for a
    /// non-active event or a not-yet-connected timer.
    pub fn tune(&self, event: &EventId, timer: &TimerId, assignment: Vec<TuneNode>) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        if let Some(live) = map.get(&(Some(event.clone()), timer.clone())) {
            live.conn.tune(assignment);
            true
        } else {
            false
        }
    }

    /// **Prepare** `(event, timer)`'s connection for an instant start (Grid owns all timing), if a
    /// live connection exists: the driver zeroes RH's current-format staging (no RH-side hold/tones)
    /// and resets RH to READY, so the eventual arm at Grid's go starts RH recording immediately. The
    /// bridge calls this when a heat is **Staged** — before the Armed hold + tone — so all the
    /// reset/format work happens ahead of go. Returns whether a live connection was found to prepare;
    /// a no-op (`false`) for a non-active event or a not-yet-connected timer.
    pub fn prepare(&self, event: &EventId, timer: &TimerId) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        if let Some(live) = map.get(&(Some(event.clone()), timer.clone())) {
            live.conn.prepare();
            true
        } else {
            false
        }
    }

    /// **Seat** `(event, timer)`'s connection with the heat's `(node_index, callsign)` bind (the
    /// laps-attribute fix), if a live connection exists: the driver builds a fresh RH heat with those
    /// pilots seated onto their nodes and makes it current, so RH **records and attributes** passes on
    /// the bound nodes (RH dismisses a crossing on a node with no seated pilot). The bridge calls this
    /// when a heat is **Staged**, alongside `prepare`/`tune`. Returns whether a live connection was
    /// found to seat; a no-op (`false`) for a non-active event or a not-yet-connected timer.
    pub fn seat(&self, event: &EventId, timer: &TimerId, seats: Vec<(u64, String)>) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        if let Some(live) = map.get(&(Some(event.clone()), timer.clone())) {
            live.conn.seat(seats);
            true
        } else {
            false
        }
    }

    /// Disarm the current heat on `(event, timer)`'s connection (the heat left `Running`): the race
    /// is stopped/cleared but the **connection stays alive**. A no-op if no such connection.
    pub fn disarm(&self, event: &EventId, timer: &TimerId) {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        if let Some(live) = map.get(&(Some(event.clone()), timer.clone())) {
            live.conn.disarm();
        }
    }

    /// **Deliver one queued write** (#457) onto `write`'s timer's live connection — a restart
    /// (#386), a calibration write (#355), a capture (#355) or a channel write (#413).
    ///
    /// One method rather than four, because the four were the same method four times: scan the map
    /// by timer id, hand the write to the driver, report whether anything took it. Keeping them
    /// apart meant every policy question — is this connection actually up? is the write cleared on
    /// reconnect? — had four answers that could silently disagree, which is how #436 and #437
    /// happened.
    ///
    /// Keyed on the **timer**, not a `(claimant, timer)` pair: every one of these is the RD acting
    /// on a piece of hardware, and a timer holds exactly one connection whichever claim opened it
    /// (the active event's, or a manual hold — see the module docs). Tuning and the guided install
    /// both happen from the Timers menu with no event necessarily active, so the manual-hold key is
    /// the *common* case here, not the exotic one. So this scans by timer id rather than guessing
    /// the claimant.
    ///
    /// # Returns whether the write **landed on a live link**
    ///
    /// `false` means nothing was emitted and nothing will be: a write is **never queued for a
    /// future connection**. That is one policy for all four variants, and each has its own reason
    /// to want it:
    ///
    /// * a **restart** arriving minutes later would take a timer down that nobody asked to restart;
    /// * a **threshold** would move a detector nobody asked to move, long after the RD gave up on
    ///   it and re-set it by hand;
    /// * a **capture** is sharper still — fired at an empty gate it would set the threshold off the
    ///   **noise floor**, which is worse than not capturing at all, because the RD would have no
    ///   reason to suspect it;
    /// * a **channel** would retune a receiver nobody asked to move, possibly on a different
    ///   physical RotorHazard now answering at that URL.
    ///
    /// GridFPV's own records of the values are unaffected (`Timer::calibration`,
    /// `Timer::node_channels`, D27); it is the *application* of them that was lost, and the RD sees
    /// that as a value that never comes back confirmed.
    ///
    /// # A connection that has not connected does not count (#437)
    ///
    /// "A live connection exists for this timer" is not the same question as "this timer's socket
    /// is up", and the difference is a real race. A write parks in the registry while the timer
    /// reads `Connected`; before the next 500 ms tick the RD edits the URL (or the active event
    /// switches), so **the same tick** supersedes the entry and `Open`s a fresh [`RhConnection`]
    /// whose driver thread is still dialling — and then drains the parked write onto it. The
    /// entry exists, so the old code answered `true`: landed, no warning, nothing logged. It had
    /// not landed: the new driver wipes its queue the instant it connects (deliberately — a
    /// threshold that fired minutes later would move a detector nobody asked to move), so the
    /// accepted write vanished with no readback. *Sent* became indistinguishable from *landed*,
    /// which is the #403 failure class the readback design exists to prevent.
    ///
    /// So a connection is only asked to carry a write once it has actually reached `Connected`
    /// ([`RhConnection::is_connected`]). A write is **never held over** for a link that is on its
    /// way up — that is refused by design, for every one of the reasons above — so a still-dialling
    /// connection reports not-landed and the RD sees a value that never comes back confirmed.
    pub fn deliver(&self, write: PendingTimerWrite) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        let mut found = false;
        for (key, live) in map.iter() {
            if &key.1 == write.timer() && live.conn.is_connected() {
                live.conn.queue(write.clone());
                found = true;
            }
        }
        found
    }

    /// **Test seam:** stand in for every open connection's driver having reached `Connected`, so
    /// the *open* half of the #437 gate is asserted too — see
    /// [`RhConnection::mark_connected_for_test`].
    #[cfg(test)]
    fn mark_all_connected_for_test(&self) {
        for live in self
            .inner
            .lock()
            .expect("rh-connections lock poisoned")
            .values()
        {
            live.conn.mark_connected_for_test();
        }
    }

    /// **Test seam:** what actually reached `timer`'s connection queue, so a test can tell a write
    /// that *landed* from one merely reported as landed.
    #[cfg(test)]
    fn queued_writes_for_test(&self, timer: &TimerId) -> Vec<PendingTimerWrite> {
        self.inner
            .lock()
            .expect("rh-connections lock poisoned")
            .iter()
            .filter(|(key, _)| &key.1 == timer)
            .flat_map(|(_, live)| live.conn.queued_writes_for_test())
            .collect()
    }

    /// Reconcile the live set against `wanted` ([`wanted_connections`]: the active event's selected
    /// RH timers plus the manually-held ones, each with its url) by applying [`plan`]: open a
    /// connection for any wanted pair not yet live *or whose URL changed under it* (#382), and
    /// cancel+remove every live connection no longer wanted (a deselected timer, the active event
    /// having changed, a released manual hold). The `timers` registry is where each opened
    /// connection publishes its status.
    ///
    /// All the *decisions* live in [`plan`] (a pure function over the live set, the wanted set and
    /// the registry) — this only carries them out, so the reconciler's behaviour is unit-testable
    /// without dialling a real RotorHazard.
    fn reconcile(&self, wanted: &[Wanted], timers: &TimerRegistry) {
        let mut map = self.inner.lock().expect("rh-connections lock poisoned");
        let live: Vec<(ConnKey, String)> = map
            .iter()
            .map(|(key, live)| (key.clone(), live.url.clone()))
            .collect();
        for step in plan(&live, wanted, timers) {
            match step {
                Step::Close(key) => {
                    if let Some(live) = map.remove(&key) {
                        // Tear down on the driver thread (stop race + disconnect + Disconnected).
                        live.conn.cancel();
                    }
                }
                Step::Supersede(key) => {
                    if let Some(live) = map.remove(&key) {
                        // Tear down, yielding the shared status cell to the successor connection.
                        live.conn.cancel_superseded();
                    }
                }
                Step::Open(key, url) => {
                    let conn = RhConnection::open(key.1.clone(), url.clone(), timers.clone());
                    map.insert(key, LiveConnection { url, conn });
                }
            }
        }
    }
}

/// One move the reconciler makes to bring the live set in line with what is wanted.
///
/// Split out from [`RhConnections::reconcile`] so the decisions — which are the whole of #382 —
/// can be asserted in a unit test without opening a socket.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Tear this live connection down and let its driver publish `Disconnected`: the timer is
    /// genuinely going dark (deselected, or its event is no longer active) and nothing else will
    /// republish its status.
    Close(ConnKey),
    /// Tear this live connection down **yielding the shared status cell** ([`RhConnection::
    /// cancel_superseded`]): either a successor connection for the same timer is about to publish
    /// `Connecting`/`Connected` over it, or the registry has just written a fresh resting status
    /// that a parting `Disconnected` would stomp.
    Supersede(ConnKey),
    /// Open a fresh connection for this pair at this URL.
    Open(ConnKey, String),
}

/// Decide what [`Step`]s take the `live` set (each entry's key + **the URL it was dialled with**)
/// to the `wanted` set (see [`wanted_connections`], each entry with its *current* URL).
///
/// The rules, in order:
///
/// - **Wanted, same URL** → leave it alone. A healthy persistent link must not churn on a tick —
///   which is also what makes a manual hold (#383) survive every reconcile tick untouched.
/// - **Wanted, different URL** (#382) → `Supersede` + `Open`. The key is unchanged by a URL edit,
///   so the old code left the entry as-is and its driver kept retrying the address it captured at
///   spawn — forever. Superseding rather than closing hands the status cell to the successor, so
///   the timer does not flash `Disconnected` on its way back up.
/// - **Not wanted, but the timer is still wanted under another key** → `Supersede`: the connection
///   is being *replaced*, so its exiting driver must not stomp the successor's status. This is the
///   active-event switch, and also the manual ⇄ event hand-off of #383 — a timer that is held *and*
///   selected is wanted only under the event key, so its manual connection is superseded into the
///   event one rather than both being live.
/// - **Not wanted, and the timer is no longer a RotorHazard timer at all** → `Supersede` as well.
///   Its kind was edited (RH → Mock) or it was deleted, and the registry has *already* written the
///   new resting status (`Ready`); a parting `Disconnected` would overwrite that with a lie.
/// - **Not wanted, still an RH timer** → `Close`: genuinely deselected (or the hold was released),
///   so `Disconnected` is true.
fn plan(live: &[(ConnKey, String)], wanted: &[Wanted], timers: &TimerRegistry) -> Vec<Step> {
    let wanted_urls: HashMap<ConnKey, &str> = wanted
        .iter()
        .map(|(e, t, url)| ((e.clone(), t.clone()), url.as_str()))
        .collect();
    // Timer ids that stay wanted under ANY key — a live connection for one of them is being
    // REPLACED rather than retired.
    let wanted_timers: HashSet<&TimerId> = wanted.iter().map(|(_, t, _)| t).collect();
    let live_keys: HashSet<&ConnKey> = live.iter().map(|(key, _)| key).collect();

    let mut steps = Vec::new();
    // Keys whose live connection is going away this tick (and so must be reopened if still wanted).
    let mut retired: HashSet<ConnKey> = HashSet::new();
    for (key, url) in live {
        match wanted_urls.get(key) {
            // Still wanted at exactly the URL it was dialled with — nothing to do.
            Some(wanted_url) if *wanted_url == url.as_str() => {}
            // Still wanted, but the RD edited the URL: supersede + reopen (#382).
            Some(_) => {
                steps.push(Step::Supersede(key.clone()));
                retired.insert(key.clone());
            }
            None => {
                steps.push(if yields_status(&key.1, &wanted_timers, timers) {
                    Step::Supersede(key.clone())
                } else {
                    Step::Close(key.clone())
                });
                retired.insert(key.clone());
            }
        }
    }
    for (event, timer, url) in wanted {
        let key = (event.clone(), timer.clone());
        if retired.contains(&key) || !live_keys.contains(&key) {
            steps.push(Step::Open(key, url.clone()));
        }
    }
    steps
}

/// Whether tearing `timer`'s connection down must **yield** the shared status cell rather than
/// publish a parting `Disconnected`.
///
/// True when the connection is being replaced (the timer is still wanted under some other key), and
/// also when the timer is **no longer a RotorHazard timer** — its kind was edited away from
/// `Rotorhazard` (or it was deleted), so the registry has already written the fresh resting status
/// for the new kind (`Ready` for a Mock) and a parting `Disconnected` would strand a lie on a timer
/// that has no connection to be disconnected from (#382).
fn yields_status(
    timer: &TimerId,
    wanted_timers: &HashSet<&TimerId>,
    timers: &TimerRegistry,
) -> bool {
    wanted_timers.contains(timer)
        || !matches!(
            timers.get(timer).map(|t| t.kind),
            Some(TimerKind::Rotorhazard { .. })
        )
}

/// Every RotorHazard connection the Director wants right now — the reconciler's source of truth,
/// and the **union of its two inputs**:
///
/// 1. the timers `registry`'s **active event** currently selects (#105), keyed `Some(event)` — the
///    key a running heat's bridge arms, tunes and seats on;
/// 2. the timers the RD is **manually holding** from the Timers menu (#383), keyed `None` — a
///    diagnostic link that exists with no event at all.
///
/// A timer claimed by both appears **once**, under the event key: that is the claim a race needs,
/// and listing it twice would open two sockets to the same RotorHazard. [`plan`] then supersedes
/// the manual connection into the event one (and back again when the event lets go) rather than
/// closing it, so the timer never flashes `Disconnected` across the hand-off.
///
/// Only `Rotorhazard` kinds appear either way: a Mock has nothing to dial.
fn wanted_connections(registry: &EventRegistry, timers: &TimerRegistry) -> Vec<Wanted> {
    let mut wanted: Vec<Wanted> = Vec::new();

    // 1. The active event's selection (#105).
    if let Some(active) = registry.active() {
        if let Some(selection) = registry.timers_of(&active.id) {
            for id in selection {
                if let Some(timer) = timers.get(&id) {
                    if let TimerKind::Rotorhazard { url } = timer.kind {
                        wanted.push((Some(active.id.clone()), id, url));
                    }
                }
            }
        }
    }

    // 2. Manual holds (#383) — skipping any timer the active event already claims above, so the
    //    two inputs can never open two connections to one timer.
    for id in timers.manual_connections() {
        if wanted.iter().any(|(_, claimed, _)| *claimed == id) {
            continue;
        }
        if let Some(timer) = timers.get(&id) {
            if let TimerKind::Rotorhazard { url } = timer.kind {
                wanted.push((None, id, url));
            }
        }
    }

    wanted
}

/// Spawn the reconciler (#105, #383): poll [`wanted_connections`] on [`RECONCILE_INTERVAL`] and keep
/// `connections` in sync — opening a persistent connection for each selected RotorHazard timer of
/// the active event and each manually-held one, and closing those no longer wanted (deselected,
/// event no longer active, hold released). Returns the reconciler's [`JoinHandle`]; it runs for the
/// process lifetime. The returned [`RhConnections`] is the shared set the per-event bridges arm
/// heats on.
pub fn spawn_rh_reconciler(registry: EventRegistry) -> (RhConnections, JoinHandle<()>) {
    let connections = RhConnections::new();
    let timers = registry.timers();
    let handle = {
        let connections = connections.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
            loop {
                ticker.tick().await;
                let wanted = wanted_connections(&registry, &timers);
                connections.reconcile(&wanted, &timers);
                // Carry every **queued write** (#457) from the timer registry — where the RD-gated
                // routes park them, the server crate having no handle on this set — onto the live
                // connections. Drained here rather than handed over directly for the same reason a
                // manual hold is a registry flag: the server crate is *below* this one, so the
                // registry is the one seam both sides already share.
                //
                // One drain and one dispatch for restarts (#386), calibration writes (#355),
                // captures (#355) and channel writes (#413) alike. Before #457 this paragraph
                // existed four times over, and the differences between the copies were not policy
                // — they were drift.
                for write in timers.take_pending_writes() {
                    if !connections.deliver(write.clone()) {
                        // The connection went away between the route accepting the write and this
                        // tick (a deselect, a URL edit, a drop). Nothing was emitted, and nothing is
                        // queued for a future connection — so say so rather than fail silently. On
                        // the page the RD sees it as a value that never comes back confirmed, which
                        // is the honest outcome; GridFPV's own record of what it decided is
                        // untouched (D27).
                        eprintln!("gridfpv: {}", orphaned_write(&write, &timers));
                    }
                }
                // …then settle every capture whose sampling window has run out (#355, #446). This
                // is where a captured level becomes **GridFPV's** value (D27): the registry
                // compares what the timer is reporting now against what it reported when the
                // capture started, and records the new level on `Timer::calibration` if one
                // arrived.
                //
                // What it says when one did not is the whole of #446. RotorHazard refuses a capture
                // in complete silence, so an unchanged level is *consistent* with a refusal — but
                // it is equally consistent with a capture that measured the same number, which is
                // an ordinary result on a stable gate. The Director says which of the three things
                // it actually saw, and claims nothing beyond that.
                for outcome in timers.resolve_captures() {
                    let name = timers.get(&outcome.timer).map(|t| t.name);
                    let name = name.as_deref().unwrap_or("that timer");
                    // Friendly name, 1-based node (CLAUDE.md): this is the line an RD reads at the
                    // gate to find out what their pass did.
                    let what = format!(
                        "{}'s {} capture on {:?}",
                        Timer::node_label(outcome.node),
                        outcome.threshold.label(),
                        name
                    );
                    match outcome.resolution {
                        CaptureResolution::Measured => eprintln!(
                            "gridfpv: {what} measured {} — recorded as GridFPV's level for that \
                             gate",
                            outcome.level.unwrap_or_default()
                        ),
                        CaptureResolution::Unchanged => eprintln!(
                            "gridfpv: {what} came back on the level it was already on ({}) — that \
                             is what a capture measuring the same number looks like AND what a \
                             capture RotorHazard refused in silence looks like, and they cannot be \
                             told apart, so nothing was recorded. Capture again if the gate is not \
                             detecting as it should.",
                            outcome.reported.unwrap_or_default()
                        ),
                        CaptureResolution::Unobserved => eprintln!(
                            "gridfpv: {what} produced no readable level at all — GridFPV was not \
                             watching (the link dropped, the node never answered, or the Tune page \
                             was never open), so nothing was recorded and nothing is claimed about \
                             whether RotorHazard measured it"
                        ),
                    }
                }
            }
        })
    };
    (connections, handle)
}

/// The operator line for a write that found **no live connection** (#457) — one place, so the four
/// kinds cannot drift into four different vocabularies for one situation.
///
/// Names the timer by its **friendly name** (CLAUDE.md), falling back only if the registry no
/// longer holds it, and the node the way the page labels it (1-based). This is what an RD reads
/// when a threshold or a channel never comes back confirmed, and `"bench-rotorhazard-xvb27q"` does
/// not tell them which box on the bench to go and look at.
fn orphaned_write(write: &PendingTimerWrite, timers: &TimerRegistry) -> String {
    let name = timers.get(write.timer()).map(|t| t.name);
    let name = name.as_deref().unwrap_or("that timer").to_string();
    match write {
        PendingTimerWrite::Restart { .. } => {
            format!("no live RotorHazard connection to restart for {name:?}")
        }
        PendingTimerWrite::Calibrate(w) => format!(
            "no live RotorHazard connection to calibrate {} on {name:?}",
            Timer::node_label(w.node)
        ),
        PendingTimerWrite::Capture(w) => format!(
            "no live RotorHazard connection to capture {}'s {} level on {name:?}",
            Timer::node_label(w.node),
            w.threshold.label()
        ),
        PendingTimerWrite::SetChannel(w) => format!(
            "no live RotorHazard connection to set {}'s channel on {name:?}",
            Timer::node_label(w.node)
        ),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_server::events::CreateEventRequest;
    use gridfpv_server::timers::{
        CalibrationRequest, CaptureRequest, CaptureThreshold, ChannelRequest, CreateTimerRequest,
        PendingCalibration, PendingChannel, UpdateTimerRequest,
    };

    const OLD_URL: &str = "http://rh-old.local:5000";
    const NEW_URL: &str = "http://rh-new.local:5000";

    fn registry() -> TimerRegistry {
        TimerRegistry::new(None, 5, 2500).expect("in-memory timer registry")
    }

    /// Create a RotorHazard timer at `url` and return its id.
    fn rh_timer(timers: &TimerRegistry, name: &str, url: &str) -> TimerId {
        timers
            .create(&CreateTimerRequest {
                name: name.to_string(),
                kind: TimerKind::Rotorhazard {
                    url: url.to_string(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .expect("timer created")
            .id
    }

    /// The **event** half of a [`ConnKey`]: the active event claims the timer (#105).
    fn event(id: &str) -> Option<EventId> {
        Some(EventId(id.to_string()))
    }

    /// The **manual** half of a [`ConnKey`]: the RD holds the timer with no event at all (#383).
    const MANUAL: Option<EventId> = None;

    /// A calibration write for `timer`, node 0, enter-only — the shape the Tune page sends most.
    fn a_calibration(timer: &TimerId) -> PendingTimerWrite {
        PendingTimerWrite::Calibrate(PendingCalibration {
            timer: timer.clone(),
            node: 0,
            enter_at: Some(96),
            exit_at: None,
            during_open_practice: false,
        })
    }

    /// A channel write for `timer`, node 0, carrying its catalog label.
    fn a_channel(timer: &TimerId) -> PendingTimerWrite {
        PendingTimerWrite::SetChannel(PendingChannel {
            timer: timer.clone(),
            node: 0,
            mhz: 5880,
            band: Some("Raceband".into()),
            channel: Some("R7".into()),
            during_open_practice: false,
        })
    }

    #[test]
    fn a_write_with_no_live_connection_is_reported_not_swallowed() {
        // #457, and the whole of what the four per-feature dispatches each used to assert
        // separately: the RD asked something of a timer that has since gone away (deselected, URL
        // edited, link dropped). There is nothing to emit on and nothing is queued for a future
        // connection, so `deliver` says `false` — which is what lets the reconciler log it rather
        // than fail silently.
        //
        // Each variant has its own reason to want that policy, and they are worth restating
        // because a future reader will be tempted to "helpfully" hold one over:
        //
        //  * a restart minutes later takes a timer down nobody asked to restart;
        //  * a threshold moves a detector nobody asked to move, long after the RD gave up on the
        //    value and re-set it by hand;
        //  * a capture is sharper still — fired at an empty gate it sets the threshold off the
        //    NOISE FLOOR, which is worse than not capturing at all;
        //  * a channel retunes a receiver nobody asked to move, possibly on a different physical
        //    RotorHazard now answering at that URL.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let connections = RhConnections::new();

        for write in [
            PendingTimerWrite::Restart { timer: rh.clone() },
            a_calibration(&rh),
            PendingTimerWrite::Capture(gridfpv_server::timers::PendingCapture {
                timer: rh.clone(),
                node: 0,
                threshold: CaptureThreshold::Enter,
                during_open_practice: false,
            }),
            a_channel(&rh),
        ] {
            assert!(
                !connections.deliver(write.clone()),
                "{write:?} must report NOT landed with no live connection"
            );
        }
    }

    #[test]
    fn an_orphaned_write_names_the_timer_and_the_node_the_way_the_page_does() {
        // CLAUDE.md's display rule, on the one surface an RD reads when a write never comes back
        // confirmed: the timer's friendly name, and the node 1-based. `"bench-rotorhazard-xvb27q"`
        // and `"node 0"` do not tell an RD which box on the bench to go and look at.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);

        let line = orphaned_write(&a_calibration(&rh), &timers);
        assert!(line.contains("\"Field RH\""), "got {line:?}");
        assert!(line.contains("Node 1"), "0-based index leaked: {line:?}");
        assert!(!line.contains(&rh.0), "raw timer id leaked: {line:?}");

        let line = orphaned_write(&PendingTimerWrite::Restart { timer: rh.clone() }, &timers);
        assert!(line.contains("restart"), "got {line:?}");
        assert!(line.contains("\"Field RH\""), "got {line:?}");

        // A timer the registry no longer holds falls back — a last resort, never the first choice.
        let gone = TimerId("no-such-timer".into());
        let line = orphaned_write(&PendingTimerWrite::Restart { timer: gone }, &timers);
        assert!(line.contains("that timer"), "got {line:?}");
    }

    #[test]
    fn calibration_writes_drain_exactly_once_and_coalesce_per_node() {
        // The registry is the seam the RD-gated route and the (higher-layer) connection reconciler
        // share; the reconciler drains it each tick. Several writes to one node before a drain
        // apply the LATEST value once — a stale threshold replayed after a fresh one would leave
        // the timer detecting against a value the page is no longer showing.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers.set_status(&rh, gridfpv_server::timers::TimerStatus::Connected);

        for enter in [80, 96] {
            timers
                .request_calibration(
                    &rh,
                    &CalibrationRequest {
                        node: 1,
                        enter_at: Some(enter),
                        exit_at: None,
                    },
                    false,
                )
                .expect("connected RH timer");
        }
        timers
            .request_calibration(
                &rh,
                &CalibrationRequest {
                    node: 1,
                    enter_at: None,
                    exit_at: Some(70),
                },
                false,
            )
            .expect("the exit half is independent of the enter half");

        let drained = timers.take_pending_writes();
        assert_eq!(drained.len(), 1, "one entry per node, not one per write");
        match &drained[0] {
            PendingTimerWrite::Calibrate(w) => {
                assert_eq!(w.enter_at, Some(96));
                assert_eq!(w.exit_at, Some(70));
            }
            other => panic!("expected a calibration write, got {other:?}"),
        }
        assert!(
            timers.take_pending_writes().is_empty(),
            "drained exactly once — nothing is re-queued"
        );
    }

    #[test]
    fn captures_drain_exactly_once_and_are_never_coalesced() {
        // Deliberately unlike the calibration queue, and the reason `PendingTimerWrite`'s
        // coalescing policy is a documented per-variant method rather than one rule. Two writes of
        // a value to one node are one intent (the latest value wins); two captures are two
        // *measurements* the RD asked for, and collapsing them would silently drop a pass they
        // flew.
        //
        // The one case where two would collide — a second capture of a threshold already
        // capturing, which RotorHazard refuses in silence — is refused by `request_capture`
        // instead, so the queue never needs to.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers.set_status(&rh, gridfpv_server::timers::TimerStatus::Connected);

        timers
            .request_capture(
                &rh,
                &CaptureRequest {
                    node: 1,
                    threshold: CaptureThreshold::Enter,
                },
                false,
            )
            .expect("connected RH timer");
        // The same node's OTHER threshold is a separate capture — RotorHazard arms them separately.
        timers
            .request_capture(
                &rh,
                &CaptureRequest {
                    node: 1,
                    threshold: CaptureThreshold::Exit,
                },
                false,
            )
            .expect("the exit capture is independent of the enter one");
        // …and a repeat of the enter one is refused while it is still running, rather than queued.
        assert!(
            timers
                .request_capture(
                    &rh,
                    &CaptureRequest {
                        node: 1,
                        threshold: CaptureThreshold::Enter,
                    },
                    false,
                )
                .is_err(),
            "RotorHazard refuses a capture already in flight in SILENCE, so the Director must \
             refuse it out loud instead"
        );

        let drained = timers.take_pending_writes();
        assert_eq!(drained.len(), 2, "one entry per capture, never coalesced");
        let thresholds: Vec<CaptureThreshold> = drained
            .iter()
            .map(|w| match w {
                PendingTimerWrite::Capture(c) => c.threshold,
                other => panic!("expected a capture, got {other:?}"),
            })
            .collect();
        assert_eq!(
            thresholds,
            vec![CaptureThreshold::Enter, CaptureThreshold::Exit]
        );
        assert!(
            timers.take_pending_writes().is_empty(),
            "drained exactly once — nothing is re-queued"
        );
    }

    #[test]
    fn channel_writes_drain_exactly_once_and_coalesce_per_node_carrying_the_label() {
        // Same seam and same discipline as the calibration drain. Two picks on one node before a
        // drain retune it once, to the LATEST value — replaying a stale channel after a fresh one
        // would leave the gate on a frequency the page is no longer showing.
        //
        // And the label rides along: RotorHazard stores band/channel on its profile, and the RD
        // validates this by refreshing RotorHazard's own page, where a bare number reads as a
        // half-failure.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers.set_status(&rh, gridfpv_server::timers::TimerStatus::Connected);

        for (mhz, band, channel) in [(5658u16, "Raceband", "R1"), (5880, "Raceband", "R7")] {
            timers
                .request_channel(
                    &rh,
                    &ChannelRequest {
                        node: 1,
                        mhz,
                        band: Some(band.into()),
                        channel: Some(channel.into()),
                    },
                    false,
                )
                .expect("connected RH timer");
        }

        let drained = timers.take_pending_writes();
        assert_eq!(drained.len(), 1, "one entry per node, not one per pick");
        match &drained[0] {
            PendingTimerWrite::SetChannel(w) => {
                assert_eq!(w.mhz, 5880);
                assert_eq!(w.band.as_deref(), Some("Raceband"));
                assert_eq!(w.channel.as_deref(), Some("R7"));
            }
            other => panic!("expected a channel write, got {other:?}"),
        }
        assert!(
            timers.take_pending_writes().is_empty(),
            "drained exactly once — nothing is re-queued"
        );
    }

    #[test]
    fn restart_requests_drain_exactly_once_and_coalesce() {
        // The registry is the seam the RD-gated route and the (higher-layer) connection reconciler
        // share; the reconciler drains it each tick. Asking twice before a drain is one restart, and
        // a drained request is never handed out again.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers.set_status(&rh, gridfpv_server::timers::TimerStatus::Connected);
        timers.request_restart(&rh).expect("connected RH timer");
        timers.request_restart(&rh).expect("coalesces");
        assert_eq!(
            timers.take_pending_writes(),
            vec![PendingTimerWrite::Restart { timer: rh.clone() }]
        );
        assert!(timers.take_pending_writes().is_empty());
    }

    #[test]
    fn the_one_queue_keeps_request_order_across_kinds() {
        // #457: four queues became one, and the order the reconciler dispatches in is now the order
        // the RD asked in — not a fixed restart→level→capture→channel sweep that could apply a
        // threshold to a node *before* the channel pick that preceded it. A coalesced write folds
        // into the entry already queued rather than jumping to the back, so the RD's order survives
        // a second press too.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers.set_status(&rh, gridfpv_server::timers::TimerStatus::Connected);

        timers
            .request_channel(
                &rh,
                &ChannelRequest {
                    node: 1,
                    mhz: 5658,
                    band: Some("Raceband".into()),
                    channel: Some("R1".into()),
                },
                false,
            )
            .expect("connected RH timer");
        timers
            .request_calibration(
                &rh,
                &CalibrationRequest {
                    node: 1,
                    enter_at: Some(96),
                    exit_at: None,
                },
                false,
            )
            .expect("connected RH timer");
        // A second pick on the already-queued channel folds in place — it does not overtake the
        // threshold that was asked for after it.
        timers
            .request_channel(
                &rh,
                &ChannelRequest {
                    node: 1,
                    mhz: 5880,
                    band: Some("Raceband".into()),
                    channel: Some("R7".into()),
                },
                false,
            )
            .expect("connected RH timer");

        let drained = timers.take_pending_writes();
        assert_eq!(drained.len(), 2, "the channel write coalesced, in place");
        match &drained[0] {
            PendingTimerWrite::SetChannel(w) => assert_eq!(w.mhz, 5880, "latest pick wins"),
            other => panic!("the channel write was asked for first, got {other:?}"),
        }
        assert!(
            matches!(drained[1], PendingTimerWrite::Calibrate(_)),
            "got {:?}",
            drained[1]
        );
    }

    #[test]
    fn an_unchanged_url_leaves_a_healthy_connection_alone() {
        // The persistent-connection invariant (#105): a reconcile tick must not churn a live link.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let key = (event("e1"), rh.clone());
        let live = vec![(key.clone(), OLD_URL.to_string())];
        let wanted = vec![(event("e1"), rh, OLD_URL.to_string())];
        assert_eq!(plan(&live, &wanted, &timers), Vec::new());
    }

    #[test]
    fn a_url_edit_supersedes_and_reopens_on_the_new_address() {
        // #382: the key `(event, timer)` is unchanged by a URL edit, so the old reconciler skipped
        // the entry and its driver kept dialling the address captured at spawn — forever.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers
            .update(
                &rh,
                &UpdateTimerRequest {
                    kind: Some(TimerKind::Rotorhazard {
                        url: NEW_URL.to_string(),
                    }),
                    ..Default::default()
                },
            )
            .expect("url edited");
        let key = (event("e1"), rh.clone());
        let live = vec![(key.clone(), OLD_URL.to_string())];
        let wanted = vec![(event("e1"), rh, NEW_URL.to_string())];
        assert_eq!(
            plan(&live, &wanted, &timers),
            vec![
                // Superseded, NOT closed: the status cell is handed to the successor, so the timer
                // does not flash `Disconnected` on its way back up.
                Step::Supersede(key.clone()),
                Step::Open(key, NEW_URL.to_string()),
            ]
        );
    }

    #[test]
    fn a_kind_change_to_mock_supersedes_without_stomping_the_new_resting_status() {
        // #382, the same blind spot for a `TimerKind` change: an RH timer edited to a Mock drops
        // out of `wanted` entirely. Closing it would let the exiting driver publish `Disconnected`
        // over the `Ready` the registry just wrote for the new kind — a permanent lie on a timer
        // that has no connection at all. Superseding leaves the registry's status alone.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        timers
            .update(
                &rh,
                &UpdateTimerRequest {
                    kind: Some(TimerKind::Mock {
                        laps: 3,
                        lap_ms: 2000,
                    }),
                    ..Default::default()
                },
            )
            .expect("kind edited");
        let key = (event("e1"), rh);
        let live = vec![(key.clone(), OLD_URL.to_string())];
        assert_eq!(plan(&live, &[], &timers), vec![Step::Supersede(key)]);
    }

    #[test]
    fn a_kind_change_from_mock_to_rotorhazard_opens_a_connection() {
        // The other direction of the same hole: a selected Mock becomes a RotorHazard timer, so it
        // enters `wanted` for the first time and must be dialled with no restart.
        let timers = registry();
        let mock = timers
            .create(&CreateTimerRequest {
                name: "Bench".into(),
                kind: TimerKind::Mock {
                    laps: 3,
                    lap_ms: 2000,
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
            })
            .expect("timer created")
            .id;
        timers
            .update(
                &mock,
                &UpdateTimerRequest {
                    kind: Some(TimerKind::Rotorhazard {
                        url: NEW_URL.to_string(),
                    }),
                    ..Default::default()
                },
            )
            .expect("kind edited");
        let wanted = vec![(event("e1"), mock.clone(), NEW_URL.to_string())];
        assert_eq!(
            plan(&[], &wanted, &timers),
            vec![Step::Open((event("e1"), mock), NEW_URL.to_string())]
        );
    }

    #[test]
    fn a_deselected_rh_timer_is_closed_and_left_disconnected() {
        // Still a RotorHazard timer, just no longer wanted: `Disconnected` is the truth here, so
        // the driver publishes it (this is the pre-existing #105 behaviour, kept).
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let key = (event("e1"), rh);
        let live = vec![(key.clone(), OLD_URL.to_string())];
        assert_eq!(plan(&live, &[], &timers), vec![Step::Close(key)]);
    }

    #[test]
    fn an_active_event_switch_supersedes_and_reopens_under_the_new_event() {
        // Pre-existing #105 behaviour, preserved by the rewrite: the same timer under a new event
        // key is a REPLACEMENT, so the exiting driver yields its status to the successor.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let old_key = (event("e1"), rh.clone());
        let new_key = (event("e2"), rh.clone());
        let live = vec![(old_key.clone(), OLD_URL.to_string())];
        let wanted = vec![(event("e2"), rh, OLD_URL.to_string())];
        assert_eq!(
            plan(&live, &wanted, &timers),
            vec![
                Step::Supersede(old_key),
                Step::Open(new_key, OLD_URL.to_string()),
            ]
        );
    }

    // ---------------------------------------------------------------------------------------
    // #383 — manual, event-independent connections.
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_manual_hold_opens_a_connection_with_no_active_event() {
        // The whole point of #383: nothing is active, nothing is selected, and the timer still
        // dials — so the Timers menu can answer "is this URL right?" before any event exists.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let event_registry_is_idle: Vec<Wanted> = vec![(MANUAL, rh.clone(), OLD_URL.to_string())];
        assert_eq!(
            plan(&[], &event_registry_is_idle, &timers),
            vec![Step::Open((MANUAL, rh), OLD_URL.to_string())]
        );
    }

    #[test]
    fn a_manual_connection_survives_a_reconcile_tick() {
        // The reconciler runs every RECONCILE_INTERVAL against the union of its two inputs; a hold
        // that is not re-derived from the active event must not be swept away as "not wanted".
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let key = (MANUAL, rh.clone());
        let live = vec![(key, OLD_URL.to_string())];
        let wanted = vec![(MANUAL, rh, OLD_URL.to_string())];
        assert_eq!(plan(&live, &wanted, &timers), Vec::new());
    }

    #[test]
    fn a_manually_held_timer_the_active_event_then_selects_is_not_double_connected() {
        // `wanted_connections` lists a doubly-claimed timer ONCE, under the event key (that is the
        // key a heat arms on). The manual connection is then superseded INTO the event one — one
        // socket, and no `Disconnected` flash across the hand-off.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let manual_key = (MANUAL, rh.clone());
        let event_key = (event("e1"), rh.clone());
        let live = vec![(manual_key.clone(), OLD_URL.to_string())];
        let wanted = vec![(event("e1"), rh, OLD_URL.to_string())];
        let steps = plan(&live, &wanted, &timers);
        assert_eq!(
            steps,
            vec![
                Step::Supersede(manual_key),
                Step::Open(event_key, OLD_URL.to_string()),
            ]
        );
        // Exactly one connection is opened — never one per claimant.
        assert_eq!(
            steps.iter().filter(|s| matches!(s, Step::Open(..))).count(),
            1
        );
    }

    #[test]
    fn releasing_the_event_hands_a_still_held_timer_back_to_its_manual_connection() {
        // The hand-off runs both ways: the event deactivates (or deselects the timer) while the RD
        // still holds it, so the connection moves back to the manual key — again by supersede, so
        // the timer stays continuously connected. This is why the hold is NOT cleared when an event
        // takes the timer over: an explicit hold means explicit, until the RD disconnects.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let event_key = (event("e1"), rh.clone());
        let manual_key = (MANUAL, rh.clone());
        let live = vec![(event_key.clone(), OLD_URL.to_string())];
        let wanted = vec![(MANUAL, rh, OLD_URL.to_string())];
        assert_eq!(
            plan(&live, &wanted, &timers),
            vec![
                Step::Supersede(event_key),
                Step::Open(manual_key, OLD_URL.to_string()),
            ]
        );
    }

    #[test]
    fn releasing_the_hold_closes_the_manual_connection() {
        // Disconnect: nothing wants the timer any more, so it goes down and reads `Disconnected` —
        // the truthful resting state for an RH timer with no live link.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let key = (MANUAL, rh);
        let live = vec![(key.clone(), OLD_URL.to_string())];
        assert_eq!(plan(&live, &[], &timers), vec![Step::Close(key)]);
    }

    #[test]
    fn a_url_edit_re_dials_a_manually_held_timer_too() {
        // #382 + #383 together — the two-second "type a URL, see whether it works, fix it, see
        // again" loop the field session did not have: the edit re-dials on the manual connection
        // with no event and no restart.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let key = (MANUAL, rh.clone());
        let live = vec![(key.clone(), OLD_URL.to_string())];
        let wanted = vec![(MANUAL, rh, NEW_URL.to_string())];
        assert_eq!(
            plan(&live, &wanted, &timers),
            vec![
                Step::Supersede(key.clone()),
                Step::Open(key, NEW_URL.to_string()),
            ]
        );
    }

    #[test]
    fn wanted_connections_unions_the_two_inputs_and_lists_a_doubly_claimed_timer_once() {
        // The union itself (#383): a manual hold is wanted with NO active event, and a timer the
        // active event also selects appears exactly once — under the event key, never twice.
        let events = EventRegistry::new(None).expect("in-memory event registry");
        let timers = events.timers();
        let selected = rh_timer(&timers, "Field RH", OLD_URL);
        let bench = rh_timer(&timers, "Bench RH", NEW_URL);
        timers.set_manual_connect(&selected, true).expect("held");
        timers.set_manual_connect(&bench, true).expect("held");

        // Nothing active: both holds stand entirely on their own.
        let mut wanted = wanted_connections(&events, &timers);
        wanted.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            wanted,
            vec![
                (MANUAL, bench.clone(), NEW_URL.to_string()),
                (MANUAL, selected.clone(), OLD_URL.to_string()),
            ]
        );

        // An event goes active and selects one of them: that one moves to the event key, the
        // other keeps its manual key, and neither is listed twice.
        let practice = EventId(
            events
                .create(&CreateEventRequest::named("Practice"))
                .expect("create the event")
                .id
                .0,
        );
        events
            .set_timers(&practice, vec![selected.clone()])
            .expect("selection set");
        events.set_active(&practice).expect("event active");
        let mut wanted = wanted_connections(&events, &timers);
        wanted.sort_by(|a, b| a.1.cmp(&b.1));
        assert_eq!(
            wanted,
            vec![
                (MANUAL, bench, NEW_URL.to_string()),
                (Some(practice), selected.clone(), OLD_URL.to_string()),
            ]
        );
        assert_eq!(
            wanted.iter().filter(|(_, t, _)| *t == selected).count(),
            1,
            "a doubly-claimed timer must be wanted once, or two sockets open to one RotorHazard"
        );
    }

    #[test]
    fn the_registry_holds_and_releases_manual_connections_for_rh_timers_only() {
        // The reconciler's second input, at the source: `manual_connections` is what
        // `wanted_connections` unions in.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        assert!(timers.manual_connections().is_empty());

        let held = timers.set_manual_connect(&rh, true).expect("held");
        assert!(held.manual_connect);
        assert_eq!(timers.manual_connections(), vec![rh.clone()]);

        timers.set_manual_connect(&rh, false).expect("released");
        assert!(timers.manual_connections().is_empty());

        // A Mock has nothing to dial, and an unknown id is an error — both are 4xx at the route.
        assert!(
            timers
                .set_manual_connect(&TimerId("mock".into()), true)
                .is_err()
        );
        assert!(
            timers
                .set_manual_connect(&TimerId("no-such-timer".into()), true)
                .is_err()
        );
    }

    // ---------------------------------------------------------------------------------------
    // #437 — a write drained onto a connection that has not connected yet.
    // ---------------------------------------------------------------------------------------

    /// Loopback port 1 is reserved and unused, so a driver pointed at it never gets past dialling.
    const DEAD_URL: &str = "http://127.0.0.1:1";

    /// **A write must not report "landed" on a connection that has not connected (#437).**
    ///
    /// The race: a calibration (or channel) write parks in the registry while the timer reads
    /// `Connected`. Before the next 500 ms reconciler tick the timer's URL is edited, or the active
    /// event switches — so the same tick supersedes the entry and `Open`s a fresh [`RhConnection`]
    /// whose driver thread is still dialling. Then the tick's own drain queues the write onto that
    /// brand-new connection, and [`RhConnections::deliver`] answers `true`: landed, no operator
    /// warning, nothing logged.
    ///
    /// It has not landed. When the new driver's `connect` succeeds it clears the queue — deliberately,
    /// because a threshold that fired minutes later would move a detector nobody asked to move — so
    /// the accepted write vanishes with no warning and no readback. *Sent* becomes indistinguishable
    /// from *landed*, which is the #403 failure class the readback design exists to prevent.
    ///
    /// Given that clear-on-connect is the intended behaviour, the write is only ever real on a link
    /// that is already up: a connection still dialling must report **not landed**, so the reconciler
    /// logs it and the RD sees a level (or a channel) that never comes back confirmed.
    #[tokio::test]
    async fn a_write_drained_onto_a_still_dialling_connection_is_not_landed() {
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", DEAD_URL);
        let connections = RhConnections::new();

        // The reconciler opens a fresh connection for the timer; its driver is still dialling.
        connections.reconcile(&[(event("e1"), rh.clone(), DEAD_URL.to_string())], &timers);
        assert_ne!(
            timers.get(&rh).map(|t| t.status),
            Some(gridfpv_server::timers::TimerStatus::Connected),
            "the precondition: this connection has not reached Connected"
        );

        // The same tick drains the parked writes onto it.
        let calibration_landed = connections.deliver(a_calibration(&rh));
        let channel_landed = connections.deliver(a_channel(&rh));

        // Tear the driver thread down before asserting, so a failure does not leave it dialling.
        connections.reconcile(&[], &timers);

        assert!(
            !calibration_landed,
            "a threshold queued on a connection that is still dialling has NOT landed: the \
             driver's clear-on-connect drops it, and reporting `true` here is what turns a lost \
             write into a silent one"
        );
        assert!(
            !channel_landed,
            "and the same for a channel write — `deliver`'s own contract is that nothing is \
             queued for a future connection"
        );
    }

    /// **…and the same connection, once it is up, takes the write (#437).**
    ///
    /// The other half of the gate, and the half that matters most if it ever breaks: a liveness
    /// check that never opens is a wall, and a wall here would silently kill every Tune-page write
    /// in the field while every test above stayed green — a check that cannot see the thing it is
    /// checking (CLAUDE.md). So this asserts the write both reports landed *and* actually reaches
    /// the driver's queue.
    #[tokio::test]
    async fn a_write_drained_onto_a_connected_connection_lands_on_its_driver() {
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", DEAD_URL);
        let connections = RhConnections::new();

        connections.reconcile(&[(event("e1"), rh.clone(), DEAD_URL.to_string())], &timers);
        // Stand in for the driver having connected (nothing here dials a real RotorHazard).
        connections.mark_all_connected_for_test();

        let landed = connections.deliver(a_calibration(&rh));
        let queued = connections.queued_writes_for_test(&rh);

        // Tear the driver thread down before asserting, so a failure does not leave it dialling.
        connections.reconcile(&[], &timers);

        assert!(landed, "a live connection must take the write");
        assert_eq!(
            queued,
            vec![a_calibration(&rh)],
            "and it must actually reach the driver's queue — reporting landed without queueing \
             would be the same lie from the other direction"
        );
    }
}
