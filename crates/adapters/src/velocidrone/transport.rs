//! Live Velocidrone WebSocket transport (feature `live`).
//!
//! Connects to a running Velocidrone build's raw WebSocket feed
//! (`ws://<host>:60003/velocidrone`), decodes each text frame into the adapter's
//! [`Raw`] message, runs it through [`VelocidroneAdapter`], and accumulates the
//! canonical [`Event`]s. This is the thin network layer the pure translator (the
//! rest of the module) was designed to sit behind: all wire-format knowledge stays
//! in `Raw`/`translate`; this file only moves bytes.
//!
//! Unlike RotorHazard, Velocidrone is **not** Socket.IO — it is a plain WebSocket
//! emitting one JSON object per text frame, plus a periodic empty-string keep-alive
//! ping (~5-10s). We mirror that ping back and skip empty frames on read.
//!
//! ## Threading model
//!
//! [`tungstenite`] is a synchronous (blocking) WebSocket; a single reader thread
//! owns the socket. To avoid a second thread contending for the socket just to send
//! the keep-alive, the reader sets a read timeout on the underlying [`TcpStream`]:
//! every read either yields a frame or times out, and a timeout is the cue to send
//! the empty-string keep-alive before looping again. So one thread does both reads
//! and pings, with no shared socket lock. Translated events land in an
//! `Arc<Mutex<Vec<Event>>>` sink drained by [`VelocidroneConnection::events`], the
//! same shape as [`RotorHazardConnection`](crate::rotorhazard::transport::RotorHazardConnection).

// `tungstenite::Error` is a sizeable external enum; we thread it through unchanged
// rather than box every signature in this thin wrapper (mirrors the RH transport).
#![allow(clippy::result_large_err)]

use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Error, Message, WebSocket};

use super::{Raw, VelocidroneAdapter};
use crate::Adapter;
use gridfpv_events::Event;

/// How often the reader wakes (on read timeout) to send the keep-alive ping.
///
/// Velocidrone pings every ~5-10s; matching that cadence keeps the connection
/// alive without spamming the feed. It also bounds how quickly a `disconnect()`
/// (which flips the stop flag) is noticed by the reader.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(5);

/// The keep-alive frame Velocidrone exchanges: an empty text frame.
fn keep_alive() -> Message {
    Message::text("")
}

/// A live connection to a Velocidrone WebSocket feed, translating its frame stream
/// into canonical [`Event`]s.
///
/// Drain it read-only via [`events`](Self::events); end it with
/// [`disconnect`](Self::disconnect).
pub struct VelocidroneConnection {
    /// Translated events accumulated by the reader thread, drained by `events()`.
    events: Arc<Mutex<Vec<Event>>>,
    /// Flipped by `disconnect()` to ask the reader thread to stop.
    stop: Arc<AtomicBool>,
    /// The reader thread handle, joined on `disconnect()`.
    reader: Option<JoinHandle<()>>,
}

impl VelocidroneConnection {
    /// Connect to `url` (e.g. `ws://localhost:60003/velocidrone`) and start
    /// translating the Velocidrone WebSocket stream through a fresh
    /// [`VelocidroneAdapter`] (the conventional `"velocidrone"` id).
    pub fn connect(url: &str) -> Result<Self, Error> {
        Self::connect_with(url, VelocidroneAdapter::with_default_id())
    }

    /// Connect to `url` and translate the stream through a caller-supplied
    /// `adapter` (e.g. one with a non-default [`AdapterId`](gridfpv_events::AdapterId)).
    pub fn connect_with(url: &str, adapter: VelocidroneAdapter) -> Result<Self, Error> {
        // Resolve the handshake synchronously so `connect()` fails fast on a bad URL
        // or an unreachable server, exactly like the RH transport's `connect`.
        let (mut socket, _response) = tungstenite::connect(url)?;

        // A read timeout turns each blocking read into a "frame or wake up" call: a
        // timeout is the cue to send the keep-alive and re-check the stop flag.
        set_read_timeout(&socket, Some(KEEP_ALIVE_INTERVAL))?;

        let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));

        let reader = {
            let events = events.clone();
            let stop = stop.clone();
            std::thread::spawn(move || run_reader(&mut socket, adapter, &events, &stop))
        };

        Ok(Self {
            events,
            stop,
            reader: Some(reader),
        })
    }

    /// Take everything translated since the last call.
    pub fn events(&self) -> Vec<Event> {
        let mut guard = self.events.lock().unwrap();
        std::mem::take(&mut *guard)
    }

    /// Stop the reader thread and disconnect from the server.
    ///
    /// Signals the reader to stop, then joins it (the reader closes the socket on
    /// the way out). Bounded by [`KEEP_ALIVE_INTERVAL`] — the longest the reader can
    /// be blocked in a read before it wakes and observes the stop flag.
    pub fn disconnect(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for VelocidroneConnection {
    fn drop(&mut self) {
        // If `disconnect()` wasn't called, still ask the reader to stop and reap it
        // so a dropped connection never leaks the thread.
        self.stop.store(true, Ordering::SeqCst);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// The reader loop: read frames, decode + translate text into the sink, send the
/// keep-alive on each idle wake-up, and exit on close / stop / a real I/O error.
fn run_reader(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    mut adapter: VelocidroneAdapter,
    events: &Arc<Mutex<Vec<Event>>>,
    stop: &Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        match socket.read() {
            Ok(Message::Text(text)) => {
                handle_text(text.as_str(), &mut adapter, events);
            }
            // Velocidrone's empty-string keep-alive arrives as a Text("") frame in
            // most builds, but tolerate an empty Binary too; either way: skip it.
            Ok(Message::Binary(_))
            | Ok(Message::Ping(_))
            | Ok(Message::Pong(_))
            | Ok(Message::Frame(_)) => {}
            // Server-initiated close: acknowledge by leaving the loop.
            Ok(Message::Close(_)) => break,
            // A read timeout (our keep-alive cadence) surfaces as an I/O WouldBlock /
            // TimedOut: send the keep-alive and loop. `disconnect()` aborts a pending
            // read by closing the socket, which can also land here.
            Err(Error::Io(io)) if is_timeout(&io) => {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                if socket.send(keep_alive()).is_err() {
                    break;
                }
            }
            // The peer is gone or the protocol broke: stop reading.
            Err(_) => break,
        }
    }
    // Best-effort close; the peer may already be gone.
    let _ = socket.close(None);
    let _ = socket.flush();
}

/// Decode one text frame into a [`Raw`], translate it, and append any events.
///
/// The empty-string keep-alive echo and any non-JSON noise are skipped silently —
/// a malformed frame must never abort the session (the [`Raw`] decoder already
/// degrades unknown *kinds* to [`Raw::Other`]; this guards the JSON layer above it).
fn handle_text(text: &str, adapter: &mut VelocidroneAdapter, events: &Arc<Mutex<Vec<Event>>>) {
    // The keep-alive is an empty (or whitespace-only) frame: nothing to decode.
    if text.trim().is_empty() {
        return;
    }
    let Ok(raw) = serde_json::from_str::<Raw>(text) else {
        return;
    };
    let translated = adapter.translate(raw);
    if !translated.is_empty() {
        events.lock().unwrap().extend(translated);
    }
}

/// Set the read timeout on the underlying plain `TcpStream`.
///
/// The feed is `ws://` (plain), so the stream is always [`MaybeTlsStream::Plain`];
/// other variants (only present with a TLS feature, which we never enable) are a
/// no-op.
fn set_read_timeout(
    socket: &WebSocket<MaybeTlsStream<TcpStream>>,
    timeout: Option<Duration>,
) -> Result<(), Error> {
    if let MaybeTlsStream::Plain(stream) = socket.get_ref() {
        stream.set_read_timeout(timeout)?;
    }
    Ok(())
}

/// Whether an I/O error is a (non-fatal) read-timeout wake-up rather than a real
/// failure. Different platforms map a socket read timeout to either `WouldBlock` or
/// `TimedOut`.
fn is_timeout(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}
