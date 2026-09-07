//! The **Velocidrone mock server** (`vd-mock`, #484): a wire-faithful, std-only stand-in
//! for the game's built-in WebSocket server, for CI and bench use.
//!
//! Velocidrone cannot be containerized (closed source), so unlike RotorHazard the mock IS
//! the server. Fidelity is the whole point: every deliberate quirk of the real
//! implementation (decompiled from VelociDrone **1.17.13**, `Assembly-CSharp.dll`
//! 2026-04-28 — see the RE workspace's `velocidrone-websocket/docs/ws-spec.md` for the
//! full spec and receipts) is reproduced here, because each one has already bitten a real
//! consumer:
//!
//! - **Every server→client message is a BINARY frame (opcode 0x2)** whose payload is
//!   UTF-8 JSON — the game never sends text frames. A text-only client hears nothing.
//! - The handshake validates the URL path by stripping *all* leading/trailing `/` and
//!   comparing **case-sensitively** against the service name (`velocidrone` as shipped);
//!   a mismatch gets `400 Bad Request` and an abort. `Sec-WebSocket-Version` must be
//!   exactly `13`, `Upgrade` exactly `websocket`, and only the **first** token of a
//!   header value is read (`Connection: keep-alive, Upgrade` fails).
//! - Client→server data frames must be **masked**; an unmasked one draws an error and an
//!   immediate abort. Text (0x1), binary (0x2) and continuation (0x0) are all accepted
//!   as data.
//! - An **empty** masked data frame is echoed back and never reaches the command layer
//!   (the silent legacy keep-alive). An RFC ping (0x9) is answered with a 2-byte binary
//!   *message* `{0x8A, 0x00}` — not an RFC pong — and the real server assumes a
//!   zero-length ping, so payload pings desync it; the mock replicates the reply but
//!   tolerates the payload (a desync would only test our own bug).
//! - **No close frames, ever**: teardown is a raw TCP close in both directions.
//! - A **sliding idle timeout** (default 40 s, configurable): any inbound frame re-arms
//!   it; expiry closes the connection.
//! - **`SocketManager` serves exactly one client — the newest.** Extra connections
//!   handshake fine and then receive nothing; every event (including command *replies*
//!   like `pilotlist`) goes to the most recent connection only.
//! - Frames are **byte-exact**: string-typed scalars, `"True"`/`"False"` booleans, `F3`
//!   times, `#`-less uppercase colours, `uid` as a bare JSON number in `racedata` but a
//!   string in `pilotlist`/`ActivateError`, and the game's key order.
//!
//! Two feed modes ([`Feed`]): **replay** (a timed script of verbatim frames — the vendored
//! `capture_2025-01-23_race4.tsv` is a genuine capture off a live build) and **scripted**
//! (a [`RacePlan`] race generated in current-build shapes, drivable over the socket with
//! `startrace` / `abortrace` / `activate` exactly like the real game). All received
//! commands are recorded with their authorization outcome for assertions
//! ([`VdMock::commands`]).
//!
//! The mock is deliberately **server-behavior only**: it does not try to emulate physics
//! or Photon — just the protocol surface a timing/control consumer can observe.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------------------------
// SHA-1 + base64 — the two primitives the RFC 6455 handshake needs. Hand-rolled so the
// testkit keeps its no-third-party-dependencies property; both are checked against RFC
// test vectors in the tests below.
// ---------------------------------------------------------------------------------------------

/// SHA-1 (RFC 3174). Only used for `Sec-WebSocket-Accept`; not security-relevant here.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let ml = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&ml.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// Standard base64 (RFC 4648, with padding).
fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// The RFC 6455 magic GUID (also verbatim in the decompiled `HandShake.GetAcceptResponse`).
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// `Sec-WebSocket-Accept` for a given `Sec-WebSocket-Key`.
fn accept_key(key: &str) -> String {
    base64(&sha1(format!("{key}{WS_GUID}").as_bytes()))
}

// ---------------------------------------------------------------------------------------------
// Minimal JSON — the command sink's parser. Mirrors the *game's* parser where it matters
// for tolerance (trailing commas accepted, invariant number parsing, malformed input is a
// soft failure), because commands the real game accepts must not be rejected by the mock.
// ---------------------------------------------------------------------------------------------

/// A parsed JSON value. Object keys keep insertion order (like the game's dictionary in
/// practice); duplicate keys last-wins (also like the game).
#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// Look up an object key (last-wins on duplicates, like the game).
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(pairs) => pairs.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// The string content, if this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }
}

/// Parse a JSON document. Returns `None` on malformed input — the mock, like the game,
/// never aborts a connection over bad JSON (the game logs and drops it).
pub fn parse_json(text: &str) -> Option<Json> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let v = parse_value(bytes, &mut pos)?;
    skip_ws(bytes, &mut pos);
    if pos == bytes.len() { Some(v) } else { None }
}

fn skip_ws(b: &[u8], pos: &mut usize) {
    while *pos < b.len() && matches!(b[*pos], b' ' | b'\t' | b'\r' | b'\n') {
        *pos += 1;
    }
}

fn parse_value(b: &[u8], pos: &mut usize) -> Option<Json> {
    skip_ws(b, pos);
    match *b.get(*pos)? {
        b'{' => {
            *pos += 1;
            let mut pairs = Vec::new();
            loop {
                skip_ws(b, pos);
                match *b.get(*pos)? {
                    b'}' => {
                        *pos += 1;
                        return Some(Json::Obj(pairs));
                    }
                    b'"' => {
                        let key = parse_string(b, pos)?;
                        skip_ws(b, pos);
                        if *b.get(*pos)? != b':' {
                            return None;
                        }
                        *pos += 1;
                        let val = parse_value(b, pos)?;
                        pairs.push((key, val));
                        skip_ws(b, pos);
                        match *b.get(*pos)? {
                            b',' => *pos += 1, // trailing comma before '}' is fine (game parity)
                            b'}' => {}
                            _ => return None,
                        }
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *pos += 1;
            let mut items = Vec::new();
            loop {
                skip_ws(b, pos);
                if *b.get(*pos)? == b']' {
                    *pos += 1;
                    return Some(Json::Arr(items));
                }
                items.push(parse_value(b, pos)?);
                skip_ws(b, pos);
                match *b.get(*pos)? {
                    b',' => *pos += 1, // trailing comma before ']' is fine (game parity)
                    b']' => {}
                    _ => return None,
                }
            }
        }
        b'"' => Some(Json::Str(parse_string(b, pos)?)),
        b't' => {
            if b.get(*pos..*pos + 4)? == b"true" {
                *pos += 4;
                Some(Json::Bool(true))
            } else {
                None
            }
        }
        b'f' => {
            if b.get(*pos..*pos + 5)? == b"false" {
                *pos += 5;
                Some(Json::Bool(false))
            } else {
                None
            }
        }
        b'n' => {
            if b.get(*pos..*pos + 4)? == b"null" {
                *pos += 4;
                Some(Json::Null)
            } else {
                None
            }
        }
        _ => {
            let start = *pos;
            while *pos < b.len()
                && matches!(b[*pos], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                *pos += 1;
            }
            std::str::from_utf8(&b[start..*pos])
                .ok()?
                .parse::<f64>()
                .ok()
                .map(Json::Num)
        }
    }
}

fn parse_string(b: &[u8], pos: &mut usize) -> Option<String> {
    // Standard escapes plus \uXXXX; the game only decodes \uXXXX, but accepting the
    // standard set here only makes the mock *more* permissive on input, never on output.
    *pos += 1; // opening quote
    let mut out = String::new();
    loop {
        match *b.get(*pos)? {
            b'"' => {
                *pos += 1;
                return Some(out);
            }
            b'\\' => {
                *pos += 1;
                match *b.get(*pos)? {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'n' => out.push('\n'),
                    b't' => out.push('\t'),
                    b'r' => out.push('\r'),
                    b'u' => {
                        let hex = std::str::from_utf8(b.get(*pos + 1..*pos + 5)?).ok()?;
                        let cp = u32::from_str_radix(hex, 16).ok()?;
                        out.push(char::from_u32(cp)?);
                        *pos += 4;
                    }
                    other => out.push(other as char),
                }
                *pos += 1;
            }
            c => {
                // Multi-byte UTF-8 continuation bytes pass through unchanged.
                let ch_len = match c {
                    0x00..=0x7F => 1,
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    _ => 4,
                };
                let s = std::str::from_utf8(b.get(*pos..*pos + ch_len)?).ok()?;
                out.push_str(s);
                *pos += ch_len;
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Wire-exact frame builders — every event the 1.17.13 game can emit, in the game's key
// order and scalar formats. These strings ARE the spec; the doc tests in `tests` assert
// the exact bytes.
// ---------------------------------------------------------------------------------------------

/// Wire-exact event frame builders (game key order, game scalar formats).
pub mod frames {
    /// One pilot's `racedata` row. `uid: None` reproduces pre-tournament builds (the
    /// 2025-01 capture has no uid); `Some` emits the 1.17.13 shape — a **bare JSON
    /// number**, the only non-string scalar in the race events.
    pub struct RaceRow<'a> {
        pub name: &'a str,
        pub position: u32,
        pub lap: u32,
        pub gate: u32,
        /// Cumulative race seconds; serialized with exactly 3 decimals (C# `"F3"`).
        pub time_s: f64,
        pub finished: bool,
        /// Uppercase RGB hex, no `#` (e.g. `00FFFF`).
        pub colour: &'a str,
        pub uid: Option<i64>,
    }

    /// C# `bool.ToString()`: capitalized.
    fn cs_bool(b: bool) -> &'static str {
        if b { "True" } else { "False" }
    }

    pub fn racestatus(action: &str) -> String {
        format!(r#"{{"racestatus":{{"raceAction":"{action}"}}}}"#)
    }

    pub fn countdown(value: u32) -> String {
        format!(r#"{{"countdown":{{"countValue":"{value}"}}}}"#)
    }

    /// The track-shape flag sent right after `countdown 0` — NOT a crossing event.
    pub fn finish_gate(active: bool) -> String {
        format!(
            r#"{{"FinishGate":{{"StartFinishGate":"{}"}}}}"#,
            cs_bool(active)
        )
    }

    pub fn racetype(mode: &str, format: &str, laps: u32) -> String {
        format!(
            r#"{{"racetype":{{"raceMode":"{mode}","raceFormat":"{format}","raceLaps":"{laps}"}}}}"#
        )
    }

    #[allow(clippy::too_many_arguments)] // mirrors the game's 8-arg newRoomCreated
    pub fn session(
        player_name: &str,
        session_name: &str,
        scenery_title: &str,
        track_name: &str,
        race_length: u32,
        race_mode: &str,
        quad_type: &str,
        quad_size: &str,
    ) -> String {
        format!(
            r#"{{"session":{{"playerName":"{player_name}","sessionName":"{session_name}","sceneryTitle":"{scenery_title}","trackName":"{track_name}","raceLength":"{race_length}","RaceMode":"{race_mode}","quadType":"{quad_type}","quadSize":"{quad_size}"}}}}"#
        )
    }

    /// The bare-string payload event (the only non-object payload on the wire).
    pub fn spectator_change(name: &str) -> String {
        format!(r#"{{"spectatorChange":"{name}"}}"#)
    }

    pub fn player(name: &str, colour: &str, flying: bool, race_manager: bool) -> String {
        format!(
            r#"{{"player":{{"PlayerName":"{name}","playerColour":"{colour}","playerFlying":"{}","raceManager":"{}"}}}}"#,
            cs_bool(flying),
            cs_bool(race_manager)
        )
    }

    /// `getpilots` reply. Note: uid is a **string** here (unlike `racedata`).
    pub fn pilotlist(pilots: &[(&str, i64)]) -> String {
        let items: Vec<String> = pilots
            .iter()
            .map(|(name, uid)| format!(r#"{{"name":"{name}","uid":"{uid}"}}"#))
            .collect();
        format!(r#"{{"pilotlist":[{}]}}"#, items.join(","))
    }

    /// One frame per missing uid, in request order (uid stringified).
    pub fn activate_error(uid: i64) -> String {
        format!(r#"{{"ActivateError":{{"UIDNotFound":"{uid}"}}}}"#)
    }

    /// A whole-field `racedata` snapshot, keyed by player name, in field order
    /// `position, lap, gate, time, finished, colour[, uid]`.
    pub fn racedata(rows: &[RaceRow<'_>]) -> String {
        let entries: Vec<String> = rows
            .iter()
            .map(|r| {
                let uid = match r.uid {
                    Some(uid) => format!(r#","uid":{uid}"#),
                    None => String::new(),
                };
                format!(
                    r#""{}":{{"position":"{}","lap":"{}","gate":"{}","time":"{:.3}","finished":"{}","colour":"{}"{}}}"#,
                    r.name,
                    r.position,
                    r.lap,
                    r.gate,
                    r.time_s,
                    cs_bool(r.finished),
                    r.colour,
                    uid
                )
            })
            .collect();
        format!(r#"{{"racedata":{{{}}}}}"#, entries.join(","))
    }

    /// The 1.17.x IMU frame — the only all-numbers event family (60 Hz, local drone,
    /// Betaflight only, opt-in). Numbers use Rust's shortest-roundtrip formatting; the
    /// game uses C# `Double.ToString()` under the current culture, which is close but
    /// not byte-identical (and locale-broken on comma-decimal machines — see the spec).
    #[allow(clippy::too_many_arguments)] // mirrors the game's sendIMUData signature
    pub fn imu(
        roll: f64,
        pitch: f64,
        yaw: f64,
        pos: (f64, f64, f64),
        att: (f64, f64, f64, f64),
        speed: (f64, f64, f64),
        timestamp_ms: f64,
    ) -> String {
        format!(
            r#"{{"imu":{{"roll":{roll},"pitch":{pitch},"yaw":{yaw},"PositionX":{},"PositionY":{},"PositionZ":{},"AttitudeX":{},"AttitudeY":{},"AttitudeZ":{},"AttitudeW":{},"SpeedX":{},"SpeedY":{},"SpeedZ":{},"timestamp":{timestamp_ms}}}}}"#,
            pos.0, pos.1, pos.2, att.0, att.1, att.2, att.3, speed.0, speed.1, speed.2
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Configuration & scenarios
// ---------------------------------------------------------------------------------------------

/// A pilot in the mock's Photon-room roster.
#[derive(Debug, Clone)]
pub struct MockPilot {
    pub name: String,
    pub uid: i64,
    /// Uppercase RGB hex, no `#`.
    pub colour: String,
    /// Whether the pilot starts in flying state (activate/allspectate mutate this).
    pub flying: bool,
    /// Average per-gate time in ms for the scripted race (pace).
    pub gate_ms: u64,
}

/// What the mock feeds connected clients.
#[derive(Debug, Clone)]
pub enum Feed {
    /// Replay a timed script of verbatim frames: `(at_ms, frame)`. Ground truth for
    /// legacy shapes — see [`capture_race4`].
    Replay(Vec<(u64, String)>),
    /// Generate a race from the roster in 1.17.13 shapes.
    Scripted(RacePlan),
}

/// The scripted race: how many laps over how many gates, in current-build shapes.
#[derive(Debug, Clone)]
pub struct RacePlan {
    pub laps: u32,
    pub gates_per_lap: u32,
    pub race_mode: String,
    pub race_format: String,
    /// Multiplayer countdown starts at 5; single player at 3 (game behavior).
    pub countdown_from: u32,
    pub start_finish_gate: bool,
    /// Emit `uid` in racedata rows (1.17.13 shape). `false` reproduces legacy builds.
    pub uids_on_wire: bool,
    /// Interleave 60 Hz `imu` frames during the race (the 1.17.x opt-in feed).
    pub imu: bool,
    /// Inject the junk-tolerance set: an unknown one-key frame, a malformed frame, and a
    /// racedata snapshot whose player name contains an unescaped quote (the game's
    /// serializer does not escape strings — consumers must survive all three).
    pub junk: bool,
    /// Abort the race this many ms after GO (emits `racestatus:"abort"`), instead of
    /// running to the finish.
    pub abort_after_go_ms: Option<u64>,
}

impl Default for RacePlan {
    fn default() -> Self {
        Self {
            laps: 3,
            gates_per_lap: 5,
            race_mode: "THREE_LAP_SINGLE_CLASS".into(),
            race_format: "NORMAL".into(),
            countdown_from: 5,
            start_finish_gate: true,
            uids_on_wire: true,
            imu: false,
            junk: false,
            abort_after_go_ms: None,
        }
    }
}

/// Server configuration. The defaults are the shipped game's.
#[derive(Debug, Clone)]
pub struct VdMockConfig {
    /// Bind address, e.g. `127.0.0.1` (tests) or `0.0.0.0` (bench). The real game binds
    /// its primary LAN IP — which is why `127.0.0.1` never reaches a real game; the mock
    /// defaults to loopback because tests are its first audience.
    pub bind: String,
    /// Port; 0 for ephemeral (tests). The game uses 60003.
    pub port: u16,
    /// The handshake service path segment. Shipped game: `velocidrone`.
    pub service: String,
    /// Sliding idle timeout (game default 40 s). Tests shrink this.
    pub idle_timeout: Duration,
    /// Whether the connected game instance is the multiplayer room **host**
    /// (`MULTI_PLAYER_HOST` + `IsMasterClient`): gates activate/lock/unlock/
    /// allspectate/getpilots and the start/abort handlers, exactly like the game.
    pub host: bool,
    /// Whether the game is in a multiplayer mode at all (gates the camera commands).
    pub multiplayer: bool,
    /// The room roster.
    pub pilots: Vec<MockPilot>,
    /// What to feed.
    pub feed: Feed,
    /// Start the feed as soon as the first client connects (replay scenarios); scripted
    /// scenarios usually wait for a `startrace` command instead.
    pub autostart: bool,
    /// Multiplies every scripted/replayed delay (0.01 = 100× faster; tests).
    pub time_scale: f64,
    /// Emit a `session` frame when the feed starts (as if the room was just created by
    /// this machine — the only time the real game sends one).
    pub session_on_start: bool,
}

impl Default for VdMockConfig {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1".into(),
            port: 0,
            service: "velocidrone".into(),
            idle_timeout: Duration::from_secs(40),
            host: true,
            multiplayer: true,
            pilots: Vec::new(),
            feed: Feed::Scripted(RacePlan::default()),
            autostart: false,
            time_scale: 1.0,
            session_on_start: false,
        }
    }
}

/// A command received from a client, as recorded for assertions.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedCommand {
    /// The lowercased command name (the game lowercases before dispatch).
    pub command: String,
    /// The raw frame text as received.
    pub raw: String,
    /// Whether the mock acted on it (false = silently dropped by the authorization
    /// gates, exactly like the game).
    pub authorized: bool,
}

// ---------------------------------------------------------------------------------------------
// The server
// ---------------------------------------------------------------------------------------------

/// A running mock server. Dropping it (or calling [`shutdown`](Self::shutdown)) stops it.
pub struct VdMock {
    inner: Arc<Inner>,
    port: u16,
    accept_thread: Option<thread::JoinHandle<()>>,
}

struct Inner {
    cfg: VdMockConfig,
    stop: AtomicBool,
    /// Monotonic connection ids; the *served* connection is the one with the highest id
    /// (SocketManager's `myConnection = connection` on every new connection).
    next_conn_id: AtomicU64,
    conns: Mutex<Vec<Conn>>,
    commands: Mutex<Vec<RecordedCommand>>,
    /// Flying state per roster index (activate/allspectate mutate it).
    flying: Mutex<Vec<bool>>,
    race: Mutex<RaceControl>,
}

struct Conn {
    id: u64,
    stream: TcpStream,
}

#[derive(Default)]
struct RaceControl {
    running: bool,
    abort: bool,
}

impl VdMock {
    /// Bind and start serving. Panics on bind failure (test/tool context).
    pub fn start(cfg: VdMockConfig) -> Self {
        let listener = TcpListener::bind((cfg.bind.as_str(), cfg.port)).expect("vd-mock: bind");
        let port = listener.local_addr().expect("vd-mock: local addr").port();
        listener
            .set_nonblocking(true)
            .expect("vd-mock: nonblocking listener");

        let flying = cfg.pilots.iter().map(|p| p.flying).collect();
        let inner = Arc::new(Inner {
            cfg,
            stop: AtomicBool::new(false),
            next_conn_id: AtomicU64::new(1),
            conns: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
            flying: Mutex::new(flying),
            race: Mutex::new(RaceControl::default()),
        });

        let accept_inner = inner.clone();
        let accept_thread = thread::spawn(move || {
            while !accept_inner.stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let conn_inner = accept_inner.clone();
                        thread::spawn(move || serve_connection(&conn_inner, stream));
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        });

        Self {
            inner,
            port,
            accept_thread: Some(accept_thread),
        }
    }

    /// The URL a client should dial.
    pub fn url(&self) -> String {
        format!(
            "ws://{}:{}/{}",
            self.inner.cfg.bind, self.port, self.inner.cfg.service
        )
    }

    /// The bound port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Everything received on the command channel so far.
    pub fn commands(&self) -> Vec<RecordedCommand> {
        self.inner.commands.lock().unwrap().clone()
    }

    /// The roster's current flying flags (post `activate`/`allspectate`), by roster order.
    pub fn flying(&self) -> Vec<bool> {
        self.inner.flying.lock().unwrap().clone()
    }

    /// Whether a scripted race is currently running.
    pub fn race_running(&self) -> bool {
        self.inner.race.lock().unwrap().running
    }

    /// Stop the server and drop all connections (raw TCP close — the game never sends
    /// close frames either).
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        for conn in self.inner.conns.lock().unwrap().drain(..) {
            let _ = conn.stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.accept_thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for VdMock {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Encode one server→client message the way the game does: **binary opcode (0x82), FIN,
/// unmasked**, standard length encoding.
fn encode_server_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x82);
    match payload.len() {
        0..=125 => out.push(payload.len() as u8),
        126..=65535 => {
            out.push(126);
            out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        }
        _ => {
            out.push(127);
            out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// `SocketManager.sendMessage`: deliver to the **newest** connection only. Every event
/// and every command reply goes through here, faithfully — even a reply to a command an
/// older connection sent lands on the newest one.
fn send_message(inner: &Inner, text: &str) {
    let frame = encode_server_frame(text.as_bytes());
    let conns = inner.conns.lock().unwrap();
    if let Some(conn) = conns.iter().max_by_key(|c| c.id) {
        let _ = (&conn.stream).write_all(&frame);
    }
}

/// Send raw bytes to one specific connection (frame-level replies: the empty-frame echo
/// and the `{0x8A,0x00}` ping answer are per-connection `WSClient` behavior, not
/// `SocketManager` behavior).
fn send_to(stream: &TcpStream, payload: &[u8]) {
    let _ = (&mut &*stream).write_all(&encode_server_frame(payload));
}

fn serve_connection(inner: &Arc<Inner>, mut stream: TcpStream) {
    stream
        .set_read_timeout(Some(Duration::from_millis(25)))
        .ok();

    // --- Handshake: request must arrive within 3 s (game's handshake timer). ---
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut buf: Vec<u8> = Vec::new();
    let header_end = loop {
        if Instant::now() > deadline || inner.stop.load(Ordering::SeqCst) {
            return; // abort — no response at all, like the game's timer path
        }
        let mut chunk = [0u8; 1024];
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if let Some(idx) = find_subslice(&buf, b"\r\n\r\n") {
                    break idx;
                }
            }
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    };

    let request = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    match validate_handshake(&request, &inner.cfg.service) {
        Ok(key) => {
            // The game's 101 (exact header set + order), terminated by the TCP wrapper's
            // \r\n\r\n — see the spec's transport section.
            let resp = format!(
                "HTTP/1.1 101 Switching Protocols\r\nServer: SocketsUnderControl.WSServer\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Accept: {}\r\n\r\n",
                accept_key(&key)
            );
            if stream.write_all(resp.as_bytes()).is_err() {
                return;
            }
        }
        Err(()) => {
            // The game's 400 + immediate abort.
            let _ = stream.write_all(
                b"HTTP/1.1 400 Bad Request\r\nServer: [eToile] SocketsUnderControl.WSServer\r\nSec-WebSocket-Version: 13\r\n\r\n",
            );
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
    }
    let mut leftovers = buf.split_off(header_end + 4);

    // Register: this connection becomes the served one (newest wins).
    let id = inner.next_conn_id.fetch_add(1, Ordering::SeqCst);
    {
        let clone = stream.try_clone().expect("vd-mock: clone stream");
        inner.conns.lock().unwrap().push(Conn { id, stream: clone });
    }

    // Autostart the feed on the first connection.
    if inner.cfg.autostart && id == 1 {
        start_feed(inner);
    }

    // --- Frame loop with the sliding idle timeout. ---
    let mut rx: VecDeque<u8> = VecDeque::new();
    rx.extend(leftovers.drain(..));
    let mut message: Vec<u8> = Vec::new();
    let mut last_activity = Instant::now();
    loop {
        if inner.stop.load(Ordering::SeqCst) {
            break;
        }
        if last_activity.elapsed() > inner.cfg.idle_timeout {
            break; // idle expiry: raw close, no close frame (game behavior)
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => rx.extend(&chunk[..n]),
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => break,
        }
        loop {
            match decode_client_frame(&mut rx) {
                FrameResult::NeedMore => break,
                FrameResult::Skipped => {}
                FrameResult::Unmasked => {
                    // The game errors and aborts on an unmasked client frame.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    remove_conn(inner, id);
                    return;
                }
                FrameResult::Close => {
                    // Raw teardown; the game never replies with a close frame.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    remove_conn(inner, id);
                    return;
                }
                FrameResult::Ping => {
                    last_activity = Instant::now();
                    // The game's answer: a 2-byte binary *message* {0x8A, 0x00}.
                    send_to(&stream, &[0x8A, 0x00]);
                }
                FrameResult::Pong => {
                    last_activity = Instant::now();
                }
                FrameResult::Data { fin, payload } => {
                    last_activity = Instant::now();
                    if payload.is_empty() {
                        // Empty-frame keep-alive: echoed, never dispatched.
                        send_to(&stream, &[]);
                        continue;
                    }
                    message.extend_from_slice(&payload);
                    if fin {
                        let text = String::from_utf8_lossy(&message).into_owned();
                        message.clear();
                        handle_command(inner, text.trim());
                    }
                }
            }
        }
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
    remove_conn(inner, id);
}

fn remove_conn(inner: &Inner, id: u64) {
    inner.conns.lock().unwrap().retain(|c| c.id != id);
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Validate a handshake request against the game's rules; returns the Sec-WebSocket-Key.
///
/// Faithful quirks: the GET target is trimmed of all leading/trailing `/` and compared
/// ordinally (case-sensitive) to the service; only the **first** space-separated token of
/// each header value is read; `Upgrade` must be exactly `websocket`; version exactly 13.
fn validate_handshake(request: &str, service: &str) -> Result<String, ()> {
    let mut path_ok = false;
    let mut upgrade_ok = false;
    let mut connection_ok = false;
    let mut version_ok = false;
    let mut key = String::new();
    for line in request.split(['\r', '\n']).filter(|l| !l.is_empty()) {
        let mut tokens = line.split(' ');
        match tokens.next() {
            Some("GET") => {
                let target = tokens.next().unwrap_or("");
                path_ok = target.trim_matches('/') == service;
            }
            Some("Connection:") => {
                // First token only, then split on ',' — `keep-alive, Upgrade` fails
                // because `Upgrade` sits in the *second* token (game parity).
                let first = tokens.next().unwrap_or("");
                connection_ok = first.split(',').any(|t| t == "Upgrade");
            }
            Some("Upgrade:") => upgrade_ok = tokens.next() == Some("websocket"),
            Some("Sec-WebSocket-Version:") => version_ok = tokens.next() == Some("13"),
            Some("Sec-WebSocket-Key:") => key = tokens.next().unwrap_or("").to_string(),
            _ => {}
        }
    }
    if path_ok && upgrade_ok && connection_ok && version_ok && !key.is_empty() {
        Ok(key)
    } else {
        Err(())
    }
}

enum FrameResult {
    NeedMore,
    /// A complete frame of an opcode the game gives no meaning to — consumed, ignored.
    Skipped,
    Unmasked,
    Close,
    Ping,
    Pong,
    Data {
        fin: bool,
        payload: Vec<u8>,
    },
}

/// Decode one client frame from the buffer, if complete.
fn decode_client_frame(rx: &mut VecDeque<u8>) -> FrameResult {
    if rx.len() < 2 {
        return FrameResult::NeedMore;
    }
    let b0 = *rx.front().unwrap();
    let b1 = rx[1];
    let fin = b0 & 0x80 != 0;
    let opcode = b0 & 0x0F;
    let masked = b1 & 0x80 != 0;
    let len7 = (b1 & 0x7F) as usize;
    let (len, header) = match len7 {
        126 => {
            if rx.len() < 4 {
                return FrameResult::NeedMore;
            }
            ((usize::from(rx[2]) << 8) | usize::from(rx[3]), 4)
        }
        127 => {
            if rx.len() < 10 {
                return FrameResult::NeedMore;
            }
            let mut n: u64 = 0;
            for b in rx.iter().take(10).skip(2) {
                n = (n << 8) | u64::from(*b);
            }
            (n as usize, 10)
        }
        n => (n, 2),
    };
    match opcode {
        0x0..=0x2 => {
            if !masked {
                return FrameResult::Unmasked; // game aborts here
            }
            let total = header + 4 + len;
            if rx.len() < total {
                return FrameResult::NeedMore;
            }
            let bytes: Vec<u8> = rx.drain(..total).collect();
            let mask = &bytes[header..header + 4];
            let payload = bytes[header + 4..]
                .iter()
                .enumerate()
                .map(|(i, b)| b ^ mask[i % 4])
                .collect();
            FrameResult::Data { fin, payload }
        }
        0x8 => FrameResult::Close,
        0x9 | 0xA => {
            // Consume the whole control frame (masked or not, payload included) — the
            // real server assumes zero payload and desyncs; we don't reproduce the
            // desync, we just answer like it would for the zero-payload case.
            let total = header + if masked { 4 } else { 0 } + len;
            if rx.len() < total {
                return FrameResult::NeedMore;
            }
            rx.drain(..total);
            if opcode == 0x9 {
                FrameResult::Ping
            } else {
                FrameResult::Pong
            }
        }
        _ => {
            // Unknown opcode: the game treats nothing else specially; drop the header
            // and payload if we can, else wait.
            let total = header + if masked { 4 } else { 0 } + len;
            if rx.len() < total {
                return FrameResult::NeedMore;
            }
            rx.drain(..total);
            FrameResult::Skipped
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Command dispatch — SocketManager.HandleCommand, with the exact two-level gating.
// ---------------------------------------------------------------------------------------------

fn handle_command(inner: &Arc<Inner>, text: &str) {
    let Some(json) = parse_json(text) else {
        // The game logs a parse error and drops the frame; it never replies.
        return;
    };
    let Some(cmd) = json.get("command").and_then(Json::as_str) else {
        return; // "Received JSON message without 'command' key" — logged, dropped.
    };
    let cmd = cmd.to_lowercase(); // the game lowercases before dispatch

    // The dispatcher's mode gates + the handlers' IsMasterClient gates, folded per the
    // spec's authorization matrix. `startrace`/`abortrace` have NO mode gate.
    let authorized = match cmd.as_str() {
        "ping" => true,
        "startrace" | "abortrace" => inner.cfg.host,
        "activate" | "lock" | "unlock" | "allspectate" | "getpilots" => inner.cfg.host,
        "cameraplayer" | "cameramode" | "cameraselect" | "camerareset" => inner.cfg.multiplayer,
        _ => false,
    };

    inner.commands.lock().unwrap().push(RecordedCommand {
        command: cmd.clone(),
        raw: text.to_string(),
        authorized,
    });
    if !authorized {
        return; // silent drop, like the game
    }

    match cmd.as_str() {
        "ping" => {}
        "startrace" => start_feed(inner),
        "abortrace" => {
            let mut race = inner.race.lock().unwrap();
            if race.running {
                race.abort = true;
            }
        }
        "activate" => {
            // Listed pilots -> flying, everyone else -> spectate; one ActivateError per
            // missing uid, in request order. Elements may be numbers or numeric strings.
            let mut requested: Vec<i64> = Vec::new();
            if let Some(Json::Arr(items)) = json.get("pilots") {
                for item in items {
                    match item {
                        Json::Num(n) => requested.push(*n as i64),
                        Json::Str(s) => {
                            if let Ok(n) = s.parse::<i64>() {
                                requested.push(n);
                            }
                        }
                        _ => {}
                    }
                }
            }
            {
                let mut flying = inner.flying.lock().unwrap();
                for (i, pilot) in inner.cfg.pilots.iter().enumerate() {
                    flying[i] = requested.contains(&pilot.uid);
                }
            }
            for uid in &requested {
                if !inner.cfg.pilots.iter().any(|p| p.uid == *uid) {
                    send_message(inner, &frames::activate_error(*uid));
                }
            }
        }
        "allspectate" => {
            let mut flying = inner.flying.lock().unwrap();
            for f in flying.iter_mut() {
                *f = false;
            }
        }
        "getpilots" => {
            let pilots: Vec<(&str, i64)> = inner
                .cfg
                .pilots
                .iter()
                .map(|p| (p.name.as_str(), p.uid))
                .collect();
            send_message(inner, &frames::pilotlist(&pilots));
        }
        // Recorded but with no observable protocol effect in the mock.
        "lock" | "unlock" | "cameraplayer" | "cameramode" | "cameraselect" | "camerareset" => {}
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// The feed
// ---------------------------------------------------------------------------------------------

/// Sleep `ms` scaled by the configured time scale.
fn scaled_sleep(inner: &Inner, ms: u64) {
    let scaled = (ms as f64 * inner.cfg.time_scale).max(0.0) as u64;
    thread::sleep(Duration::from_millis(scaled));
}

/// Kick off the configured feed on its own thread (no-op if one is already running).
fn start_feed(inner: &Arc<Inner>) {
    {
        let mut race = inner.race.lock().unwrap();
        if race.running {
            return;
        }
        race.running = true;
        race.abort = false;
    }
    let inner = inner.clone();
    thread::spawn(move || {
        match inner.cfg.feed.clone() {
            Feed::Replay(script) => run_replay(&inner, &script),
            Feed::Scripted(plan) => run_scripted(&inner, &plan),
        }
        inner.race.lock().unwrap().running = false;
    });
}

fn run_replay(inner: &Inner, script: &[(u64, String)]) {
    if inner.cfg.session_on_start {
        send_session(inner);
    }
    let mut last = 0u64;
    for (at_ms, frame) in script {
        if inner.stop.load(Ordering::SeqCst) || inner.race.lock().unwrap().abort {
            send_message(inner, &frames::racestatus("abort"));
            return;
        }
        scaled_sleep(inner, at_ms.saturating_sub(last));
        last = *at_ms;
        send_message(inner, frame);
    }
}

fn send_session(inner: &Inner) {
    send_message(
        inner,
        &frames::session(
            "GridDirector",
            "Grid Bench",
            "NEC Birmingham",
            "A Main",
            3,
            "Single Class",
            "Official 5 inch Race Quad",
            "5 inch",
        ),
    );
}

/// The scripted race in 1.17.13 shapes: start/racetype, countdown, FinishGate, a
/// racedata snapshot per crossing bucket (100 ms coalescing like the game's dirty-flag
/// coroutine), finished tail, race finished — or an abort partway.
fn run_scripted(inner: &Inner, plan: &RacePlan) {
    if inner.cfg.session_on_start {
        send_session(inner);
    }
    send_message(inner, &frames::racestatus("start"));
    send_message(
        inner,
        &frames::racetype(&plan.race_mode, &plan.race_format, plan.laps),
    );
    for v in (1..=plan.countdown_from).rev() {
        scaled_sleep(inner, 1000);
        send_message(inner, &frames::countdown(v));
    }
    scaled_sleep(inner, 1000);
    send_message(inner, &frames::countdown(0));
    send_message(inner, &frames::finish_gate(plan.start_finish_gate));

    // Active pilots: the roster's flying set at GO.
    let flying = inner.flying.lock().unwrap().clone();
    let active: Vec<&MockPilot> = inner
        .cfg
        .pilots
        .iter()
        .zip(&flying)
        .filter_map(|(p, f)| if *f { Some(p) } else { None })
        .collect();
    if active.is_empty() {
        send_message(inner, &frames::racestatus("race finished"));
        return;
    }

    // Build the crossing timeline: pilot i crosses gate g of lap l at
    // (l*gates + g) * gate_ms (+ tiny per-pilot phase so ties don't collapse).
    #[derive(Clone)]
    struct Crossing {
        at_ms: u64,
        pilot: usize,
        lap: u32,
        gate: u32,
        finished: bool,
    }
    let total_gates = plan.laps * plan.gates_per_lap;
    let mut crossings: Vec<Crossing> = Vec::new();
    for (i, pilot) in active.iter().enumerate() {
        for n in 1..=total_gates {
            let lap = (n - 1) / plan.gates_per_lap + 1;
            let gate = (n - 1) % plan.gates_per_lap + 1;
            crossings.push(Crossing {
                at_ms: u64::from(n) * pilot.gate_ms + (i as u64) * 37,
                pilot: i,
                lap,
                gate,
                finished: n == total_gates,
            });
        }
    }
    crossings.sort_by_key(|c| c.at_ms);

    // Latest state per pilot, updated as crossings replay.
    struct PilotState {
        lap: u32,
        gate: u32,
        time_s: f64,
        finished: bool,
        seen: bool,
    }
    let mut state: Vec<PilotState> = active
        .iter()
        .map(|_| PilotState {
            lap: 0,
            gate: 0,
            time_s: 0.0,
            finished: false,
            seen: false,
        })
        .collect();

    let mut elapsed = 0u64;
    let mut idx = 0usize;
    let mut imu_t = 0u64;
    let mut junk_sent = false;
    while idx < crossings.len() {
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        if inner.race.lock().unwrap().abort
            || plan.abort_after_go_ms.is_some_and(|cut| elapsed >= cut)
        {
            send_message(inner, &frames::racestatus("abort"));
            return;
        }

        // 100 ms buckets — the dirty-flag coroutine's cadence.
        let bucket_end = crossings[idx].at_ms.max(elapsed) / 100 * 100 + 100;
        // Optional IMU chatter up to the bucket boundary (60 Hz ≈ every 16 ms).
        if plan.imu {
            while imu_t + 16 <= bucket_end {
                imu_t += 16;
                send_message(
                    inner,
                    &frames::imu(
                        0.5,
                        -0.25,
                        0.0,
                        (1.0, 2.0, 3.0),
                        (0.0, 0.0, 0.0, 1.0),
                        (12.5, 0.0, 1.5),
                        imu_t as f64,
                    ),
                );
            }
        }
        scaled_sleep(inner, bucket_end - elapsed);
        elapsed = bucket_end;
        let mut changed = false;
        while idx < crossings.len() && crossings[idx].at_ms < bucket_end {
            let c = &crossings[idx];
            let s = &mut state[c.pilot];
            s.lap = c.lap;
            s.gate = c.gate;
            s.time_s = c.at_ms as f64 / 1000.0;
            s.finished = c.finished;
            s.seen = true;
            changed = true;
            idx += 1;
        }
        if !changed {
            continue;
        }

        // Rank exactly like the game: packed lap*1000+gate desc, then time asc.
        let mut order: Vec<usize> = (0..active.len()).filter(|&i| state[i].seen).collect();
        order.sort_by(|&a, &b| {
            let pa = state[a].lap * 1000 + state[a].gate;
            let pb = state[b].lap * 1000 + state[b].gate;
            pb.cmp(&pa).then(
                state[a]
                    .time_s
                    .partial_cmp(&state[b].time_s)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
        });
        let rows: Vec<frames::RaceRow<'_>> = order
            .iter()
            .enumerate()
            .map(|(pos, &i)| frames::RaceRow {
                name: &active[i].name,
                position: pos as u32 + 1,
                lap: state[i].lap,
                gate: state[i].gate,
                time_s: state[i].time_s,
                finished: state[i].finished,
                colour: &active[i].colour,
                uid: plan.uids_on_wire.then_some(active[i].uid),
            })
            .collect();
        send_message(inner, &frames::racedata(&rows));

        if plan.junk && !junk_sent {
            // The junk-tolerance set: an unknown one-key frame, and a malformed frame
            // with an unescaped-quote name (the serializer writes strings raw — see
            // spec). Sent once, right after the first real snapshot.
            send_message(inner, r#"{"somenewthing":{"x":"1"}}"#);
            send_message(inner, r#"{"racedata":{"Bro"ken":{"position":"1"}}}"#);
            junk_sent = true;
        }
    }
    send_message(inner, &frames::racestatus("race finished"));
}

// ---------------------------------------------------------------------------------------------
// Scenario library — the menu `cargo xtask vd-mock` serves and the tests replay.
// ---------------------------------------------------------------------------------------------

/// The vendored **real capture**: race 4 of dargust/VDSplitViewer's `messages.log`
/// (2025-01-23, MIT), 134 verbatim frames off a live pre-1.17 build — single player,
/// three laps, 43 gate crossings ending in the `finished:"True"` tail. Legacy shapes:
/// no `uid`, countdown from 3, no `FinishGate` frame.
pub fn capture_race4() -> Vec<(u64, String)> {
    parse_replay_tsv(include_str!("vd_fixtures/capture_2025-01-23_race4.tsv"))
}

/// Parse a replay fixture: one `<at_ms>\t<frame>` per line.
pub fn parse_replay_tsv(text: &str) -> Vec<(u64, String)> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (at, frame) = l
                .split_once('\t')
                .expect("replay fixture line is <at_ms>\\t<frame>");
            (
                at.parse().expect("replay fixture at_ms parses"),
                frame.to_string(),
            )
        })
        .collect()
}

/// The standard 4-pilot roster for scripted scenarios (paces differ so positions change
/// hands; uids are realistic Velocidrone account-id magnitudes).
pub fn bench_roster() -> Vec<MockPilot> {
    let mk = |name: &str, uid: i64, colour: &str, gate_ms: u64| MockPilot {
        name: name.into(),
        uid,
        colour: colour.into(),
        flying: true,
        gate_ms,
    };
    vec![
        mk("Ace", 41231, "FF0000", 900),
        mk("Bee", 52340, "00FFFF", 960),
        mk("Cyn", 63455, "00FF00", 1020),
        mk("Dex", 74569, "FFFF00", 1100),
    ]
}

/// One selectable scenario: a name, a blurb, and its server configuration.
pub struct VdScenario {
    pub name: &'static str,
    pub blurb: &'static str,
    pub build: fn() -> VdMockConfig,
}

/// The scenario menu (shared by `cargo xtask vd-mock` and the tests).
pub fn scenarios() -> Vec<VdScenario> {
    vec![
        VdScenario {
            name: "capture-race4",
            blurb: "replay the real 2025-01-23 capture verbatim (legacy shapes: no uid, SP countdown, autostarts on connect)",
            build: || VdMockConfig {
                feed: Feed::Replay(capture_race4()),
                autostart: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "heat",
            blurb: "4-pilot multiplayer heat in 1.17.13 shapes; command-driven: seat with activate, start with startrace",
            build: || VdMockConfig {
                pilots: bench_roster(),
                feed: Feed::Scripted(RacePlan::default()),
                session_on_start: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "sprint117",
            blurb: "single pilot, 1.17.13 shapes, autostarts on connect",
            build: || VdMockConfig {
                pilots: bench_roster().into_iter().take(1).collect(),
                feed: Feed::Scripted(RacePlan::default()),
                autostart: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "abort",
            blurb: "4-pilot heat aborted ~4 s after GO (racestatus:\"abort\")",
            build: || VdMockConfig {
                pilots: bench_roster(),
                feed: Feed::Scripted(RacePlan {
                    abort_after_go_ms: Some(4000),
                    ..RacePlan::default()
                }),
                autostart: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "legacy-no-uid",
            blurb: "scripted heat with pre-tournament wire shapes (no uid field in racedata)",
            build: || VdMockConfig {
                pilots: bench_roster(),
                feed: Feed::Scripted(RacePlan {
                    uids_on_wire: false,
                    countdown_from: 3,
                    ..RacePlan::default()
                }),
                autostart: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "imu",
            blurb: "sprint plus the 60 Hz all-numbers imu feed interleaved (1.17.x opt-in)",
            build: || VdMockConfig {
                pilots: bench_roster().into_iter().take(1).collect(),
                feed: Feed::Scripted(RacePlan {
                    imu: true,
                    ..RacePlan::default()
                }),
                autostart: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "junk",
            blurb: "heat with unknown frames, a malformed frame and an unescaped-quote name injected — consumers must survive",
            build: || VdMockConfig {
                pilots: bench_roster(),
                feed: Feed::Scripted(RacePlan {
                    junk: true,
                    ..RacePlan::default()
                }),
                autostart: true,
                ..VdMockConfig::default()
            },
        },
        VdScenario {
            name: "observer",
            blurb: "non-host room: every control/roster command is silently dropped (the host-gating trap)",
            build: || VdMockConfig {
                pilots: bench_roster(),
                feed: Feed::Scripted(RacePlan::default()),
                host: false,
                ..VdMockConfig::default()
            },
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// Tests — including a std-only WebSocket *client*, so the codec is exercised from the
// other side of the wire without any third-party dependency. These run under plain
// `cargo test --all`, which is the point: the mock's correctness is checked by the same
// job that runs everything else (no `live` feature, no Docker).
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal std-only WebSocket client for exercising the mock.
    struct TestClient {
        stream: TcpStream,
        rx: VecDeque<u8>,
    }

    impl TestClient {
        /// Connect + handshake; panics on failure (tests). `path` is the raw GET target.
        fn connect(port: u16, path: &str) -> Result<Self, String> {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("tcp connect");
            stream
                .set_read_timeout(Some(Duration::from_millis(50)))
                .unwrap();
            let req = format!(
                "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            );
            stream.write_all(req.as_bytes()).unwrap();
            let mut buf = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(2);
            while find_subslice(&buf, b"\r\n\r\n").is_none() {
                if Instant::now() > deadline {
                    return Err(format!(
                        "no complete handshake response: {:?}",
                        String::from_utf8_lossy(&buf)
                    ));
                }
                let mut chunk = [0u8; 1024];
                match stream.read(&mut chunk) {
                    Ok(0) => {
                        return Err(format!(
                            "closed during handshake: {:?}",
                            String::from_utf8_lossy(&buf)
                        ));
                    }
                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(e) => return Err(format!("handshake read error: {e}")),
                }
            }
            let text = String::from_utf8_lossy(&buf).into_owned();
            if !text.starts_with("HTTP/1.1 101") {
                return Err(text);
            }
            // RFC 6455's own sample key must produce its sample accept value.
            assert!(
                text.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="),
                "accept key mismatch in: {text}"
            );
            let header_end = find_subslice(&buf, b"\r\n\r\n").unwrap() + 4;
            let mut rx = VecDeque::new();
            rx.extend(buf.split_off(header_end));
            Ok(Self { stream, rx })
        }

        /// Send one masked frame with the given opcode.
        fn send_frame(&mut self, opcode: u8, payload: &[u8], masked: bool) {
            let mut out = vec![0x80 | opcode];
            let mask_bit = if masked { 0x80 } else { 0 };
            match payload.len() {
                0..=125 => out.push(mask_bit | payload.len() as u8),
                126..=65535 => {
                    out.push(mask_bit | 126);
                    out.extend_from_slice(&(payload.len() as u16).to_be_bytes());
                }
                _ => {
                    out.push(mask_bit | 127);
                    out.extend_from_slice(&(payload.len() as u64).to_be_bytes());
                }
            }
            if masked {
                let mask = [0x11, 0x22, 0x33, 0x44];
                out.extend_from_slice(&mask);
                out.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
            } else {
                out.extend_from_slice(payload);
            }
            self.stream.write_all(&out).unwrap();
        }

        fn send_text(&mut self, text: &str) {
            self.send_frame(0x1, text.as_bytes(), true);
        }

        /// Read the next complete message (opcode, payload), waiting up to `timeout`.
        fn recv(&mut self, timeout: Duration) -> Option<(u8, Vec<u8>)> {
            let deadline = Instant::now() + timeout;
            loop {
                // Try to decode a full frame from what we have.
                if self.rx.len() >= 2 {
                    let b0 = self.rx[0];
                    let b1 = self.rx[1];
                    let opcode = b0 & 0x0F;
                    assert_eq!(b1 & 0x80, 0, "server frames must be unmasked");
                    let len7 = (b1 & 0x7F) as usize;
                    let (len, header) = match len7 {
                        126 if self.rx.len() >= 4 => {
                            ((usize::from(self.rx[2]) << 8) | usize::from(self.rx[3]), 4)
                        }
                        127 if self.rx.len() >= 10 => {
                            let mut n = 0u64;
                            for i in 2..10 {
                                n = (n << 8) | u64::from(self.rx[i]);
                            }
                            (n as usize, 10)
                        }
                        n if n < 126 => (n, 2),
                        _ => (usize::MAX, 0), // extended header incomplete
                    };
                    if header > 0 && self.rx.len() >= header + len {
                        let bytes: Vec<u8> = self.rx.drain(..header + len).collect();
                        return Some((opcode, bytes[header..].to_vec()));
                    }
                }
                if Instant::now() > deadline {
                    return None;
                }
                let mut chunk = [0u8; 4096];
                match self.stream.read(&mut chunk) {
                    Ok(0) => return None,
                    Ok(n) => self.rx.extend(&chunk[..n]),
                    Err(ref e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) => {}
                    Err(_) => return None,
                }
            }
        }

        /// Collect message payloads as UTF-8 until `quiet` elapses with nothing new.
        fn drain_text(&mut self, quiet: Duration) -> Vec<String> {
            let mut out = Vec::new();
            while let Some((op, payload)) = self.recv(quiet) {
                assert_eq!(op, 0x2, "the game/mock sends BINARY frames only");
                out.push(String::from_utf8(payload).expect("utf-8 payload"));
            }
            out
        }
    }

    fn quick_cfg(feed: Feed, pilots: Vec<MockPilot>) -> VdMockConfig {
        VdMockConfig {
            time_scale: 0.01, // 100× faster than real time
            pilots,
            feed,
            ..VdMockConfig::default()
        }
    }

    // --- primitives ---

    #[test]
    fn sha1_matches_rfc3174_vectors() {
        let hex = |b: [u8; 20]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn accept_key_matches_rfc6455_sample() {
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn json_parser_handles_commands_and_game_tolerances() {
        let v = parse_json(r#"{"command":"activate","pilots":[123,"456",]}"#).unwrap();
        assert_eq!(v.get("command").unwrap().as_str(), Some("activate"));
        assert_eq!(
            v.get("pilots"),
            Some(&Json::Arr(vec![Json::Num(123.0), Json::Str("456".into())]))
        );
        assert!(
            parse_json(r#"{"a":1,}"#).is_some(),
            "trailing comma accepted (game parity)"
        );
        assert!(parse_json("not json").is_none());
    }

    // --- wire-exact frames (these strings ARE the documented wire format) ---

    #[test]
    fn frames_are_wire_exact() {
        assert_eq!(
            frames::racestatus("start"),
            r#"{"racestatus":{"raceAction":"start"}}"#
        );
        assert_eq!(frames::countdown(3), r#"{"countdown":{"countValue":"3"}}"#);
        assert_eq!(
            frames::finish_gate(true),
            r#"{"FinishGate":{"StartFinishGate":"True"}}"#
        );
        assert_eq!(
            frames::racetype("THREE_LAP_SINGLE_CLASS", "NORMAL", 3),
            r#"{"racetype":{"raceMode":"THREE_LAP_SINGLE_CLASS","raceFormat":"NORMAL","raceLaps":"3"}}"#
        );
        assert_eq!(
            frames::spectator_change("Dacus"),
            r#"{"spectatorChange":"Dacus"}"#
        );
        assert_eq!(
            frames::pilotlist(&[("Ace", 41231)]),
            r#"{"pilotlist":[{"name":"Ace","uid":"41231"}]}"#
        );
        assert_eq!(
            frames::activate_error(99999),
            r#"{"ActivateError":{"UIDNotFound":"99999"}}"#
        );
        // racedata: string scalars, F3 time, "False", no '#' colour — and uid as a bare
        // JSON number (the 1.17.13 shape).
        let row = frames::RaceRow {
            name: "Dacus",
            position: 1,
            lap: 1,
            gate: 2,
            time_s: 2.152,
            finished: false,
            colour: "00FFFF",
            uid: Some(12345),
        };
        assert_eq!(
            frames::racedata(&[row]),
            r#"{"racedata":{"Dacus":{"position":"1","lap":"1","gate":"2","time":"2.152","finished":"False","colour":"00FFFF","uid":12345}}}"#
        );
        // Legacy shape: no uid key at all.
        let legacy = frames::RaceRow {
            name: "Dacus",
            position: 1,
            lap: 1,
            gate: 1,
            time_s: 1.369,
            finished: false,
            colour: "00FFFF",
            uid: None,
        };
        assert_eq!(
            frames::racedata(&[legacy]),
            r#"{"racedata":{"Dacus":{"position":"1","lap":"1","gate":"1","time":"1.369","finished":"False","colour":"00FFFF"}}}"#
        );
    }

    #[test]
    fn capture_fixture_loads_and_matches_the_builders() {
        let frames_list = capture_race4();
        assert_eq!(frames_list.len(), 134);
        assert_eq!(frames_list[0].1, frames::racestatus("start"));
        assert_eq!(
            frames_list[1].1,
            frames::racetype("THREE_LAP_SINGLE_CLASS", "NORMAL", 3)
        );
        assert_eq!(frames_list[2].1, frames::countdown(3));
        assert_eq!(
            frames_list.last().unwrap().1,
            frames::racestatus("race finished")
        );
        // The finish tail: the last racedata crossing carries finished:"True" at gate 43.
        let last_data = frames_list
            .iter()
            .rev()
            .find(|(_, f)| f.starts_with(r#"{"racedata""#))
            .unwrap();
        assert!(last_data.1.contains(r#""gate":"43""#));
        assert!(last_data.1.contains(r#""finished":"True""#));
    }

    // --- handshake behavior ---

    #[test]
    fn handshake_accepts_slash_variants_and_rejects_wrong_path() {
        let mock = VdMock::start(quick_cfg(Feed::Scripted(RacePlan::default()), vec![]));
        for path in ["/velocidrone", "/velocidrone/", "//velocidrone//"] {
            TestClient::connect(mock.port(), path)
                .unwrap_or_else(|e| panic!("{path} should handshake: {e}"));
        }
        for path in ["/ws/", "/Velocidrone/", "/velocidrone?x=1"] {
            let err = TestClient::connect(mock.port(), path)
                .err()
                .unwrap_or_else(|| panic!("{path} should be rejected"));
            assert!(err.contains("400"), "{path}: {err}");
        }
        mock.shutdown();
    }

    #[test]
    fn unmasked_data_frame_aborts_the_connection() {
        let mock = VdMock::start(quick_cfg(Feed::Scripted(RacePlan::default()), vec![]));
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        client.send_frame(0x1, br#"{"command":"ping"}"#, false);
        // The mock closes without any close frame (raw teardown, like the game).
        let got = client.recv(Duration::from_millis(300));
        assert!(got.is_none(), "expected raw close, got {got:?}");
        mock.shutdown();
    }

    #[test]
    fn empty_frame_is_echoed_and_rfc_ping_gets_the_quirk_pong() {
        let mock = VdMock::start(quick_cfg(Feed::Scripted(RacePlan::default()), vec![]));
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        client.send_frame(0x2, &[], true);
        let (op, payload) = client.recv(Duration::from_secs(1)).expect("echo");
        assert_eq!((op, payload.as_slice()), (0x2, &[][..]));
        client.send_frame(0x9, &[], true);
        let (op, payload) = client.recv(Duration::from_secs(1)).expect("quirk pong");
        assert_eq!((op, payload.as_slice()), (0x2, &[0x8Au8, 0x00][..]));
        mock.shutdown();
    }

    // --- SocketManager semantics ---

    #[test]
    fn newest_connection_wins_the_feed() {
        let mock = VdMock::start(VdMockConfig {
            pilots: bench_roster(),
            feed: Feed::Scripted(RacePlan::default()),
            time_scale: 0.01,
            ..VdMockConfig::default()
        });
        let mut old = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        let mut new = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        // The OLD connection sends getpilots — but the reply lands on the NEWEST
        // connection (all sends go through SocketManager's single myConnection).
        old.send_text(r#"{"command":"getpilots"}"#);
        let reply = new.recv(Duration::from_secs(2)).expect("reply on newest");
        let text = String::from_utf8(reply.1).unwrap();
        assert!(text.starts_with(r#"{"pilotlist""#), "{text}");
        assert!(
            old.recv(Duration::from_millis(200)).is_none(),
            "old conn must starve"
        );
        mock.shutdown();
    }

    #[test]
    fn activate_reseats_and_reports_each_missing_uid_in_order() {
        let mock = VdMock::start(quick_cfg(
            Feed::Scripted(RacePlan::default()),
            bench_roster(),
        ));
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        // Numbers and numeric strings both work; 2 unknown uids -> 2 errors, in order.
        client.send_text(r#"{"command":"activate","pilots":[41231,"52340",99999,88888]}"#);
        let f1 = client
            .recv(Duration::from_secs(2))
            .expect("first ActivateError");
        let f2 = client
            .recv(Duration::from_secs(2))
            .expect("second ActivateError");
        assert_eq!(
            String::from_utf8(f1.1).unwrap(),
            frames::activate_error(99999)
        );
        assert_eq!(
            String::from_utf8(f2.1).unwrap(),
            frames::activate_error(88888)
        );
        assert_eq!(mock.flying(), vec![true, true, false, false]);
        mock.shutdown();
    }

    #[test]
    fn non_host_commands_are_silently_dropped_but_recorded() {
        let mock = VdMock::start(VdMockConfig {
            host: false,
            pilots: bench_roster(),
            feed: Feed::Scripted(RacePlan::default()),
            time_scale: 0.01,
            ..VdMockConfig::default()
        });
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        client.send_text(r#"{"command":"startrace"}"#);
        client.send_text(r#"{"command":"getpilots"}"#);
        client.send_text(r#"{"command":"ping"}"#);
        assert!(
            client.recv(Duration::from_millis(400)).is_none(),
            "silence expected"
        );
        assert!(!mock.race_running());
        let commands = mock.commands();
        assert_eq!(
            commands
                .iter()
                .map(|c| (c.command.as_str(), c.authorized))
                .collect::<Vec<_>>(),
            vec![("startrace", false), ("getpilots", false), ("ping", true)]
        );
        mock.shutdown();
    }

    // --- feeds ---

    #[test]
    fn replay_feed_delivers_the_capture_verbatim_as_binary_frames() {
        let mock = VdMock::start(VdMockConfig {
            feed: Feed::Replay(capture_race4()),
            autostart: true,
            time_scale: 0.0, // replay as fast as possible
            ..VdMockConfig::default()
        });
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        let got = client.drain_text(Duration::from_millis(600));
        let want: Vec<String> = capture_race4().into_iter().map(|(_, f)| f).collect();
        assert_eq!(got, want);
        mock.shutdown();
    }

    #[test]
    fn scripted_race_runs_the_full_lifecycle_in_game_order() {
        let mock = VdMock::start(quick_cfg(
            Feed::Scripted(RacePlan {
                gates_per_lap: 3,
                ..RacePlan::default()
            }),
            bench_roster(),
        ));
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        client.send_text(r#"{"command":"startrace"}"#);
        let got = client.drain_text(Duration::from_millis(700));
        assert_eq!(got[0], frames::racestatus("start"));
        assert_eq!(
            got[1],
            frames::racetype("THREE_LAP_SINGLE_CLASS", "NORMAL", 3)
        );
        // Multiplayer countdown 5..1, then 0 followed immediately by FinishGate.
        for (i, v) in (1..=5).rev().enumerate() {
            assert_eq!(got[2 + i], frames::countdown(v));
        }
        assert_eq!(got[7], frames::countdown(0));
        assert_eq!(got[8], frames::finish_gate(true));
        assert_eq!(got.last().unwrap(), &frames::racestatus("race finished"));
        // racedata snapshots only ever contain pilots who have crossed at least once —
        // a pilot's first appearance IS the holeshot (game behavior). By the final
        // snapshot the whole field has appeared, and uid rides as a bare number.
        let data: Vec<&String> = got
            .iter()
            .filter(|f| f.starts_with(r#"{"racedata""#))
            .collect();
        assert!(!data.is_empty());
        assert!(data[0].contains(r#""uid":41231"#), "{}", data[0]);
        let last = data.last().unwrap();
        for pilot in ["Ace", "Bee", "Cyn", "Dex"] {
            assert!(last.contains(&format!(r#""{pilot}":"#)), "{last}");
        }
        // The finish tail: the last snapshot carries finished:"True" for the leader.
        assert!(data.last().unwrap().contains(r#""finished":"True""#));
        mock.shutdown();
    }

    #[test]
    fn abortrace_stops_the_race_with_the_abort_action() {
        let mock = VdMock::start(quick_cfg(
            Feed::Scripted(RacePlan::default()),
            bench_roster(),
        ));
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        client.send_text(r#"{"command":"startrace"}"#);
        // Let the countdown pass, then abort mid-race.
        thread::sleep(Duration::from_millis(150));
        client.send_text(r#"{"command":"abortrace"}"#);
        let got = client.drain_text(Duration::from_millis(500));
        assert_eq!(got.last().unwrap(), &frames::racestatus("abort"));
        assert!(
            !got.iter()
                .any(|f| f == &frames::racestatus("race finished"))
        );
        mock.shutdown();
    }

    #[test]
    fn idle_timeout_closes_the_connection_without_a_close_frame() {
        let mock = VdMock::start(VdMockConfig {
            idle_timeout: Duration::from_millis(200),
            feed: Feed::Scripted(RacePlan::default()),
            ..VdMockConfig::default()
        });
        let mut client = TestClient::connect(mock.port(), "/velocidrone/").unwrap();
        // Stay silent past the timeout: the mock must drop us raw.
        let got = client.recv(Duration::from_secs(2));
        assert!(got.is_none(), "expected raw close, got {got:?}");
        mock.shutdown();
    }
}
