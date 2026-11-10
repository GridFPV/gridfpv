//! The connections to OFT timers.
//!
//! [`spawn`] starts one task that watches the timer directory and keeps one connection per
//! OFT timer in it. Each connection:
//!
//! 1. Opens the timer's WebSocket and waits for its `hello`.
//! 2. Subscribes to crossings **after the last one it saw**, so a dropped link loses nothing
//!    the timer still holds.
//! 3. Logs what the timer reports into the **active event**. A pass is stamped with the race
//!    running at that moment by the Director (see `gridfpv_server::control`). With no active
//!    event there is nowhere to log to, and reports are dropped.
//! 4. Pings the timer every two seconds to measure how its clock lines up with the
//!    Director's, and logs that when it moves.
//!
//! 5. Takes the timer's signal at full rate and logs it while a race is on, with a few
//!    seconds either side (see `SignalBuffer`). Signal outside a race is not kept.
//!
//! 6. Passes signal and each seat's levels on, live, to anyone tuning the timer. That is
//!    never logged.
//!
//! A connection that fails says why in the timer's status and tries again, waiting a little
//! longer each time, up to ten seconds.
//!
//! The translation itself is pure and lives in `gridfpv_adapters::oft`.

use std::collections::BTreeMap;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use gridfpv_adapters::Adapter;
use gridfpv_adapters::oft::{ClockFit, OftAdapter, OftMessage, SignalBuffer, seat_ref};
use gridfpv_events::{AdapterId, Event, SeatRef, SignalThresholds, SignalUnit};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::timers::{SeatLevels, Timer, TimerKind, TimerLive};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;

/// How often the timer is pinged to measure its clock.
const PING_EVERY: Duration = Duration::from_secs(2);
/// A logged clock offset is logged again when the measurement moves by more than this many
/// microseconds.
const OFFSET_MOVED: i64 = 500;
/// How long a silent timer is waited on before the link is called dead. The timer answers
/// every ping, so silence this long means the link is gone.
const SILENCE: Duration = Duration::from_secs(8);

/// Start keeping a connection to every OFT timer in the directory. Runs until aborted.
pub fn spawn(registry: EventRegistry) -> JoinHandle<()> {
    tokio::spawn(supervise(registry))
}

async fn supervise(registry: EventRegistry) {
    let directory = registry.timers();
    let mut changes = directory.watch();
    let mut held: BTreeMap<AdapterId, (Timer, JoinHandle<()>)> = BTreeMap::new();
    loop {
        let wanted: BTreeMap<AdapterId, Timer> = directory
            .records()
            .into_iter()
            .filter(|timer| timer.kind == TimerKind::Oft)
            .map(|timer| (timer.id.clone(), timer))
            .collect();
        // Drop connections to timers that were removed or changed.
        held.retain(|id, (timer, task)| {
            let keep = wanted.get(id) == Some(timer);
            if !keep {
                task.abort();
            }
            keep
        });
        for (id, timer) in wanted {
            held.entry(id).or_insert_with(|| {
                let task = tokio::spawn(hold(registry.clone(), timer.clone()));
                (timer, task)
            });
        }
        if changes.changed().await.is_err() {
            return;
        }
    }
}

/// Keep one timer connected for ever.
async fn hold(registry: EventRegistry, timer: Timer) {
    let mut adapter = OftAdapter::new(timer.id.clone());
    let mut link = Link {
        registry: registry.clone(),
        id: timer.id.clone(),
        fit: ClockFit::new(),
        signal: SignalBuffer::new(),
        levels: BTreeMap::new(),
        unit: SignalUnit::TenthsDb,
        levels_logged: BTreeMap::new(),
        boot_id: None,
        logged: None,
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
    /// Each seat's enter and exit levels as the timer last reported them, and the unit.
    levels: BTreeMap<SeatRef, (i32, i32)>,
    unit: SignalUnit,
    /// The levels logged for the race that is on. Empty while no race is on.
    levels_logged: BTreeMap<SeatRef, (i32, i32)>,
    /// The boot the clock fit belongs to. A new boot is a new clock.
    boot_id: Option<String>,
    /// The event the clock offset was last logged to, and the offset logged.
    logged: Option<(EventId, i64)>,
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
    /// comes on, and again when they move by more than a dB (a timer that follows the noise
    /// floor moves them by itself). They sit beside the race's signal for the marshal.
    fn log_levels(&mut self) {
        if !self.registry.race_on() {
            self.levels_logged.clear();
            return;
        }
        let moved: Vec<(SeatRef, (i32, i32))> = self
            .levels
            .iter()
            .filter(|(seat, now)| {
                self.levels_logged.get(*seat).is_none_or(|logged| {
                    (logged.0 - now.0).abs() > 10 || (logged.1 - now.1).abs() > 10
                })
            })
            .map(|(seat, now)| (seat.clone(), *now))
            .collect();
        for (seat, (enter, exit)) in moved {
            self.log(Event::SignalThresholds(SignalThresholds {
                adapter: self.id.clone(),
                seat: seat.clone(),
                enter,
                exit,
                unit: self.unit,
            }));
            self.levels_logged.insert(seat, (enter, exit));
        }
    }

    /// One connection, from open to the first failure. `Ok` means the timer closed it.
    async fn run(&mut self, timer: &Timer, adapter: &mut OftAdapter) -> Result<(), String> {
        let url = format!("ws://{}/oft/v1/stream", timer.address);
        let (mut socket, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| format!("Could not reach the timer at {}: {e}", timer.address))?;
        let mut ping = tokio::time::interval(PING_EVERY);
        // When the ping in flight was sent, on the Director's clock.
        let mut ping_sent: Option<i64> = None;
        loop {
            tokio::select! {
                _ = ping.tick() => {
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
                    match &message {
                        OftMessage::Hello {
                            boot_id,
                            seats,
                            calibrated,
                            ..
                        } => {
                            self.unit = if *calibrated {
                                SignalUnit::TenthsDbm
                            } else {
                                SignalUnit::TenthsDb
                            };
                            if self.boot_id.as_deref() != Some(boot_id.as_str()) {
                                self.boot_id = Some(boot_id.clone());
                                self.fit.reset();
                                self.logged = None;
                            }
                            let seats = seats.iter().map(|s| seat_ref(s.seat)).collect();
                            self.registry.timers().report(&self.id, |status| {
                                status.connected = true;
                                status.problem = None;
                                status.seats = seats;
                            });
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
                        OftMessage::Status { seats } => {
                            // For whoever is tuning, live. Never logged.
                            let levels = seats
                                .iter()
                                .map(|seat| SeatLevels {
                                    seat: seat_ref(seat.seat),
                                    level: seat.rssi,
                                    floor: seat.floor,
                                    enter: seat.enter,
                                    exit: seat.exit,
                                    crossing: seat.crossing,
                                })
                                .collect();
                            self.registry
                                .timers()
                                .publish(&self.id, TimerLive::Levels(levels));
                            self.levels = seats
                                .iter()
                                .map(|seat| (seat_ref(seat.seat), (seat.enter, seat.exit)))
                                .collect();
                            self.log_levels();
                        }
                        _ => {}
                    }
                    let is_hello = matches!(message, OftMessage::Hello { .. });
                    for event in adapter.translate(message) {
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
                    }
                }
            }
        }
    }
}
