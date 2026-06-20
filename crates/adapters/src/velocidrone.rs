//! Velocidrone adapter (#22).
//!
//! Built first, on purpose — the simulator has no RSSI, no thresholds, no
//! frequencies, and a clock that isn't ours, so making the canonical model fit it
//! cleanly keeps the abstraction honest. Velocidrone exposes a WebSocket feed
//! carrying gate passes, lap splits, lap times and totals from the game engine;
//! each gate crossing becomes a [`Pass`] (with split/gate index), the sim player name
//! is reported via [`Event::CompetitorSeen`], and the game's lap times/totals are
//! advisory cross-checks (the engine derives laps from the pass stream).
//!
//! Capabilities: live passes ✓, splits ✓, source lifecycle ✓; signal ✗,
//! calibration ✗, frequency ✗. Ref: `docs/timer-adapters.html` §7.
//!
//! # The wire format (researched, not yet validated against a live capture)
//!
//! Velocidrone serves a raw (non-Socket.IO) WebSocket at `ws://<host>:60003/velocidrone`
//! once *Websocket Communication* is enabled (Options → Main Settings) and the player
//! is race manager. Each frame is one JSON object, **externally keyed** by a single
//! discriminator field. The shapes below are reconstructed from two independent
//! open-source consumers (see module SOURCES); the design doc (§9, "Velocidrone
//! WebSocket message schema") flags the exact shapes as *to be captured against a live
//! build*, so the fixture and these structs are marked accordingly and tracked by #25.
//!
//! - `racestatus` — `{ "racestatus": { "raceAction": "start" | "abort" | "race finished" } }`.
//!   The race lifecycle: `start` → [`Event::SessionStarted`]; `abort` / `race finished`
//!   → [`Event::SessionEnded`].
//! - `racedata` — `{ "racedata": { "<PlayerName>": { uid, name?, lap, time, gate,
//!   finished } } }`. A per-crossing telemetry snapshot keyed by sim player name. `time`
//!   is the **game-engine timestamp in floating-point seconds**, cumulative from the
//!   race start; `lap` is the integer lap counter; `gate` is the gate ordinal (1 =
//!   start/finish, >1 = a split on the lap); `finished` is the string `"true"`/`"false"`.
//!   Each new `(lap, gate)` for a player is one gate crossing → one [`Pass`].
//! - `FinishGate` — `{ "FinishGate": { "StartFinishGate": "true" | "false" } }`. Track
//!   config: whether the track has a dedicated start/finish gate. Advisory; retained in
//!   [`Raw`] for completeness but emits nothing.
//! - `pilotlist` — `{ "pilotlist": [ { uid, name } ] }`. A roster reply (to a
//!   `getpilots` command). Each entry → [`Event::CompetitorSeen`].
//!
//! ## SOURCES
//!
//! - `rh-velocidrone` RotorHazard plugin (vikibaarathi) — the message handler that
//!   switches on `FinishGate` / `racestatus` / `racedata` / `ActivateError` /
//!   `pilotlist`, reads `pilot_data["uid"|"lap"|"time"|"gate"|"finished"]`, parses
//!   `time` as a float and `gate`/`lap` as ints, and treats `finished` as a
//!   `"true"`/`"false"` string: <https://github.com/vikibaarathi/rh-velocidrone>
//!   (`custom_plugins/velocidrone_controls/velocidrone_controller.py`).
//! - `VelocidroneWebSocketConsumer` (eedok) — an independent JS consumer that reads the
//!   same `racestatus.raceAction` and `racedata.<name>.{time,lap,finished}` fields and
//!   computes lap times as successive `time` differences (confirming `time` is
//!   cumulative seconds): <https://github.com/eedok/VelocidroneWebSocketConsumer>
//!   (`main.js`, `socketTest.js`).
//!
//! Confidence: **high** on field names, the `racestatus`/`racedata`/`pilotlist`
//! discriminators, and `time` being cumulative seconds (two independent consumers
//! agree). **Lower** on: the exact `racestatus.raceAction` string set (only `start`,
//! `abort`, `race finished` are observed in the wild code); whether a session id is ever
//! carried (none seen — we synthesize one); and the precise `gate` numbering for splits
//! (consumers only special-case gate `1`). These are the items #25 must confirm.
//!
//! # Assumptions flagged for validation against a real capture (#25)
//!
//! - **Time unit = seconds.** `time` is multiplied by 1_000_000 to reach the
//!   microsecond [`SourceTime`]. If a future build reports milliseconds this constant is
//!   the single thing to change ([`SECONDS_TO_MICROS`]).
//! - **Gate numbering.** Velocidrone gate `1` is taken as the lap (start/finish) gate
//!   → [`GateIndex::LAP`]; gate `n > 1` maps to split [`GateIndex`]`(n - 1)`. If the
//!   feed is 0-based, or uses a different convention for the start/finish gate, adjust
//!   [`map_gate`].
//! - **No source sequence counter.** The feed exposes none, so emitted passes carry
//!   `sequence: None` and dedup falls back to `(adapter, competitor, at)` — fine here
//!   because `time` is unique per `(player, crossing)`.
//! - **Session lifecycle / id.** Velocidrone does not send a race id, so
//!   [`SessionStarted`](Event::SessionStarted)/[`SessionEnded`](Event::SessionEnded)
//!   carry a synthesized, monotonically increasing `race-<n>` id.
//!
//! # Deferred: the live WebSocket transport
//!
//! This module is a **pure translator** only (the #22 scope): it converts
//! already-decoded [`Raw`] messages into canonical events. Establishing the
//! `ws://…:60003/velocidrone` connection, the empty-frame keep-alive ping, reconnect,
//! and feeding decoded frames into [`VelocidroneAdapter::translate`] is left to the
//! live-transport task (no network crates are pulled in here).

#[cfg(feature = "live")]
pub mod transport;

use serde::Deserialize;
use std::collections::HashMap;

use gridfpv_events::{AdapterId, CompetitorRef, Event, GateIndex, Pass, SessionId, SourceTime};

use crate::dedup::Deduplicator;
use crate::{Adapter, Capabilities};

/// Multiplier from Velocidrone's floating-point **seconds** game-engine time to the
/// microsecond [`SourceTime`] the canonical model uses. Flagged for validation (#25):
/// if a build is found to report milliseconds, this is the one constant to change.
const SECONDS_TO_MICROS: f64 = 1_000_000.0;

/// One decoded Velocidrone WebSocket frame.
///
/// Velocidrone frames are JSON objects with a single discriminator key, mapped here to
/// serde's externally-tagged enum representation: `{ "racestatus": { … } }`,
/// `{ "racedata": { … } }`, etc. Unknown frame kinds are surfaced as [`Raw::Other`]
/// (with their payload ignored) rather than failing to deserialize, so an unexpected
/// message type on the wire never aborts a session.
///
/// The explicit [`Deserialize`] impl exists for exactly that catch-all: serde's derived
/// `#[serde(other)]` only supports a *unit* fallback for externally-tagged enums, but
/// Velocidrone's unknown frames (e.g. `ActivateError`) carry an object payload, so we
/// read the single discriminator key by hand and route the value. See the
/// [module docs](self) for the full format and SOURCES.
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    /// `{ "racestatus": { "raceAction": "…" } }` — the race lifecycle.
    RaceStatus(RaceStatus),
    /// `{ "racedata": { "<player>": { … } } }` — a per-crossing telemetry snapshot,
    /// keyed by sim player name.
    RaceData(HashMap<String, PilotData>),
    /// `{ "pilotlist": [ { uid, name } ] }` — a roster reply.
    PilotList(Vec<PilotEntry>),
    /// `{ "FinishGate": { "StartFinishGate": "…" } }` — track config (advisory).
    FinishGate(FinishGate),
    /// Any other frame kind (e.g. `ActivateError`) — payload ignored, emits nothing.
    Other,
}

impl<'de> Deserialize<'de> for Raw {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        // A Velocidrone frame is a one-key object; read it as a generic map and switch
        // on the single discriminator key. Unknown keys route to `Other` with the value
        // ignored, so a new message type never breaks the stream.
        let map: serde_json::Map<String, serde_json::Value> =
            serde_json::Map::deserialize(deserializer)?;
        let Some((key, value)) = map.into_iter().next() else {
            return Err(D::Error::custom("velocidrone frame is an empty object"));
        };
        Ok(match key.as_str() {
            "racestatus" => {
                Raw::RaceStatus(serde_json::from_value(value).map_err(D::Error::custom)?)
            }
            "racedata" => Raw::RaceData(serde_json::from_value(value).map_err(D::Error::custom)?),
            "pilotlist" => Raw::PilotList(serde_json::from_value(value).map_err(D::Error::custom)?),
            "FinishGate" => {
                Raw::FinishGate(serde_json::from_value(value).map_err(D::Error::custom)?)
            }
            _ => Raw::Other,
        })
    }
}

/// The `racestatus` payload: a single race-lifecycle action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RaceStatus {
    /// `"start"`, `"abort"` or `"race finished"` (the actions seen in the wild). Other
    /// values are treated as "no lifecycle change".
    #[serde(rename = "raceAction")]
    pub race_action: String,
}

/// Per-player telemetry inside a `racedata` frame — one gate crossing's worth of state.
///
/// `time` is the **cumulative game-engine timestamp in seconds**; `gate` is the gate
/// ordinal (1 = start/finish); `lap` is the lap counter; `finished` is the string
/// `"true"`/`"false"`. `uid` and `name` identify the player (the map key is also the
/// name). See the [module docs](self).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PilotData {
    /// Velocidrone account UID for the player.
    pub uid: String,
    /// Player display name (usually equal to the map key); optional in the feed.
    #[serde(default)]
    pub name: Option<String>,
    /// Lap counter for this crossing.
    pub lap: u32,
    /// Cumulative game-engine time of the crossing, in **seconds** (see
    /// [`SECONDS_TO_MICROS`]).
    pub time: f64,
    /// Gate ordinal crossed (1 = start/finish lap gate; >1 = a split).
    pub gate: u32,
    /// Whether the player has finished the race. String `"true"`/`"false"` on the wire.
    #[serde(default)]
    pub finished: Option<String>,
}

/// A `pilotlist` entry — a known player in the roster reply.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PilotEntry {
    /// Velocidrone account UID.
    pub uid: String,
    /// Player display name.
    pub name: String,
}

/// The `FinishGate` payload — whether the track has a dedicated start/finish gate.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FinishGate {
    /// String `"true"`/`"false"` on the wire.
    #[serde(rename = "StartFinishGate")]
    pub start_finish_gate: String,
}

/// Map a Velocidrone gate ordinal to a canonical [`GateIndex`].
///
/// Velocidrone numbers the start/finish (lap) gate as `1`; the canonical model numbers
/// the lap gate `0` ([`GateIndex::LAP`]) and splits from `1` up. So gate `1` → lap gate,
/// and gate `n > 1` → split `n - 1`. Gate `0`, if it ever appears, is treated as the lap
/// gate too. **Flagged for #25**: confirm the live feed's gate numbering.
fn map_gate(velocidrone_gate: u32) -> GateIndex {
    match velocidrone_gate {
        0 | 1 => GateIndex::LAP,
        n => GateIndex(n - 1),
    }
}

/// Convert a Velocidrone seconds timestamp to a microsecond [`SourceTime`].
fn to_source_time(seconds: f64) -> SourceTime {
    SourceTime::from_micros((seconds * SECONDS_TO_MICROS).round() as i64)
}

/// The crossing identity an adapter tracks per player to recognise a *new* gate
/// crossing: a `racedata` frame restates a player's whole state, so only a change in
/// `(lap, gate, finished)` marks a fresh crossing to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossingMark {
    lap: u32,
    gate: u32,
    finished: bool,
}

/// Velocidrone adapter — a **pure translator** from decoded [`Raw`] frames to canonical
/// events (the #22 scope). It owns the per-source translation state: a [`Deduplicator`]
/// (so a reconnect that replays an overlapping tail injects no duplicate passes), the
/// per-player last-seen crossing mark, the set of players already announced via
/// [`Event::CompetitorSeen`], and the synthesized session id.
///
/// Live transport (connecting the socket, keep-alive ping, reconnect) is **deferred**;
/// see the [module docs](self).
#[derive(Debug)]
pub struct VelocidroneAdapter {
    id: AdapterId,
    dedup: Deduplicator,
    /// Per-player last crossing seen, to suppress restated (unchanged) telemetry.
    last_crossing: HashMap<String, CrossingMark>,
    /// Players already announced via `CompetitorSeen`, so each is announced once.
    seen_players: HashMap<String, ()>,
    /// Whether a session is currently open (a `start` was seen, no end since).
    session_open: bool,
    /// Monotonic counter for synthesizing session ids (Velocidrone sends none).
    race_counter: u64,
}

impl VelocidroneAdapter {
    /// A fresh adapter stamping every event with `id`.
    pub fn new(id: AdapterId) -> Self {
        Self {
            id,
            dedup: Deduplicator::new(),
            last_crossing: HashMap::new(),
            seen_players: HashMap::new(),
            session_open: false,
            race_counter: 0,
        }
    }

    /// Construct with the conventional `"velocidrone"` adapter id.
    pub fn with_default_id() -> Self {
        Self::new(AdapterId("velocidrone".to_string()))
    }

    /// The session id for the currently-open race (synthesized; Velocidrone sends none).
    fn current_session(&self) -> SessionId {
        SessionId(format!("race-{}", self.race_counter))
    }

    /// Announce a player via [`Event::CompetitorSeen`] the first time it is seen, into
    /// `out`. No-op on subsequent sightings.
    fn announce_player(&mut self, player: &str, out: &mut Vec<Event>) {
        if self.seen_players.insert(player.to_string(), ()).is_none() {
            out.push(Event::CompetitorSeen {
                adapter: self.id.clone(),
                competitor: CompetitorRef(player.to_string()),
            });
        }
    }

    /// Translate a `racestatus` lifecycle action.
    fn translate_race_status(&mut self, status: &RaceStatus, out: &mut Vec<Event>) {
        match status.race_action.to_ascii_lowercase().as_str() {
            "start" => {
                // A new race: close any stale open session first, then open a fresh one
                // and reset per-race crossing state so lap/gate counters start clean.
                if self.session_open {
                    out.push(Event::SessionEnded {
                        adapter: self.id.clone(),
                        session: self.current_session(),
                    });
                }
                self.race_counter += 1;
                self.session_open = true;
                self.last_crossing.clear();
                out.push(Event::SessionStarted {
                    adapter: self.id.clone(),
                    session: self.current_session(),
                });
            }
            // Both "abort" and "race finished" end the session; the engine derives
            // results from the pass stream, so we only mark the lifecycle boundary.
            "abort" | "race finished" if self.session_open => {
                self.session_open = false;
                out.push(Event::SessionEnded {
                    adapter: self.id.clone(),
                    session: self.current_session(),
                });
                self.last_crossing.clear();
            }
            // Unknown action: no lifecycle change.
            _ => {}
        }
    }

    /// Translate one player's `racedata` telemetry into a [`Pass`] when it represents a
    /// genuinely new crossing (a change in lap/gate/finished vs. the last seen).
    fn translate_pilot(&mut self, player: &str, data: &PilotData, out: &mut Vec<Event>) {
        self.announce_player(player, out);

        let finished =
            matches!(data.finished.as_deref(), Some(s) if s.eq_ignore_ascii_case("true"));
        let mark = CrossingMark {
            lap: data.lap,
            gate: data.gate,
            finished,
        };

        // A `racedata` frame restates the player's whole state on every event; only a
        // changed (lap, gate, finished) is a fresh crossing. Identical restatements are
        // dropped here, and any that slip through are caught by the dedup window.
        if self.last_crossing.get(player) == Some(&mark) {
            return;
        }
        self.last_crossing.insert(player.to_string(), mark);

        out.push(Event::Pass(Pass {
            adapter: self.id.clone(),
            competitor: CompetitorRef(player.to_string()),
            at: to_source_time(data.time),
            // Velocidrone exposes no monotonic sequence counter; dedup keys on
            // (adapter, competitor, at), and `time` is unique per crossing.
            sequence: None,
            gate: map_gate(data.gate),
            signal: None,
        }));
    }
}

impl Adapter for VelocidroneAdapter {
    type Raw = Raw;

    fn id(&self) -> &AdapterId {
        &self.id
    }

    fn capabilities(&self) -> Capabilities {
        // Sim: live passes, splits, and its own race lifecycle; no signal/calibration/
        // frequency. Mirrors the capability matrix in `docs/timer-adapters.html` §5/§7.
        Capabilities::none()
            .with_live_passes()
            .with_gates_splits()
            .with_source_lifecycle()
    }

    fn translate(&mut self, raw: Self::Raw) -> Vec<Event> {
        let mut out = Vec::new();
        match raw {
            Raw::RaceStatus(status) => self.translate_race_status(&status, &mut out),
            Raw::RaceData(players) => {
                // Order by player name so a frame carrying several players translates
                // deterministically (HashMap iteration order is otherwise unspecified).
                let mut names: Vec<&String> = players.keys().collect();
                names.sort();
                for name in names {
                    let data = &players[name];
                    self.translate_pilot(name, data, &mut out);
                }
            }
            Raw::PilotList(entries) => {
                for entry in &entries {
                    self.announce_player(&entry.name, &mut out);
                }
            }
            // Track config / unknown frames carry no canonical event.
            Raw::FinishGate(_) | Raw::Other => {}
        }
        // Run every batch (including a reconnect's replayed tail) through the dedup
        // window so a resume can't double-count a pass. Non-Pass events pass through.
        self.dedup.retain_new(&mut out);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Capability;

    /// Parse a single Velocidrone frame from JSON into a [`Raw`].
    fn raw(json: &str) -> Raw {
        serde_json::from_str(json).expect("fixture frame parses")
    }

    fn adapter() -> VelocidroneAdapter {
        VelocidroneAdapter::with_default_id()
    }

    #[test]
    fn capabilities_match_the_sim_profile() {
        let caps = adapter().capabilities();
        assert!(caps.has(Capability::LivePasses));
        assert!(caps.has(Capability::GatesSplits));
        assert!(caps.has(Capability::SourceLifecycle));
        assert!(!caps.has(Capability::SignalContext));
        assert!(!caps.has(Capability::Calibration));
        assert!(!caps.has(Capability::FrequencyMgmt));
    }

    #[test]
    fn raw_frames_deserialize_to_the_right_variant() {
        assert!(matches!(
            raw(r#"{ "racestatus": { "raceAction": "start" } }"#),
            Raw::RaceStatus(_)
        ));
        assert!(matches!(
            raw(
                r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 1, "time": 1.5, "gate": 1, "finished": "false" } } }"#
            ),
            Raw::RaceData(_)
        ));
        assert!(matches!(
            raw(r#"{ "pilotlist": [ { "uid": "u1", "name": "Ace" } ] }"#),
            Raw::PilotList(_)
        ));
        assert!(matches!(
            raw(r#"{ "FinishGate": { "StartFinishGate": "true" } }"#),
            Raw::FinishGate(_)
        ));
        // An unknown frame kind degrades to `Other`, never a parse failure.
        assert_eq!(
            raw(r#"{ "ActivateError": { "UIDNotFound": "u9" } }"#),
            Raw::Other
        );
    }

    #[test]
    fn gate_mapping_lap_and_splits() {
        assert_eq!(map_gate(1), GateIndex::LAP);
        assert_eq!(map_gate(0), GateIndex::LAP);
        assert_eq!(map_gate(2), GateIndex(1));
        assert_eq!(map_gate(3), GateIndex(2));
    }

    #[test]
    fn seconds_convert_to_microseconds() {
        assert_eq!(to_source_time(1.5), SourceTime::from_micros(1_500_000));
        assert_eq!(to_source_time(0.0), SourceTime::from_micros(0));
        assert_eq!(
            to_source_time(12.345678),
            SourceTime::from_micros(12_345_678)
        );
    }

    #[test]
    fn race_start_opens_a_session() {
        let mut a = adapter();
        let events = a.translate(raw(r#"{ "racestatus": { "raceAction": "start" } }"#));
        assert_eq!(
            events,
            vec![Event::SessionStarted {
                adapter: AdapterId("velocidrone".into()),
                session: SessionId("race-1".into()),
            }]
        );
    }

    #[test]
    fn race_finished_ends_the_open_session() {
        let mut a = adapter();
        a.translate(raw(r#"{ "racestatus": { "raceAction": "start" } }"#));
        let events = a.translate(raw(
            r#"{ "racestatus": { "raceAction": "race finished" } }"#,
        ));
        assert_eq!(
            events,
            vec![Event::SessionEnded {
                adapter: AdapterId("velocidrone".into()),
                session: SessionId("race-1".into()),
            }]
        );
    }

    #[test]
    fn abort_ends_the_session_too() {
        let mut a = adapter();
        a.translate(raw(r#"{ "racestatus": { "raceAction": "start" } }"#));
        let events = a.translate(raw(r#"{ "racestatus": { "raceAction": "abort" } }"#));
        assert!(matches!(events.as_slice(), [Event::SessionEnded { .. }]));
    }

    #[test]
    fn a_crossing_announces_the_player_then_emits_a_pass() {
        let mut a = adapter();
        let events = a.translate(raw(
            r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 1, "time": 2.0, "gate": 1, "finished": "false" } } }"#,
        ));
        assert_eq!(
            events,
            vec![
                Event::CompetitorSeen {
                    adapter: AdapterId("velocidrone".into()),
                    competitor: CompetitorRef("Ace".into()),
                },
                Event::Pass(Pass {
                    adapter: AdapterId("velocidrone".into()),
                    competitor: CompetitorRef("Ace".into()),
                    at: SourceTime::from_micros(2_000_000),
                    sequence: None,
                    gate: GateIndex::LAP,
                    signal: None,
                }),
            ]
        );
    }

    #[test]
    fn player_is_announced_only_once() {
        let mut a = adapter();
        a.translate(raw(
            r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 1, "time": 1.0, "gate": 1, "finished": "false" } } }"#,
        ));
        let events = a.translate(raw(
            r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 2, "time": 5.0, "gate": 1, "finished": "false" } } }"#,
        ));
        // Second crossing: no CompetitorSeen, just the new Pass.
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Pass(_)));
    }

    #[test]
    fn a_split_gate_carries_a_split_index() {
        let mut a = adapter();
        let events = a.translate(raw(
            r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 1, "time": 1.2, "gate": 2, "finished": "false" } } }"#,
        ));
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .expect("a pass");
        assert_eq!(pass.gate, GateIndex(1));
        assert!(!pass.gate.is_lap_gate());
    }

    #[test]
    fn restated_unchanged_telemetry_emits_no_duplicate_pass() {
        let mut a = adapter();
        let frame = r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 1, "time": 1.0, "gate": 1, "finished": "false" } } }"#;
        let first = a.translate(raw(frame));
        assert_eq!(
            first.iter().filter(|e| matches!(e, Event::Pass(_))).count(),
            1
        );
        // The exact same frame restated (Velocidrone re-sends whole state) yields no
        // new Pass — neither the crossing-mark guard nor the dedup window let it through.
        let second = a.translate(raw(frame));
        assert!(second.is_empty());
    }

    #[test]
    fn finished_transition_emits_the_final_pass() {
        let mut a = adapter();
        a.translate(raw(
            r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 3, "time": 30.0, "gate": 1, "finished": "false" } } }"#,
        ));
        // Same lap/gate but now finished -> a distinct crossing mark -> a final Pass.
        let events = a.translate(raw(
            r#"{ "racedata": { "Ace": { "uid": "u1", "lap": 3, "time": 31.5, "gate": 1, "finished": "true" } } }"#,
        ));
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .expect("a finishing pass");
        assert_eq!(pass.at, SourceTime::from_micros(31_500_000));
    }

    #[test]
    fn pilotlist_announces_each_player() {
        let mut a = adapter();
        let events = a.translate(raw(
            r#"{ "pilotlist": [ { "uid": "u1", "name": "Ace" }, { "uid": "u2", "name": "Bee" } ] }"#,
        ));
        let seen: Vec<&str> = events
            .iter()
            .filter_map(|e| match e {
                Event::CompetitorSeen { competitor, .. } => Some(competitor.0.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(seen, vec!["Ace", "Bee"]);
    }

    #[test]
    fn finish_gate_and_unknown_frames_emit_nothing() {
        let mut a = adapter();
        assert!(
            a.translate(raw(r#"{ "FinishGate": { "StartFinishGate": "true" } }"#))
                .is_empty()
        );
        assert!(
            a.translate(raw(r#"{ "ActivateError": { "UIDNotFound": "u9" } }"#))
                .is_empty()
        );
    }

    /// Drive the RECORDED session fixture (synthesized from the documented format —
    /// validate against a real capture, #25) end to end and assert the exact canonical
    /// event stream: lifecycle, CompetitorSeen, and every Pass with the right
    /// competitor / timestamp / gate.
    #[test]
    fn recorded_session_translates_to_the_expected_event_stream() {
        let fixture = include_str!("velocidrone/fixtures/sprint.frames.json");
        let frames: Vec<Raw> = serde_json::from_str(fixture).expect("fixture parses");

        let mut a = adapter();
        let mut events = Vec::new();
        for frame in frames {
            events.extend(a.translate(frame));
        }

        let adapter_id = AdapterId("velocidrone".into());
        let session = SessionId("race-1".into());
        let pass = |competitor: &str, micros: i64, gate: GateIndex| {
            Event::Pass(Pass {
                adapter: adapter_id.clone(),
                competitor: CompetitorRef(competitor.into()),
                at: SourceTime::from_micros(micros),
                sequence: None,
                gate,
                signal: None,
            })
        };
        let seen = |competitor: &str| Event::CompetitorSeen {
            adapter: adapter_id.clone(),
            competitor: CompetitorRef(competitor.into()),
        };

        let expected = vec![
            // Roster reply before the race: both pilots announced.
            seen("Ace"),
            seen("Bee"),
            // Race starts.
            Event::SessionStarted {
                adapter: adapter_id.clone(),
                session: session.clone(),
            },
            // Holeshot (lap 1, gate 1) for each pilot.
            pass("Ace", 2_000_000, GateIndex::LAP),
            pass("Bee", 2_500_000, GateIndex::LAP),
            // Ace crosses a split gate (gate 2) on lap 1.
            pass("Ace", 8_000_000, GateIndex(1)),
            // Lap 2 start/finish crossings.
            pass("Ace", 14_000_000, GateIndex::LAP),
            pass("Bee", 15_500_000, GateIndex::LAP),
            // Ace finishes (lap 3, gate 1, finished).
            pass("Ace", 27_000_000, GateIndex::LAP),
            // Race finished.
            Event::SessionEnded {
                adapter: adapter_id.clone(),
                session,
            },
        ];
        assert_eq!(events, expected);
    }
}
