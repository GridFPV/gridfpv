//! Live Velocidrone WebSocket transport (feature `live`).
//!
//! Connects to a running Velocidrone build's WebSocket feed
//! (`ws://<lan-ip>:60003/velocidrone`), decodes each frame into the adapter's [`Raw`]
//! message, runs it through [`VelocidroneAdapter`], accumulates the canonical [`Event`]s,
//! and carries the **outbound command surface** the Director needs to seat and run a heat.
//! All wire-format knowledge stays in `Raw`/`translate`; this file moves bytes and
//! proves writes landed.
//!
//! # What the 1.17.13 decompile changed here (#484, #494)
//!
//! The authority is the RE workspace's `velocidrone-websocket/docs/ws-spec.md` (read off
//! `Assembly-CSharp.dll` 1.17.13). Four of its findings are load-bearing in this file, and
//! two of them were live bugs:
//!
//! 1. **Every server→client frame is a BINARY frame (opcode 0x2)** whose payload is UTF-8
//!    JSON — the game never sends text frames. This reader used to match `Message::Text`
//!    and discard `Message::Binary`, so against a real game it connected cleanly and then
//!    dropped **100% of the feed**, silently. [`decode_payload`] now takes either.
//! 2. **The keep-alive is `{"command":"ping"}`.** We used to send an empty text frame:
//!    tolerated (echoed at frame level) but not the documented form. Any *non-empty,
//!    non-command* frame — the literal `"heartbeat"` some community tools send — reaches
//!    the game's JSON layer, fails to parse, and is logged every time; that is the origin
//!    of the "websocket stutters the sim" reports.
//! 3. **The server never sends a close frame** in either direction — it tears the TCP
//!    connection down with linger 0 (≈ RST). Never wait for a close reply; a connection
//!    reset on the way out is normal shutdown, not an error worth shouting about.
//! 4. **An RFC ping (opcode 0x9) is answered with a normal binary *message* carrying the
//!    two bytes `{0x8A, 0x00}`**, not a pong — and the game's ping handler assumes a
//!    zero-length ping, so *a ping carrying a payload desynchronizes its frame parser*.
//!    We never send RFC pings, and we filter that exact byte pair on read
//!    ([`is_stack_ping_answer`]).
//!
//! One more piece of context that shapes the design: **`SocketManager` serves exactly one
//! client — the newest.** An older connection stays open at the socket level and silently
//! stops receiving events. So "connected but silent" is a real, expected state that means
//! *someone else connected to this sim*, and the Director must be able to say so rather
//! than show a healthy-looking dead feed.
//!
//! # Threading model
//!
//! [`tungstenite`] is a synchronous (blocking) WebSocket and a single reader thread owns
//! the socket — but the Director also has to *write* to it (seating a heat, starting a
//! race) from its own threads. Rather than share the socket behind a lock the reader would
//! hold across a blocking read, commands go through an [`mpsc`] queue and **the reader
//! thread is the only writer**:
//!
//! - the socket carries a short [`POLL_INTERVAL`] read timeout, so every read either
//!   yields a frame or wakes up;
//! - each wake-up drains the command queue, sends the keep-alive if
//!   [`KEEP_ALIVE_INTERVAL`] has elapsed, and re-checks the stop flag.
//!
//! So a queued command reaches the wire within `POLL_INTERVAL` (250 ms) rather than
//! waiting out the 5 s ping cadence, with no shared socket lock and no second writer.
//! Translated events land in an `Arc<Mutex<Vec<Event>>>` drained by
//! [`VelocidroneConnection::events`], the same shape as
//! [`RotorHazardConnection`](crate::rotorhazard::transport::RotorHazardConnection);
//! readback state lands in a parallel [`Readback`].

// `tungstenite::Error` is a sizeable external enum; we thread it through unchanged
// rather than box every signature in this thin wrapper (mirrors the RH transport).
#![allow(clippy::result_large_err)]

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message, WebSocket};

use super::{PilotEntry, Raw, VelocidroneAdapter};
use crate::Adapter;
use gridfpv_events::Event;

/// How often the reader wakes to drain the command queue and re-check the stop flag.
///
/// Short on purpose: it bounds how long a queued command (seating a heat, starting a
/// race) waits before it reaches the wire, and how quickly `disconnect()` is noticed.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// How often the keep-alive command is sent.
///
/// The game enforces a **sliding 40 s idle timeout** that any inbound frame re-arms, and
/// the vendor's own changelog recommends 5 s. Well inside the timeout, and cheap.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// How long [`VelocidroneConnection::seat`] waits for `ActivateError` frames before
/// calling the seating good.
///
/// There is no positive ack for `activate`: the readback is the **absence** of an
/// `ActivateError` per requested uid, so this is how long we listen for a complaint. The
/// game emits them synchronously in its command handler, so this is generous.
const SEAT_READBACK_WINDOW: Duration = Duration::from_millis(1_500);

/// The stack's answer to an RFC ping: a two-byte binary **message**, not a pong.
///
/// We never send RFC pings, but a peer or proxy might, and this pair must never reach the
/// JSON decoder as a malformed frame.
const STACK_PING_ANSWER: [u8; 2] = [0x8A, 0x00];

/// One command the Director can send to Velocidrone.
///
/// Wire form is `{"command":"<name>", …}`. The game lowercases the command value before
/// matching but does **not** normalise the additional field keys, so `pilots` is spelled
/// exactly as the binary reads it. **Failed authorization is silent** — an unauthorized
/// command is dropped with no error frame — which is why every command that matters has a
/// readback (see [`VelocidroneConnection::seat`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VdCommand {
    /// Keep-alive no-op. Accepted from any client, no gate.
    Ping,
    /// Start the race. Requires being the Photon master client (no game-mode gate, so it
    /// works in single player too).
    StartRace,
    /// Abort the race. Master client only.
    AbortRace,
    /// Everyone to spectator. Multiplayer host only. Sent immediately before
    /// [`Activate`](VdCommand::Activate) so the seated set is exactly the heat.
    AllSpectate,
    /// Seat exactly these account uids: listed pilots fly, everyone else spectates.
    /// Multiplayer host only. Readback: one `ActivateError` per uid not in the room.
    Activate(Vec<String>),
    /// Request the room roster; the game replies with a `pilotlist` frame. Multiplayer
    /// host only.
    GetPilots,
    /// Close the Photon room to new joins — the sim's "the field is set". Host only.
    Lock,
    /// Re-open the room to joins. Host only.
    Unlock,
}

impl VdCommand {
    /// The exact JSON this command is sent as.
    ///
    /// `activate`'s uids are emitted as **numbers** where they parse as such (the game
    /// accepts either, but numbers are the shape its own code comment shows), and any
    /// non-numeric uid is dropped rather than guessed — a uid we cannot state confidently
    /// would seat the wrong pilot, and the CLAUDE.md rule is to omit, never guess. The
    /// dropped uid still shows up as a seating failure, because it can never be confirmed.
    pub fn to_json(&self) -> String {
        match self {
            VdCommand::Ping => r#"{"command":"ping"}"#.to_string(),
            VdCommand::StartRace => r#"{"command":"startrace"}"#.to_string(),
            VdCommand::AbortRace => r#"{"command":"abortrace"}"#.to_string(),
            VdCommand::AllSpectate => r#"{"command":"allspectate"}"#.to_string(),
            VdCommand::GetPilots => r#"{"command":"getpilots"}"#.to_string(),
            VdCommand::Lock => r#"{"command":"lock"}"#.to_string(),
            VdCommand::Unlock => r#"{"command":"unlock"}"#.to_string(),
            VdCommand::Activate(uids) => {
                let list = uids
                    .iter()
                    .filter_map(|u| u.trim().parse::<i64>().ok())
                    .map(|u| u.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                format!(r#"{{"command":"activate","pilots":[{list}]}}"#)
            }
        }
    }
}

/// What the connection has heard back from the sim, as distinct from the canonical event
/// stream.
///
/// These are the **readbacks**: the sim acks nothing, so proving a write landed means
/// re-reading what it says about itself. Snapshot it with
/// [`VelocidroneConnection::readback`].
#[derive(Debug, Clone, Default)]
pub struct Readback {
    /// The most recent `racestatus.raceAction` literal seen (`start` / `abort` /
    /// `race finished`) — the readback for `startrace` and `abortrace`.
    pub last_race_action: Option<String>,
    /// The most recent countdown value; also evidence a `startrace` took effect.
    pub last_countdown: Option<u32>,
    /// The lap count the sim says it will run, from `racetype.raceLaps`.
    pub race_laps: Option<u32>,
    /// Uids the sim reported it could not seat, one entry per `ActivateError` frame.
    /// Cleared at the start of each [`VelocidroneConnection::seat`].
    pub activate_errors: Vec<String>,
    /// The most recent `pilotlist` reply — the positive confirm for seating, and the only
    /// frame pairing a player **name** with an account **uid**.
    pub roster: Option<Vec<PilotEntry>>,
    /// Whether the sim last reported us as race manager (`player.raceManager`). `None`
    /// when no `player` frame has arrived. The host-gated commands need this true.
    pub race_manager: Option<bool>,
    /// Frames that arrived but could not be decoded. Non-zero means schema drift — a
    /// Velocidrone build newer than this adapter — and it must never be silent.
    pub malformed_frames: u64,
    /// When the last frame of any kind arrived. `None` before the first frame. A live
    /// socket that has gone quiet is the signature of another client taking the feed
    /// over (the sim serves only its newest connection).
    pub last_frame_at: Option<Instant>,
}

/// The outcome of a seating write.
///
/// `activate` has no positive ack, so this is assembled from the absence of complaints
/// plus the roster: `not_found` is what the sim said it could not seat, and `roster` is
/// the `getpilots` cross-check pairing names to uids.
#[derive(Debug, Clone, Default)]
pub struct SeatOutcome {
    /// The uids the sim raised no complaint about.
    pub seated: Vec<String>,
    /// The uids the sim reported as not present in the room (`ActivateError`).
    pub not_found: Vec<String>,
    /// Uids we refused to send because they are not numeric — omitted, never guessed.
    pub unusable: Vec<String>,
    /// The room roster at the time of seating, when the sim answered `getpilots`.
    pub roster: Option<Vec<PilotEntry>>,
}

impl SeatOutcome {
    /// Whether every requested uid was seated without complaint.
    pub fn is_clean(&self) -> bool {
        self.not_found.is_empty() && self.unusable.is_empty()
    }
}

/// A live connection to a Velocidrone WebSocket feed: translates its frame stream into
/// canonical [`Event`]s and carries the command surface back the other way.
///
/// Drain events with [`events`](Self::events), read what the sim said about itself with
/// [`readback`](Self::readback), and end it with [`disconnect`](Self::disconnect).
pub struct VelocidroneConnection {
    /// Translated events accumulated by the reader thread, drained by `events()`.
    events: Arc<Mutex<Vec<Event>>>,
    /// What the sim has told us about itself — the write readbacks.
    readback: Arc<Mutex<Readback>>,
    /// Queued commands; the reader thread is the only writer to the socket.
    commands: Sender<VdCommand>,
    /// Frames seen, so a caller can tell "no new frames" from "no new events".
    frames_seen: Arc<AtomicU64>,
    /// Flipped by `disconnect()` to ask the reader thread to stop.
    stop: Arc<AtomicBool>,
    /// Cleared by the reader when it exits, so `is_connected` is honest.
    alive: Arc<AtomicBool>,
    /// The reader thread handle, joined on `disconnect()`.
    reader: Option<JoinHandle<()>>,
}

impl VelocidroneConnection {
    /// Connect to `url` (e.g. `ws://192.168.1.20:60003/velocidrone`) and start translating
    /// the stream through a fresh [`VelocidroneAdapter`] (the conventional `"velocidrone"`
    /// id).
    ///
    /// **The host must be the machine's LAN IP.** Velocidrone's TCP layer binds to the
    /// primary LAN IPv4 address it discovers, not `IPAddress.Any` and not loopback, so
    /// `ws://127.0.0.1:60003` cannot connect however local the sim is.
    pub fn connect(url: &str) -> Result<Self, Error> {
        Self::connect_with(url, VelocidroneAdapter::with_default_id())
    }

    /// Connect to `url` and translate the stream through a caller-supplied `adapter` (e.g.
    /// one with a non-default [`AdapterId`](gridfpv_events::AdapterId)).
    pub fn connect_with(url: &str, adapter: VelocidroneAdapter) -> Result<Self, Error> {
        // Resolve the handshake synchronously so `connect()` fails fast on a bad URL or an
        // unreachable sim, exactly like the RH transport's `connect`. The game's handshake
        // parser is strict — the path must match `velocidrone` after stripping slashes,
        // and a query string breaks it — but those are the caller's URL to get right.
        let (mut socket, _response) = tungstenite::connect(url)?;

        // A short read timeout turns each blocking read into a "frame or wake up" call:
        // waking is how commands get sent and how the stop flag is noticed.
        set_read_timeout(&socket, Some(POLL_INTERVAL))?;

        let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let readback = Arc::new(Mutex::new(Readback::default()));
        let frames_seen = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let (commands, rx) = channel();

        let reader = {
            let ctx = ReaderCtx {
                events: events.clone(),
                readback: readback.clone(),
                frames_seen: frames_seen.clone(),
                stop: stop.clone(),
                alive: alive.clone(),
            };
            std::thread::spawn(move || run_reader(&mut socket, adapter, &ctx, &rx))
        };

        Ok(Self {
            events,
            readback,
            commands,
            frames_seen,
            stop,
            alive,
            reader: Some(reader),
        })
    }

    /// Take everything translated since the last call.
    pub fn events(&self) -> Vec<Event> {
        let mut guard = self.events.lock().unwrap();
        std::mem::take(&mut *guard)
    }

    /// A snapshot of what the sim has said about itself.
    pub fn readback(&self) -> Readback {
        self.readback.lock().unwrap().clone()
    }

    /// How many frames of any kind have arrived. Distinguishes "the sim is quiet" from
    /// "the sim is talking but nothing is happening" — and a live socket whose count has
    /// stopped moving is the signature of another client having taken the feed.
    pub fn frames_seen(&self) -> u64 {
        self.frames_seen.load(Ordering::SeqCst)
    }

    /// Whether the reader thread is still running. `false` means the sim closed the
    /// socket (it does so by tearing down the TCP connection, never with a close frame).
    pub fn is_connected(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Queue a command. Returns `false` only if the reader thread is gone.
    ///
    /// The command reaches the wire on the reader's next wake-up (within
    /// [`POLL_INTERVAL`]). **Landing on the wire is not the same as being honoured** —
    /// the sim drops unauthorized commands silently — so anything that matters needs a
    /// readback.
    pub fn send(&self, command: VdCommand) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Seat exactly `uids`: everyone to spectator, then activate the heat, then read back.
    ///
    /// This is the write that puts the right pilots in the air, so it follows the
    /// foreign-system rule in full. `activate` has **no positive ack** and an
    /// unauthorized command is dropped in silence, so the readback is assembled from what
    /// the sim volunteers:
    ///
    /// - one `ActivateError` per uid not in the room, collected over
    ///   [`SEAT_READBACK_WINDOW`] — the complaint channel;
    /// - a `getpilots` → `pilotlist` reply — the positive confirm, and the only frame that
    ///   pairs a player **name** with an account **uid**, which is what lets the caller
    ///   check that the pilot it seated is the player whose laps it will count.
    ///
    /// A non-numeric uid is reported in [`SeatOutcome::unusable`] rather than sent: the
    /// game parses uids with `int.TryParse` and silently skips what fails, and a guessed
    /// uid seats the wrong pilot.
    pub fn seat(&self, uids: &[String]) -> SeatOutcome {
        let (usable, unusable): (Vec<String>, Vec<String>) = uids
            .iter()
            .cloned()
            .partition(|u| u.trim().parse::<i64>().is_ok());

        // Clear the complaint channel so this seating's errors cannot be confused with a
        // previous one's, then write: spectate everyone, seat the heat, ask for the roster.
        {
            let mut rb = self.readback.lock().unwrap();
            rb.activate_errors.clear();
            rb.roster = None;
        }
        self.send(VdCommand::AllSpectate);
        self.send(VdCommand::Activate(usable.clone()));
        self.send(VdCommand::GetPilots);

        // Listen for complaints. There is nothing to wait *for* on the happy path — the
        // signal is silence — so this window is spent in full unless the roster arrives
        // and something has already gone wrong.
        std::thread::sleep(SEAT_READBACK_WINDOW);

        let rb = self.readback.lock().unwrap().clone();
        let not_found: Vec<String> = rb.activate_errors.clone();
        let seated = usable
            .into_iter()
            .filter(|u| !not_found.contains(u))
            .collect();

        SeatOutcome {
            seated,
            not_found,
            unusable,
            roster: rb.roster,
        }
    }

    /// Stop the reader thread and disconnect.
    ///
    /// Bounded by [`POLL_INTERVAL`] — the longest the reader can be blocked in a read
    /// before it wakes and observes the stop flag.
    pub fn disconnect(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for VelocidroneConnection {
    fn drop(&mut self) {
        // If `disconnect()` wasn't called, still ask the reader to stop and reap it so a
        // dropped connection never leaks the thread.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// The shared handles the reader thread writes through.
struct ReaderCtx {
    events: Arc<Mutex<Vec<Event>>>,
    readback: Arc<Mutex<Readback>>,
    frames_seen: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
}

/// The reader loop: read frames, decode + translate them, drain queued commands, send the
/// keep-alive on schedule, and exit on close / stop / a real I/O error.
fn run_reader(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    mut adapter: VelocidroneAdapter,
    ctx: &ReaderCtx,
    commands: &Receiver<VdCommand>,
) {
    let mut last_ping = Instant::now();
    while !ctx.stop.load(Ordering::SeqCst) {
        match socket.read() {
            // The game sends BINARY frames carrying UTF-8 JSON; a text frame would come
            // from a mock or a proxy. Both are the same payload to us.
            Ok(Message::Text(text)) => {
                handle_payload(text.as_bytes(), &mut adapter, ctx);
            }
            Ok(Message::Binary(bytes)) => {
                handle_payload(&bytes, &mut adapter, ctx);
            }
            // We never send RFC pings (a payload-carrying one desyncs the game's parser),
            // but tungstenite answers an inbound ping itself; nothing to do here.
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) | Ok(Message::Frame(_)) => {}
            // Server-initiated close: the real game never sends one, but a mock or proxy
            // may. Acknowledge by leaving the loop.
            Ok(Message::Close(_)) => break,
            // A read timeout (our poll cadence) surfaces as an I/O WouldBlock / TimedOut:
            // this is the wake-up, not a failure.
            Err(Error::Io(io)) if is_timeout(&io) => {}
            // The peer is gone or the protocol broke: stop reading. The game tears the TCP
            // connection down with linger 0 rather than sending a close frame, so a reset
            // here is ordinary shutdown.
            Err(_) => break,
        }

        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }

        // The reader is the only writer: drain whatever the Director queued, then keep the
        // connection alive. A send failure means the socket is gone.
        if !drain_commands(socket, commands) {
            break;
        }
        if last_ping.elapsed() >= KEEP_ALIVE_INTERVAL {
            last_ping = Instant::now();
            if socket
                .send(Message::text(VdCommand::Ping.to_json()))
                .is_err()
            {
                break;
            }
        }
    }
    ctx.alive.store(false, Ordering::SeqCst);
    // Best-effort close; the peer is very likely already gone, and never answers anyway.
    let _ = socket.close(None);
    let _ = socket.flush();
}

/// Send every queued command. Returns `false` when the socket has failed.
fn drain_commands(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    commands: &Receiver<VdCommand>,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(command) => {
                if socket.send(Message::text(command.to_json())).is_err() {
                    return false;
                }
            }
            Err(TryRecvError::Empty) => return true,
            // Every sender is gone: the connection is being dropped.
            Err(TryRecvError::Disconnected) => return true,
        }
    }
}

/// Decode one frame payload, record its readback, translate it, and append any events.
///
/// Empty frames (the frame-level keep-alive echo) and the stack's two-byte ping answer are
/// skipped silently — they are protocol noise, not messages. **Anything else that fails to
/// decode is counted and reported**, because the alternative is the #494 failure mode: a
/// connected socket quietly discarding a whole race.
fn handle_payload(payload: &[u8], adapter: &mut VelocidroneAdapter, ctx: &ReaderCtx) {
    ctx.frames_seen.fetch_add(1, Ordering::SeqCst);
    ctx.readback.lock().unwrap().last_frame_at = Some(Instant::now());

    if payload.is_empty() || is_stack_ping_answer(payload) {
        return;
    }
    let Some(raw) = decode_payload(payload, ctx) else {
        return;
    };

    note_readback(&raw, ctx);

    let translated = adapter.translate(raw);
    if !translated.is_empty() {
        ctx.events.lock().unwrap().extend(translated);
    }
}

/// Whether a payload is the eToile stack's answer to an RFC ping — a normal binary message
/// carrying exactly `{0x8A, 0x00}`, not a pong frame.
fn is_stack_ping_answer(payload: &[u8]) -> bool {
    payload == STACK_PING_ANSWER
}

/// Decode a frame payload into a [`Raw`], charging a failure to the malformed counter.
///
/// The old code did `let Ok(raw) = … else { return; }` — a silent drop, which is exactly
/// how a real Velocidrone's every frame went missing with no signal anywhere (#494). A
/// build newer than this adapter must be visible as schema drift, not as a dead feed.
fn decode_payload(payload: &[u8], ctx: &ReaderCtx) -> Option<Raw> {
    let text = match std::str::from_utf8(payload) {
        Ok(text) => text.trim(),
        Err(_) => {
            note_malformed(ctx, "frame payload is not valid UTF-8");
            return None;
        }
    };
    if text.is_empty() {
        return None;
    }
    match serde_json::from_str::<Raw>(text) {
        Ok(raw) => Some(raw),
        Err(err) => {
            // Truncate: a 60 Hz `imu` frame or a huge roster must not put the whole
            // payload in the log on every occurrence.
            let sample: String = text.chars().take(200).collect();
            note_malformed(ctx, &format!("{err} (frame began: {sample})"));
            None
        }
    }
}

/// Count an undecodable frame and say so once, loudly.
///
/// Once, not per frame: `imu` runs at 60 Hz, and a warning per frame would bury the log it
/// is trying to make legible. The running count is the ongoing signal.
fn note_malformed(ctx: &ReaderCtx, detail: &str) {
    let first = {
        let mut rb = ctx.readback.lock().unwrap();
        rb.malformed_frames += 1;
        rb.malformed_frames == 1
    };
    if first {
        crate::diag!(
            "gridfpv: velocidrone: WARNING — could not decode a frame from the sim, so it \
             was DROPPED: {detail}. This is schema drift (a Velocidrone build newer than \
             this adapter), not a dead feed — laps can go missing while the sim looks \
             perfectly connected. Further undecodable frames are counted in the \
             connection's readback."
        );
    }
}

/// Record what a frame says about the sim's own state — the readback half of every write.
fn note_readback(raw: &Raw, ctx: &ReaderCtx) {
    let mut rb = ctx.readback.lock().unwrap();
    match raw {
        Raw::RaceStatus(status) => rb.last_race_action = Some(status.race_action.clone()),
        Raw::Countdown(c) => rb.last_countdown = Some(c.count_value),
        Raw::RaceType(t) => rb.race_laps = t.race_laps,
        // One frame per uid the sim could not seat, in request order.
        Raw::ActivateError(e) => rb.activate_errors.push(e.uid_not_found.clone()),
        Raw::PilotList(entries) => rb.roster = Some(entries.clone()),
        // The host flag: the gate on every seating and room command.
        Raw::Player(p) => rb.race_manager = Some(p.race_manager),
        _ => {}
    }
}

/// Set the read timeout on the underlying plain `TcpStream`.
///
/// The feed is `ws://` (plain), so the stream is always [`MaybeTlsStream::Plain`]; other
/// variants (only present with a TLS feature, which we never enable) are a no-op.
fn set_read_timeout(
    socket: &WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> Result<(), Error> {
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream.set_read_timeout(timeout)?;
    }
    Ok(())
}

/// Whether an I/O error is a (non-fatal) read-timeout wake-up rather than a real failure.
/// Different platforms map a socket read timeout to either `WouldBlock` or `TimedOut`.
fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The keep-alive is the vendor-blessed command, not the empty frame we used to send.
    /// An empty frame is tolerated (echoed at frame level) but any non-empty *non-command*
    /// frame reaches the game's JSON layer and is logged every time — the origin of the
    /// "websocket stutters the sim" reports.
    #[test]
    fn the_keep_alive_is_the_ping_command() {
        assert_eq!(VdCommand::Ping.to_json(), r#"{"command":"ping"}"#);
    }

    #[test]
    fn commands_serialize_to_the_documented_wire_form() {
        assert_eq!(VdCommand::StartRace.to_json(), r#"{"command":"startrace"}"#);
        assert_eq!(VdCommand::AbortRace.to_json(), r#"{"command":"abortrace"}"#);
        assert_eq!(
            VdCommand::AllSpectate.to_json(),
            r#"{"command":"allspectate"}"#
        );
        assert_eq!(VdCommand::GetPilots.to_json(), r#"{"command":"getpilots"}"#);
        assert_eq!(VdCommand::Lock.to_json(), r#"{"command":"lock"}"#);
        assert_eq!(VdCommand::Unlock.to_json(), r#"{"command":"unlock"}"#);
    }

    #[test]
    fn activate_sends_uids_as_a_numeric_array() {
        assert_eq!(
            VdCommand::Activate(vec!["12345".into(), "67890".into()]).to_json(),
            r#"{"command":"activate","pilots":[12345,67890]}"#
        );
        assert_eq!(
            VdCommand::Activate(vec![]).to_json(),
            r#"{"command":"activate","pilots":[]}"#
        );
    }

    /// A uid we cannot state confidently is omitted from the write rather than guessed —
    /// the game skips what `int.TryParse` rejects, and a guessed uid seats the wrong pilot.
    #[test]
    fn a_non_numeric_uid_is_omitted_from_the_activate_write() {
        assert_eq!(
            VdCommand::Activate(vec!["12345".into(), "not-a-uid".into()]).to_json(),
            r#"{"command":"activate","pilots":[12345]}"#
        );
    }

    /// The stack answers an RFC ping with a two-byte binary *message*. It must be filtered
    /// before the JSON decoder, or every ping would be charged as a malformed frame.
    #[test]
    fn the_stack_ping_answer_is_not_a_message() {
        assert!(is_stack_ping_answer(&[0x8A, 0x00]));
        assert!(!is_stack_ping_answer(&[0x8A]));
        assert!(!is_stack_ping_answer(b"{}"));
    }

    fn ctx() -> ReaderCtx {
        ReaderCtx {
            events: Arc::new(Mutex::new(Vec::new())),
            readback: Arc::new(Mutex::new(Readback::default())),
            frames_seen: Arc::new(AtomicU64::new(0)),
            stop: Arc::new(AtomicBool::new(false)),
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    /// **The #494 regression, at the transport layer.** A real (binary) `racedata` frame
    /// must translate into a pass. The old reader matched `Message::Text` only and
    /// discarded binary, so against a real game it dropped the entire feed in silence.
    #[test]
    fn a_binary_racedata_frame_is_decoded_and_translated() {
        let ctx = ctx();
        let mut adapter = VelocidroneAdapter::with_default_id();
        let frame = br#"{"racedata":{"Dacus":{"position":"1","lap":"1","gate":"1","time":"1.369","finished":"False","colour":"00FFFF","uid":12345}}}"#;
        handle_payload(frame, &mut adapter, &ctx);

        let events = ctx.events.lock().unwrap();
        assert!(
            events.iter().any(|e| matches!(e, Event::Pass(_))),
            "a binary racedata frame must produce a pass, got {events:?}"
        );
        assert_eq!(ctx.readback.lock().unwrap().malformed_frames, 0);
    }

    /// An undecodable frame is counted, not silently dropped — the difference between
    /// "this build drifted" and "the timer is dead".
    #[test]
    fn an_undecodable_frame_is_counted_and_not_silent() {
        let ctx = ctx();
        let mut adapter = VelocidroneAdapter::with_default_id();
        handle_payload(b"{not json at all", &mut adapter, &ctx);
        assert_eq!(ctx.readback.lock().unwrap().malformed_frames, 1);
        assert!(ctx.events.lock().unwrap().is_empty());
    }

    /// An unknown *frame kind* is not malformed — it decodes to `Raw::Other` and is
    /// ignored. Only an undecodable payload is drift.
    #[test]
    fn an_unknown_frame_kind_is_not_charged_as_malformed() {
        let ctx = ctx();
        let mut adapter = VelocidroneAdapter::with_default_id();
        handle_payload(br#"{"somethingNew":{"a":1}}"#, &mut adapter, &ctx);
        assert_eq!(ctx.readback.lock().unwrap().malformed_frames, 0);
    }

    /// Empty frames (the frame-level keep-alive echo) and the ping answer are protocol
    /// noise: counted as frames seen, never as malformed.
    #[test]
    fn protocol_noise_is_skipped_without_complaint() {
        let ctx = ctx();
        let mut adapter = VelocidroneAdapter::with_default_id();
        handle_payload(b"", &mut adapter, &ctx);
        handle_payload(&STACK_PING_ANSWER, &mut adapter, &ctx);
        assert_eq!(ctx.readback.lock().unwrap().malformed_frames, 0);
        assert_eq!(ctx.frames_seen.load(Ordering::SeqCst), 2);
    }

    /// The readback half: each frame that says something about a write is recorded.
    #[test]
    fn readback_records_what_the_sim_says_about_itself() {
        let ctx = ctx();
        let mut adapter = VelocidroneAdapter::with_default_id();
        for frame in [
            br#"{"racestatus":{"raceAction":"start"}}"#.as_slice(),
            br#"{"racetype":{"raceMode":"THREE_LAP_SINGLE_CLASS","raceFormat":"NORMAL","raceLaps":"3"}}"#,
            br#"{"countdown":{"countValue":"0"}}"#,
            br#"{"ActivateError":{"UIDNotFound":"99999"}}"#,
            br#"{"ActivateError":{"UIDNotFound":"11111"}}"#,
            br#"{"pilotlist":[{"name":"Ace","uid":"12345"}]}"#,
            br#"{"player":{"PlayerName":"Ace","playerColour":"00FFFF","playerFlying":"True","raceManager":"True"}}"#,
        ] {
            handle_payload(frame, &mut adapter, &ctx);
        }
        let rb = ctx.readback.lock().unwrap();
        assert_eq!(rb.last_race_action.as_deref(), Some("start"));
        assert_eq!(rb.race_laps, Some(3));
        assert_eq!(rb.last_countdown, Some(0));
        // One entry per missing uid, in request order — the sim emits them per uid.
        assert_eq!(rb.activate_errors, vec!["99999", "11111"]);
        assert_eq!(rb.roster.as_ref().map(|r| r.len()), Some(1));
        assert_eq!(rb.race_manager, Some(true));
    }
}
