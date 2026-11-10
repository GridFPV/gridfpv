//! The OFT adapter: an Open FPV Timer's messages, translated to Grid's events.
//!
//! OFT is specified in the `open-fpv-timer` project (`oft-spec/oft-spec.md`, protocol v1
//! under `/oft/v1`). A Timer always listens and always reports: it has no notion of a race.
//! It sends a `hello` when a client connects, then `crossing`, `samples` and `status`
//! messages on one WebSocket. Grid owns the race; the Timer only says when a seat crossed
//! the gate.
//!
//! This module is the **pure** half: parse a message, keep the little state that needs
//! keeping, and give back events. It opens no socket and reads no clock, so it is tested
//! with messages copied from the spec and from `oftd`. The half that holds the connection
//! lives in the Director.
//!
//! What is translated so far (conformance level L1):
//!
//! - `hello` → [`Event::AdapterConnected`], and a [`Event::SeatSeen`] for each seat.
//! - `crossing` → [`Event::Pass`] at `t_mid`, the lap instant (spec §8).
//! - `pong` → a clock sample for [`ClockFit`], which maps the Director's clock onto the
//!   Timer's so the start tone can be placed on the Timer's clock.
//!
//! - `samples` → [`Event::SignalChunk`], with every level exactly as the Timer sent it
//!   (signed tenths of a dB). The Director does not log all of them: [`SignalBuffer`]
//!   decides which, so a race's signal is kept and idle hours are not.
//!
//! `status` gives no events: its per-seat levels are for the Tune screen, live, and are
//! never logged. [`OftConfig`] is the Timer's settings as `/config` reads and writes them.

use std::collections::{BTreeMap, VecDeque};

use gridfpv_events::{
    AdapterId, Event, GateIndex, Pass, SeatRef, SignalChunk, SignalContext, SignalUnit, SourceTime,
};
use serde::{Deserialize, Serialize};

use crate::{Adapter, Capabilities};

/// One seat in a Timer's `hello` or `/info`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OftSeat {
    /// The seat's index on the Timer.
    pub seat: u32,
    /// The seat's actual sample rate. More seats on one Node means a lower rate each.
    #[serde(default)]
    pub rate_hz: u32,
}

/// One seat's levels right now, from a `status` message. Power is in tenths of a dB.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OftSeatStatus {
    pub seat: u32,
    /// The latest signal level.
    #[serde(default)]
    pub rssi: Option<i32>,
    /// The tracked noise floor.
    #[serde(default)]
    pub floor: Option<i32>,
    /// The level that opens a crossing, as the Timer is using it now.
    #[serde(default)]
    pub enter: i32,
    /// The level that closes it.
    #[serde(default)]
    pub exit: i32,
    /// Whether the seat is inside a crossing right now.
    #[serde(default)]
    pub crossing: bool,
}

/// One seat in a Timer's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OftSeatConfig {
    pub seat: u32,
    pub freq_mhz: u32,
    pub enabled: bool,
}

/// How a Timer sets its crossing levels. Values are tenths of a dB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum OftThreshold {
    /// Enter at the noise floor plus `snr_enter`; exit `hysteresis` below that.
    Auto { snr_enter: i32, hysteresis: i32 },
    /// Fixed enter and exit levels.
    Manual { enter: i32, exit: i32 },
}

/// A Timer's settings: the body of `GET` and `PUT /oft/v1/config` (spec §6.3). In protocol
/// v1 the threshold is one setting for the whole Timer; per-seat thresholds are agreed for
/// the next version of the spec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OftConfig {
    pub seats: Vec<OftSeatConfig>,
    pub seats_per_node: u32,
    pub threshold: OftThreshold,
    pub min_lap_ms: u32,
}

/// One message from a Timer's WebSocket. Fields Grid does not use are left out; unknown
/// fields and unknown message types are ignored, as the spec requires.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OftMessage {
    /// Sent once on connect.
    Hello {
        /// The Timer's id ("oft-3fa2").
        #[serde(default)]
        timer: String,
        /// Changes every time the Timer boots. Crossing `seq` numbers restart with it.
        boot_id: String,
        /// The Timer's clock now, in microseconds since it booted.
        #[serde(default)]
        time_us: u64,
        #[serde(default)]
        seats: Vec<OftSeat>,
        /// Whether power values are tenths of a dBm (true) or relative tenths of a dB.
        #[serde(default)]
        calibrated: bool,
    },
    /// A seat crossed the gate.
    Crossing {
        /// Counts up by one per crossing since boot.
        seq: u64,
        seat: u32,
        /// The lap instant: the middle of the crossing, on the Timer's clock.
        t_mid: u64,
        /// Peak power, in tenths of a dB.
        #[serde(default)]
        peak: Option<i32>,
    },
    /// The answer to a `ping`: the Timer's clock now.
    Pong { time_us: u64 },
    /// A batch of signal samples for one seat.
    Samples {
        seat: u32,
        /// The Timer-clock time of the first sample.
        t0: u64,
        /// Microseconds between samples.
        dt_us: u32,
        /// Power, in tenths of a dB.
        p: Vec<i32>,
        /// The noise floor, in tenths of a dB.
        #[serde(default)]
        nf: Option<i32>,
    },
    /// Periodic health, with each seat's levels right now.
    Status {
        #[serde(default)]
        seats: Vec<OftSeatStatus>,
    },
    /// The Timer acknowledged a subscription.
    Ack {},
    /// The Timer refused something the client sent.
    Error {
        #[serde(default)]
        code: String,
        #[serde(default)]
        msg: String,
    },
    /// A message type this adapter does not know. Ignored.
    #[serde(other)]
    Unknown,
}

impl OftMessage {
    /// Parse one WebSocket text frame.
    pub fn parse(frame: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(frame)
    }
}

/// The Grid seat handle for an OFT seat index. A handle, not a name: a console shows
/// "Seat 1" for seat 0.
pub fn seat_ref(seat: u32) -> SeatRef {
    SeatRef(seat.to_string())
}

/// The translator for one Timer.
#[derive(Debug, Clone)]
pub struct OftAdapter {
    id: AdapterId,
    /// The boot the `seq` numbers below belong to.
    boot_id: Option<String>,
    /// The highest crossing `seq` seen this boot.
    last_seq: Option<u64>,
    /// Whether the Timer said its power values are calibrated.
    calibrated: bool,
}

impl OftAdapter {
    /// A translator for the Timer Grid knows by `id`.
    pub fn new(id: AdapterId) -> Self {
        Self {
            id,
            boot_id: None,
            last_seq: None,
            calibrated: false,
        }
    }

    /// The subscription to send after `hello`: every crossing after the last one seen, so a
    /// reconnect picks up what was missed. After a reboot, or on first contact, everything
    /// the Timer still holds.
    /// It also asks for every seat's signal at the Timer's full rate: Grid keeps signal as
    /// fine as the Timer has it.
    pub fn subscription(&self) -> String {
        let samples = r#""samples":{"seats":"all","decimate":1},"status":true"#;
        match self.last_seq {
            Some(seq) => {
                format!(r#"{{"type":"sub","crossings":{{"since":{seq}}},{samples}}}"#)
            }
            None => format!(r#"{{"type":"sub","crossings":{{}},{samples}}}"#),
        }
    }
}

impl Adapter for OftAdapter {
    type Raw = OftMessage;

    fn id(&self) -> &AdapterId {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::none()
            .with_live_passes()
            .with_signal_context()
            .with_frequency_mgmt()
            .with_calibration()
    }

    fn translate(&mut self, raw: OftMessage) -> Vec<Event> {
        match raw {
            OftMessage::Hello {
                boot_id,
                seats,
                calibrated,
                ..
            } => {
                self.calibrated = calibrated;
                if self.boot_id.as_deref() != Some(boot_id.as_str()) {
                    // A new boot: the Timer's crossing numbers start again.
                    self.boot_id = Some(boot_id);
                    self.last_seq = None;
                }
                let mut events = vec![Event::AdapterConnected {
                    adapter: self.id.clone(),
                }];
                events.extend(seats.into_iter().map(|seat| Event::SeatSeen {
                    adapter: self.id.clone(),
                    seat: seat_ref(seat.seat),
                }));
                events
            }
            OftMessage::Crossing {
                seq,
                seat,
                t_mid,
                peak,
            } => {
                // A replay after a reconnect repeats crossings already logged.
                if self.last_seq.is_some_and(|last| seq <= last) {
                    return Vec::new();
                }
                self.last_seq = Some(seq);
                vec![Event::Pass(Pass {
                    adapter: self.id.clone(),
                    seat: seat_ref(seat),
                    at: SourceTime::from_micros(i64::try_from(t_mid).unwrap_or(i64::MAX)),
                    sequence: Some(seq),
                    gate: GateIndex::LAP,
                    signal: peak.map(|tenths| SignalContext {
                        rssi_peak: Some(tenths as f32 / 10.0),
                    }),
                    race: None,
                })]
            }
            OftMessage::Samples {
                seat,
                t0,
                dt_us,
                p,
                nf,
            } => vec![Event::SignalChunk(SignalChunk {
                adapter: self.id.clone(),
                seat: seat_ref(seat),
                from: SourceTime::from_micros(i64::try_from(t0).unwrap_or(i64::MAX)),
                period_micros: dt_us,
                levels: p,
                unit: if self.calibrated {
                    SignalUnit::TenthsDbm
                } else {
                    SignalUnit::TenthsDb
                },
                noise_floor: nf,
            })],
            OftMessage::Pong { .. }
            | OftMessage::Status { .. }
            | OftMessage::Ack {}
            | OftMessage::Error { .. }
            | OftMessage::Unknown => Vec::new(),
        }
    }
}

/// Maps the Director's clock onto a Timer's clock, from ping round trips.
///
/// Each sample is one `ping`: when it was sent and when the `pong` came back, both on the
/// Director's clock, and the Timer's clock in the `pong`. The Timer read its clock somewhere
/// inside the round trip; the middle is the best guess, and the error is at most half the
/// round trip. So the fit uses the sample with the **shortest round trip** (spec §7), out
/// of the latest [`ClockFit::WINDOW`] pings.
///
/// Lap times never use this: they come from the Timer's clock alone. It is used to place
/// the start tone, which the Director decides, on the Timer's clock.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClockFit {
    /// The latest pings, oldest first: (Timer clock − Director clock, round trip).
    recent: std::collections::VecDeque<(i64, i64)>,
}

impl ClockFit {
    /// How many pings are kept. Two clocks drift apart (tens of microseconds a second is
    /// normal), so an old ping, however quick, stops being true. Only the latest ones count.
    pub const WINDOW: usize = 30;

    /// A fit with no samples yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add one ping. Times are microseconds: `sent` and `received` on the Director's clock,
    /// `timer_us` from the `pong`. A sample that came back before it was sent is dropped.
    pub fn sample(&mut self, sent: i64, received: i64, timer_us: u64) {
        let round_trip = received - sent;
        if round_trip < 0 {
            return;
        }
        let middle = sent + round_trip / 2;
        let offset = i64::try_from(timer_us).unwrap_or(i64::MAX) - middle;
        if self.recent.len() == Self::WINDOW {
            self.recent.pop_front();
        }
        self.recent.push_back((offset, round_trip));
    }

    /// The kept ping with the shortest round trip.
    fn best(&self) -> Option<(i64, i64)> {
        self.recent
            .iter()
            .copied()
            .min_by_key(|(_, round_trip)| *round_trip)
    }

    /// The Timer's clock minus the Director's, in microseconds. `None` until there is a
    /// sample.
    pub fn offset_micros(&self) -> Option<i64> {
        self.best().map(|(offset, _)| offset)
    }

    /// A moment on the Director's clock, as the Timer's clock would read it. `None` until
    /// there is a sample.
    pub fn to_timer(&self, director_micros: i64) -> Option<SourceTime> {
        self.offset_micros()
            .map(|offset| SourceTime::from_micros(director_micros + offset))
    }

    /// The most the mapping can be off by, in microseconds: half the best round trip.
    pub fn error_micros(&self) -> Option<i64> {
        self.best().map(|(_, round_trip)| round_trip / 2)
    }

    /// Forget every sample. Call this when the Timer reboots: its clock started again.
    pub fn reset(&mut self) {
        self.recent.clear();
    }
}

/// Decides which signal chunks are logged, and joins small ones into bigger ones.
///
/// A Timer sends signal all day, in batches a few tens of milliseconds long. Logging all of
/// it would bury an event's log, and logging none would leave a marshal nothing to review.
/// So:
///
/// - While a race is **on** (counting down or running), everything is logged.
/// - The last [`SignalBuffer::LEAD_MICROS`] before a race came on is logged too, so the
///   trace shows the pilots sitting on the start, and so is the
///   [`SignalBuffer::TAIL_MICROS`] after it went off.
/// - Outside that, signal is held briefly and then dropped.
///
/// Batches are joined into chunks about a second long before they are logged, so a race is
/// a few hundred log entries, not tens of thousands. Joining changes no sample value. Only
/// batches that follow on from each other (to within half a sample), at the same rate and
/// unit, are joined; a real gap starts a new chunk. A chunk is kept to about a second so
/// that the time of a sample inside it, counted from the chunk's start, stays true to a
/// small fraction of a sample even when the Timer's clocks drift. The joined chunk carries
/// the latest noise floor of its batches.
///
/// Times are on the Timer's clock, taken from the samples themselves. This is pure: no clock
/// is read.
#[derive(Debug, Clone, Default)]
pub struct SignalBuffer {
    /// Held while no race is on: the last few seconds per seat.
    lead: BTreeMap<SeatRef, VecDeque<SignalChunk>>,
    /// Being joined while a race is on (or its tail runs): one growing chunk per seat.
    open: BTreeMap<SeatRef, SignalChunk>,
    /// While the tail after a race runs: the Timer-clock time it ends at.
    tail_until: Option<i64>,
    was_on: bool,
}

/// The Timer-clock time just after a chunk's last sample.
fn end_of(chunk: &SignalChunk) -> i64 {
    chunk.from.micros + chunk.levels.len() as i64 * i64::from(chunk.period_micros)
}

impl SignalBuffer {
    /// How much signal from before a race came on is kept.
    pub const LEAD_MICROS: i64 = 3_000_000;
    /// How much signal from after a race went off is kept.
    pub const TAIL_MICROS: i64 = 3_000_000;
    /// A joined chunk is logged once it is this long.
    pub const CHUNK_MICROS: i64 = 1_000_000;

    /// An empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Take one chunk from the Timer. `race_on` says whether a race is counting down or
    /// running right now. Gives back the chunks to log, in order.
    pub fn push(&mut self, chunk: SignalChunk, race_on: bool) -> Vec<SignalChunk> {
        let now = end_of(&chunk);
        let mut out = Vec::new();
        if race_on {
            self.tail_until = None;
            if !self.was_on {
                // The race just came on: what was held becomes the start of the trace.
                let held = std::mem::take(&mut self.lead);
                for chunk in held.into_values().flatten() {
                    self.join(chunk, &mut out);
                }
            }
        } else if self.was_on {
            self.tail_until = Some(now + Self::TAIL_MICROS);
        }
        self.was_on = race_on;

        let logging = race_on
            || self
                .tail_until
                .is_some_and(|until| chunk.from.micros < until);
        if logging {
            self.join(chunk, &mut out);
        } else {
            if self.tail_until.take().is_some() {
                // The tail just ended: log what was still being joined.
                out.extend(std::mem::take(&mut self.open).into_values());
            }
            let held = self.lead.entry(chunk.seat.clone()).or_default();
            held.push_back(chunk);
            while held
                .front()
                .is_some_and(|oldest| end_of(oldest) < now - Self::LEAD_MICROS)
            {
                held.pop_front();
            }
        }
        out
    }

    /// Everything still being joined, to log now. Call this when the link drops, so a race
    /// in progress keeps what it had.
    pub fn flush(&mut self) -> Vec<SignalChunk> {
        self.tail_until = None;
        std::mem::take(&mut self.open).into_values().collect()
    }

    /// Add a chunk to its seat's growing chunk, logging that one first if this does not
    /// follow on from it, or once it is long enough.
    fn join(&mut self, chunk: SignalChunk, out: &mut Vec<SignalChunk>) {
        let seat = chunk.seat.clone();
        // "Follows on" allows for less than half a sample of slack. A real Timer maps its
        // receiver's clock onto its own, so a batch starts a microsecond or two off where
        // the one before ended (*seen* against `oftd`: -1, 0 or +1 µs).
        let follows = self.open.get(&seat).is_some_and(|open| {
            (end_of(open) - chunk.from.micros).abs() * 2 < i64::from(open.period_micros)
                && open.period_micros == chunk.period_micros
                && open.unit == chunk.unit
        });
        match self.open.get_mut(&seat) {
            Some(open) if follows => {
                open.levels.extend(chunk.levels);
                open.noise_floor = chunk.noise_floor.or(open.noise_floor);
            }
            _ => {
                if let Some(done) = self.open.insert(seat.clone(), chunk) {
                    out.push(done);
                }
            }
        }
        let long_enough = self
            .open
            .get(&seat)
            .is_some_and(|open| end_of(open) - open.from.micros >= Self::CHUNK_MICROS);
        if long_enough {
            out.extend(self.open.remove(&seat));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> OftAdapter {
        OftAdapter::new(AdapterId("gate".into()))
    }

    /// The `hello` from the spec (§6.4), with the seat list `oftd` adds.
    const HELLO: &str = r#"{
        "type": "hello", "proto": 1, "timer": "oft-3fa2", "boot_id": "9c1e",
        "time_us": 81234567, "level": "L3", "encodings": ["json"],
        "seats": [{"seat": 0, "node": 0, "rate_hz": 500}, {"seat": 1, "node": 0, "rate_hz": 500}],
        "capabilities": { "crossings": { "algorithms": ["edge_mid"], "replay": 256 } }
    }"#;

    /// The `crossing` from the spec (§6.4).
    const CROSSING: &str = r#"{"type":"crossing","seq":42,"seat":0,"t_enter":81230120,
        "t_exit":81262480,"t_mid":81246300,"peak":-235,"snr":685,"flags":[]}"#;

    fn crossing(seq: u64, seat: u32, t_mid: u64) -> OftMessage {
        OftMessage::Crossing {
            seq,
            seat,
            t_mid,
            peak: None,
        }
    }

    #[test]
    fn a_hello_reports_the_timer_and_its_seats() {
        let mut adapter = adapter();
        let events = adapter.translate(OftMessage::parse(HELLO).unwrap());
        assert_eq!(
            events,
            vec![
                Event::AdapterConnected {
                    adapter: AdapterId("gate".into())
                },
                Event::SeatSeen {
                    adapter: AdapterId("gate".into()),
                    seat: SeatRef("0".into())
                },
                Event::SeatSeen {
                    adapter: AdapterId("gate".into()),
                    seat: SeatRef("1".into())
                },
            ]
        );
    }

    #[test]
    fn a_crossing_is_a_pass_at_its_middle_instant() {
        let mut adapter = adapter();
        let events = adapter.translate(OftMessage::parse(CROSSING).unwrap());
        let [Event::Pass(pass)] = &events[..] else {
            panic!("expected one pass, got {events:?}");
        };
        assert_eq!(pass.adapter, AdapterId("gate".into()));
        assert_eq!(pass.seat, SeatRef("0".into()));
        assert_eq!(pass.at, SourceTime::from_micros(81_246_300));
        assert_eq!(pass.sequence, Some(42));
        assert_eq!(pass.signal.unwrap().rssi_peak, Some(-23.5));
        // The adapter does not know the race. The Director stamps it.
        assert_eq!(pass.race, None);
    }

    #[test]
    fn a_replay_after_a_reconnect_adds_no_duplicates() {
        let mut adapter = adapter();
        adapter.translate(OftMessage::parse(HELLO).unwrap());
        assert_eq!(
            adapter.subscription(),
            r#"{"type":"sub","crossings":{},"samples":{"seats":"all","decimate":1},"status":true}"#
        );
        assert_eq!(adapter.translate(crossing(1, 0, 100)).len(), 1);
        assert_eq!(adapter.translate(crossing(2, 0, 200)).len(), 1);
        // The link drops. The same Timer says hello again, and Grid asks from where it was.
        adapter.translate(OftMessage::parse(HELLO).unwrap());
        assert_eq!(
            adapter.subscription(),
            r#"{"type":"sub","crossings":{"since":2},"samples":{"seats":"all","decimate":1},"status":true}"#
        );
        // A Timer that replays more than was asked for still adds nothing twice.
        assert!(adapter.translate(crossing(2, 0, 200)).is_empty());
        assert_eq!(adapter.translate(crossing(3, 0, 300)).len(), 1);
    }

    #[test]
    fn a_reboot_starts_the_crossing_numbers_again() {
        let mut adapter = adapter();
        adapter.translate(OftMessage::parse(HELLO).unwrap());
        adapter.translate(crossing(7, 0, 100));
        let rebooted = HELLO.replace("9c1e", "a000");
        adapter.translate(OftMessage::parse(&rebooted).unwrap());
        assert!(adapter.subscription().contains(r#""crossings":{}"#));
        // Crossing 1 of the new boot is new, though 1 is below 7.
        assert_eq!(adapter.translate(crossing(1, 0, 50)).len(), 1);
    }

    #[test]
    fn messages_grid_does_not_use_yet_are_parsed_and_ignored() {
        let mut adapter = adapter();
        for frame in [
            r#"{"type":"status","uptime_s":3600,"temp_c":41,"dropped":0,"clock_err_us":40}"#,
            r#"{"type":"ack","for":"sub"}"#,
            r#"{"type":"error","code":"unsupported","msg":"no samples"}"#,
            r#"{"type":"something_from_v2","x":1}"#,
        ] {
            let message = OftMessage::parse(frame).unwrap();
            assert!(adapter.translate(message).is_empty(), "{frame}");
        }
        assert!(OftMessage::parse("not json").is_err());
    }

    #[test]
    fn the_clock_fit_keeps_the_ping_with_the_shortest_round_trip() {
        let mut fit = ClockFit::new();
        assert_eq!(fit.to_timer(0), None);
        // A slow ping: 10 ms round trip. The Timer's clock is 1 s behind, give or take.
        fit.sample(5_000_000, 5_010_000, 4_006_000);
        assert_eq!(fit.error_micros(), Some(5_000));
        // A fast ping: 400 µs round trip, and a truer reading.
        fit.sample(6_000_000, 6_000_400, 5_000_200);
        assert_eq!(fit.error_micros(), Some(200));
        assert_eq!(
            fit.to_timer(7_000_000),
            Some(SourceTime::from_micros(6_000_000))
        );
        // A later slow ping does not displace it.
        fit.sample(8_000_000, 8_050_000, 7_040_000);
        assert_eq!(fit.error_micros(), Some(200));
        assert_eq!(fit.offset_micros(), Some(-1_000_000));
        // Clocks drift, so the quick ping only counts while it is recent.
        for i in 0..ClockFit::WINDOW as i64 {
            let sent = 9_000_000 + i * 1_000_000;
            fit.sample(sent, sent + 2_000, (sent + 1_000 - 999_000) as u64);
        }
        assert_eq!(fit.offset_micros(), Some(-999_000));
        assert_eq!(fit.error_micros(), Some(1_000));
        fit.reset();
        assert_eq!(fit.to_timer(0), None);
    }

    #[test]
    fn samples_keep_every_level_as_the_timer_sent_it() {
        let mut adapter = adapter();
        let frame = r#"{"type":"samples","seat":0,"t0":81234000,"dt_us":1000,"p":[-912,-910,-640,-235],"nf":-920}"#;
        let events = adapter.translate(OftMessage::parse(frame).unwrap());
        assert_eq!(
            events,
            vec![Event::SignalChunk(SignalChunk {
                adapter: AdapterId("gate".into()),
                seat: SeatRef("0".into()),
                from: SourceTime::from_micros(81_234_000),
                period_micros: 1000,
                levels: vec![-912, -910, -640, -235],
                unit: SignalUnit::TenthsDb,
                noise_floor: Some(-920),
            })]
        );
        // A calibrated Timer's levels are tenths of a dBm.
        let calibrated = HELLO.replace(r#""level": "L3""#, r#""level": "L3", "calibrated": true"#);
        adapter.translate(OftMessage::parse(&calibrated).unwrap());
        let events = adapter.translate(OftMessage::parse(frame).unwrap());
        let [Event::SignalChunk(chunk)] = &events[..] else {
            panic!("expected one chunk");
        };
        assert_eq!(chunk.unit, SignalUnit::TenthsDbm);
    }

    /// A 50 ms batch at 1 kHz for a seat, starting at `from_ms`, every level equal to `from_ms`.
    fn batch(seat: &str, from_ms: i64) -> SignalChunk {
        SignalChunk {
            adapter: AdapterId("gate".into()),
            seat: SeatRef(seat.into()),
            from: SourceTime::from_micros(from_ms * 1000),
            period_micros: 1000,
            levels: vec![from_ms as i32; 50],
            unit: SignalUnit::TenthsDb,
            noise_floor: Some(-900),
        }
    }

    /// Feed batches covering `[from_ms, to_ms)` and collect what is logged.
    fn feed(
        buffer: &mut SignalBuffer,
        from_ms: i64,
        to_ms: i64,
        race_on: bool,
    ) -> Vec<SignalChunk> {
        (from_ms..to_ms)
            .step_by(50)
            .flat_map(|ms| buffer.push(batch("0", ms), race_on))
            .collect()
    }

    fn span(chunks: &[SignalChunk]) -> (i64, i64) {
        (
            chunks.first().unwrap().from.micros / 1000,
            end_of(chunks.last().unwrap()) / 1000,
        )
    }

    #[test]
    fn signal_outside_a_race_is_not_logged() {
        let mut buffer = SignalBuffer::new();
        assert!(feed(&mut buffer, 0, 60_000, false).is_empty());
    }

    #[test]
    fn a_race_logs_its_signal_with_a_lead_and_a_tail_in_one_second_chunks() {
        let mut buffer = SignalBuffer::new();
        let mut logged = feed(&mut buffer, 0, 10_000, false);
        // The race comes on at 10 s and goes off at 20 s.
        logged.extend(feed(&mut buffer, 10_000, 20_000, true));
        logged.extend(feed(&mut buffer, 20_000, 40_000, false));
        // Three seconds of lead, the race, three seconds of tail (to the batch edge).
        let (from, to) = span(&logged);
        assert!((6_900..=7_000).contains(&from), "lead starts at {from} ms");
        assert!((23_000..=23_100).contains(&to), "tail ends at {to} ms");
        // Joined into chunks of about a second, with nothing lost and nothing repeated.
        assert!(logged.len() <= 18, "{} chunks", logged.len());
        for pair in logged.windows(2) {
            assert_eq!(end_of(&pair[0]), pair[1].from.micros);
        }
        let levels: Vec<i32> = logged.iter().flat_map(|c| c.levels.clone()).collect();
        assert_eq!(levels.len() as i64, to - from);
        // Every sample is the one the Timer sent: batch N holds the value N.
        assert_eq!(levels[0] as i64, from);
        assert_eq!(*levels.last().unwrap() as i64, to - 50);
    }

    #[test]
    fn a_gap_in_the_signal_is_never_joined_over() {
        let mut buffer = SignalBuffer::new();
        let mut logged = buffer.push(batch("0", 0), true);
        // 50 ms are missing here.
        logged.extend(buffer.push(batch("0", 100), true));
        logged.extend(buffer.flush());
        assert_eq!(logged.len(), 2);
        assert_eq!(logged[0].levels.len(), 50);
        assert_eq!(logged[1].from, SourceTime::from_micros(100_000));
    }

    #[test]
    fn seats_are_joined_apart_and_a_dropped_link_keeps_what_it_had() {
        let mut buffer = SignalBuffer::new();
        let mut logged = Vec::new();
        for ms in (0..500).step_by(50) {
            logged.extend(buffer.push(batch("0", ms), true));
            logged.extend(buffer.push(batch("1", ms), true));
        }
        // Half a second each: not long enough to log yet.
        assert!(logged.is_empty());
        let flushed = buffer.flush();
        assert_eq!(flushed.len(), 2);
        assert!(flushed.iter().all(|chunk| chunk.levels.len() == 500));
        assert!(buffer.flush().is_empty());
    }

    #[test]
    fn a_status_message_carries_each_seats_levels() {
        let frame = r#"{"type":"status","uptime_s":12,"time_us":12000000,"temp_c":41.5,
            "dropped":0,"clock_err_us":null,"clock_synced":false,"clock_source":"ping",
            "drift_ppm":null,"node_connected":true,"clients":1,
            "seats":[{"seat":0,"node":"00aabbccddee","rate_hz":1000,"rssi":-880,
                      "floor":-900,"enter":-750,"exit":-800,"crossing":false}]}"#;
        let OftMessage::Status { seats } = OftMessage::parse(frame).unwrap() else {
            panic!("expected a status");
        };
        assert_eq!(
            seats,
            vec![OftSeatStatus {
                seat: 0,
                rssi: Some(-880),
                floor: Some(-900),
                enter: -750,
                exit: -800,
                crossing: false,
            }]
        );
    }

    #[test]
    fn a_timers_settings_round_trip_in_the_timers_own_shape() {
        // The default `/config` of `oftd`.
        let body = r#"{"seats":[{"seat":0,"freq_mhz":5800,"enabled":true}],"seats_per_node":8,
            "threshold":{"mode":"auto","snr_enter":150,"hysteresis":50},"min_lap_ms":2000}"#;
        let config: OftConfig = serde_json::from_str(body).unwrap();
        assert_eq!(
            config.threshold,
            OftThreshold::Auto {
                snr_enter: 150,
                hysteresis: 50
            }
        );
        let again: serde_json::Value = serde_json::to_value(&config).unwrap();
        assert_eq!(
            again,
            serde_json::from_str::<serde_json::Value>(body).unwrap()
        );
        let manual = r#"{"mode":"manual","enter":-700,"exit":-760}"#;
        assert_eq!(
            serde_json::from_str::<OftThreshold>(manual).unwrap(),
            OftThreshold::Manual {
                enter: -700,
                exit: -760
            }
        );
    }

    #[test]
    fn batches_a_microsecond_off_are_joined_as_a_real_timer_sends_them() {
        let mut buffer = SignalBuffer::new();
        let mut second = batch("0", 50);
        second.from = SourceTime::from_micros(50_001);
        let mut third = batch("0", 100);
        third.from = SourceTime::from_micros(99_999);
        let mut logged = buffer.push(batch("0", 0), true);
        logged.extend(buffer.push(second, true));
        logged.extend(buffer.push(third, true));
        logged.extend(buffer.flush());
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].levels.len(), 150);
        assert_eq!(logged[0].from, SourceTime::from_micros(0));
    }
}
