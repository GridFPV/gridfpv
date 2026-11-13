//! The connection to a RotorHazard timer.
//!
//! RotorHazard is used as it ships: nothing is installed on it. The translation and every
//! fact about RotorHazard's messages live in `gridfpv_adapters::rotorhazard`. This is the
//! part that holds the socket. It:
//!
//! 1. Opens RotorHazard's WebSocket and asks for its state, so a link that dropped
//!    mid-race catches up on the laps it missed.
//! 2. Logs each crossing into the active event, once.
//! 3. Asks RotorHazard for its clock every two seconds, to line it up with the Director's.
//! 4. **Tells RotorHazard when to listen.** RotorHazard only records crossings while its
//!    own race runs. So while a Grid race that uses this timer is counting down or running,
//!    RotorHazard's race is kept on with the same start; when Grid's race is over it is
//!    stopped and cleared. Grid never asks RotorHazard to save anything.
//! 5. Passes each seat's level on, live, for tuning.
//!
//! A connection that fails says why in the timer's status and tries again, waiting a
//! little longer each time, up to ten seconds.

use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use gridfpv_adapters::Adapter;
use gridfpv_adapters::oft::ClockFit;
use gridfpv_adapters::rotorhazard::wire::{self, Frame};
use gridfpv_adapters::rotorhazard::{
    self as rh, RhMessage, RhRaceStatus, RotorHazardAdapter, Step, Want, seat_ref,
};
use gridfpv_events::{AdapterId, Event};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::timers::{SeatLevels, Timer, TimerLive};
use tokio_tungstenite::tungstenite::Message;

/// How often RotorHazard is asked for its clock.
const ASK_TIME_EVERY: Duration = Duration::from_secs(2);
/// How often what RotorHazard is doing is compared with what Grid wants.
const CHECK_EVERY: Duration = Duration::from_millis(200);
/// A step is not sent again until RotorHazard has had this long to act on it.
const STEP_SETTLE: Duration = Duration::from_millis(1500);
/// A logged clock offset is logged again when the measurement moves by more than this many
/// microseconds.
const OFFSET_MOVED: i64 = 500;
/// RotorHazard sends a heartbeat ten times a second, so silence this long means the link
/// is gone.
const SILENCE: Duration = Duration::from_secs(8);

fn now_micros() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_micros() as i64)
        .unwrap_or(0)
}

/// Keep one RotorHazard connected for ever.
pub(crate) async fn hold(registry: EventRegistry, timer: Timer) {
    let mut wait = Duration::from_millis(500);
    loop {
        let mut link = Link::new(registry.clone(), timer.id.clone());
        let problem = match link.run(&timer).await {
            Ok(()) => "The timer closed the connection.".to_owned(),
            Err(problem) => problem,
        };
        let was_connected = link.connected;
        registry.timers().report(&timer.id, |status| {
            status.connected = false;
            status.problem = Some(problem);
        });
        if was_connected {
            link.log(Event::AdapterDisconnected {
                adapter: timer.id.clone(),
            });
            wait = Duration::from_millis(500);
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(Duration::from_secs(10));
    }
}

/// One connection's state. A new one is made for every connect: RotorHazard has no boot
/// id, so nothing learned on an old link (its clock above all) is trusted on a new one.
struct Link {
    registry: EventRegistry,
    id: AdapterId,
    adapter: RotorHazardAdapter,
    fit: ClockFit,
    connected: bool,
    /// RotorHazard's race status, once it has said.
    status: Option<RhRaceStatus>,
    /// The start Grid last asked RotorHazard for, while that race is still RotorHazard's.
    asked_start_s: Option<f64>,
    /// The last step sent, and when: it is not sent again until it has had time to land.
    last_step: Option<(Step, Instant)>,
    /// The clock question in flight: its number and when it was sent.
    time_asked: Option<(u64, i64)>,
    next_ack: u64,
    /// Each node's enter and exit levels, as RotorHazard last reported them.
    levels: (Vec<i32>, Vec<i32>),
    /// The event the clock offset was last logged to, and the offset logged.
    logged: Option<(EventId, i64)>,
}

impl Link {
    fn new(registry: EventRegistry, id: AdapterId) -> Self {
        let mut adapter = RotorHazardAdapter::new(id.clone());
        // What is already in the active event's log must not be logged again when
        // RotorHazard sends its whole lap list on connect.
        if let Some(log) = registry
            .active()
            .and_then(|active| registry.log(&active.id))
        {
            if let Ok(log) = log.lock() {
                for stored in log.read_all().unwrap_or_default() {
                    if let Event::Pass(pass) = stored.event {
                        if pass.adapter == id {
                            adapter.already_logged(&pass.seat, pass.at);
                        }
                    }
                }
            }
        }
        Self {
            registry,
            id,
            adapter,
            fit: ClockFit::new(),
            connected: false,
            status: None,
            asked_start_s: None,
            last_step: None,
            time_asked: None,
            next_ack: 0,
            levels: (Vec::new(), Vec::new()),
            logged: None,
        }
    }

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

    /// What Grid needs from RotorHazard right now. A Grid race only counts once this
    /// timer's clock is known: without it there is no way to say when to start.
    fn want(&self) -> Want {
        let race = self
            .registry
            .race_now()
            .filter(|race| race.timers.contains(&self.id));
        match (race, self.fit.offset_micros()) {
            (Some(race), Some(offset)) => Want::Listening {
                start_at_s: (race.starts_at + offset) as f64 / 1_000_000.0,
            },
            _ => Want::Idle,
        }
    }

    /// The frame to send next, if RotorHazard is not where Grid wants it.
    fn next_frame(&mut self) -> Option<String> {
        let status = self.status?;
        if status == RhRaceStatus::Ready && self.want() == Want::Idle {
            // RotorHazard's race is over and cleared: it is nobody's now.
            self.asked_start_s = None;
        }
        let step = rh::next_step(
            status,
            self.adapter.race_started_at_s(),
            self.asked_start_s.is_some(),
            self.want(),
        )?;
        // Give the last step time to land before sending the same one again.
        if let Some((last, at)) = self.last_step {
            let same = std::mem::discriminant(&last) == std::mem::discriminant(&step);
            if same && at.elapsed() < STEP_SETTLE {
                return None;
            }
            if same && matches!(step, Step::Stage { .. }) {
                // Asked, waited, and RotorHazard is still not listening.
                self.registry.timers().report(&self.id, |status| {
                    status.problem = Some(
                        "The timer did not start listening for this race. Check its own Run page."
                            .to_owned(),
                    );
                });
            }
        }
        self.last_step = Some((step, Instant::now()));
        Some(match step {
            Step::Stage { start_at_s } => {
                self.asked_start_s = Some(start_at_s);
                rh::stage(start_at_s)
            }
            Step::Stop => rh::stop(),
            Step::Discard => rh::discard(),
        })
    }

    /// One connection, from open to the first failure. `Ok` means RotorHazard closed it.
    async fn run(&mut self, timer: &Timer) -> Result<(), String> {
        let url = format!("ws://{}{}", timer.address, wire::SOCKET_PATH);
        let broke = |e| format!("The link to the timer broke: {e}");
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.map_err(|e| {
            eprintln!(
                "WARNING: timer {:?} at {} did not answer: {e}",
                timer.name, timer.address
            );
            format!(
                "Could not reach {}. Check that it is switched on and on the network.",
                timer.name
            )
        })?;
        let mut ask_time = tokio::time::interval(ASK_TIME_EVERY);
        let mut check = tokio::time::interval(CHECK_EVERY);
        loop {
            tokio::select! {
                _ = ask_time.tick(), if self.connected => {
                    let id = self.next_ack;
                    self.next_ack += 1;
                    self.time_asked = Some((id, now_micros()));
                    socket.send(Message::text(rh::ask_time(id))).await.map_err(broke)?;
                }
                _ = check.tick(), if self.connected => {
                    if let Some(frame) = self.next_frame() {
                        socket.send(Message::text(frame)).await.map_err(broke)?;
                    }
                }
                frame = tokio::time::timeout(SILENCE, socket.next()) => {
                    let frame = frame.map_err(|_| "The timer stopped answering.".to_owned())?;
                    let text = match frame {
                        None | Some(Ok(Message::Close(_))) => return Ok(()),
                        Some(Err(e)) => return Err(broke(e)),
                        Some(Ok(Message::Text(text))) => text,
                        Some(Ok(_)) => continue,
                    };
                    match wire::parse(&text) {
                        Frame::Open => {
                            socket.send(Message::text(wire::connect())).await.map_err(broke)?;
                        }
                        Frame::Ping => {
                            socket.send(Message::text(wire::pong())).await.map_err(broke)?;
                        }
                        Frame::Connected => {
                            self.connected = true;
                            self.registry.timers().report(&self.id, |status| {
                                status.connected = true;
                                status.problem = None;
                            });
                            self.log(Event::AdapterConnected { adapter: self.id.clone() });
                            socket.send(Message::text(rh::ask_state())).await.map_err(broke)?;
                        }
                        Frame::Disconnected => return Ok(()),
                        Frame::Ack { id, args } => {
                            let asked = self.time_asked.filter(|(asked, _)| *asked == id);
                            if let (Some((_, sent)), Some(seconds)) = (asked, rh::server_time_s(&args)) {
                                self.time_asked = None;
                                self.fit.sample(sent, now_micros(), (seconds * 1_000_000.0) as u64);
                                let error = self.fit.error_micros();
                                self.registry.timers().report(&self.id, |status| {
                                    status.clock_error_micros = error;
                                });
                                self.log_clock();
                            }
                        }
                        Frame::Event { name, args } => {
                            if let Some(message) = RhMessage::read(&name, &args) {
                                self.take(message);
                            }
                        }
                        Frame::Other => {}
                    }
                }
            }
        }
    }

    /// Act on one message from RotorHazard.
    fn take(&mut self, message: RhMessage) {
        match &message {
            RhMessage::RaceStatus { status, .. } => {
                if self.status != Some(*status) {
                    // RotorHazard moved: whatever was sent has landed, so the next step
                    // need not wait.
                    self.last_step = None;
                    if *status == RhRaceStatus::Racing {
                        self.registry
                            .timers()
                            .report(&self.id, |status| status.problem = None);
                    }
                }
                self.status = Some(*status);
            }
            RhMessage::Frequencies { mhz } => {
                let seats = mhz
                    .iter()
                    .enumerate()
                    .filter(|(_, mhz)| **mhz > 0)
                    .map(|(node, _)| seat_ref(node))
                    .collect();
                self.registry
                    .timers()
                    .report(&self.id, |status| status.seats = seats);
            }
            RhMessage::Levels { enter, exit } => self.levels = (enter.clone(), exit.clone()),
            RhMessage::Heartbeat { levels, crossing } => {
                // For whoever is tuning, live. Never logged.
                let seats = levels
                    .iter()
                    .enumerate()
                    .map(|(node, level)| SeatLevels {
                        seat: seat_ref(node),
                        level: Some(*level),
                        floor: None,
                        enter: self.levels.0.get(node).copied().unwrap_or(0),
                        exit: self.levels.1.get(node).copied().unwrap_or(0),
                        crossing: crossing.get(node).copied().unwrap_or(false),
                    })
                    .collect();
                self.registry
                    .timers()
                    .publish(&self.id, TimerLive::Levels(seats));
            }
            RhMessage::StageReady { .. } | RhMessage::CurrentLaps { .. } => {}
        }
        for event in self.adapter.translate(message) {
            self.log(event);
        }
    }
}
