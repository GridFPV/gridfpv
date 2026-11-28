//! The connections to OFT timers.
//!
//! [`crate::links`] keeps one connection per timer in the directory. This is what a
//! connection to an OFT timer does:
//!
//! 1. Opens the timer's WebSocket and waits for its `hello`.
//! 2. Subscribes to crossings **after the last one it saw**, so a dropped link loses nothing
//!    the timer still holds.
//! 3. Logs what the timer reports into the **active event**. A pass is stamped with the race
//!    running at that moment by the Director (see `gridfpv_server::control`). With no active
//!    event there is nowhere to log to, and reports are dropped.
//! 4. Pings the timer to measure how its clock lines up with the Director's (a quick burst
//!    on connect, then every two seconds), and logs that when it moves.
//!
//! 5. Takes the timer's signal at full rate and logs it while a race is on, with a few
//!    seconds either side (see `SignalBuffer`). Signal outside a race is not kept.
//!
//! 6. Passes signal and each seat's levels on, live, to anyone tuning the timer. That is
//!    never logged.
//!
//! 7. Asks for control of the timer, on a timer that lets one program at a time change it
//!    (OFT spec §6.6), and keeps the timer's status saying who has it. While Grid has
//!    control, the key the timer gave it rides on every write (`gridfpv_server::timers`).
//!    After a dropped link Grid asks again with that key and gets its place back.
//!
//! 8. Follows the timer's `config_rev`: when the timer's settings change, whoever changed
//!    them, what Grid remembers of them is read again.
//!
//! A connection that fails says why in the timer's status and tries again, waiting a little
//! longer each time, up to ten seconds.
//!
//! The translation itself is pure and lives in `gridfpv_adapters::oft`.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use gridfpv_adapters::Adapter;
use gridfpv_adapters::oft::{
    ClockFit, ControlStanding, OftAdapter, OftInfo, OftMessage, SignalBuffer, claim, claim_name,
    manage_page, seat_ref,
};
use gridfpv_events::{AdapterId, Event, SeatRef, SignalThresholds, SignalUnit};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::timers::{SeatLevels, Timer, TimerControl, TimerLive};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// How many quick pings open a connection, and how far apart. The first ping or two share
/// the link with the timer's hello and the first rush of signal and come back slowly, so
/// one of them alone would place a race start tens of milliseconds out. A quick burst gets
/// a good measurement before anyone can start a race.
const OPENING_PINGS: u32 = 15;
const OPENING_PING_EVERY: Duration = Duration::from_millis(100);
/// How often the timer is pinged to measure its clock.
const PING_EVERY: Duration = Duration::from_secs(2);
/// A logged clock offset is logged again when the measurement moves by more than this many
/// microseconds.
const OFFSET_MOVED: i64 = 500;
/// How long a silent timer is waited on before the link is called dead. The timer answers
/// every ping, so silence this long means the link is gone.
const SILENCE: Duration = Duration::from_secs(8);
/// How often the timer is asked again what its receivers are. Its `hello` lists them once,
/// but one can report in later, or come back from a firmware update measuring its levels
/// in another unit; a level logged under the wrong unit would mislead a marshal.
const INFO_EVERY: Duration = Duration::from_secs(15);
/// How soon it is asked when its status names a receiver the list does not have.
const INFO_SOON: Duration = Duration::from_secs(1);

/// Start keeping a connection to every timer in the directory. Runs until aborted. (Kept
/// here under its first name; the work is in [`crate::links`].)
pub fn spawn(registry: EventRegistry) -> JoinHandle<()> {
    crate::links::spawn(registry)
}

/// Keep one timer connected for ever.
pub(crate) async fn hold(registry: EventRegistry, timer: Timer) {
    let mut adapter = OftAdapter::new(timer.id.clone());
    let mut link = Link {
        registry: registry.clone(),
        id: timer.id.clone(),
        fit: ClockFit::new(),
        signal: SignalBuffer::new(),
        levels: BTreeMap::new(),
        levels_logged: BTreeMap::new(),
        boot_id: None,
        logged: None,
        control_name: control_name(),
        control_key: None,
        config_rev: None,
        held_back: None,
    };
    let mut wait = Duration::from_millis(500);
    loop {
        let problem = match link.run(&timer, &mut adapter).await {
            Ok(()) => "The timer closed the connection.".to_owned(),
            Err(problem) => problem,
        };
        // Keep the signal a race in progress had so far.
        for chunk in link.signal.flush() {
            link.log(Event::SignalChunk(chunk));
        }
        let was_connected = registry
            .timers()
            .list()
            .iter()
            .any(|view| view.timer.id == timer.id && view.status.connected);
        registry.timers().report(&timer.id, |status| {
            status.connected = false;
            status.problem = Some(problem);
            // What the timer said was wrong may not be true by the time it is back.
            status.timer_problems.clear();
            // Who has control is not known while the timer cannot say. The key is kept:
            // the timer holds Grid's place for ten seconds, and takes writes with it.
            status.control = None;
        });
        if was_connected {
            link.log(Event::AdapterDisconnected {
                adapter: timer.id.clone(),
            });
            // It was up, so try again soon.
            wait = Duration::from_millis(500);
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(10));
    }
}

struct Link {
    registry: EventRegistry,
    id: AdapterId,
    fit: ClockFit,
    signal: SignalBuffer,
    /// Each seat's enter and exit levels as the timer last reported them, and their unit.
    levels: BTreeMap<SeatRef, (i32, i32, SignalUnit)>,
    /// The levels logged for the race that is on. Empty while no race is on.
    levels_logged: BTreeMap<SeatRef, (i32, i32, SignalUnit)>,
    /// The boot the clock fit belongs to. A new boot is a new clock.
    boot_id: Option<String>,
    /// The event the clock offset was last logged to, and the offset logged.
    logged: Option<(EventId, i64)>,
    /// The name Grid asks for control under.
    control_name: String,
    /// The key the timer gave Grid when Grid last had control. Sent with the next claim,
    /// so a dropped link gets its place back. A new boot of the timer forgets it.
    control_key: Option<String>,
    /// The timer's `config_rev` as last seen this boot.
    config_rev: Option<u32>,
    /// The holder the timer named before Grid's claim was in, while Grid had a key that
    /// may make that holder Grid itself. See [`Link::learn_control`].
    held_back: Option<String>,
}

/// The name Grid asks for control under: "GridFPV" and this computer's name, so two
/// installations can be told apart on the timer's own page.
fn control_name() -> String {
    let host = gethostname::gethostname();
    claim_name(&format!("GridFPV {}", host.to_string_lossy()))
}

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

impl Link {
    /// Log one observation into the active event, if there is one.
    fn log(&self, event: Event) {
        let Some(active) = self.registry.active() else {
            return;
        };
        if let Err(e) = with_event(&self.registry, &active.id, |session| session.observe(event)) {
            eprintln!("WARNING: could not log a timer report: {}", e.message);
        }
    }

    /// Log the clock offset when the active event has none from this link yet, or when the
    /// measurement has moved.
    fn log_clock(&mut self) {
        let (Some(offset), Some(error)) = (self.fit.offset_micros(), self.fit.error_micros())
        else {
            return;
        };
        let Some(active) = self.registry.active() else {
            return;
        };
        let stale = match &self.logged {
            Some((event, logged)) => event != &active.id || (logged - offset).abs() > OFFSET_MOVED,
            None => true,
        };
        if stale {
            self.log(Event::ClockOffset {
                adapter: self.id.clone(),
                offset_micros: offset,
                error_micros: error,
            });
            self.logged = Some((active.id, offset));
        }
    }

    /// While a race is on, log the levels the timer detects against: once when the race
    /// comes on, and again when they move by more than a dB, or by more than 10 mV on a
    /// timer that measures millivolts (a timer that follows the noise floor moves them by
    /// itself). They sit beside the race's signal for the marshal.
    fn log_levels(&mut self) {
        if !self.registry.race_on() {
            self.levels_logged.clear();
            return;
        }
        let moved: Vec<(SeatRef, (i32, i32, SignalUnit))> = self
            .levels
            .iter()
            .filter(|(seat, now)| {
                self.levels_logged.get(*seat).is_none_or(|logged| {
                    logged.2 != now.2
                        || (logged.0 - now.0).abs() > 10
                        || (logged.1 - now.1).abs() > 10
                })
            })
            .map(|(seat, now)| (seat.clone(), *now))
            .collect();
        for (seat, (enter, exit, unit)) in moved {
            self.log(Event::SignalThresholds(SignalThresholds {
                adapter: self.id.clone(),
                seat: seat.clone(),
                enter,
                exit,
                unit,
            }));
            self.levels_logged.insert(seat, (enter, exit, unit));
        }
    }

    /// Take in a `control` message: where it leaves Grid, for the timer's status and for
    /// Grid's writes. `claimed` is whether the message names a claim of this connection's.
    fn learn_control(&mut self, standing: ControlStanding, claimed: bool) {
        self.held_back = None;
        if let (ControlStanding::Other { name }, false, true) =
            (&standing, claimed, self.control_key.is_some())
        {
            // The timer's first word after `hello`, before Grid's claim is in. The holder
            // it names may be Grid's own claim from before the link dropped, which the key
            // is about to take up again. Calling that "another program" would be wrong, so
            // nothing is said until the timer answers the claim, which it does at once.
            self.held_back = Some(name.clone());
            return;
        }
        let control = match &standing {
            ControlStanding::Nobody => TimerControl::Nobody,
            ControlStanding::Ours { .. } => TimerControl::Grid,
            ControlStanding::Other { name } => TimerControl::Other { name: name.clone() },
        };
        match standing {
            ControlStanding::Ours { key } => {
                self.registry
                    .timers()
                    .set_control_key(&self.id, Some(key.clone()));
                self.control_key = Some(key);
            }
            // Another program has control: a key Grid still has is dead.
            ControlStanding::Other { .. } => {
                self.registry.timers().set_control_key(&self.id, None);
                self.control_key = None;
            }
            // Nobody holds control: a write needs no key and one that carries a key is
            // taken all the same.
            ControlStanding::Nobody => {}
        }
        let mut was = None;
        self.registry.timers().report(&self.id, |status| {
            was = status.control.replace(control.clone());
        });
        // Control moved, or this is the first word of it on this connection. Check the
        // event's pass window: Grid may be able to write where it could not, and another
        // program may have changed the window while it had control. Where Grid cannot
        // write, the check still says on the timer's status that the window is not the
        // event's.
        if was.as_ref() != Some(&control) {
            gridfpv_server::timers::spawn_pass_window_sync(&self.registry);
        }
    }

    /// The timer had no room for Grid's claim, so no answer to it is coming. What was held
    /// back while waiting for one is said now: another program has control.
    fn claim_refused(&mut self) {
        if let Some(name) = self.held_back.take() {
            self.control_key = None;
            self.learn_control(ControlStanding::Other { name }, true);
        }
    }

    /// Take in the timer's `config_rev`. When it has moved, the timer's settings changed:
    /// its seat channels are read again by the next asker, and the event's pass window is
    /// checked. Grid's own writes move it too; checking again after those costs one read.
    fn learn_config_rev(&mut self, rev: Option<u32>) {
        let Some(rev) = rev else {
            return;
        };
        let moved = self.config_rev.is_some_and(|seen| seen != rev);
        self.config_rev = Some(rev);
        self.registry.timers().report(&self.id, |status| {
            status.config_rev = Some(rev);
        });
        if moved {
            self.registry.timers().age_channels(&self.id);
            gridfpv_server::timers::spawn_pass_window_sync(&self.registry);
        }
    }

    /// One connection, from open to the first failure. `Ok` means the timer closed it.
    async fn run(&mut self, timer: &Timer, adapter: &mut OftAdapter) -> Result<(), String> {
        let url = format!("ws://{}/oft/v1/stream", timer.address);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| format!("Could not reach the timer at {}: {e}", timer.address))?;
        let mut next_ping = tokio::time::Instant::now();
        let mut pings: u32 = 0;
        // When the ping in flight was sent, on the Director's clock.
        let mut ping_sent: Option<i64> = None;
        // The timer's list of receivers is read again now and then, off this loop so a slow
        // answer never holds up a crossing.
        let (info_tx, mut info_rx) = tokio::sync::mpsc::channel::<OftInfo>(1);
        // The first read is at once: only `/info` says where the timer's own page is.
        let mut next_info = tokio::time::Instant::now();
        // Whether this connection has asked for control.
        let mut claimed = false;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(next_info) => {
                    next_info += INFO_EVERY;
                    let (timer, tx) = (timer.clone(), info_tx.clone());
                    tokio::spawn(async move {
                        // A read that fails is tried again at the next turn; the link's
                        // own health is the pings' business.
                        if let Ok(info) = gridfpv_server::timers::read_info(&timer).await {
                            let _ = tx.send(info).await;
                        }
                    });
                }
                Some(info) = info_rx.recv() => {
                    adapter.learn_nodes(&info.nodes, info.calibrated);
                    let unit = adapter.shared_unit();
                    let page = manage_page(&timer.address, info.manage_url.as_deref());
                    self.registry.timers().report(&self.id, |status| {
                        status.level_unit = unit;
                        status.manage_url = page;
                    });
                }
                _ = tokio::time::sleep_until(next_ping) => {
                    pings += 1;
                    next_ping += if pings < OPENING_PINGS {
                        OPENING_PING_EVERY
                    } else {
                        PING_EVERY
                    };
                    ping_sent = Some(now_micros());
                    socket
                        .send(Message::text(r#"{"type":"ping"}"#))
                        .await
                        .map_err(|e| format!("The link to the timer broke: {e}"))?;
                }
                frame = tokio::time::timeout(SILENCE, socket.next()) => {
                    let frame = frame
                        .map_err(|_| "The timer stopped answering.".to_owned())?;
                    let text = match frame {
                        None | Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(format!("The link to the timer broke: {e}")),
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(_)) => continue,
                    };
                    // A frame that is not a message this adapter can read is skipped: one
                    // odd frame must not cost the link.
                    let Ok(message) = OftMessage::parse(&text) else {
                        continue;
                    };
                    let mut status_levels: Option<Vec<SeatLevels>> = None;
                    match &message {
                        OftMessage::Hello {
                            timer: own_id,
                            boot_id,
                            seats,
                            capabilities,
                            ..
                        } => {
                            // The timer's own id is what lets it be followed to a new
                            // address later.
                            self.registry.timers().learn_own_id(&self.id, own_id);
                            if self.boot_id.as_deref() != Some(boot_id.as_str()) {
                                self.boot_id = Some(boot_id.clone());
                                self.fit.reset();
                                self.logged = None;
                                // A restarted timer has no claims and counts its
                                // settings' changes from the start again.
                                self.control_key = None;
                                self.registry.timers().set_control_key(&self.id, None);
                                self.config_rev = None;
                            }
                            let control_name =
                                capabilities.control.then(|| self.control_name.clone());
                            if !capabilities.control {
                                self.control_key = None;
                                self.registry.timers().set_control_key(&self.id, None);
                            }
                            let config_rev = self.config_rev;
                            let seats = seats.iter().map(|s| seat_ref(s.seat)).collect();
                            self.registry.timers().report(&self.id, |status| {
                                status.connected = true;
                                status.problem = None;
                                status.seats = seats;
                                status.seat_thresholds = capabilities.config.seat_thresholds;
                                status.keeps_settings = capabilities.config.persist;
                                status.pass_window = capabilities.config.pass_window_ms;
                                // What it has is not known until it is read again.
                                status.pass_window_ms = None;
                                status.pass_window_problem = None;
                                // Who has control comes in the timer's next message.
                                status.control = None;
                                status.control_name = control_name;
                                status.config_rev = config_rev;
                                // Said again by the timer's next `status`.
                                status.timer_problems.clear();
                            });
                            // A timer that has just connected (or restarted) may not have
                            // the event's pass window: put it there. A timer with control
                            // says who has it in its next message, and that starts the
                            // same check with the answer known.
                            if !capabilities.control {
                                gridfpv_server::timers::spawn_pass_window_sync(&self.registry);
                            }
                            self.held_back = None;
                        }
                        OftMessage::Pong { time_us } => {
                            if let Some(sent) = ping_sent.take() {
                                self.fit.sample(sent, now_micros(), *time_us);
                                let error = self.fit.error_micros();
                                self.registry.timers().report(&self.id, |status| {
                                    status.clock_error_micros = error;
                                });
                                self.log_clock();
                            }
                        }
                        OftMessage::Control { holder, claim, key } => {
                            let standing =
                                ControlStanding::read(holder.as_ref(), *claim, key.as_deref());
                            self.learn_control(standing, claimed && claim.is_some());
                        }
                        OftMessage::Error { code, msg } if code == "control_full" => {
                            // Grid has no claim, so it will not be handed control when the
                            // holder leaves. It still writes whenever nobody has control.
                            eprintln!(
                                "WARNING: timer {:?} has no room for Grid's control claim: {msg}",
                                timer.name
                            );
                            self.claim_refused();
                        }
                        OftMessage::Status { seats, config_rev, problems } => {
                            self.learn_config_rev(*config_rev);
                            if let Some(problems) = problems {
                                self.registry.timers().report(&self.id, |status| {
                                    if status.timer_problems != *problems {
                                        status.timer_problems.clone_from(problems);
                                    }
                                });
                            }
                            // For whoever is tuning, live. Never logged.
                            let levels = seats
                                .iter()
                                .map(|seat| SeatLevels {
                                    seat: seat_ref(seat.seat),
                                    // Filled in below, from the adapter.
                                    unit: SignalUnit::TenthsDb,
                                    level: seat.rssi,
                                    floor: seat.floor,
                                    enter: seat.enter,
                                    exit: seat.exit,
                                    crossing: seat.crossing,
                                })
                                .collect::<Vec<_>>();
                            // Published and logged below, once the adapter has read which
                            // receiver serves each seat and so knows each seat's unit.
                            status_levels = Some(levels);
                        }
                        _ => {}
                    }
                    let is_hello = matches!(message, OftMessage::Hello { .. });
                    let has_control = matches!(
                        &message,
                        OftMessage::Hello { capabilities, .. } if capabilities.control
                    );
                    let translated = adapter.translate(message);
                    if is_hello {
                        let unit = adapter.shared_unit();
                        self.registry.timers().report(&self.id, |status| {
                            status.level_unit = unit;
                        });
                    }
                    if let Some(mut levels) = status_levels.take() {
                        for seat in &mut levels {
                            if let Ok(index) = seat.seat.0.parse() {
                                seat.unit = adapter.unit(index);
                            }
                        }
                        self.levels = levels
                            .iter()
                            .map(|seat| (seat.seat.clone(), (seat.enter, seat.exit, seat.unit)))
                            .collect();
                        self.registry
                            .timers()
                            .publish(&self.id, TimerLive::Levels(levels));
                        self.log_levels();
                        if adapter.has_unlisted_node() {
                            // A receiver the list does not have: ask soon, not at the
                            // next slow turn.
                            next_info = next_info.min(tokio::time::Instant::now() + INFO_SOON);
                        }
                    }
                    for event in translated {
                        match event {
                            Event::SignalChunk(chunk) => {
                                self.registry
                                    .timers()
                                    .publish(&self.id, TimerLive::Signal(chunk.clone()));
                                let race_on = self.registry.race_on();
                                if race_on && self.levels_logged.is_empty() {
                                    // The race just came on: its levels go in first.
                                    self.log_levels();
                                }
                                for chunk in self.signal.push(chunk, race_on) {
                                    self.log(Event::SignalChunk(chunk));
                                }
                            }
                            other => self.log(other),
                        }
                    }
                    if is_hello {
                        // Ask for everything after the last crossing seen.
                        socket
                            .send(Message::text(adapter.subscription()))
                            .await
                            .map_err(|e| format!("The link to the timer broke: {e}"))?;
                        if has_control {
                            // Ask to be the program that may change this timer. With the
                            // key from before a dropped link, Grid gets its place back.
                            socket
                                .send(Message::text(claim(
                                    &self.control_name,
                                    self.control_key.as_deref(),
                                )))
                                .await
                                .map_err(|e| format!("The link to the timer broke: {e}"))?;
                            claimed = true;
                        }
                    }
                }
            }
        }
    }
}
