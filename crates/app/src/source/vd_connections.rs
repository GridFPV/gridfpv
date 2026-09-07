//! The shared set of persistent **Velocidrone** connections, and the reconciler that keeps it in
//! sync with what the Director wants (#483, #484).
//!
//! The twin of [`rh_connections`](super::rh_connections), and deliberately the same shape: a map
//! keyed by *(claimant, timer)*, a pure [`plan`] over (live, wanted) that decides what to open,
//! supersede and close, and a reconciler task that applies it every
//! [`RECONCILE_INTERVAL`](super::rh_connections::RECONCILE_INTERVAL).
//!
//! It is **much smaller** than its RotorHazard counterpart, and every difference is something
//! Velocidrone genuinely does not have: no tune plan, no pending-write queue (no calibration, no
//! capture, no channel writes — the sim has no receivers), and no plugin probe (the sim's websocket
//! *is* the integration surface, so there is nothing to install and nothing that can be missing).
//! What remains is the part both adapters share, which is exactly the interface worth extracting
//! once this has run a few race days — see the note at the top of [`velocidrone`](super::velocidrone).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gridfpv_server::events::EventRegistry;
use gridfpv_server::scope::EventId;
use gridfpv_server::timers::{TimerId, TimerKind, TimerRegistry, TimerStatus};
use tokio::task::JoinHandle;

use gridfpv_adapters::velocidrone::transport::url_for_host;
use gridfpv_events::CompetitorRef;

use super::PassSink;
use super::velocidrone::{VdConnection, VdSeat};

/// How often the reconciler polls the active event + its selected timers to sync the live set.
///
/// Matches the RotorHazard reconciler's cadence; the two run independently over the same registry.
pub const RECONCILE_INTERVAL: Duration = Duration::from_millis(500);

/// What identifies one live connection: **who wants it** plus the Velocidrone timer.
///
/// `Some(event)` is the active event that selects the timer — the key a running heat's bridge seats
/// and arms on. `None` is a **manual** hold from the Timers menu: a diagnostic link that belongs to
/// no event, so the RD can see whether the sim answers at all. A timer never holds both at once.
type ConnKey = (Option<EventId>, TimerId);

/// One connection the Director wants: its [`ConnKey`] parts plus the **dial URL** built from the
/// timer's *current* host.
///
/// The URL rather than the host, because it is the thing the driver actually captured at spawn and
/// therefore the thing the reconciler must compare against to notice an edit.
type Wanted = (Option<EventId>, TimerId, String);

/// One live connection plus the URL its driver dialled.
///
/// The URL travels in the value rather than the key for the same reason it does on the RotorHazard
/// side: the key does not change when the RD edits the URL, and the driver captured the old address
/// by value at spawn — so without this the entry would retry the wrong host forever.
struct LiveConnection {
    url: String,
    conn: VdConnection,
}

/// The shared set of persistent Velocidrone connections.
///
/// Cloning shares the one map, so the reconciler (which opens and closes) and the per-event source
/// bridges (which seat and arm a running heat) act on the same connections.
#[derive(Clone, Default)]
pub struct VdConnections {
    inner: Arc<Mutex<HashMap<ConnKey, LiveConnection>>>,
    /// Keys whose driver spent its automatic connect attempts and rested.
    ///
    /// Kept beside the live map because the point of a rest is that there is no live connection to
    /// read it off — the driver thread has exited and its entry has been reaped. Without this the
    /// next tick would see "wanted, nothing live" and dial again every 500 ms forever, which is the
    /// loop the attempt cap exists to stop. This matters more for the sim than for RotorHazard: a
    /// Velocidrone that is simply not running is the *normal* resting state of the timer.
    rested: Arc<Mutex<HashSet<ConnKey>>>,
}

impl VdConnections {
    /// An empty connection set.
    pub fn new() -> Self {
        Self::default()
    }

    /// End every rest held on `timer` — *"an event needs this timer"*.
    ///
    /// Called from the stage and go paths when they find no live connection: a sim that was not
    /// running when the Director started, and is then staged into a heat, must be dialled again
    /// rather than let the heat race blind.
    fn clear_rest(&self, timer: &TimerId) {
        let mut rested = self.rested.lock().expect("vd-rested lock poisoned");
        rested.retain(|key| &key.1 != timer);
    }

    /// Seat a heat on `(event, timer)`'s live connection. Returns whether one was found **and had
    /// actually connected**.
    ///
    /// Called from the `Staged` transition, mirroring RotorHazard's seat: it is the write that puts
    /// the right pilots in the air, and its readback is handled on the driver thread.
    ///
    /// The `is_connected` half is #437's lesson, which is not RotorHazard-specific: "an entry exists
    /// in the map" is not the same question as "the socket is up". A connection whose driver is
    /// still dialling would take the seating into a queue it wipes the moment it connects, and this
    /// method would have answered `true` — *sent* becoming indistinguishable from *landed*, which is
    /// the exact failure class the readback design exists to prevent.
    pub fn seat(&self, event: &EventId, timer: &TimerId, seats: Vec<VdSeat>) -> bool {
        let map = self.inner.lock().expect("vd-connections lock poisoned");
        if let Some(live) = map
            .get(&(Some(event.clone()), timer.clone()))
            .filter(|live| live.conn.is_connected())
        {
            live.conn.seat(seats);
            true
        } else {
            drop(map);
            // No connection to seat on: if this timer rested, an event needing it is exactly the
            // thing that ends the rest.
            self.clear_rest(timer);
            false
        }
    }

    /// Arm a running heat onto the live connection for `(event, timer)`, if one exists **and has
    /// connected**: the driver starts the sim's race and routes its passes into `sink`'s log.
    /// Returns whether the heat actually armed.
    ///
    /// `fallback` is the heat's callsign → competitor pairing, used to attribute passes when
    /// seating never ran (no pilot in the heat has a Velocidrone id). Without it such a heat would
    /// record zero laps in silence — see [`VdConnection::arm_heat`].
    ///
    /// The connected check matters more here than anywhere: the return value is what tells the
    /// bridge whether this heat has *any* source at all, and arming a still-dialling connection
    /// would report a heat as sourced while nothing was ever going to feed it.
    pub fn arm_heat(
        &self,
        event: &EventId,
        timer: &TimerId,
        sink: PassSink,
        fallback: Vec<(String, CompetitorRef)>,
    ) -> bool {
        let map = self.inner.lock().expect("vd-connections lock poisoned");
        if let Some(live) = map
            .get(&(Some(event.clone()), timer.clone()))
            .filter(|live| live.conn.is_connected())
        {
            live.conn.arm_heat(sink, fallback);
            true
        } else {
            drop(map);
            self.clear_rest(timer);
            false
        }
    }

    /// Disarm the current heat on `(event, timer)`'s connection (the heat left `Running`): the sim's
    /// race is stopped but the **connection stays alive**. A no-op if no such connection.
    pub fn disarm(&self, event: &EventId, timer: &TimerId) {
        let map = self.inner.lock().expect("vd-connections lock poisoned");
        if let Some(live) = map.get(&(Some(event.clone()), timer.clone())) {
            live.conn.disarm();
        }
    }

    /// Reconcile the live set against `wanted` by applying [`plan`].
    fn reconcile(&self, wanted: &[Wanted], timers: &TimerRegistry) {
        let mut map = self.inner.lock().expect("vd-connections lock poisoned");
        let mut rested = self.rested.lock().expect("vd-rested lock poisoned");
        rested.retain(|key| still_resting(key, wanted, timers));

        let live: Vec<(ConnKey, String, bool)> = map
            .iter()
            .map(|(key, live)| (key.clone(), live.url.clone(), live.conn.driver_finished()))
            .collect();
        let (steps, newly_rested) = plan(&live, wanted, timers, &rested);
        rested.extend(newly_rested);

        for step in steps {
            match step {
                Step::Close(key) => {
                    if let Some(live) = map.remove(&key) {
                        live.conn.cancel();
                    }
                }
                Step::Supersede(key) => {
                    if let Some(live) = map.remove(&key) {
                        live.conn.cancel_superseded();
                    }
                }
                Step::Open(key, url) => {
                    let conn = VdConnection::open(key.1.clone(), url.clone(), timers.clone());
                    map.insert(key, LiveConnection { url, conn });
                }
            }
        }
    }
}

/// One move the reconciler makes to bring the live set in line with what is wanted.
///
/// Split out from [`VdConnections::reconcile`] so the decisions can be asserted without opening a
/// socket — the same reason the RotorHazard side does it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// Open a connection for this key at this URL.
    Open(ConnKey, String),
    /// Tear down and publish `Disconnected` — genuinely no longer wanted.
    Close(ConnKey),
    /// Tear down **yielding the status cell** to a successor for the same timer.
    Supersede(ConnKey),
}

/// Decide what [`Step`]s take the `live` set to the `wanted` set.
///
/// The rules, in order — the same rules as the RotorHazard reconciler, because they are properties
/// of *persistent connections keyed by claimant*, not of either protocol:
///
/// - **Wanted, same URL, driver alive** → leave it alone. A healthy link must not churn on a tick.
/// - **Wanted, different URL** → `Supersede` + `Open`. The key does not change when the RD edits the
///   URL, and the driver captured the old one at spawn, so the entry would otherwise retry the wrong
///   address forever. Superseding rather than closing hands the status cell to the successor, so the
///   timer does not flash `Disconnected` on its way back up.
/// - **Wanted, same URL, but the driver has exited** → `Supersede`, and — when the timer has gone
///   [`Unreachable`](TimerStatus::Unreachable) — record the key as **rested**. The driver spent its
///   attempts and left; the map entry is a corpse and must be reaped, but reopening immediately is
///   the dial loop the cap exists to stop.
/// - **Not wanted, but the timer is still wanted under another key** → `Supersede`: the connection is
///   being *replaced* (the active event changed, or the manual ⇄ event hand-off), so its exiting
///   driver must not stomp the successor's status.
/// - **Not wanted, and no longer a Velocidrone timer at all** → `Supersede` too: the kind was edited
///   or the timer deleted, and the registry has already written the new resting status; a parting
///   `Disconnected` would overwrite that with a lie.
/// - **Not wanted, still a Velocidrone timer** → `Close`: genuinely deselected, so `Disconnected` is
///   the truth.
fn plan(
    live: &[(ConnKey, String, bool)],
    wanted: &[Wanted],
    timers: &TimerRegistry,
    rested: &HashSet<ConnKey>,
) -> (Vec<Step>, Vec<ConnKey>) {
    let mut steps = Vec::new();
    let mut newly_rested = Vec::new();
    let wanted_timers: HashSet<&TimerId> = wanted.iter().map(|(_, timer, _)| timer).collect();

    // Tear down what should not be live as it stands.
    let mut superseded_for_reopen: HashSet<ConnKey> = HashSet::new();
    for (key, url, finished) in live {
        let want = wanted
            .iter()
            .find(|(event, timer, _)| (event, timer) == (&key.0, &key.1));
        match want {
            Some((_, _, wanted_url)) if wanted_url == url && !finished => {}
            Some((_, _, wanted_url)) if wanted_url != url => {
                steps.push(Step::Supersede(key.clone()));
                superseded_for_reopen.insert(key.clone());
            }
            // Wanted at the same URL, but the driver has exited: reap it, and rest if it gave up.
            Some(_) => {
                steps.push(Step::Supersede(key.clone()));
                if at_rest(&key.1, timers) {
                    newly_rested.push(key.clone());
                } else {
                    superseded_for_reopen.insert(key.clone());
                }
            }
            None if yields_status(&key.1, &wanted_timers, timers) => {
                steps.push(Step::Supersede(key.clone()));
            }
            None => steps.push(Step::Close(key.clone())),
        }
    }

    // Open what is wanted and not (validly) live.
    let all_rested: HashSet<&ConnKey> = rested.iter().chain(newly_rested.iter()).collect();
    for (event, timer, url) in wanted {
        let key = (event.clone(), timer.clone());
        if all_rested.contains(&key) {
            continue;
        }
        let already_live = live
            .iter()
            .any(|(k, u, finished)| k == &key && u == url && !finished);
        if already_live && !superseded_for_reopen.contains(&key) {
            continue;
        }
        steps.push(Step::Open(key, url.clone()));
    }

    (steps, newly_rested)
}

/// Whether `timer` has given up on its own and is resting at [`TimerStatus::Unreachable`].
fn at_rest(timer: &TimerId, timers: &TimerRegistry) -> bool {
    timers.get(timer).map(|t| t.status) == Some(TimerStatus::Unreachable)
}

/// Whether a recorded rest still applies: the key must still be wanted, and the timer must still be
/// resting. Anything else — the RD pressed Connect, the timer was reconfigured, the event let go —
/// ends it.
fn still_resting(key: &ConnKey, wanted: &[Wanted], timers: &TimerRegistry) -> bool {
    let wanted_here = wanted
        .iter()
        .any(|(event, timer, _)| (event, timer) == (&key.0, &key.1));
    wanted_here && at_rest(&key.1, timers)
}

/// Whether tearing `timer`'s connection down must **yield** the status cell rather than publish a
/// parting `Disconnected`: true when it is being replaced, and when it is no longer a Velocidrone
/// timer at all (the registry has already written the new kind's resting status).
fn yields_status(
    timer: &TimerId,
    wanted_timers: &HashSet<&TimerId>,
    timers: &TimerRegistry,
) -> bool {
    wanted_timers.contains(timer)
        || !matches!(
            timers.get(timer).map(|t| t.kind),
            Some(TimerKind::Velocidrone { .. })
        )
}

/// Every Velocidrone connection the Director wants right now: the active event's selected sims,
/// plus the manually-held ones. A timer claimed by both appears **once**, under the event key.
fn wanted_connections(registry: &EventRegistry, timers: &TimerRegistry) -> Vec<Wanted> {
    let mut wanted: Vec<Wanted> = Vec::new();

    if let Some(active) = registry.active() {
        if let Some(selection) = registry.timers_of(&active.id) {
            for id in selection {
                if let Some(timer) = timers.get(&id) {
                    if let TimerKind::Velocidrone { host } = timer.kind {
                        wanted.push((Some(active.id.clone()), id, url_for_host(&host)));
                    }
                }
            }
        }
    }

    for id in timers.manual_connections() {
        if wanted.iter().any(|(_, claimed, _)| *claimed == id) {
            continue;
        }
        if let Some(timer) = timers.get(&id) {
            if let TimerKind::Velocidrone { host } = timer.kind {
                wanted.push((None, id, url_for_host(&host)));
            }
        }
    }

    wanted
}

/// The add/startup dial: hold every Velocidrone timer the first time this reconciler sees it, so the
/// driver's capped automatic attempts run without the RD pressing Connect.
///
/// `seen` is the memory of ids already granted the hold, which is what keeps this from overriding
/// the RD: a timer they explicitly Disconnect is *seen*, so only Connect or an event's selection
/// dials it again.
fn auto_hold_new_timers(timers: &TimerRegistry, seen: &mut HashSet<TimerId>) {
    for timer in timers.list() {
        if !seen.insert(timer.id.clone()) {
            continue;
        }
        if matches!(timer.kind, TimerKind::Velocidrone { .. }) && !timer.manual_connect {
            let _ = timers.set_manual_connect(&timer.id, true);
        }
    }
}

/// Spawn the Velocidrone reconciler: poll [`wanted_connections`] on [`RECONCILE_INTERVAL`] and keep
/// `connections` in sync. Returns the shared set the per-event bridges seat and arm heats on, plus
/// the reconciler's handle (it runs for the process lifetime).
pub fn spawn_vd_reconciler(registry: EventRegistry) -> (VdConnections, JoinHandle<()>) {
    let connections = VdConnections::new();
    let timers = registry.timers();
    let handle = {
        let connections = connections.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(RECONCILE_INTERVAL);
            let mut seen: HashSet<TimerId> = HashSet::new();
            loop {
                ticker.tick().await;
                auto_hold_new_timers(&timers, &mut seen);
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
    use gridfpv_server::timers::CreateTimerRequest;

    const HOST: &str = "192.168.1.20";
    const NEW_HOST: &str = "192.168.1.99";
    /// What [`url_for_host`] builds from [`HOST`] — the shape the reconciler compares against.
    const URL: &str = "ws://192.168.1.20:60003/velocidrone";
    const NEW_URL: &str = "ws://192.168.1.99:60003/velocidrone";

    /// Create a Velocidrone timer at `host` and return its id.
    fn vd_timer(timers: &TimerRegistry, name: &str, host: &str) -> TimerId {
        timers
            .create(&CreateTimerRequest {
                name: name.to_string(),
                kind: TimerKind::Velocidrone {
                    host: host.to_string(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
                same_pass_window_micros: None,
            })
            .expect("create the velocidrone timer")
            .id
    }

    fn registry_with(host: &str) -> (TimerRegistry, TimerId) {
        let timers = TimerRegistry::new(None, 5, 2500).expect("in-memory timer registry");
        let id = vd_timer(&timers, "Ryan's sim", host);
        (timers, id)
    }

    fn event() -> Option<EventId> {
        Some(EventId("evt-1".into()))
    }

    #[test]
    fn a_wanted_timer_with_nothing_live_is_opened() {
        let (timers, id) = registry_with(HOST);
        let wanted = vec![(event(), id.clone(), URL.to_string())];
        let (steps, rested) = plan(&[], &wanted, &timers, &HashSet::new());
        assert_eq!(steps, vec![Step::Open((event(), id), URL.into())]);
        assert!(rested.is_empty());
    }

    /// A healthy link must not churn on a tick — that is what makes a manual hold survive.
    #[test]
    fn a_healthy_live_connection_is_left_alone() {
        let (timers, id) = registry_with(HOST);
        let url = URL.to_string();
        let live = vec![((event(), id.clone()), url.clone(), false)];
        let wanted = vec![(event(), id, url)];
        let (steps, _) = plan(&live, &wanted, &timers, &HashSet::new());
        assert!(steps.is_empty(), "no churn on a healthy tick: {steps:?}");
    }

    /// The URL edit case: the key is unchanged, so without this the driver would retry the old
    /// address forever. Supersede (not Close) so the timer does not flash `Disconnected`.
    #[test]
    fn an_edited_url_supersedes_and_reopens() {
        let (timers, id) = registry_with(NEW_HOST);
        let live = vec![((event(), id.clone()), URL.to_string(), false)];
        let wanted = vec![(event(), id.clone(), NEW_URL.to_string())];
        let (steps, _) = plan(&live, &wanted, &timers, &HashSet::new());
        assert_eq!(
            steps,
            vec![
                Step::Supersede((event(), id.clone())),
                Step::Open((event(), id), NEW_URL.into()),
            ]
        );
    }

    /// A deselected timer is genuinely disconnected — `Disconnected` is the truth here.
    #[test]
    fn a_no_longer_wanted_connection_is_closed() {
        let (timers, id) = registry_with(HOST);
        let live = vec![((event(), id.clone()), URL.to_string(), false)];
        let (steps, _) = plan(&live, &[], &timers, &HashSet::new());
        assert_eq!(steps, vec![Step::Close((event(), id))]);
    }

    /// The manual ⇄ event hand-off: the timer is still wanted, under a different key, so the old
    /// connection must yield the status cell rather than stamp a parting `Disconnected`.
    #[test]
    fn a_replaced_connection_supersedes_rather_than_closing() {
        let (timers, id) = registry_with(HOST);
        let url = URL.to_string();
        // Live under the manual hold; wanted under the active event.
        let live = vec![((None, id.clone()), url.clone(), false)];
        let wanted = vec![(event(), id.clone(), url.clone())];
        let (steps, _) = plan(&live, &wanted, &timers, &HashSet::new());
        assert_eq!(
            steps,
            vec![
                Step::Supersede((None, id.clone())),
                Step::Open((event(), id), url),
            ]
        );
    }

    /// A driver that spent its attempts and rested is reaped, recorded, and **not** immediately
    /// redialled — the sim simply not running is its normal resting state, and a 500 ms dial loop
    /// against it would be pure noise.
    #[test]
    fn a_rested_driver_is_reaped_and_not_redialled() {
        let (timers, id) = registry_with(HOST);
        timers.set_status(&id, TimerStatus::Unreachable);
        let url = URL.to_string();
        // Driver finished (`true`) at the wanted URL.
        let live = vec![((event(), id.clone()), url.clone(), true)];
        let wanted = vec![(event(), id.clone(), url)];

        let (steps, newly_rested) = plan(&live, &wanted, &timers, &HashSet::new());
        assert_eq!(steps, vec![Step::Supersede((event(), id.clone()))]);
        assert_eq!(newly_rested, vec![(event(), id.clone())]);

        // And on the next tick, with the rest recorded, it stays down.
        let rested: HashSet<ConnKey> = newly_rested.into_iter().collect();
        let wanted = vec![(event(), id, URL.to_string())];
        let (steps, _) = plan(&[], &wanted, &timers, &rested);
        assert!(steps.is_empty(), "a rested key must not redial: {steps:?}");
    }

    /// A driver that exited **without** resting (a dropped link) is reaped and reopened.
    #[test]
    fn a_dropped_driver_is_reaped_and_reopened() {
        let (timers, id) = registry_with(HOST);
        timers.set_status(&id, TimerStatus::Disconnected);
        let url = URL.to_string();
        let live = vec![((event(), id.clone()), url.clone(), true)];
        let wanted = vec![(event(), id.clone(), url.clone())];
        let (steps, newly_rested) = plan(&live, &wanted, &timers, &HashSet::new());
        assert_eq!(
            steps,
            vec![
                Step::Supersede((event(), id.clone())),
                Step::Open((event(), id), url),
            ]
        );
        assert!(newly_rested.is_empty());
    }

    /// Only Velocidrone timers are wanted here — a RotorHazard or Mock has nothing for this
    /// reconciler to dial, and dialling one would open a second socket to a timer the RH reconciler
    /// already owns.
    #[test]
    fn only_velocidrone_timers_are_wanted() {
        let events = EventRegistry::new(None).expect("in-memory event registry");
        let timers = events.timers();
        let vd = vd_timer(&timers, "Ryan's sim", HOST);
        let rh = timers
            .create(&CreateTimerRequest {
                name: "Bench RH".into(),
                kind: TimerKind::Rotorhazard {
                    url: "http://rotorhazard.local:5000".into(),
                },
                channel_capability: None,
                node_count: None,
                available_channels: None,
                same_pass_window_micros: None,
            })
            .expect("create the RH")
            .id;
        timers.set_manual_connect(&vd, true).expect("hold the sim");
        timers.set_manual_connect(&rh, true).expect("hold the RH");

        let wanted = wanted_connections(&events, &timers);
        let ids: Vec<TimerId> = wanted.into_iter().map(|(_, id, _)| id).collect();
        assert_eq!(ids, vec![vd]);
    }
}
