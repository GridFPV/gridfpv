//! Velocidrone adapter (#22, #484).
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
//! # The wire format — binary-derived, not inferred
//!
//! **The authority for everything below is the decompile of `Assembly-CSharp.dll`
//! 1.17.13**, written up in the RE workspace as
//! `~/development/fpv/velocidrone/velocidrone-libraries/velocidrone-websocket/docs/ws-spec.md`
//! (dnre-mcp/ILSpy, spec revision 2026-08-29). That document outranks every community
//! consumer this module originally leaned on, and it **corrected four things we had
//! shipped wrong** (#494). The CLAUDE.md foreign-system rule applies to Velocidrone
//! exactly as it does to RotorHazard: do not infer a field or a literal from its name —
//! check it against the spec, and add to these notes when you learn something new.
//!
//! Velocidrone serves a raw (non-Socket.IO) WebSocket at `ws://<lan-ip>:60003/velocidrone`
//! once *Websocket Communication* is enabled (Options → Main Settings, then restart).
//! Each frame is one JSON object, **externally keyed** by a single discriminator field.
//!
//! ## The four corrections that made this module work (#494)
//!
//! 1. **Every server→client frame is a BINARY frame (opcode 0x2)** carrying UTF-8 JSON —
//!    the game never sends text frames. A text-only reader hears *nothing*, with no error
//!    anywhere. That is a transport concern; see [`transport`].
//! 2. **Every race-event scalar is a quoted string** — `lap`, `gate`, `position`, `time`,
//!    and the booleans. The emitters stringify at the call site (`value.ToString()`), and
//!    C# `bool.ToString()` yields **`"True"`/`"False"`**, capitalized. The two exceptions
//!    that carry real JSON numbers are `racedata.<player>.uid` and the whole `imu`
//!    payload. Our structs used to declare `lap: u32, time: f64, gate: u32` and a
//!    required `uid: String`, so a genuine frame failed to deserialize and the transport
//!    swallowed the error: we connected cleanly and discarded 100% of the laps. Every
//!    scalar here now goes through [`wire`]'s string-or-number tolerant readers.
//! 3. **The `raceAction` vocabulary is exactly `"start"`, `"abort"`, `"race finished"`.**
//!    Community tools disagreed three ways (`"started"`/`"aborted"`/`"reset"`/`"race
//!    aborted"`); none of those literals exist in the binary. An abort spelled wrong would
//!    have left the session open forever.
//! 4. **A `finished: "True"` crossing is the race-ending lap gate whatever its gate index
//!    says.** The final crossing arrives one ordinal past the per-lap gate count
//!    (`gate = gates_per_lap + 1`) and `lap` never increments to `raceLaps + 1`, so the
//!    old "gate n > 1 is a split" rule filed the finish as a split and **the finishing lap
//!    was never recorded**.
//!
//! ## Frames we model
//!
//! - `racestatus` — `{ "racestatus": { "raceAction": "start" | "abort" | "race finished" } }`.
//!   `start` → [`Event::SessionStarted`]; `abort` / `race finished` → [`Event::SessionEnded`].
//! - `racedata` — `{ "racedata": { "<PlayerName>": { position, lap, gate, time, finished,
//!   colour, uid } } }`. **A whole-field snapshot**, not a per-crossing delta: every
//!   active pilot's latest state, re-sent (at most 10×/s, via the game's 0.1 s dirty-flag
//!   coroutine) whenever *anyone* crosses. So a new [`Pass`] is emitted only where a
//!   player's `(lap, gate, finished)` actually changed — see [`CrossingMark`].
//!   `time` is cumulative seconds from race start, `F3` formatted; `gate` is 1-based,
//!   counts *every* track gate and resets each lap; `uid` is a **JSON number** here.
//! - `pilotlist` — `{ "pilotlist": [ { name, uid } ] }`, the `getpilots` reply. `uid` is a
//!   **string** here (a different call site stringifies it). Each entry →
//!   [`Event::CompetitorSeen`], and it is the positive readback for seating.
//! - `ActivateError` — `{ "ActivateError": { "UIDNotFound": "12345" } }`, emitted **once
//!   per requested uid that is not in the room**. This is the whole readback story for
//!   the `activate` write; it carries no canonical event, and [`transport`] captures it.
//! - `racetype` — `{ "racetype": { raceMode, raceFormat, raceLaps } }`, sent right after
//!   `racestatus: "start"`. Advisory: it tells the Director the lap count the sim will
//!   actually run.
//! - `countdown` — `{ "countdown": { "countValue": "3" } }`. Multiplayer counts 5→0,
//!   single player 3→0, and **nothing at all** if the SP countdown setting is off. `0` is GO.
//! - `FinishGate` — `{ "FinishGate": { "StartFinishGate": "True" } }`. Emitted once after
//!   `countdown: 0`; it is the track-shape flag (whether the track has a distinct
//!   start/finish gate), **not** a crossing event. Advisory.
//! - `session` / `player` / `spectatorChange` / `imu` — modelled so they parse and are
//!   ignored. `spectatorChange`'s payload is a **bare string** (the only non-object
//!   payload on the wire), and `imu` arrives at **60 Hz**, so neither may log per-frame.
//!
//! # Identity: attribute by name, seat by uid
//!
//! `racedata` keys players by **name**, and that name is what becomes the
//! [`CompetitorRef`] — so the Director's existing `reconcile_seen` callsign match binds a
//! sim player to a roster pilot exactly as it does for any other source, and attribution
//! works on pre-uid builds too. The **`activate` write** goes the other way: it takes
//! account uids, which the Director reads from `Pilot::velocidrone_id`. `pilotlist` pairs
//! the two (`name` + `uid`) and is therefore the cross-check that the pilot we seated is
//! the player whose laps we are counting.
//!
//! # Still assumed, and where to change it
//!
//! - **Time unit = seconds.** Multiplied by [`SECONDS_TO_MICROS`] to reach the microsecond
//!   [`SourceTime`]. One constant to change if a build ever reports milliseconds.
//! - **No source sequence counter.** The feed exposes none, so passes carry
//!   `sequence: None` and dedup falls back to `(adapter, competitor, at)` — sound here
//!   because `time` is unique per `(player, crossing)`.
//! - **Session id.** Velocidrone sends none, so
//!   [`SessionStarted`](Event::SessionStarted)/[`SessionEnded`](Event::SessionEnded) carry
//!   a synthesized, monotonically increasing `race-<n>`.

#[cfg(feature = "live")]
pub mod transport;

use serde::Deserialize;
use std::collections::HashMap;

use gridfpv_events::{AdapterId, CompetitorRef, Event, GateIndex, Pass, SessionId, SourceTime};

use crate::dedup::Deduplicator;
use crate::{Adapter, Capabilities};

/// Multiplier from Velocidrone's floating-point **seconds** game-engine time to the
/// microsecond [`SourceTime`] the canonical model uses.
const SECONDS_TO_MICROS: f64 = 1_000_000.0;

/// String-or-number tolerant readers for Velocidrone's scalars.
///
/// The game stringifies race-event scalars at the emit site but sends `racedata.uid` and
/// the whole `imu` payload as real JSON numbers, and older builds differ again — so every
/// scalar is read tolerantly rather than pinned to one JSON type. Booleans arrive as C#
/// `"True"`/`"False"`; we accept those, plain `true`/`false`, and lowercase spellings.
///
/// Losing a scalar to a type surprise is exactly the #494 failure (a whole race silently
/// discarded), so these readers are deliberately generous.
mod wire {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    /// Read a JSON value that may be a string, a number, or a bool.
    fn value<'de, D: Deserializer<'de>>(d: D) -> Result<Value, D::Error> {
        Value::deserialize(d)
    }

    /// A `u32` written either as a JSON number or as a quoted decimal string.
    pub fn u32_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
        use serde::de::Error as _;
        match value(d)? {
            Value::Number(n) => n
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| D::Error::custom("velocidrone: integer out of u32 range")),
            Value::String(s) => s
                .trim()
                .parse::<u32>()
                .map_err(|e| D::Error::custom(format!("velocidrone: bad integer {s:?}: {e}"))),
            other => Err(D::Error::custom(format!(
                "velocidrone: expected an integer, got {other}"
            ))),
        }
    }

    /// An `f64` written either as a JSON number or as a quoted decimal string.
    ///
    /// The game formats `time` with `F3` under the **current culture**, so a
    /// comma-decimal locale emits `"69,711"`. We accept that spelling rather than drop
    /// the crossing (the serializer bug is the game's, and it is the operator's whole
    /// race).
    pub fn f64_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
        use serde::de::Error as _;
        match value(d)? {
            Value::Number(n) => n
                .as_f64()
                .ok_or_else(|| D::Error::custom("velocidrone: number is not representable")),
            Value::String(s) => {
                let s = s.trim();
                s.parse::<f64>()
                    .or_else(|_| s.replacen(',', ".", 1).parse::<f64>())
                    .map_err(|e| D::Error::custom(format!("velocidrone: bad decimal {s:?}: {e}")))
            }
            other => Err(D::Error::custom(format!(
                "velocidrone: expected a decimal, got {other}"
            ))),
        }
    }

    /// A bool written as C# `"True"`/`"False"`, as `"true"`/`"false"`, or as a real JSON
    /// bool. Absent or unrecognised reads as `false`.
    pub fn bool_lenient<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
        Ok(match value(d)? {
            Value::Bool(b) => b,
            Value::String(s) => s.trim().eq_ignore_ascii_case("true"),
            _ => false,
        })
    }

    /// An optional `u32` in either spelling; absent or unparseable reads as `None`.
    ///
    /// Used for the advisory scalars (`position`, `raceLaps`, `raceLength`) where a
    /// surprise must cost the field, not the frame — unlike `lap`/`gate`, which are
    /// load-bearing and fail loudly.
    pub fn opt_u32<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
        Ok(match Option::<Value>::deserialize(d)? {
            Some(Value::Number(n)) => n.as_u64().and_then(|v| u32::try_from(v).ok()),
            Some(Value::String(s)) => s.trim().parse::<u32>().ok(),
            _ => None,
        })
    }

    /// An optional uid: a JSON number in `racedata`, a quoted string in `pilotlist` and
    /// `ActivateError`, and absent entirely on pre-tournament builds.
    pub fn opt_uid<'de, D: Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
        Ok(match Option::<Value>::deserialize(d)? {
            Some(Value::Number(n)) => Some(n.to_string()),
            Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        })
    }

    /// A required uid in the same two spellings (`pilotlist` / `ActivateError`).
    pub fn uid<'de, D: Deserializer<'de>>(d: D) -> Result<String, D::Error> {
        use serde::de::Error as _;
        opt_uid(d)?.ok_or_else(|| D::Error::custom("velocidrone: missing uid"))
    }
}

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
/// Velocidrone's unknown frames carry object payloads, so we read the single
/// discriminator key by hand and route the value. See the [module docs](self).
#[derive(Debug, Clone, PartialEq)]
pub enum Raw {
    /// `{ "racestatus": { "raceAction": "…" } }` — the race lifecycle.
    RaceStatus(RaceStatus),
    /// `{ "racedata": { "<player>": { … } } }` — the whole-field telemetry snapshot,
    /// keyed by sim player name.
    RaceData(HashMap<String, PilotData>),
    /// `{ "pilotlist": [ { name, uid } ] }` — the `getpilots` roster reply.
    PilotList(Vec<PilotEntry>),
    /// `{ "ActivateError": { "UIDNotFound": "…" } }` — one per uid the `activate` write
    /// could not seat. The seating readback; carries no canonical event.
    ActivateError(ActivateError),
    /// `{ "racetype": { raceMode, raceFormat, raceLaps } }` — the format the sim will run.
    RaceType(RaceType),
    /// `{ "countdown": { "countValue": "3" } }` — the pre-race countdown; `0` is GO.
    Countdown(Countdown),
    /// `{ "FinishGate": { "StartFinishGate": "True" } }` — track shape (advisory).
    FinishGate(FinishGate),
    /// `{ "session": { … } }` — room created **on this machine** (never when joining).
    Session(SessionInfo),
    /// `{ "player": { PlayerName, playerColour, playerFlying, raceManager } }`.
    Player(PlayerState),
    /// `{ "spectatorChange": "Dacus" }` — a **bare string** payload, the only one.
    SpectatorChange(String),
    /// `{ "imu": { … } }` — 60 Hz attitude telemetry for the local drone. Parsed as
    /// nothing: it must be tolerated cheaply, never logged per frame.
    Imu,
    /// Any other frame kind — payload ignored, emits nothing.
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
        // A generic fn, not a closure: each arm decodes into a different payload type,
        // and a closure would monomorphize to whichever one came first.
        fn from<T: serde::de::DeserializeOwned, E: serde::de::Error>(
            v: serde_json::Value,
        ) -> Result<T, E> {
            serde_json::from_value(v).map_err(E::custom)
        }
        Ok(match key.as_str() {
            "racestatus" => Raw::RaceStatus(from(value)?),
            "racedata" => Raw::RaceData(from(value)?),
            "pilotlist" => Raw::PilotList(from(value)?),
            "ActivateError" => Raw::ActivateError(from(value)?),
            "racetype" => Raw::RaceType(from(value)?),
            "countdown" => Raw::Countdown(from(value)?),
            "FinishGate" => Raw::FinishGate(from(value)?),
            "session" => Raw::Session(from(value)?),
            "player" => Raw::Player(from(value)?),
            // The one bare-string payload on the wire.
            "spectatorChange" => Raw::SpectatorChange(from(value)?),
            // 60 Hz; the payload is deliberately not decoded.
            "imu" => Raw::Imu,
            _ => Raw::Other,
        })
    }
}

/// The `racestatus` payload: a single race-lifecycle action.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RaceStatus {
    /// Exactly one of `"start"`, `"abort"`, `"race finished"` on 1.17.13 — the complete
    /// vocabulary in the binary. Anything else is treated as "no lifecycle change".
    #[serde(rename = "raceAction")]
    pub race_action: String,
}

/// Per-player telemetry inside a `racedata` snapshot.
///
/// Every field is read string-or-number tolerantly ([`wire`]): the game stringifies all
/// of these except `uid`, which is a bare JSON number here (but a *string* in
/// [`PilotEntry`] and [`ActivateError`] — different call sites).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PilotData {
    /// Live rank, `"1"` = leader. Advisory: the engine derives standings itself.
    #[serde(default, deserialize_with = "wire::opt_u32")]
    pub position: Option<u32>,
    /// Lap counter for this crossing (`calculatedposition / 1000`).
    #[serde(deserialize_with = "wire::u32_lenient")]
    pub lap: u32,
    /// Gate ordinal crossed — **1-based**, counts every track gate, resets each lap
    /// (`calculatedposition % 1000`).
    #[serde(deserialize_with = "wire::u32_lenient")]
    pub gate: u32,
    /// Cumulative game-engine time of the crossing, in **seconds** (`F3` on the wire).
    #[serde(deserialize_with = "wire::f64_lenient")]
    pub time: f64,
    /// Whether this crossing ended the player's race. `"True"`/`"False"` on the wire.
    #[serde(default, deserialize_with = "wire::bool_lenient")]
    pub finished: bool,
    /// The player's assigned colour, uppercase RGB hex with **no `#`** (e.g. `"00FFFF"`).
    #[serde(default)]
    pub colour: Option<String>,
    /// Velocidrone account id. A JSON **number** here; absent on pre-tournament builds.
    #[serde(default, deserialize_with = "wire::opt_uid")]
    pub uid: Option<String>,
}

/// A `pilotlist` entry — a player in the room, from the `getpilots` reply.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PilotEntry {
    /// Player display name.
    pub name: String,
    /// Velocidrone account id — a **string** in this frame.
    #[serde(deserialize_with = "wire::uid")]
    pub uid: String,
}

/// The `ActivateError` payload — one uid the `activate` write could not seat.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ActivateError {
    /// The stringified uid that was not present in the room.
    #[serde(rename = "UIDNotFound", deserialize_with = "wire::uid")]
    pub uid_not_found: String,
}

/// The `racetype` payload — the format the sim is about to run.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RaceType {
    /// The event mode enum's `ToString()`, e.g. `"THREE_LAP_SINGLE_CLASS"`.
    #[serde(rename = "raceMode", default)]
    pub race_mode: Option<String>,
    /// The race format enum's `ToString()`, e.g. `"NORMAL"`.
    #[serde(rename = "raceFormat", default)]
    pub race_format: Option<String>,
    /// The lap count the sim will run.
    #[serde(rename = "raceLaps", default, deserialize_with = "wire::opt_u32")]
    pub race_laps: Option<u32>,
}

/// The `countdown` payload — the pre-race count, `0` being GO.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Countdown {
    /// Counts 5→0 in multiplayer, 3→0 in single player.
    #[serde(rename = "countValue", deserialize_with = "wire::u32_lenient")]
    pub count_value: u32,
}

/// The `FinishGate` payload — whether the track has a distinct start/finish gate.
///
/// Emitted **once**, right after `countdown: 0`. Despite the name it is not a crossing.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct FinishGate {
    /// `"True"`/`"False"` on the wire.
    #[serde(rename = "StartFinishGate", deserialize_with = "wire::bool_lenient")]
    pub start_finish_gate: bool,
}

/// The `session` payload — sent only when *this* machine creates the room.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SessionInfo {
    /// The local player's name.
    #[serde(rename = "playerName", default)]
    pub player_name: Option<String>,
    /// The multiplayer room name.
    #[serde(rename = "sessionName", default)]
    pub session_name: Option<String>,
    /// The scenery the track sits in.
    #[serde(rename = "sceneryTitle", default)]
    pub scenery_title: Option<String>,
    /// The track name.
    #[serde(rename = "trackName", default)]
    pub track_name: Option<String>,
    /// The configured race length in laps.
    #[serde(rename = "raceLength", default, deserialize_with = "wire::opt_u32")]
    pub race_length: Option<u32>,
}

/// The `player` payload — one player's state. Note the inconsistent `PlayerName` casing;
/// that is the game's, not ours.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PlayerState {
    /// Player display name.
    #[serde(rename = "PlayerName", default)]
    pub player_name: Option<String>,
    /// Assigned colour, RGB hex without `#`.
    #[serde(rename = "playerColour", default)]
    pub player_colour: Option<String>,
    /// Whether the player is flying (vs spectating).
    #[serde(
        rename = "playerFlying",
        default,
        deserialize_with = "wire::bool_lenient"
    )]
    pub player_flying: bool,
    /// The room-host flag — the readback for "are we allowed to send commands".
    #[serde(
        rename = "raceManager",
        default,
        deserialize_with = "wire::bool_lenient"
    )]
    pub race_manager: bool,
}

/// Map a Velocidrone gate ordinal to a canonical [`GateIndex`].
///
/// Velocidrone numbers the start/finish (lap) gate as `1` and counts every track gate
/// from there, resetting each lap; the canonical model numbers the lap gate `0`
/// ([`GateIndex::LAP`]) and splits from `1` up. So gate `1` → lap gate, gate `n > 1` →
/// split `n - 1`. Gate `0`, if it ever appears, is the lap gate too.
///
/// **The finish is not routed through here.** A `finished: "True"` crossing arrives one
/// ordinal past the per-lap gate count, so this mapping would file the race-ending
/// crossing as a split and lose the final lap — see [`VelocidroneAdapter::translate_pilot`].
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

/// The crossing identity tracked per player to recognise a *new* gate crossing.
///
/// `racedata` is a whole-field snapshot re-sent up to 10×/s whenever *any* pilot crosses,
/// so every frame restates every active player. Only a change in `(lap, gate, finished)`
/// marks a fresh crossing worth emitting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CrossingMark {
    lap: u32,
    gate: u32,
    finished: bool,
}

/// Velocidrone adapter — a **pure translator** from decoded [`Raw`] frames to canonical
/// events. It owns the per-source translation state: a [`Deduplicator`] (so a reconnect
/// that replays an overlapping tail injects no duplicate passes), the per-player last-seen
/// crossing mark, the set of players already announced via [`Event::CompetitorSeen`], and
/// the synthesized session id.
///
/// Moving bytes — the socket, the keep-alive, reconnect, and the outbound command surface
/// — is [`transport`]'s job.
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
    ///
    /// The three literals are the complete 1.17.13 vocabulary (see the module docs); we
    /// still lowercase before matching so a future build's capitalisation cannot silently
    /// leave a session open.
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

    /// Translate one player's slot of a `racedata` snapshot into a [`Pass`] when it
    /// represents a genuinely new crossing.
    fn translate_pilot(&mut self, player: &str, data: &PilotData, out: &mut Vec<Event>) {
        self.announce_player(player, out);

        let mark = CrossingMark {
            lap: data.lap,
            gate: data.gate,
            finished: data.finished,
        };

        // Every frame restates the whole field, so only a changed (lap, gate, finished)
        // is a fresh crossing. Identical restatements are dropped here, and any that slip
        // through are caught by the dedup window.
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
            // The race-ending crossing is a LAP gate whatever its ordinal says: it
            // arrives one past the per-lap gate count, and `map_gate` would file it as a
            // split — which is how the finishing lap used to go missing (#494).
            gate: if data.finished {
                GateIndex::LAP
            } else {
                map_gate(data.gate)
            },
            signal: None,
            // The adapter doesn't know the heat; the bridge sink stamps it at append.
            heat: None,
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
                // Order by player name so a snapshot carrying several players translates
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
            // Readback, advisory and cosmetic frames carry no canonical event.
            // `ActivateError` is the `activate` readback and is consumed by the
            // transport; `racetype`/`countdown`/`FinishGate`/`session`/`player` are
            // advisory; `spectatorChange` and `imu` are cosmetic (and `imu` is 60 Hz, so
            // it must stay this cheap).
            Raw::ActivateError(_)
            | Raw::RaceType(_)
            | Raw::Countdown(_)
            | Raw::FinishGate(_)
            | Raw::Session(_)
            | Raw::Player(_)
            | Raw::SpectatorChange(_)
            | Raw::Imu
            | Raw::Other => {}
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

    /// One `racedata` slot in **real** wire shapes — every scalar quoted, the boolean
    /// capitalized, `uid` a bare number. Written as a helper so no test can quietly
    /// re-introduce the invented numeric shapes that made #494 possible.
    fn slot(pos: u32, lap: u32, gate: u32, time: &str, finished: bool, uid: i64) -> String {
        format!(
            r#"{{"position":"{pos}","lap":"{lap}","gate":"{gate}","time":"{time}","finished":"{}","colour":"00FFFF","uid":{uid}}}"#,
            if finished { "True" } else { "False" }
        )
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

    /// The regression that names #494: a genuine `racedata` frame — all scalars quoted,
    /// `"False"` capitalized, `uid` a bare JSON number, **no `name` key** — must
    /// deserialize. The struct this replaced declared `lap: u32, time: f64, gate: u32`
    /// and a required `uid: String`, so this exact frame failed with
    /// `invalid type: string "1", expected u32` and the transport swallowed it: we
    /// connected cleanly and discarded every lap of the race.
    #[test]
    fn a_real_racedata_frame_deserializes() {
        let frame = r#"{"racedata":{"Dacus":{"position":"1","lap":"1","gate":"1","time":"1.369","finished":"False","colour":"00FFFF","uid":12345}}}"#;
        let Raw::RaceData(players) = raw(frame) else {
            panic!("expected racedata");
        };
        let d = &players["Dacus"];
        assert_eq!(d.position, Some(1));
        assert_eq!(d.lap, 1);
        assert_eq!(d.gate, 1);
        assert!((d.time - 1.369).abs() < f64::EPSILON);
        assert!(!d.finished);
        assert_eq!(d.colour.as_deref(), Some("00FFFF"));
        assert_eq!(d.uid.as_deref(), Some("12345"));
    }

    /// The pre-tournament capture (2025-01-23) carries **no `uid` and no `name`** — the
    /// player is identified only by the map key. That build must still translate.
    #[test]
    fn a_pre_uid_capture_frame_still_deserializes() {
        let frame = r#"{"racedata":{"Dacus":{"position":"1","lap":"3","gate":"43","time":"69.711","finished":"True","colour":"00FFFF"}}}"#;
        let Raw::RaceData(players) = raw(frame) else {
            panic!("expected racedata");
        };
        let d = &players["Dacus"];
        assert_eq!(d.uid, None);
        assert_eq!(d.gate, 43);
        assert!(d.finished);
    }

    /// Scalars are read string-or-number tolerantly in both directions, so a build that
    /// switches a field to a real JSON number cannot silently cost us the feed again.
    #[test]
    fn scalars_are_read_in_either_json_spelling() {
        let numeric = r#"{"racedata":{"Ace":{"position":1,"lap":2,"gate":3,"time":4.5,"finished":false,"uid":"9"}}}"#;
        let Raw::RaceData(players) = raw(numeric) else {
            panic!("expected racedata");
        };
        let d = &players["Ace"];
        assert_eq!((d.position, d.lap, d.gate), (Some(1), 2, 3));
        assert!((d.time - 4.5).abs() < f64::EPSILON);
        assert!(!d.finished);
        // uid is a number in `racedata` and a string in `pilotlist`; both normalise.
        assert_eq!(d.uid.as_deref(), Some("9"));
    }

    /// The game formats numbers under the **current culture**, so a comma-decimal locale
    /// emits `"69,711"` — malformed JSON as a number, but recoverable as a string. That
    /// is the operator's whole race, so we take it rather than drop the crossing.
    #[test]
    fn a_comma_decimal_locale_time_still_parses() {
        let frame =
            r#"{"racedata":{"Ace":{"lap":"1","gate":"1","time":"69,711","finished":"False"}}}"#;
        let Raw::RaceData(players) = raw(frame) else {
            panic!("expected racedata");
        };
        assert!((players["Ace"].time - 69.711).abs() < 1e-9);
    }

    #[test]
    fn raw_frames_deserialize_to_the_right_variant() {
        assert!(matches!(
            raw(r#"{"racestatus":{"raceAction":"start"}}"#),
            Raw::RaceStatus(_)
        ));
        assert!(matches!(
            raw(&format!(
                r#"{{"racedata":{{"Ace":{}}}}}"#,
                slot(1, 1, 1, "1.500", false, 1)
            )),
            Raw::RaceData(_)
        ));
        assert!(matches!(
            raw(r#"{"pilotlist":[{"name":"Ace","uid":"12345"}]}"#),
            Raw::PilotList(_)
        ));
        assert!(matches!(
            raw(r#"{"FinishGate":{"StartFinishGate":"True"}}"#),
            Raw::FinishGate(_)
        ));
        assert!(matches!(
            raw(
                r#"{"racetype":{"raceMode":"THREE_LAP_SINGLE_CLASS","raceFormat":"NORMAL","raceLaps":"3"}}"#
            ),
            Raw::RaceType(_)
        ));
        assert!(matches!(
            raw(r#"{"countdown":{"countValue":"3"}}"#),
            Raw::Countdown(_)
        ));
        assert!(matches!(
            raw(
                r#"{"session":{"playerName":"Ace","sessionName":"room","sceneryTitle":"s","trackName":"t","raceLength":"3","RaceMode":"m","quadType":"q","quadSize":"5"}}"#
            ),
            Raw::Session(_)
        ));
        assert!(matches!(
            raw(
                r#"{"player":{"PlayerName":"Ace","playerColour":"00FFFF","playerFlying":"True","raceManager":"False"}}"#
            ),
            Raw::Player(_)
        ));
        // `ActivateError` is the seating readback — it used to fall into `Other`.
        assert_eq!(
            raw(r#"{"ActivateError":{"UIDNotFound":"99999"}}"#),
            Raw::ActivateError(ActivateError {
                uid_not_found: "99999".into()
            })
        );
        // The one bare-string payload on the wire.
        assert_eq!(
            raw(r#"{"spectatorChange":"Dacus"}"#),
            Raw::SpectatorChange("Dacus".into())
        );
        // 60 Hz, all-numeric: recognised and discarded without decoding the payload.
        assert_eq!(
            raw(r#"{"imu":{"roll":1.23,"pitch":-0.5,"yaw":0.01,"timestamp":123456.78}}"#),
            Raw::Imu
        );
        // A frame kind we have never seen degrades to `Other`, never a parse failure.
        assert_eq!(raw(r#"{"somethingNew":{"a":1}}"#), Raw::Other);
    }

    /// `raceManager` is the readback for "may we send host-gated commands at all".
    #[test]
    fn the_player_frame_carries_the_race_manager_flag() {
        let Raw::Player(p) = raw(
            r#"{"player":{"PlayerName":"Ace","playerColour":"00FFFF","playerFlying":"True","raceManager":"True"}}"#,
        ) else {
            panic!("expected player");
        };
        assert_eq!(p.player_name.as_deref(), Some("Ace"));
        assert!(p.player_flying);
        assert!(p.race_manager);
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
        let events = a.translate(raw(r#"{"racestatus":{"raceAction":"start"}}"#));
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
        a.translate(raw(r#"{"racestatus":{"raceAction":"start"}}"#));
        let events = a.translate(raw(r#"{"racestatus":{"raceAction":"race finished"}}"#));
        assert_eq!(
            events,
            vec![Event::SessionEnded {
                adapter: AdapterId("velocidrone".into()),
                session: SessionId("race-1".into()),
            }]
        );
    }

    /// `"abort"` is the literal in the 1.17.13 binary. Community tools variously send
    /// `"aborted"` and `"race aborted"`; neither exists, and matching only those would
    /// leave the session open forever — so this test pins the real spelling.
    #[test]
    fn abort_ends_the_session_too() {
        let mut a = adapter();
        a.translate(raw(r#"{"racestatus":{"raceAction":"start"}}"#));
        let events = a.translate(raw(r#"{"racestatus":{"raceAction":"abort"}}"#));
        assert!(matches!(events.as_slice(), [Event::SessionEnded { .. }]));
    }

    #[test]
    fn a_crossing_announces_the_player_then_emits_a_pass() {
        let mut a = adapter();
        let events = a.translate(raw(&format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 1, 1, "2.000", false, 12345)
        )));
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
                    heat: None,
                }),
            ]
        );
    }

    #[test]
    fn player_is_announced_only_once() {
        let mut a = adapter();
        a.translate(raw(&format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 1, 1, "1.000", false, 1)
        )));
        let events = a.translate(raw(&format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 2, 1, "5.000", false, 1)
        )));
        // Second crossing: no CompetitorSeen, just the new Pass.
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Event::Pass(_)));
    }

    #[test]
    fn a_split_gate_carries_a_split_index() {
        let mut a = adapter();
        let events = a.translate(raw(&format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 1, 2, "1.200", false, 1)
        )));
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

    /// The whole-field snapshot is re-sent up to 10×/s whenever *anyone* crosses, so an
    /// unchanged restatement must emit nothing.
    #[test]
    fn restated_unchanged_telemetry_emits_no_duplicate_pass() {
        let mut a = adapter();
        let frame = format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 1, 1, "1.000", false, 1)
        );
        let first = a.translate(raw(&frame));
        assert_eq!(
            first.iter().filter(|e| matches!(e, Event::Pass(_))).count(),
            1
        );
        let second = a.translate(raw(&frame));
        assert!(second.is_empty());
    }

    /// **The lost finish (#494).** On a 3-gate track the race-ending crossing arrives as
    /// `gate = 4` with `finished: "True"`, and `lap` never reaches 4. Under the old
    /// mapping that became `GateIndex(3)` — a split — so the finishing lap was never
    /// recorded as a lap at all. It must be a LAP gate whatever the ordinal says.
    #[test]
    fn the_finishing_crossing_is_a_lap_gate_whatever_its_ordinal() {
        let mut a = adapter();
        a.translate(raw(&format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 3, 1, "11.900", false, 1)
        )));
        let events = a.translate(raw(&format!(
            r#"{{"racedata":{{"Ace":{}}}}}"#,
            slot(1, 3, 4, "17.220", true, 1)
        )));
        let pass = events
            .iter()
            .find_map(|e| match e {
                Event::Pass(p) => Some(p),
                _ => None,
            })
            .expect("a finishing pass");
        assert_eq!(pass.gate, GateIndex::LAP, "the finish must count as a lap");
        assert!(pass.gate.is_lap_gate());
        assert_eq!(pass.at, SourceTime::from_micros(17_220_000));
    }

    #[test]
    fn pilotlist_announces_each_player() {
        let mut a = adapter();
        let events = a.translate(raw(
            r#"{"pilotlist":[{"name":"Ace","uid":"12345"},{"name":"Bee","uid":"67890"}]}"#,
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

    /// Advisory, readback and cosmetic frames translate to nothing — but they must all
    /// still *parse*, which is what keeps `imu` at 60 Hz from costing anything.
    #[test]
    fn advisory_and_readback_frames_emit_nothing() {
        let mut a = adapter();
        for frame in [
            r#"{"FinishGate":{"StartFinishGate":"True"}}"#,
            r#"{"ActivateError":{"UIDNotFound":"99999"}}"#,
            r#"{"racetype":{"raceMode":"THREE_LAP_SINGLE_CLASS","raceFormat":"NORMAL","raceLaps":"3"}}"#,
            r#"{"countdown":{"countValue":"0"}}"#,
            r#"{"spectatorChange":"Dacus"}"#,
            r#"{"imu":{"roll":1.23,"pitch":-0.5,"yaw":0.01,"timestamp":123456.78}}"#,
            r#"{"somethingNew":{"a":1}}"#,
        ] {
            assert!(a.translate(raw(frame)).is_empty(), "{frame} emitted events");
        }
    }

    /// Drive the recorded two-pilot sprint — real 1.17.13 wire shapes, whole-field
    /// snapshots — end to end and assert the exact canonical event stream: lifecycle,
    /// CompetitorSeen, and every Pass with the right competitor / timestamp / gate.
    #[test]
    fn recorded_session_translates_to_the_expected_event_stream() {
        let fixture = include_str!("velocidrone/fixtures/sprint.frames.json");
        // The fixture interleaves bare strings as commentary; only the objects are frames.
        let entries: Vec<serde_json::Value> =
            serde_json::from_str(fixture).expect("fixture parses");
        let frames: Vec<Raw> = entries
            .into_iter()
            .filter(|v| v.is_object())
            .map(|v| serde_json::from_value(v).expect("fixture frame parses"))
            .collect();

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
                heat: None,
            })
        };
        let seen = |competitor: &str| Event::CompetitorSeen {
            adapter: adapter_id.clone(),
            competitor: CompetitorRef(competitor.into()),
        };

        let expected = vec![
            // The roster reply announces both pilots before the race.
            seen("Ace"),
            seen("Bee"),
            Event::SessionStarted {
                adapter: adapter_id.clone(),
                session: session.clone(),
            },
            // Holeshot for each.
            pass("Ace", 1_336_000, GateIndex::LAP),
            pass("Bee", 1_502_000, GateIndex::LAP),
            // Splits, each emitted only for the pilot whose slot actually changed.
            pass("Ace", 2_965_000, GateIndex(1)),
            pass("Bee", 3_101_000, GateIndex(1)),
            pass("Ace", 4_200_000, GateIndex(2)),
            // Ace starts lap 2 while Bee takes her last split of lap 1.
            pass("Ace", 6_010_000, GateIndex::LAP),
            pass("Bee", 4_905_000, GateIndex(2)),
            // (the repeated snapshot in between emits nothing)
            pass("Bee", 6_850_000, GateIndex::LAP),
            pass("Ace", 11_900_000, GateIndex::LAP),
            // Ace finishes on gate 4 of a 3-gate track — a LAP gate, not a split.
            pass("Ace", 17_220_000, GateIndex::LAP),
            pass("Bee", 12_700_000, GateIndex::LAP),
            pass("Bee", 18_400_000, GateIndex::LAP),
            Event::SessionEnded {
                adapter: adapter_id.clone(),
                session,
            },
        ];
        assert_eq!(events, expected);
    }
}
