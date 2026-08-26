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
use gridfpv_server::timers::{TimerId, TimerKind, TimerRegistry};
use tokio::task::JoinHandle;

use super::PassSink;
use super::rotorhazard::{CalibrationWrite, ChannelWrite, RhConnection};

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
    /// Returns whether a live connection was found to tune. A no-op (returns `false`) for a
    /// non-active event or a not-yet-connected timer.
    pub fn tune(&self, event: &EventId, timer: &TimerId, assignment: Vec<(u64, u16)>) -> bool {
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

    /// **Restart the RotorHazard server** behind `timer`'s live connection (#386) — the guided
    /// plugin install's last step, so the RD never leaves GridFPV to press Restart in RotorHazard's
    /// own web UI.
    ///
    /// Keyed on the **timer**, not a `(claimant, timer)` pair: the RD is restarting a piece of
    /// hardware, and it holds exactly one connection whichever claim opened it (the event's, or a
    /// manual hold — see the module docs). Whichever one is live is the one that carries the emit,
    /// so this scans the map by timer id rather than guessing the claimant.
    ///
    /// Returns whether a live connection was found to restart. `false` means the timer is not
    /// connected right now — nothing was emitted, and nothing will be: a restart is not queued for
    /// a future connection (the RD asked to restart *this* live timer, and a request that lands
    /// minutes later on a reconnect would be a surprise).
    pub fn restart(&self, timer: &TimerId) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        let mut found = false;
        for (key, live) in map.iter() {
            if &key.1 == timer {
                live.conn.restart();
                found = true;
            }
        }
        found
    }

    /// **Set a node's enter/exit detection thresholds** on `timer`'s live connection (#355) — the
    /// Tune page's write, carried onto the socket the Director is already holding.
    ///
    /// Keyed on the **timer** for the same reason [`restart`](Self::restart) is: the RD is
    /// calibrating a piece of hardware, and it holds exactly one connection whichever claim opened
    /// it (the active event's, or a manual hold). Tuning happens from the Timers menu with no event
    /// necessarily active at all, so the manual-hold key is the *common* case here, not the exotic
    /// one.
    ///
    /// Returns whether a live connection was found. `false` means the timer is not connected right
    /// now — nothing was emitted, and nothing is queued for a future connection: a threshold that
    /// landed minutes later on a reconnect would move a detector nobody asked to move. GridFPV's own
    /// record of the value is unaffected (`Timer::calibration`, D27); it is the *application* of it
    /// that was lost, and the RD sees that as a level that never comes back confirmed.
    pub fn calibrate(&self, timer: &TimerId, write: CalibrationWrite) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        let mut found = false;
        for (key, live) in map.iter() {
            if &key.1 == timer {
                live.conn.calibrate(write);
                found = true;
            }
        }
        found
    }

    /// **Set a node's channel** on `timer`'s live connection (#413) — the Tune page's other write,
    /// carried onto the socket the Director is already holding.
    ///
    /// Keyed on the **timer** for the same reason [`calibrate`](Self::calibrate) is: the RD is
    /// retuning a piece of hardware, and it holds exactly one connection whichever claim opened it.
    /// Tuning happens from the Timers menu with no event necessarily active, so the manual-hold key
    /// is the common case here.
    ///
    /// Note this is **not** [`tune`](Self::tune): that is the heat's whole-timer channel plan,
    /// keyed on `(event, timer)` and pushed at Stage. This is one node, from the bench, with no
    /// event needed. A heat legitimately overwrites what this set.
    ///
    /// Returns whether a live connection was found. `false` means the timer is not connected right
    /// now — nothing was emitted and nothing is queued for a future connection: a node retuned
    /// minutes later on a reconnect would move a receiver nobody asked to move. GridFPV's own record
    /// of the channel is unaffected (`Timer::node_channels`, D27); it is the *application* of it
    /// that was lost, and the RD sees a channel that never comes back confirmed.
    pub fn set_channel(&self, timer: &TimerId, write: ChannelWrite) -> bool {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        let mut found = false;
        for (key, live) in map.iter() {
            if &key.1 == timer {
                live.conn.set_channel(write.clone());
                found = true;
            }
        }
        found
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
                // Carry any **restart requests** (#386) from the timer registry — where the
                // RD-gated route parks them, the server crate having no handle on this set — onto
                // the live connections. Drained here rather than handed over directly for the same
                // reason a manual hold is a registry flag: the server crate is *below* this one,
                // so the registry is the one seam both sides already share.
                for timer in timers.take_restart_requests() {
                    if !connections.restart(&timer) {
                        // The connection went away between the route accepting the request and this
                        // tick (a deselect, a URL edit, a drop). Nothing to restart, and nothing is
                        // queued for a future connection — say so rather than fail silently.
                        let name = timers.get(&timer).map(|t| t.name);
                        eprintln!(
                            "gridfpv: no live RotorHazard connection to restart for {:?}",
                            name.as_deref().unwrap_or("that timer")
                        );
                    }
                }
                // …and any **calibration writes** (#355) the Tune page parked on the registry: the
                // RD moved an enter/exit threshold and it goes onto the live socket now. Same seam
                // and same drain-exactly-once discipline as the restart above.
                for write in timers.take_calibration_requests() {
                    let landed = connections.calibrate(
                        &write.timer,
                        CalibrationWrite {
                            node: u64::from(write.node),
                            enter_at: write.enter_at,
                            exit_at: write.exit_at,
                            during_open_practice: write.during_open_practice,
                        },
                    );
                    if !landed {
                        // The connection went away between the route accepting the write and this
                        // tick. Nothing is queued for a future connection — a threshold arriving
                        // minutes later would move a detector nobody asked to move — so say so
                        // rather than fail silently. The RD sees it as a level that never comes
                        // back confirmed on the page.
                        let name = timers.get(&write.timer).map(|t| t.name);
                        eprintln!(
                            "gridfpv: no live RotorHazard connection to calibrate node {} on {:?}",
                            write.node + 1,
                            name.as_deref().unwrap_or("that timer")
                        );
                    }
                }
                // …and any **channel writes** (#413) the Tune page parked on the registry: the RD
                // picked a channel for a node and it goes onto the live socket now. Same seam and
                // same drain-exactly-once discipline as the two above. The band/channel label was
                // resolved server-side from GridFPV's catalog and rides along, so RotorHazard's own
                // UI shows `Raceband R7` and not a bare frequency.
                for write in timers.take_channel_requests() {
                    let landed = connections.set_channel(
                        &write.timer,
                        ChannelWrite {
                            node: u64::from(write.node),
                            mhz: write.mhz,
                            band: write.band,
                            channel: write.channel,
                            during_open_practice: write.during_open_practice,
                        },
                    );
                    if !landed {
                        // The connection went away between the route accepting the write and this
                        // tick. Nothing is queued for a future connection — say so rather than fail
                        // silently; the RD sees a channel that never comes back confirmed.
                        let name = timers.get(&write.timer).map(|t| t.name);
                        eprintln!(
                            "gridfpv: no live RotorHazard connection to set node {}'s channel on \
                             {:?}",
                            write.node + 1,
                            name.as_deref().unwrap_or("that timer")
                        );
                    }
                }
            }
        })
    };
    (connections, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_server::events::CreateEventRequest;
    use gridfpv_server::timers::{
        CalibrationRequest, ChannelRequest, CreateTimerRequest, UpdateTimerRequest,
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

    #[test]
    fn a_restart_request_with_no_live_connection_is_reported_not_swallowed() {
        // #386: the RD asked to restart a timer that has since gone away (deselected, URL edited,
        // link dropped). There is nothing to emit on and nothing is queued for a future connection —
        // `restart` says so, which is what lets the reconciler log it rather than fail silently.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let connections = RhConnections::new();
        assert!(!connections.restart(&rh));
    }

    #[test]
    fn a_calibration_write_with_no_live_connection_is_reported_not_swallowed() {
        // #355: the RD moved a threshold on a timer that has since gone away (deselected, URL
        // edited, link dropped). There is nothing to emit on and nothing is queued for a future
        // connection — a threshold landing minutes later would move a detector nobody asked to
        // move — so `calibrate` says so, which is what lets the reconciler log it rather than fail
        // silently. On the page it shows as a level that never comes back confirmed.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let connections = RhConnections::new();
        assert!(!connections.calibrate(
            &rh,
            CalibrationWrite {
                node: 0,
                enter_at: Some(96),
                exit_at: None,
                during_open_practice: false,
            }
        ));
    }

    #[test]
    fn calibration_writes_drain_exactly_once_and_coalesce_per_node() {
        // The registry is the seam the RD-gated route and the (higher-layer) connection reconciler
        // share, exactly as it is for a restart; the reconciler drains it each tick. Several writes
        // to one node before a drain apply the LATEST value once — a stale threshold replayed after
        // a fresh one would leave the timer detecting against a value the page is no longer showing.
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

        let drained = timers.take_calibration_requests();
        assert_eq!(drained.len(), 1, "one entry per node, not one per write");
        assert_eq!(drained[0].enter_at, Some(96));
        assert_eq!(drained[0].exit_at, Some(70));
        assert!(
            timers.take_calibration_requests().is_empty(),
            "drained exactly once — nothing is re-queued"
        );
    }

    #[test]
    fn a_channel_write_with_no_live_connection_is_reported_not_swallowed() {
        // #413, and the exact twin of the calibration case above: the RD picked a channel for a
        // timer that has since gone away. Nothing is emitted and nothing is queued for a future
        // connection — a node retuned minutes later would move a receiver nobody asked to move — so
        // `set_channel` says so and the reconciler logs it rather than failing silently.
        let timers = registry();
        let rh = rh_timer(&timers, "Field RH", OLD_URL);
        let connections = RhConnections::new();
        assert!(!connections.set_channel(
            &rh,
            ChannelWrite {
                node: 0,
                mhz: 5880,
                band: Some("Raceband".into()),
                channel: Some("R7".into()),
                during_open_practice: false,
            }
        ));
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

        let drained = timers.take_channel_requests();
        assert_eq!(drained.len(), 1, "one entry per node, not one per pick");
        assert_eq!(drained[0].mhz, 5880);
        assert_eq!(drained[0].band.as_deref(), Some("Raceband"));
        assert_eq!(drained[0].channel.as_deref(), Some("R7"));
        assert!(
            timers.take_channel_requests().is_empty(),
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
        assert_eq!(timers.take_restart_requests(), vec![rh.clone()]);
        assert!(timers.take_restart_requests().is_empty());
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
}
