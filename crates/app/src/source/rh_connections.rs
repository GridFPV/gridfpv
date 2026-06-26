//! The **active-event RotorHazard connection set** + its reconciler (#105) — `live`-gated.
//!
//! A RotorHazard timer **connects when it is selected for the active event and stays connected**
//! (#105), so its live link is monitored continuously and a drop-off surfaces *before and between*
//! races. This module owns the set of persistent [`RhConnection`]s and the background task that
//! keeps that set in sync with the Director's state:
//!
//! - [`RhConnections`] — a shared `(EventId, TimerId) → RhConnection` map. The per-event source
//!   bridge consults it to **arm/disarm** a running heat onto the *already-live* connection (race
//!   driving decoupled from connecting); the reconciler opens/closes entries.
//! - [`spawn_rh_reconciler`] — polls the registry's **active event** and **its selected timers**,
//!   and reconciles: open a connection for every selected RotorHazard timer of the active event,
//!   close any connection whose timer was deselected or whose event is no longer active. On the
//!   active event changing, the previous event's connections are dropped (left `Disconnected`) and
//!   the new event's selected RH timers connect.
//!
//! Only the **active** event's RH timers hold a live socket — an idle, non-active event does not
//! tie up the timer. (A non-active event's heat can still be driven through its bridge, but it just
//! finds no live connection to arm; RH racing is for the event the Director is running.)

use std::collections::HashMap;
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
    inner: Arc<Mutex<HashMap<(EventId, TimerId), RhConnection>>>,
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
        if let Some(conn) = map.get(&(event.clone(), timer.clone())) {
            conn.arm_heat(lineup, sink);
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
        if let Some(conn) = map.get(&(event.clone(), timer.clone())) {
            conn.tune(assignment);
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
        if let Some(conn) = map.get(&(event.clone(), timer.clone())) {
            conn.prepare();
            true
        } else {
            false
        }
    }

    /// Disarm the current heat on `(event, timer)`'s connection (the heat left `Running`): the race
    /// is stopped/cleared but the **connection stays alive**. A no-op if no such connection.
    pub fn disarm(&self, event: &EventId, timer: &TimerId) {
        let map = self.inner.lock().expect("rh-connections lock poisoned");
        if let Some(conn) = map.get(&(event.clone(), timer.clone())) {
            conn.disarm();
        }
    }

    /// Reconcile the live set against `wanted` (the active event's selected RH timers, each with its
    /// url): open a connection for any wanted pair not yet live, and cancel+remove every live
    /// connection no longer wanted (a deselected timer, or the active event having changed). The
    /// `timers` registry is where each opened connection publishes its status.
    fn reconcile(&self, wanted: &[(EventId, TimerId, String)], timers: &TimerRegistry) {
        let mut map = self.inner.lock().expect("rh-connections lock poisoned");

        // Close connections no longer wanted (deselected timer / active event changed / no active).
        let keep: std::collections::HashSet<(EventId, TimerId)> = wanted
            .iter()
            .map(|(e, t, _)| (e.clone(), t.clone()))
            .collect();
        let stale: Vec<(EventId, TimerId)> = map
            .keys()
            .filter(|key| !keep.contains(*key))
            .cloned()
            .collect();
        for key in stale {
            if let Some(conn) = map.remove(&key) {
                // Tear down on the driver thread (stop race + disconnect + leave Disconnected).
                conn.cancel();
            }
        }

        // Open connections for newly-wanted pairs (lazily — an already-live pair is left as is).
        for (event, timer, url) in wanted {
            let key = (event.clone(), timer.clone());
            map.entry(key)
                .or_insert_with(|| RhConnection::open(timer.clone(), url.clone(), timers.clone()));
        }
    }
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
