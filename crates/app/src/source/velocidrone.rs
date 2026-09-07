//! The persistent **Velocidrone** connection and the set the reconciler keeps (#483, #484).
//!
//! The twin of [`rotorhazard`](super::rotorhazard) + [`rh_connections`](super::rh_connections) for
//! the sim: a driver thread per selected Velocidrone timer that dials the game's WebSocket, keeps it
//! alive, seats a heat, starts and stops the race, and routes the translated passes into the running
//! heat's log.
//!
//! # Why this is a twin rather than a shared abstraction
//!
//! #484 asked the question directly, and the answer taken (2026-09-07) is a **parallel
//! `VdConnection`**, not an extraction of the interface `RhConnection` already implies. Both are
//! push-shaped and both want roughly `open / prepare / seat / arm_heat / disarm / is_connected`, so
//! the shared trait is real and worth having. It is also a refactor of the one connection path that
//! currently runs live races, and doing it in the same change that introduces the second adapter
//! would put both at risk at once. The extraction stays worth doing; it is its own issue, and this
//! file is deliberately shaped to make it easy — the method names match `RhConnection`'s on purpose.
//!
//! # What Velocidrone does NOT have, and why that is fine
//!
//! No RSSI, no thresholds, no frequencies, no calibration, no per-node anything — and structurally
//! there cannot be: it is a simulator, the crossings are ground truth from the game engine. So this
//! driver has no `tune`, no pending-write queue, and no dense post-race marshal pull, and those
//! surfaces degrade to *absent* rather than broken (`docs/timer-adapters.html` §7). It also has no
//! plugin to probe: the sim's own websocket **is** the integration surface.
//!
//! # Attribution: the roster is both the readback and the mapping
//!
//! RotorHazard hands us a node seat (`node-3`) and the heat's lineup position resolves it. The sim
//! has no seats: `racedata` is keyed by **player name**, and the seating write takes **account
//! uids**. Those are two different namespaces, and nothing on the wire pairs them except the
//! `pilotlist` reply to `getpilots`.
//!
//! So `getpilots` does double duty here, and both duties matter:
//!
//! - it is the **positive readback** for `activate` (which has no ack, and whose failure mode is
//!   silence — an unauthorized command is simply dropped);
//! - it is the **only** frame pairing a player name with a uid, so it is what lets
//!   [`VdConnection::seat`] build the name → pilot map [`remap`] then attributes laps through.
//!
//! A player the map does not know is dropped, exactly as RotorHazard drops a pass on an idle node:
//! it is someone else in the room, not someone in this heat. The fallback, when the roster never
//! answers, is a **callsign** match on the player name — which is what makes a non-host room or a
//! pre-tournament build still time a race, just without the confirmation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use gridfpv_adapters::velocidrone::transport::{VdCommand, VelocidroneConnection};
use gridfpv_events::{AdapterId, CompetitorRef, Event};
use gridfpv_server::timers::{TimerId, TimerRegistry, TimerStatus};

use super::PassSink;

/// How often the driver drains the connection's translated-event queue.
const DRAIN_INTERVAL: Duration = Duration::from_millis(100);

/// The minimum backoff between reconnect attempts after a dropped/failed connection.
const RECONNECT_BACKOFF_MIN: Duration = Duration::from_millis(500);

/// The maximum backoff between reconnect attempts (the backoff doubles up to this ceiling).
const RECONNECT_BACKOFF_MAX: Duration = Duration::from_secs(10);

/// How many automatic connect attempts a driver makes before resting at
/// [`TimerStatus::Unreachable`] (#462's rule, applied to the sim).
///
/// The sim is very often simply not running — Velocidrone is a game the RD launches when they feel
/// like it — so pulsing `Connecting`/`Error` at a dead port forever is even less useful here than it
/// is against a RotorHazard. The Timers menu's Connect, or an event needing the timer, asks again.
const CONNECT_ATTEMPT_CAP: u32 = 5;

/// How long the driver keeps routing events into a **finishing** heat's sink after the heat left
/// `Running`, before clearing the slot. Short: unlike RotorHazard there is no dense post-race pull
/// to wait for, only the last in-flight `racedata` snapshot.
const FINISH_DRAIN_SETTLE: Duration = Duration::from_millis(750);

/// One seat of a heat on a Velocidrone timer: the sim account to activate, and the GridFPV pilot its
/// laps belong to.
///
/// `callsign` is carried alongside the ref because it is the **fallback** attribution key when the
/// sim never answers `getpilots` (a non-host room), and because an operator-facing line about a
/// seating failure must name the pilot, not print a uid (CLAUDE.md: friendly names everywhere).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VdSeat {
    /// The pilot's Velocidrone account id (`Pilot::velocidrone_id`).
    pub uid: String,
    /// The competitor ref this player's laps attribute to (the pilot id).
    pub competitor: CompetitorRef,
    /// The pilot's callsign — the fallback name match, and what an RD-facing line says.
    pub callsign: String,
}

/// A heat armed onto a live Velocidrone connection.
struct VdArmedHeat {
    /// The sink (the event's log) translated passes are appended through while armed.
    sink: PassSink,
    /// Set once the driver has asked the sim to start the race for this arming, so a re-drain
    /// cannot re-start it.
    started: bool,
    /// Set by [`VdConnection::disarm`] when the heat left `Running`: the driver stops the sim's race
    /// (if it is still going), drains briefly, then clears the slot.
    finishing: bool,
    /// When the finishing drain settle began.
    finished_at: Option<Instant>,
}

/// A persistent connection to one Velocidrone timer, driven on its own thread.
///
/// Mirrors [`RhConnection`](super::rotorhazard::RhConnection)'s contract — `open`, `seat`,
/// `arm_heat`, `disarm`, `is_connected`, `cancel`, `cancel_superseded`, `driver_finished` — so the
/// two can be pulled behind one interface later without either side moving.
pub struct VdConnection {
    /// The cancel flag the driver polls; flipped on [`cancel`](Self::cancel) / drop.
    cancel: Arc<AtomicBool>,
    /// Set when this connection is being SUPERSEDED, so its exiting driver does not stomp the
    /// successor's status cell with a parting `Disconnected`.
    yield_status: Arc<AtomicBool>,
    /// Whether the driver currently holds a live socket.
    connected: Arc<AtomicBool>,
    /// A pending seating request, taken by the driver on its next pass.
    seat: Arc<Mutex<Option<Vec<VdSeat>>>>,
    /// The heat currently armed on this connection, if any.
    armed: Arc<Mutex<Option<VdArmedHeat>>>,
    /// The driver thread handle.
    driver: JoinHandle<()>,
}

impl VdConnection {
    /// Open a persistent connection for `timer_id` at `url`, publishing status through `timers`.
    ///
    /// Spawns the driver thread immediately: it sets the timer `Connecting`, dials, and on success
    /// runs the maintain loop until cancelled. A failed dial backs off and retries up to
    /// [`CONNECT_ATTEMPT_CAP`] times, then rests at [`TimerStatus::Unreachable`].
    pub fn open(timer_id: TimerId, url: String, timers: TimerRegistry) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let yield_status = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let seat: Arc<Mutex<Option<Vec<VdSeat>>>> = Arc::new(Mutex::new(None));
        let attribution: Arc<Mutex<HashMap<String, CompetitorRef>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let armed: Arc<Mutex<Option<VdArmedHeat>>> = Arc::new(Mutex::new(None));

        let driver = {
            let ctx = DriverCtx {
                timer_id: timer_id.clone(),
                url,
                timers,
                cancel: cancel.clone(),
                yield_status: yield_status.clone(),
                connected: connected.clone(),
                seat: seat.clone(),
                attribution: attribution.clone(),
                armed: armed.clone(),
            };
            std::thread::spawn(move || run_driver(ctx))
        };

        Self {
            cancel,
            yield_status,
            connected,
            seat,
            armed,
            driver,
        }
    }

    /// Seat this heat on the sim: everyone to spectator, then activate exactly these accounts.
    ///
    /// Queued for the driver thread (the socket has one owner). The write's readback — the roster,
    /// and the absence of `ActivateError` — is handled there, where it can also be reported.
    pub fn seat(&self, seats: Vec<VdSeat>) {
        let mut slot = self.seat.lock().expect("vd-seat lock poisoned");
        *slot = Some(seats);
    }

    /// Arm a running heat: from now until [`disarm`](Self::disarm), translated passes are attributed
    /// and appended through `sink`, and the driver asks the sim to start the race.
    pub fn arm_heat(&self, sink: PassSink) {
        let mut slot = self.armed.lock().expect("vd-armed lock poisoned");
        *slot = Some(VdArmedHeat {
            sink,
            started: false,
            finishing: false,
            finished_at: None,
        });
    }

    /// The heat left `Running`: stop the sim's race and clear the arming after a brief drain.
    pub fn disarm(&self) {
        let mut slot = self.armed.lock().expect("vd-armed lock poisoned");
        if let Some(heat) = slot.as_mut() {
            heat.finishing = true;
        }
    }

    /// Whether the driver currently holds a live socket.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    /// Tear down: stop the race if one is armed, disconnect, publish `Disconnected`.
    pub fn cancel(&self) {
        self.connected.store(false, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Tear down **yielding the status cell** to a successor connection for the same timer, so the
    /// timer does not flash `Disconnected` on its way back up.
    pub fn cancel_superseded(&self) {
        self.connected.store(false, Ordering::Relaxed);
        self.yield_status.store(true, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Whether the driver thread has exited (it spent its attempts and rested, or it was cancelled).
    pub fn driver_finished(&self) -> bool {
        self.driver.is_finished()
    }
}

impl Drop for VdConnection {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::Relaxed);
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Everything the driver thread owns or shares.
struct DriverCtx {
    timer_id: TimerId,
    url: String,
    timers: TimerRegistry,
    cancel: Arc<AtomicBool>,
    yield_status: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    seat: Arc<Mutex<Option<Vec<VdSeat>>>>,
    attribution: Arc<Mutex<HashMap<String, CompetitorRef>>>,
    armed: Arc<Mutex<Option<VdArmedHeat>>>,
}

/// The driver thread: dial with backoff, then maintain until cancelled or rested.
fn run_driver(ctx: DriverCtx) {
    let mut backoff = RECONNECT_BACKOFF_MIN;
    let mut attempts = 0u32;

    while !ctx.cancel.load(Ordering::Relaxed) {
        ctx.timers
            .set_status(&ctx.timer_id, TimerStatus::Connecting);
        match VelocidroneConnection::connect(&ctx.url) {
            Ok(conn) => {
                attempts = 0;
                backoff = RECONNECT_BACKOFF_MIN;
                ctx.connected.store(true, Ordering::Relaxed);
                ctx.timers.set_status(&ctx.timer_id, TimerStatus::Connected);
                maintain(&ctx, &conn);
                ctx.connected.store(false, Ordering::Relaxed);
                conn.disconnect();
                if ctx.cancel.load(Ordering::Relaxed) {
                    break;
                }
                // The link dropped on its own (the sim quit, or another client took the feed and
                // the socket eventually closed). Say so, then dial again.
                ctx.timers
                    .set_status(&ctx.timer_id, TimerStatus::Disconnected);
            }
            Err(err) => {
                attempts += 1;
                if attempts >= CONNECT_ATTEMPT_CAP {
                    // Rest rather than pulse at a dead address forever (#462). The sim is very often
                    // just not running; the RD's Connect, or an event needing the timer, asks again.
                    eprintln!(
                        "gridfpv: velocidrone: {} did not answer at {} after {attempts} attempts \
                         ({err}) — resting. Check that Velocidrone is running with Options → Main \
                         Settings → Websocket Communication = Yes, and that the URL uses the \
                         machine's LAN IP (the sim does not bind loopback).",
                        timer_name(&ctx.timers, &ctx.timer_id),
                        ctx.url,
                    );
                    ctx.timers
                        .set_status(&ctx.timer_id, TimerStatus::Unreachable);
                    return;
                }
                ctx.timers.set_status(&ctx.timer_id, TimerStatus::Error);
            }
        }

        // Back off before the next attempt, waking often enough to notice a cancel.
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline && !ctx.cancel.load(Ordering::Relaxed) {
            std::thread::sleep(DRAIN_INTERVAL.min(backoff));
        }
        backoff = (backoff * 2).min(RECONNECT_BACKOFF_MAX);
    }

    // A superseded connection leaves the status cell to its successor rather than stamping a
    // `Disconnected` the RD would read as a link that just dropped.
    if !ctx.yield_status.load(Ordering::Relaxed) {
        ctx.timers
            .set_status(&ctx.timer_id, TimerStatus::Disconnected);
    }
}

/// The maintain loop over one live socket: drain and route events, service a pending seating, start
/// the race when a heat arms, and finish it when the heat leaves `Running`.
fn maintain(ctx: &DriverCtx, conn: &VelocidroneConnection) {
    // Ask for the roster up front: it is the only frame that pairs a player name with a uid, so
    // having it before the heat stages makes attribution possible even if seating is never called.
    conn.send(VdCommand::GetPilots);

    while !ctx.cancel.load(Ordering::Relaxed) {
        if !conn.is_connected() {
            return;
        }

        // A pending seating: the socket has one owner, so the write happens here.
        let pending = ctx.seat.lock().expect("vd-seat lock poisoned").take();
        if let Some(seats) = pending {
            apply_seating(ctx, conn, &seats);
        }

        // Route whatever the sim said into the armed heat's log.
        let events = conn.events();
        if !events.is_empty() {
            deliver(ctx, events);
        }

        // Start the race once, when a heat arms; finish it when the heat leaves `Running`.
        service_arming(ctx, conn);

        std::thread::sleep(DRAIN_INTERVAL);
    }
}

/// Perform a seating write and build the attribution map from its readback.
///
/// The sim acks nothing and drops unauthorized commands in silence, so this is where the
/// foreign-system rule is paid: the outcome is assembled from the absence of `ActivateError` plus
/// the `getpilots` roster, and **anything short of a clean, confirmed seating is said out loud**.
/// A seating that cannot be confirmed is not a seating that worked — it is the one an RD needs to
/// know about before the heat flies, not after.
fn apply_seating(ctx: &DriverCtx, conn: &VelocidroneConnection, seats: &[VdSeat]) {
    let name = timer_name(&ctx.timers, &ctx.timer_id);
    let uids: Vec<String> = seats.iter().map(|s| s.uid.clone()).collect();
    let outcome = conn.seat(&uids);

    // Attribution: the roster pairs a player NAME (what `racedata` is keyed by) with a uid (what we
    // seated). Fall back to the pilot's callsign, which is what a non-host room or a pre-tournament
    // build leaves us with.
    let mut map: HashMap<String, CompetitorRef> = HashMap::new();
    for seat in seats {
        map.insert(seat.callsign.to_lowercase(), seat.competitor.clone());
    }
    if let Some(roster) = &outcome.roster {
        for entry in roster {
            if let Some(seat) = seats.iter().find(|s| s.uid == entry.uid) {
                map.insert(entry.name.to_lowercase(), seat.competitor.clone());
            }
        }
    }
    *ctx.attribution
        .lock()
        .expect("vd-attribution lock poisoned") = map;

    // Report, by callsign — never by uid (CLAUDE.md: a raw id reaching an operator is a bug).
    let named = |uid: &String| {
        seats
            .iter()
            .find(|s| &s.uid == uid)
            .map(|s| s.callsign.clone())
            .unwrap_or_else(|| uid.clone())
    };
    if !outcome.not_found.is_empty() {
        let who: Vec<String> = outcome.not_found.iter().map(named).collect();
        eprintln!(
            "gridfpv: velocidrone: {name} could not seat {} — the sim says they are not in the \
             room. Their laps will NOT be recorded. Have them join the room, then re-stage the heat.",
            who.join(", "),
        );
    }
    if !outcome.unusable.is_empty() {
        let who: Vec<String> = outcome.unusable.iter().map(named).collect();
        eprintln!(
            "gridfpv: velocidrone: {name} has no usable Velocidrone account id for {} — the seating \
             write omitted them rather than guess. Set their Velocidrone ID on the pilot.",
            who.join(", "),
        );
    }
    if outcome.roster.is_none() {
        eprintln!(
            "gridfpv: velocidrone: {name} did not answer `getpilots`, so the seating is \
             UNCONFIRMED — most likely this Velocidrone is not the multiplayer room host, in which \
             case every seating command was dropped SILENTLY and the field is not what GridFPV \
             thinks it is. Laps will be attributed by callsign instead."
        );
    }
}

/// Start the sim's race when a heat arms; stop it and clear the slot when the heat finishes.
fn service_arming(ctx: &DriverCtx, conn: &VelocidroneConnection) {
    let mut slot = ctx.armed.lock().expect("vd-armed lock poisoned");
    let Some(heat) = slot.as_mut() else {
        return;
    };

    if heat.finishing {
        // Stop the sim's race, once — but only if it is still going. A race that reached
        // `race finished` on its own must not be aborted after the fact.
        if heat.finished_at.is_none() {
            let still_racing = !matches!(
                conn.readback().last_race_action.as_deref(),
                Some("race finished") | Some("abort") | None
            );
            if still_racing {
                conn.send(VdCommand::AbortRace);
            }
            heat.finished_at = Some(Instant::now());
        }
        // Keep routing the last in-flight snapshot into this heat's sink, then let go.
        if heat
            .finished_at
            .is_some_and(|at| at.elapsed() >= FINISH_DRAIN_SETTLE)
        {
            *slot = None;
        }
        return;
    }

    if !heat.started {
        heat.started = true;
        conn.send(VdCommand::StartRace);
    }
}

/// Attribute and append the drained events into the armed heat's log.
fn deliver(ctx: &DriverCtx, events: Vec<Event>) {
    let attribution = ctx
        .attribution
        .lock()
        .expect("vd-attribution lock poisoned")
        .clone();
    let adapter = AdapterId(format!("velocidrone:{}", ctx.timer_id.0));

    let slot = ctx.armed.lock().expect("vd-armed lock poisoned");
    let Some(heat) = slot.as_ref() else {
        return;
    };
    for event in events {
        let Some(event) = remap(event, &attribution, &adapter) else {
            continue;
        };
        if let Err(e) = heat.sink.append_event(event) {
            eprintln!("gridfpv: velocidrone: could not append a pass: {e:?}");
        }
    }
}

/// Remap one canonical Velocidrone [`Event`] onto the heat's pilots, or `None` to drop it.
///
/// `racedata` is keyed by **player name**, so a pass arrives attributed to whatever the pilot calls
/// themselves in the sim; `attribution` (built from the `getpilots` roster, keyed lowercased) turns
/// that into the GridFPV pilot whose lap it is. A player the map does not know is **dropped** — they
/// are someone else in the room, not someone in this heat, exactly as RotorHazard's remap drops a
/// pass on a node outside the lineup.
///
/// Lifecycle and `CompetitorSeen` events carry no lap and are dropped: the heat's lineup comes from
/// the control path, as it does for RotorHazard.
fn remap(
    event: Event,
    attribution: &HashMap<String, CompetitorRef>,
    adapter: &AdapterId,
) -> Option<Event> {
    match event {
        Event::Pass(mut pass) => {
            let competitor = attribution.get(&pass.competitor.0.to_lowercase())?.clone();
            pass.adapter = adapter.clone();
            pass.competitor = competitor;
            Some(Event::Pass(pass))
        }
        _ => None,
    }
}

/// A timer's **friendly name** for an operator log line, falling back to its raw id only if the
/// registry no longer holds it (CLAUDE.md: friendly names everywhere, raw ids as a last resort).
fn timer_name(timers: &TimerRegistry, id: &TimerId) -> String {
    timers
        .get(id)
        .map(|t| t.name.clone())
        .unwrap_or_else(|| id.0.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{GateIndex, Pass, SourceTime};

    fn pass_for(name: &str) -> Event {
        Event::Pass(Pass {
            adapter: AdapterId("velocidrone".into()),
            competitor: CompetitorRef(name.into()),
            at: SourceTime::from_micros(1_000_000),
            sequence: None,
            gate: GateIndex::LAP,
            signal: None,
            heat: None,
        })
    }

    fn adapter() -> AdapterId {
        AdapterId("velocidrone:t1".into())
    }

    #[test]
    fn a_pass_is_attributed_to_the_seated_pilot() {
        let map = HashMap::from([("dacus".to_string(), CompetitorRef("pilot-7".into()))]);
        let Some(Event::Pass(p)) = remap(pass_for("Dacus"), &map, &adapter()) else {
            panic!("expected a remapped pass");
        };
        assert_eq!(p.competitor, CompetitorRef("pilot-7".into()));
        assert_eq!(p.adapter, adapter());
    }

    /// The sim reports every player in the room, not just this heat's. Someone else practising in
    /// the same room must not have their laps recorded against this heat.
    #[test]
    fn a_player_who_is_not_in_the_heat_is_dropped() {
        let map = HashMap::from([("dacus".to_string(), CompetitorRef("pilot-7".into()))]);
        assert!(remap(pass_for("Someone Else"), &map, &adapter()).is_none());
    }

    /// Sim names are user-set and their casing is not stable; the map is keyed lowercased.
    #[test]
    fn attribution_is_case_insensitive() {
        let map = HashMap::from([("dacus".to_string(), CompetitorRef("pilot-7".into()))]);
        assert!(remap(pass_for("DACUS"), &map, &adapter()).is_some());
    }

    /// Lifecycle and presence events carry no lap; the lineup comes from the control path.
    #[test]
    fn lifecycle_events_are_not_appended() {
        let map = HashMap::new();
        let seen = Event::CompetitorSeen {
            adapter: AdapterId("velocidrone".into()),
            competitor: CompetitorRef("Dacus".into()),
        };
        assert!(remap(seen, &map, &adapter()).is_none());
    }
}
