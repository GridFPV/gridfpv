//! The RotorHazard adapter: a stock RotorHazard's messages, translated to Grid's events.
//!
//! **No plugin is installed on the timer.** RotorHazard is used as it ships, over its own
//! socket (see [`wire`]). The plan and its evidence are in
//! `docs/rotorhazard-adapter-plan.md`; the facts this module leans on are repeated here,
//! each marked *seen* (checked against live RotorHazard 4.3.0 and 4.4.0 on 2026-11-10) or
//! *read* (read in RotorHazard's source, `src/server/`, 4.4.0 line numbers).
//!
//! RotorHazard's words stay in this module. Outside it there are only seats, crossings and
//! races.
//!
//! # How Grid uses RotorHazard
//!
//! Grid owns the race. RotorHazard only listens for crossings while its own race is
//! running, so the connection tells it when to listen:
//!
//! - **Start:** [`discard`], then [`stage`] with RotorHazard's built-in "secondary" format
//!   and the start instant Grid chose. *Seen:* the race starts at exactly that instant, and
//!   a lap of 0.6 s is kept, so RotorHazard's own minimum lap filter is off in this mode.
//!   *Read:* that format has no time limit, no win rule and no tones (`server.py:4025-4037`).
//! - **Stop:** [`stop`]. **Save or discard:** [`discard`]. Grid never asks RotorHazard to
//!   save, so RotorHazard's own heats and results are never written. *Seen:* stop gives
//!   status "done", discard gives "ready" with the lap list empty.
//!
//! # What is translated
//!
//! - `current_laps` → [`Event::Pass`] for each crossing not logged before. *Seen:*
//!   RotorHazard sends its whole lap list on every crossing; each lap has `lap_time_stamp`,
//!   milliseconds since the race start on RotorHazard's own clock. A crossing's time is the
//!   race start (`pi_starts_at_s`) plus that.
//! - `frequency_data` → [`Event::SeatSeen`] for each node with a frequency.
//! - A node is a seat, named by its number.
//!
//! *Read:* RotorHazard renumbers laps when one is deleted (`RHRace.py:1133-1160`), so
//! `lap_number` is never used. A crossing is known by its seat and its time, and that is
//! what the translator remembers so a repeated list adds nothing twice.

pub mod wire;

use std::collections::BTreeSet;

use gridfpv_events::{AdapterId, Event, GateIndex, Pass, SeatRef, SourceTime};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Adapter, Capabilities};

/// RotorHazard's race status numbers (`RHRace.py`, `RaceStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RhRaceStatus {
    /// 0: nothing is running and the lap list is empty.
    Ready,
    /// 3: counting down to a start.
    Staging,
    /// 1: racing. Crossings are recorded only in this state.
    Racing,
    /// 2: stopped, with laps still held. It must be discarded before the next start.
    Done,
}

impl RhRaceStatus {
    fn from_number(number: i64) -> Option<Self> {
        match number {
            0 => Some(Self::Ready),
            1 => Some(Self::Racing),
            2 => Some(Self::Done),
            3 => Some(Self::Staging),
            _ => None,
        }
    }
}

/// One message from RotorHazard that Grid reads. Built from a [`wire::Frame::Event`].
#[derive(Debug, Clone, PartialEq)]
pub enum RhMessage {
    /// `race_status`: where RotorHazard's own race is, and when it started.
    RaceStatus {
        status: RhRaceStatus,
        /// The race start on RotorHazard's clock, in seconds. Zero when there is none.
        starts_at_s: f64,
    },
    /// `stage_ready`: a start was accepted, and this is the instant it will start at.
    StageReady { starts_at_s: f64 },
    /// `current_laps`: every node's laps in the race so far, as milliseconds since the
    /// race start. Laps RotorHazard marks deleted are left out.
    CurrentLaps { nodes: Vec<Vec<f64>> },
    /// `frequency_data`: each node's frequency in MHz. Zero means the node is off.
    Frequencies { mhz: Vec<u32> },
    /// `enter_and_exit_at_levels`: each node's levels, in RotorHazard's own counts.
    Levels { enter: Vec<i32>, exit: Vec<i32> },
    /// `heartbeat`: each node's signal level now and whether it is in a crossing. About
    /// ten a second, with no time on them.
    Heartbeat {
        levels: Vec<i32>,
        crossing: Vec<bool>,
    },
}

#[derive(Deserialize)]
struct RawStatus {
    race_status: i64,
    #[serde(default)]
    pi_starts_at_s: f64,
}

#[derive(Deserialize)]
struct RawStageReady {
    pi_starts_at_s: f64,
}

#[derive(Deserialize)]
struct RawLap {
    lap_time_stamp: f64,
    /// 4.4.0 only. *Seen:* 4.3.0 laps have no such field.
    #[serde(default)]
    deleted: Option<bool>,
}

#[derive(Deserialize)]
struct RawNode {
    #[serde(default)]
    laps: Vec<RawLap>,
}

#[derive(Deserialize)]
struct RawCurrent {
    node_index: Vec<RawNode>,
}

#[derive(Deserialize)]
struct RawLaps {
    current: RawCurrent,
}

#[derive(Deserialize)]
struct RawFrequency {
    #[serde(default)]
    frequency: u32,
}

#[derive(Deserialize)]
struct RawFrequencies {
    fdata: Vec<RawFrequency>,
}

#[derive(Deserialize)]
struct RawLevels {
    enter_at_levels: Vec<i32>,
    exit_at_levels: Vec<i32>,
}

#[derive(Deserialize)]
struct RawHeartbeat {
    #[serde(default)]
    current_rssi: Vec<i32>,
    #[serde(default)]
    crossing_flag: Vec<bool>,
}

impl RhMessage {
    /// Read an event by its name and first argument. `None` for an event Grid does not
    /// use, or one whose body is not the shape expected: an odd message is skipped, never
    /// an error.
    pub fn read(name: &str, args: &[Value]) -> Option<Self> {
        let arg = args.first()?.clone();
        match name {
            "race_status" => {
                let raw: RawStatus = serde_json::from_value(arg).ok()?;
                Some(Self::RaceStatus {
                    status: RhRaceStatus::from_number(raw.race_status)?,
                    starts_at_s: raw.pi_starts_at_s,
                })
            }
            "stage_ready" => {
                let raw: RawStageReady = serde_json::from_value(arg).ok()?;
                Some(Self::StageReady {
                    starts_at_s: raw.pi_starts_at_s,
                })
            }
            "current_laps" => {
                let raw: RawLaps = serde_json::from_value(arg).ok()?;
                Some(Self::CurrentLaps {
                    nodes: raw
                        .current
                        .node_index
                        .into_iter()
                        .map(|node| {
                            node.laps
                                .into_iter()
                                .filter(|lap| lap.deleted != Some(true))
                                .map(|lap| lap.lap_time_stamp)
                                .collect()
                        })
                        .collect(),
                })
            }
            "frequency_data" => {
                let raw: RawFrequencies = serde_json::from_value(arg).ok()?;
                Some(Self::Frequencies {
                    mhz: raw.fdata.into_iter().map(|f| f.frequency).collect(),
                })
            }
            "enter_and_exit_at_levels" => {
                let raw: RawLevels = serde_json::from_value(arg).ok()?;
                Some(Self::Levels {
                    enter: raw.enter_at_levels,
                    exit: raw.exit_at_levels,
                })
            }
            "heartbeat" => {
                let raw: RawHeartbeat = serde_json::from_value(arg).ok()?;
                Some(Self::Heartbeat {
                    levels: raw.current_rssi,
                    crossing: raw.crossing_flag,
                })
            }
            _ => None,
        }
    }
}

/// The Grid seat handle for a RotorHazard node. A handle, not a name: a console shows
/// "Seat 1" for node 0.
pub fn seat_ref(node: usize) -> SeatRef {
    SeatRef(node.to_string())
}

/// The node a Grid seat handle names, if it is one of this adapter's.
pub fn node_of(seat: &SeatRef) -> Option<usize> {
    seat.0.parse().ok()
}

/// Clear RotorHazard's lap list and return it to "ready". Sent before every start, and for
/// Grid's save and discard alike. *Read:* a start while RotorHazard is racing is silently
/// ignored (`RHRace.py:245-247`), which is why this always goes first.
pub fn discard() -> String {
    wire::event("discard_laps", None)
}

/// Start RotorHazard's race at `start_at_s` on RotorHazard's own clock, in the built-in
/// format that has no rules of its own.
///
/// *Read:* if the instant is less than 0.9 s away RotorHazard starts 0.9 s after it gets
/// this instead (`RHRace.py:303-304, 328-329`) and drops crossings before then. The
/// connection keeps the instant far enough ahead.
pub fn stage(start_at_s: f64) -> String {
    wire::event(
        "stage_race",
        Some(&json!({
            "secondary_format": true,
            "ignore_secondary_heat": true,
            "start_time_s": start_at_s,
        })),
    )
}

/// Stop RotorHazard's race. *Read:* the handler passes its arguments on as "save too", so
/// none are sent (`server.py:2353-2355`).
pub fn stop() -> String {
    wire::event("stop_race", None)
}

/// Ask for RotorHazard's clock. The answer is a [`wire::Frame::Ack`] holding
/// `{"server_time_s": …}`; read it with [`server_time_s`].
pub fn ask_time(id: u64) -> String {
    wire::event_with_ack(id, "get_server_time", None)
}

/// RotorHazard's clock, in seconds, from the answer to [`ask_time`].
pub fn server_time_s(ack_args: &[Value]) -> Option<f64> {
    ack_args.first()?.get("server_time_s")?.as_f64()
}

/// Ask RotorHazard to send its state again: the race, the laps, the frequencies and the
/// levels. Sent on every connect, so a link that dropped mid-race catches up.
pub fn ask_state() -> String {
    wire::event(
        "load_data",
        Some(&json!({"load_types": [
            "race_status", "current_laps", "frequency_data", "enter_and_exit_at_levels"
        ]})),
    )
}

/// What Grid needs from RotorHazard right now.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Want {
    /// A Grid race that uses this timer is counting down or running: RotorHazard must be
    /// listening, with its race started at this instant on its own clock.
    Listening { start_at_s: f64 },
    /// No Grid race needs this timer.
    Idle,
}

/// The one thing to tell RotorHazard next, to move it toward what Grid wants.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    /// Send [`stage`] with this start.
    Stage { start_at_s: f64 },
    /// Send [`stop`].
    Stop,
    /// Send [`discard`].
    Discard,
}

/// Starts closer together than this are the same start. RotorHazard starts on the instant
/// asked (*seen:* a difference of 0.0), so this only allows for rounding.
const SAME_START_S: f64 = 0.05;

/// Work out the next thing to tell RotorHazard, from where its race is and what Grid wants.
///
/// The rule is simple: RotorHazard's race is only a switch that makes it listen. It should
/// be on, with Grid's start, exactly while a Grid race needs it.
///
/// - `status` and `rh_start_s` are RotorHazard's race now.
/// - `grid_started_it` says whether Grid asked for RotorHazard's current race. Grid never
///   stops or clears a race it did not start while it wants nothing itself: someone may be
///   using RotorHazard's own page.
///
/// One step at a time: after each, RotorHazard reports its new status and this is asked
/// again. `None` means there is nothing to do.
pub fn next_step(
    status: RhRaceStatus,
    rh_start_s: Option<f64>,
    grid_started_it: bool,
    want: Want,
) -> Option<Step> {
    match want {
        Want::Listening { start_at_s } => {
            let same_start =
                rh_start_s.is_some_and(|start| (start - start_at_s).abs() <= SAME_START_S);
            match status {
                RhRaceStatus::Ready => Some(Step::Stage { start_at_s }),
                // Already on with Grid's start (the Director restarted mid-race, perhaps):
                // leave it be.
                RhRaceStatus::Staging | RhRaceStatus::Racing if same_start => None,
                // Just asked, and RotorHazard has not said its start yet.
                RhRaceStatus::Staging if grid_started_it && rh_start_s.is_none() => None,
                // On, but for some other start: it must be stopped and cleared first.
                RhRaceStatus::Staging | RhRaceStatus::Racing => Some(Step::Stop),
                // Stopped (someone pressed stop on RotorHazard's page, or an old race is
                // still held): clear it, and the next step starts it again.
                RhRaceStatus::Done => Some(Step::Discard),
            }
        }
        Want::Idle if grid_started_it => match status {
            RhRaceStatus::Staging | RhRaceStatus::Racing => Some(Step::Stop),
            RhRaceStatus::Done => Some(Step::Discard),
            RhRaceStatus::Ready => None,
        },
        Want::Idle => None,
    }
}

/// The translator for one RotorHazard.
#[derive(Debug, Clone)]
pub struct RotorHazardAdapter {
    id: AdapterId,
    /// The start of RotorHazard's race now, on its clock, in seconds. Lap times are counted
    /// from it. `None` while RotorHazard has no race.
    starts_at_s: Option<f64>,
    /// Every crossing already given out, as (node, microseconds on RotorHazard's clock).
    seen: BTreeSet<(usize, i64)>,
    /// The nodes already reported as seats.
    seats: BTreeSet<usize>,
}

impl RotorHazardAdapter {
    /// A translator for the RotorHazard Grid knows by `id`.
    pub fn new(id: AdapterId) -> Self {
        Self {
            id,
            starts_at_s: None,
            seen: BTreeSet::new(),
            seats: BTreeSet::new(),
        }
    }

    /// Tell the translator about a crossing that is already in the log. After the Director
    /// restarts, this is how it knows not to log RotorHazard's lap list a second time.
    pub fn already_logged(&mut self, seat: &SeatRef, at: SourceTime) {
        if let Some(node) = node_of(seat) {
            self.seen.insert((node, at.micros));
        }
    }

    /// The start of RotorHazard's race, on its clock, if it has one.
    pub fn race_started_at_s(&self) -> Option<f64> {
        self.starts_at_s
    }
}

impl Adapter for RotorHazardAdapter {
    type Raw = RhMessage;

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

    fn translate(&mut self, raw: RhMessage) -> Vec<Event> {
        match raw {
            RhMessage::RaceStatus {
                status,
                starts_at_s,
            } => {
                self.starts_at_s = match status {
                    // *Seen:* the status sent when staging begins still carries the old
                    // start (zero); the real one comes in `stage_ready` and in later
                    // statuses. So staging never overwrites a start with zero.
                    RhRaceStatus::Staging | RhRaceStatus::Racing | RhRaceStatus::Done => {
                        (starts_at_s > 0.0)
                            .then_some(starts_at_s)
                            .or(self.starts_at_s)
                    }
                    RhRaceStatus::Ready => None,
                };
                Vec::new()
            }
            RhMessage::StageReady { starts_at_s } => {
                self.starts_at_s = Some(starts_at_s);
                Vec::new()
            }
            RhMessage::CurrentLaps { nodes } => {
                // Without the race start a lap has no time. The list is sent again on the
                // next crossing, and on every connect, so nothing is lost by waiting.
                let Some(starts_at_s) = self.starts_at_s else {
                    return Vec::new();
                };
                let mut passes: Vec<Pass> = Vec::new();
                for (node, laps) in nodes.into_iter().enumerate() {
                    for stamp_ms in laps {
                        let at = ((starts_at_s + stamp_ms / 1000.0) * 1_000_000.0).round() as i64;
                        if self.seen.insert((node, at)) {
                            passes.push(Pass {
                                adapter: self.id.clone(),
                                seat: seat_ref(node),
                                at: SourceTime::from_micros(at),
                                // RotorHazard renumbers laps, so there is no number to give.
                                sequence: None,
                                gate: GateIndex::LAP,
                                signal: None,
                                race: None,
                            });
                        }
                    }
                }
                // One list can hold several new crossings: log them in the order they
                // happened.
                passes.sort_by_key(|pass| pass.at);
                passes.into_iter().map(Event::Pass).collect()
            }
            RhMessage::Frequencies { mhz } => mhz
                .iter()
                .enumerate()
                .filter(|(node, mhz)| **mhz > 0 && self.seats.insert(*node))
                .map(|(node, _)| Event::SeatSeen {
                    adapter: self.id.clone(),
                    seat: seat_ref(node),
                })
                .collect(),
            RhMessage::Levels { .. } | RhMessage::Heartbeat { .. } => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter() -> RotorHazardAdapter {
        RotorHazardAdapter::new(AdapterId("rh".into()))
    }

    fn read(frame: &str) -> RhMessage {
        match wire::parse(frame) {
            wire::Frame::Event { name, args } => RhMessage::read(&name, &args).unwrap(),
            other => panic!("not an event: {other:?}"),
        }
    }

    // Frames captured from live RotorHazard on 2026-11-10, trimmed to the fields read.
    const STATUS_STAGING: &str = r#"42["race_status",{"race_status":3,"race_format_id":null,"pi_starts_at_s":0,"pi_staging_at_s":0}]"#;
    const STAGE_READY: &str = r#"42["stage_ready",{"hide_stage_timer":true,"pi_staging_at_s":1904545.4,"pi_starts_at_s":1904547.919090051,"race_time_sec":0}]"#;
    const STATUS_RACING: &str = r#"42["race_status",{"race_status":1,"race_format_id":null,"pi_starts_at_s":1904547.919090051}]"#;
    const STATUS_READY: &str =
        r#"42["race_status",{"race_status":0,"race_format_id":1,"pi_starts_at_s":0}]"#;
    /// 4.4.0: laps carry `deleted` and `source`.
    const LAPS_44: &str = r#"42["current_laps",{"current":{"node_index":[
        {"laps":[{"lap_index":0,"lap_number":0,"lap_time_stamp":1722.5506061222404,"deleted":false,"source":0,"lap_time":0},
                 {"lap_index":1,"lap_number":1,"lap_time_stamp":2328.547843033448,"deleted":false,"source":0,"lap_time":605.9}],
         "fastest_lap_index":null,"pilot":null,"finished_flag":null},
        {"laps":[{"lap_index":0,"lap_number":0,"lap_time_stamp":2632.7828629873693,"deleted":false,"source":0,"lap_time":0}],
         "fastest_lap_index":null,"pilot":null,"finished_flag":null}]}}]"#;
    /// 4.3.0: laps carry `lap_raw` and no `deleted`.
    const LAPS_43: &str = r#"42["current_laps",{"current":{"node_index":[
        {"laps":[{"lap_index":0,"lap_number":0,"lap_raw":1717.2,"lap_time":0,"lap_time_stamp":1717.205597087741,"late_lap":false,"splits":[]}],
         "fastest_lap_index":null,"pilot":null,"finished_flag":null}]}}]"#;

    fn passes(events: Vec<Event>) -> Vec<(String, i64)> {
        events
            .into_iter()
            .map(|event| match event {
                Event::Pass(pass) => (pass.seat.0, pass.at.micros),
                other => panic!("not a pass: {other:?}"),
            })
            .collect()
    }

    fn started(adapter: &mut RotorHazardAdapter) {
        assert!(adapter.translate(read(STATUS_STAGING)).is_empty());
        assert!(adapter.translate(read(STAGE_READY)).is_empty());
        assert!(adapter.translate(read(STATUS_RACING)).is_empty());
    }

    #[test]
    fn laps_become_crossings_at_the_race_start_plus_their_stamp() {
        let mut adapter = adapter();
        started(&mut adapter);
        assert_eq!(adapter.race_started_at_s(), Some(1904547.919090051));
        let got = passes(adapter.translate(read(LAPS_44)));
        assert_eq!(
            got,
            vec![
                // 1904547.919090051 s + 1722.5506 ms, in microseconds, and so on.
                ("0".to_owned(), 1_904_549_641_641),
                ("0".to_owned(), 1_904_550_247_638),
                ("1".to_owned(), 1_904_550_551_873),
            ]
        );
    }

    #[test]
    fn the_whole_list_arriving_again_adds_nothing_twice() {
        let mut adapter = adapter();
        started(&mut adapter);
        assert_eq!(adapter.translate(read(LAPS_44)).len(), 3);
        assert!(adapter.translate(read(LAPS_44)).is_empty());
        // A status sent again mid-race does not move the start, so times stay the same.
        adapter.translate(read(STATUS_RACING));
        assert!(adapter.translate(read(LAPS_44)).is_empty());
    }

    #[test]
    fn crossings_already_in_the_log_are_not_logged_again_after_a_restart() {
        let mut adapter = adapter();
        adapter.already_logged(
            &SeatRef("0".into()),
            SourceTime::from_micros(1_904_549_641_641),
        );
        // The Director restarted mid-race: RotorHazard says it is racing, and since when.
        adapter.translate(read(STATUS_RACING));
        let got = passes(adapter.translate(read(LAPS_44)));
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].1, 1_904_550_247_638);
    }

    #[test]
    fn laps_with_no_known_race_start_wait_for_it() {
        let mut adapter = adapter();
        assert!(adapter.translate(read(LAPS_44)).is_empty());
        adapter.translate(read(STATUS_RACING));
        assert_eq!(adapter.translate(read(LAPS_44)).len(), 3);
    }

    #[test]
    fn a_lap_rotorhazard_marked_deleted_is_left_out_and_4_3_0_laps_read_the_same() {
        let mut adapter = adapter();
        started(&mut adapter);
        let deleted = LAPS_44.replacen(r#""deleted":false"#, r#""deleted":true"#, 1);
        assert_eq!(adapter.translate(read(&deleted)).len(), 2);

        let mut older = RotorHazardAdapter::new(AdapterId("rh".into()));
        started(&mut older);
        assert_eq!(passes(older.translate(read(LAPS_43))).len(), 1);
    }

    #[test]
    fn a_new_race_starts_its_times_from_its_own_start() {
        let mut adapter = adapter();
        started(&mut adapter);
        adapter.translate(read(LAPS_44));
        // Discarded: RotorHazard is ready and has no race.
        adapter.translate(read(STATUS_READY));
        assert_eq!(adapter.race_started_at_s(), None);
        assert!(adapter.translate(read(LAPS_44)).is_empty());
        // The next race, ten seconds later, with the same stamps: all new crossings.
        adapter.translate(read(
            &STAGE_READY.replace("1904547.919090051", "1904557.919090051"),
        ));
        let got = passes(adapter.translate(read(LAPS_44)));
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].1, 1_904_559_641_641);
    }

    #[test]
    fn nodes_with_a_frequency_are_seats_each_reported_once() {
        let mut adapter = adapter();
        let frame = r#"42["frequency_data",{"fdata":[{"band":"R","channel":1,"frequency":5658},{"band":null,"channel":null,"frequency":0},{"band":"F","channel":2,"frequency":5760}]}]"#;
        assert_eq!(
            adapter.translate(read(frame)),
            vec![
                Event::SeatSeen {
                    adapter: AdapterId("rh".into()),
                    seat: SeatRef("0".into())
                },
                Event::SeatSeen {
                    adapter: AdapterId("rh".into()),
                    seat: SeatRef("2".into())
                },
            ]
        );
        assert!(adapter.translate(read(frame)).is_empty());
    }

    #[test]
    fn levels_and_the_heartbeat_are_read_and_events_grid_does_not_use_are_skipped() {
        assert_eq!(
            read(
                r#"42["enter_and_exit_at_levels",{"enter_at_levels":[90,91],"exit_at_levels":[80,81]}]"#
            ),
            RhMessage::Levels {
                enter: vec![90, 91],
                exit: vec![80, 81]
            }
        );
        assert_eq!(
            read(
                r#"42["heartbeat",{"current_rssi":[57,0],"frequency":[5658,0],"loop_time":[10,10],"crossing_flag":[true,false]}]"#
            ),
            RhMessage::Heartbeat {
                levels: vec![57, 0],
                crossing: vec![true, false]
            }
        );
        assert_eq!(
            RhMessage::read("cluster_status", &[json!({"secondaries": []})]),
            None
        );
        assert_eq!(
            RhMessage::read("race_status", &[json!({"race_status": 9})]),
            None
        );
        assert_eq!(RhMessage::read("race_status", &[]), None);
    }

    #[test]
    fn the_commands_are_the_ones_checked_against_live_rotorhazard() {
        assert_eq!(discard(), r#"42["discard_laps"]"#);
        assert_eq!(stop(), r#"42["stop_race"]"#);
        assert_eq!(
            stage(1904547.5),
            r#"42["stage_race",{"ignore_secondary_heat":true,"secondary_format":true,"start_time_s":1904547.5}]"#
        );
        assert_eq!(ask_time(4), r#"424["get_server_time"]"#);
        assert_eq!(
            server_time_s(&[json!({"server_time_s": 1904518.493188328})]),
            Some(1904518.493188328)
        );
        assert_eq!(server_time_s(&[]), None);
    }

    const T: f64 = 1_904_547.5;
    const LISTEN: Want = Want::Listening { start_at_s: T };

    #[test]
    fn a_grid_race_turns_rotorhazard_on_at_grids_start() {
        assert_eq!(
            next_step(RhRaceStatus::Ready, None, false, LISTEN),
            Some(Step::Stage { start_at_s: T })
        );
        // Asked, counting down, start not reported yet: wait.
        assert_eq!(next_step(RhRaceStatus::Staging, None, true, LISTEN), None);
        // On with Grid's start: nothing to do, whoever asked (a Director restart forgets).
        assert_eq!(
            next_step(RhRaceStatus::Staging, Some(T), true, LISTEN),
            None
        );
        assert_eq!(
            next_step(RhRaceStatus::Racing, Some(T), false, LISTEN),
            None
        );
    }

    #[test]
    fn rotorhazard_in_the_wrong_state_is_walked_back_to_a_fresh_start() {
        // An old race is still held: clear it, then start.
        assert_eq!(
            next_step(RhRaceStatus::Done, Some(T - 300.0), false, LISTEN),
            Some(Step::Discard)
        );
        // Racing for some other start (someone used RotorHazard's own page): stop, clear,
        // start. One step each time it reports back.
        assert_eq!(
            next_step(RhRaceStatus::Racing, Some(T - 300.0), false, LISTEN),
            Some(Step::Stop)
        );
        // Someone pressed stop on RotorHazard's page in the middle of Grid's race.
        assert_eq!(
            next_step(RhRaceStatus::Done, Some(T), true, LISTEN),
            Some(Step::Discard)
        );
        assert_eq!(
            next_step(RhRaceStatus::Ready, None, true, LISTEN),
            Some(Step::Stage { start_at_s: T })
        );
    }

    #[test]
    fn when_grids_race_is_over_rotorhazard_is_stopped_and_cleared() {
        assert_eq!(
            next_step(RhRaceStatus::Racing, Some(T), true, Want::Idle),
            Some(Step::Stop)
        );
        assert_eq!(
            next_step(RhRaceStatus::Done, Some(T), true, Want::Idle),
            Some(Step::Discard)
        );
        assert_eq!(next_step(RhRaceStatus::Ready, None, true, Want::Idle), None);
    }

    #[test]
    fn a_race_grid_did_not_start_is_left_alone_while_grid_wants_nothing() {
        for status in [
            RhRaceStatus::Ready,
            RhRaceStatus::Staging,
            RhRaceStatus::Racing,
            RhRaceStatus::Done,
        ] {
            assert_eq!(next_step(status, Some(T), false, Want::Idle), None);
        }
    }
}
