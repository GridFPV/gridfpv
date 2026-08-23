//! The **active-event RotorHazard connection set** + its reconciler (#105) — `live`-gated.
//!
//! A RotorHazard timer **connects when it is selected for the active event and stays connected**
//! (#105), so its live link is monitored continuously and a drop-off surfaces *before and between*
//! races. This module owns the set of persistent [`RhConnection`]s and the background task that
//! keeps that set in sync with the Director's state:
//!
//! - [`RhConnections`] — a shared `(EventId, TimerId) → LiveConnection` map (the value carries the
//!   **URL the connection was dialled with**, so an edited URL is noticed — #382). The per-event
//!   source bridge consults it to **arm/disarm** a running heat onto the *already-live* connection
//!   (race driving decoupled from connecting); the reconciler opens/closes entries.
//! - [`spawn_rh_reconciler`] — polls the registry's **active event** and **its selected timers**,
//!   and reconciles: open a connection for every selected RotorHazard timer of the active event,
//!   close any connection whose timer was deselected or whose event is no longer active. On the
//!   active event changing, the previous event's connections are dropped (left `Disconnected`) and
//!   the new event's selected RH timers connect.
//!
//! Only the **active** event's RH timers hold a live socket — an idle, non-active event does not
//! tie up the timer. (A non-active event's heat can still be driven through its bridge, but it just
//! finds no live connection to arm; RH racing is for the event the Director is running.)

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gridfpv_events::CompetitorRef;
use gridfpv_server::events::EventRegistry;
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{TimerId, TimerKind, TimerRegistry};
use tokio::task::JoinHandle;

use super::PassSink;
use super::rotorhazard::RhConnection;

/// How often the reconciler polls the active event + its selected timers to sync the live set.
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

/// The shared set of persistent RotorHazard connections, keyed by *(active event, RH timer)* (#105).
///
/// Cloning shares the one map (`Arc<Mutex<…>>`), so the reconciler (which opens/closes connections)
/// and the per-event source bridges (which arm/disarm a running heat onto a live connection) act on
/// the same connections. A connection exists only while its timer is selected for the active event.
#[derive(Clone, Default)]
pub struct RhConnections {
    inner: Arc<Mutex<HashMap<ConnKey, LiveConnection>>>,
}

/// What identifies one live connection: the *(active event, RH timer)* pair (#105).
type ConnKey = (EventId, TimerId);

/// One live connection **plus the dial config it was opened with** (#382).
///
/// The map key is `(event, timer)`, which does **not** change when the RD edits the timer's URL —
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
        if let Some(live) = map.get(&(event.clone(), timer.clone())) {
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
        if let Some(live) = map.get(&(event.clone(), timer.clone())) {
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
        if let Some(live) = map.get(&(event.clone(), timer.clone())) {
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
        if let Some(live) = map.get(&(event.clone(), timer.clone())) {
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
        if let Some(live) = map.get(&(event.clone(), timer.clone())) {
            live.conn.disarm();
        }
    }

    /// Reconcile the live set against `wanted` (the active event's selected RH timers, each with its
    /// url) by applying [`plan`]: open a connection for any wanted pair not yet live *or whose URL
    /// changed under it* (#382), and cancel+remove every live connection no longer wanted (a
    /// deselected timer, or the active event having changed). The `timers` registry is where each
    /// opened connection publishes its status.
    ///
    /// All the *decisions* live in [`plan`] (a pure function over the live set, the wanted set and
    /// the registry) — this only carries them out, so the reconciler's behaviour is unit-testable
    /// without dialling a real RotorHazard.
    fn reconcile(&self, wanted: &[(EventId, TimerId, String)], timers: &TimerRegistry) {
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
/// to the `wanted` set (the active event's selected RH timers, each with its *current* URL).
///
/// The rules, in order:
///
/// - **Wanted, same URL** → leave it alone. A healthy persistent link must not churn on a tick.
/// - **Wanted, different URL** (#382) → `Supersede` + `Open`. The key `(event, timer)` is unchanged
///   by a URL edit, so the old code left the entry as-is and its driver kept retrying the address
///   it captured at spawn — forever. Superseding rather than closing hands the status cell to the
///   successor, so the timer does not flash `Disconnected` on its way back up.
/// - **Not wanted, but the timer is still wanted under another key** → `Supersede`: the connection
///   is being *replaced* (an active-event switch), so its exiting driver must not stomp the
///   successor's status.
/// - **Not wanted, and the timer is no longer a RotorHazard timer at all** → `Supersede` as well.
///   Its kind was edited (RH → Mock) or it was deleted, and the registry has *already* written the
///   new resting status (`Ready`); a parting `Disconnected` would overwrite that with a lie.
/// - **Not wanted, still an RH timer** → `Close`: genuinely deselected, `Disconnected` is true.
fn plan(
    live: &[(ConnKey, String)],
    wanted: &[(EventId, TimerId, String)],
    timers: &TimerRegistry,
) -> Vec<Step> {
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

/// The set of RotorHazard timers `registry`'s **active event** currently selects, each paired with
/// the active event id and its url. The reconciler's source of truth (#105): when the active event
/// changes or its selection changes, this set changes and the live connections follow.
fn wanted_connections(
    registry: &EventRegistry,
    timers: &TimerRegistry,
) -> Vec<(EventId, TimerId, String)> {
    let Some(active) = registry.active() else {
        return Vec::new();
    };
    let Some(selection) = registry.timers_of(&active.id) else {
        return Vec::new();
    };
    let mut wanted = Vec::new();
    for id in selection {
        if let Some(timer) = timers.get(&id) {
            if let TimerKind::Rotorhazard { url } = timer.kind {
                wanted.push((active.id.clone(), id, url));
            }
        }
    }
    wanted
}

/// Spawn the reconciler (#105): poll the active event + its selected RH timers on
/// [`RECONCILE_INTERVAL`] and keep `connections` in sync — opening a persistent connection for each
/// selected RotorHazard timer of the active event and closing those no longer selected (or whose
/// event is no longer active). Returns the reconciler's [`JoinHandle`]; it runs for the process
/// lifetime. The returned [`RhConnections`] is the shared set the per-event bridges arm heats on.
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
            }
        })
    };
    (connections, handle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_server::timers::{CreateTimerRequest, UpdateTimerRequest};

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

    fn event(id: &str) -> EventId {
        EventId(id.to_string())
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
}
