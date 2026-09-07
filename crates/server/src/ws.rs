//! The WebSocket change-stream transport (protocol.html §3, §9.1) — issue #43.
//!
//! After fetching a [`Snapshot`](crate::snapshot::Snapshot) over HTTP (#42), a client
//! opens this single WebSocket (`GET /stream`), sends one
//! [`SubscribeRequest`](crate::scope::SubscribeRequest), and receives an ordered run of
//! [`StreamMessage`]s that keep its scoped projection identical to the server's. v0.4 is
//! **WebSocket-only** (§9.1); the SSE read-only fallback (§9.1 open decision) is not built.
//!
//! # The subscribe protocol
//!
//! 1. Client → server: exactly one text frame, a JSON [`SubscribeRequest`] — the
//!    [`Scope`](crate::scope::Scope) it wants and an optional `from` resume cursor.
//! 2. Server → client: a stream of JSON [`StreamMessage`] text frames, each either a
//!    [`ChangeEnvelope`](crate::stream::ChangeEnvelope) or the terminal
//!    [`StreamMessage::ReSnapshotRequired`] signal.
//!
//! A malformed or missing subscribe frame closes the socket with a
//! [`ProtocolError`]-bearing close frame ([`ErrorCode::BadRequest`]); no auth is enforced
//! here (#44 layers it in front of this handler).
//!
//! # Per-stream sequence vs the resume cursor (protocol.html §3, §9.5)
//!
//! Two distinct integers, deliberately:
//!
//! - The **resume cursor** ([`Cursor`]) a client presents in `from` is a **log offset** —
//!   the same value the [`Snapshot`](crate::snapshot::Snapshot) handed it (the log length
//!   at snapshot time, i.e. the offset of the first event *after* the snapshot). It names a
//!   position in the Director's private append-only log (§1, §9.5) and is what makes resume
//!   work across reconnects: a fresh connection re-presents the last offset it had caught
//!   up to.
//! - The **per-stream sequence** ([`ChangeEnvelope::sequence`]) is this stream's own
//!   public ordering: a monotonic integer **starting at 1**, incremented by one for every
//!   envelope *this connection* emits (§3 "increases by one per envelope"). It is not the
//!   log offset — one log append can fan out into several projection changes or none the
//!   scope subscribes to, so the sequence advances independently of the offset.
//!
//! The mapping between them: as the engine folds the log forward it tracks the log offset
//! it has consumed up to; whenever the scoped projection's value *changes* it emits one
//! envelope, assigns it the next per-stream sequence (1, 2, 3, …), and **echoes the offset
//! it folded through** as [`ChangeEnvelope::cursor`] (#422). A client persists *both* — it
//! renders by sequence order and resumes by the offset cursor. (The two coincide numerically
//! only by accident; the protocol keeps them separate because they answer different
//! questions: "in what order?" and "from where do I resume?".)
//!
//! Echoing the offset is what makes the resume **exact**. Before #422 the wire carried only
//! the sequence, so a client inferred its resume position by advancing `+1` per applied
//! envelope — an at-or-behind lower bound that drifted further every time an append moved no
//! projection. See [`ChangeEnvelope::cursor`] for what that drift did to live lap counts.
//!
//! # The bounded retained window + re-snapshot (protocol.html §3, §9.3)
//!
//! The Director is memory-bounded (§9.3 "keep it simple — one event"): it retains a
//! window of [`RETAINED_WINDOW`] log offsets behind the current tail. A resume `from`
//! cursor older than `tail - RETAINED_WINDOW` cannot be replayed, so instead of streaming
//! a hole the server sends a single [`StreamMessage::ReSnapshotRequired`]
//! ([`ErrorCode::StaleCursor`]) and closes — the client re-snapshots and resubscribes from
//! the fresh cursor (§3 "re-snapshot is always correct because projections are
//! recomputable"). A `from` of `0` (or `None`, a fresh subscribe) is *never* stale: replay
//! from the start of the log is always in-window by definition.
//!
//! An in-window `from` **behind** the tail is not replayed offset by offset: the catch-up
//! span is folded once at its end and delivered as a single settled envelope (#422). See
//! [`Engine::advance`]. That is the guard's complement, not a weakening of it — the guard
//! still decides *whether* a cursor can resume at all; the collapse only decides what a
//! resumable one is shown.
//!
//! # Guarantees (protocol.html §3)
//!
//! - **Total order, gap-free.** Envelopes carry strictly increasing per-stream sequences
//!   1, 2, 3, … with no gaps; a client applying them in order converges to server state.
//! - **Idempotent, at-least-once with exactly-once effect.** Applying an envelope is keyed
//!   by its sequence, so a redelivery after a flaky reconnect is a no-op. The engine folds
//!   from the log (the source of truth) on every wakeup, so a resumed stream re-derives the
//!   same envelopes for the same offsets — overlap on resume is harmless.
//!
//! # Delta vs fresh-value — deferred encodings (protocol.html §9.2)
//!
//! Every envelope this engine emits today is a [`Change::FreshValue`]: the whole recomputed
//! [`ProjectionBody`]. The per-projection delta *encodings* (a single appended lap, a
//! heat-state transition) are deferred to #59 — [`Change::Delta`] is still an opaque
//! placeholder. What is wired now is the **distinction** §9.2 fixes: [`ScopeProjection`]
//! tags each scope as *delta-preferring* (the append-heavy lap-list and live-state scopes,
//! where #59 will encode incremental deltas) or *fresh-value* (a cheap re-fold like a heat
//! result after a marshaling correction, which stays a fresh value). The engine records
//! that preference per envelope so #59 can swap the encoding in without reshaping the
//! stream or its sequencing.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use gridfpv_events::{CompetitorRef, Event};
use gridfpv_projection::{LapList, lap_list_marshaled};
use gridfpv_storage::StoredEvent;

use crate::app::{AppState, resolve_event};
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventRegistry, RoundDef};
use crate::live_state::{
    live_state_over_with_floor, live_state_over_with_rounds, live_state_with_rounds,
    with_heat_timing,
};
use crate::scope::{EventId, Scope};
use crate::snapshot::{ProjectionBody, ProjectionKind};
use crate::stream::{Change, ChangeEnvelope, Cursor, StreamMessage};

/// How many log offsets behind the tail the Director keeps replayable before forcing a
/// re-snapshot (protocol.html §3, §9.3).
///
/// "Keep it simple" (§9.3): a fixed window, not a byte/time budget. A resume cursor older
/// than `tail - RETAINED_WINDOW` is answered with [`StreamMessage::ReSnapshotRequired`].
/// The Cloud (later) keeps a much larger window over durable storage; the Director's is
/// deliberately small and memory-bounded. The value is generous enough that a brief network
/// blip resumes seamlessly while an offset far in the past is correctly rejected.
pub const RETAINED_WINDOW: u64 = 256;

/// Whether a projection is append-heavy (so #59 will encode incremental **deltas**) or a
/// cheap re-fold re-sent whole as a **fresh value** (protocol.html §9.2).
///
/// Recorded per envelope so the delta-vs-fresh *distinction* is wired even though every
/// envelope is a fresh value today (see the module docs). The engine consults it only to
/// document intent for #59; it does not yet change the encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// Append-heavy: a lap list grows by laps, live-state by passes — #59 sends deltas.
    DeltaPreferring,
    /// Cheap to recompute and re-sent whole (a re-folded result / ranking / outcome, e.g.
    /// after a marshaling correction, §9.2) — stays a fresh value even after #59.
    FreshValue,
}

impl Encoding {
    /// The delta-vs-fresh preference §9.2 fixes for a projection kind: the append-heavy
    /// live-state and lap-list are delta-preferring; the re-folded result, ranking, and
    /// event outcome stay fresh values (a single marshaling correction re-folds the whole
    /// thing, so a delta would be no smaller).
    fn of(kind: ProjectionKind) -> Self {
        match kind {
            // The signal trace appends incrementally as a heat runs (chunk after chunk), so it is
            // delta-preferring like the lap list and live state (#59 will encode the appended chunks).
            ProjectionKind::LiveRaceState
            | ProjectionKind::LapList
            | ProjectionKind::SignalTrace => Encoding::DeltaPreferring,
            ProjectionKind::HeatResult
            | ProjectionKind::Ranking
            | ProjectionKind::EventOutcome
            // The marshaling audit re-folds wholesale on each correction (it is short — one entry
            // per ruling), so a delta would be no smaller; serve it fresh (#55).
            | ProjectionKind::MarshalingAudit => Encoding::FreshValue,
        }
    }
}

/// The projection a [`Scope`] folds to on the change stream, plus the delta-vs-fresh
/// preference §9.2 fixes for it.
///
/// This binds each of the four addressable scopes (§4) to the one projection its change
/// stream advances, mirroring the snapshot folds in [`crate::app`] so a subscriber and a
/// snapshot of the same scope agree.
struct ScopeProjection {
    kind: ProjectionKind,
    encoding: Encoding,
}

/// Where a stream re-reads the event's **round config** from, so the D26 min-lap floor can be
/// resolved per fold rather than captured once at subscribe (#409).
///
/// `RoundDef::min_lap_secs` lives in registry meta, not in the log, so it is invisible to a
/// pure-log fold and it can be **edited while a stream is open**. Holding the registry handle
/// (rather than a `Vec<RoundDef>` snapshot taken at subscribe) is what gives the stream the
/// property the snapshot path gets for free by re-resolving per request: the config the fold
/// applies is the config that stands *now*.
#[derive(Clone)]
struct RoundFloors {
    registry: EventRegistry,
    event: EventId,
}

impl RoundFloors {
    /// The event's rounds as registry meta holds them **at this instant**. Read once per
    /// [`Engine::advance`] — i.e. once per wake — and shared across the prefixes that batch
    /// folds, which are all folded within the same instant anyway.
    ///
    /// An unknown event (deleted mid-stream) yields no rounds, which reads as "no floor": the
    /// stream degrades to the pre-#409 counts rather than failing, and the socket closes on the
    /// next log read regardless.
    fn rounds(&self) -> Vec<RoundDef> {
        self.registry.rounds_of(&self.event).unwrap_or_default()
    }
}

impl ScopeProjection {
    /// The projection + encoding preference a scope's change stream uses.
    fn of(scope: &Scope) -> Self {
        // Event / class fold the whole-event live race-state (the class log filter is
        // still deferred, exactly as the snapshot path notes); the heat scope folds its
        // own window's live race-state (the tightest, lowest-latency scope); the pilot
        // scope folds that pilot's lap list across the event.
        let kind = match scope {
            Scope::Event { .. } | Scope::Class { .. } | Scope::Heat { .. } => {
                ProjectionKind::LiveRaceState
            }
            Scope::Pilot { .. } => ProjectionKind::LapList,
        };
        ScopeProjection {
            kind,
            encoding: Encoding::of(kind),
        }
    }

    /// Fold the scope's projection over the event prefix `events` (offsets `0..n`).
    ///
    /// Pure: the same prefix always yields the same body, so folding `0..n` then `0..n+1`
    /// and comparing tells the engine whether offset `n` *changed* the projection (and thus
    /// whether to emit an envelope). Reuses the same fold helpers as the snapshot path so a
    /// subscriber and a snapshot of the same scope converge to the same value.
    ///
    /// The fold is a **pure fold of the log for every format**, open practice included (D5,
    /// reversed 2026-08-24): practice laps are ordinary logged `Pass` events, so there is no
    /// accumulator to consult and no overlay to splice. That is what makes the change-suppression
    /// in [`Engine::advance`] sound — an append that does not move the folded body (a
    /// `SignalHistory` chunk, say) emits nothing at all.
    ///
    /// # The D26 min-lap floor is resolved PER PREFIX (#409)
    ///
    /// `rounds` is the event's registry meta — the only place `RoundDef::min_lap_secs` lives, so
    /// a pure-log fold cannot see it. The floor itself is **not** a parameter: it is re-derived
    /// inside this fold, from *this* prefix, because the floor belongs to the heat the prefix
    /// reports as current and the current heat changes as the stream crosses into the next round.
    /// A floor captured once at subscribe — or once per `advance` batch — would be wrong from
    /// that crossing onward. What each scope hands down is the `rounds` list, and the live fold
    /// resolves the floor from the heat it *itself* picks
    /// ([`live_state_with_rounds`](crate::live_state::live_state_with_rounds) /
    /// [`live_state_over_with_rounds`](crate::live_state::live_state_over_with_rounds)), which is
    /// the same value the snapshot handlers get from the same entry points — so a subscriber and
    /// a snapshot of one scope still converge, and neither pays for a pre-pass that re-folds
    /// `current_heat` over the same slice (#460).
    fn fold(scope: &Scope, stored: &[StoredEvent], rounds: &[RoundDef]) -> Option<ProjectionBody> {
        // The bare-event view the lap/phase fold consumes; the live-state clock timing is
        // folded separately from `stored` (which carries the `recorded_at` server timestamps).
        let events: Vec<Event> = stored.iter().map(|s| s.event.clone()).collect();
        let events = events.as_slice();
        match scope {
            Scope::Event { .. } => {
                // `with_heat_timing` anchors the clock to the current heat's race-go (#62 follow-up).
                // `rounds` carries both the D26 floor and the rounds the event still defines (#439),
                // which decide which heats it may report as current / on deck — exactly as they do
                // for the snapshot.
                Some(ProjectionBody::LiveRaceState(with_heat_timing(
                    live_state_with_rounds(events, rounds),
                    stored,
                )))
            }
            Scope::Class { class, .. } => {
                // The class's REAL filtered window, with preserved global offsets — the same
                // fold as `snapshot_class`, so snapshot and stream converge (they used to
                // diverge: the stream folded the whole event). Offsets matter so marshaling
                // adjudications (global LogRef targets) resolve inside the filtered view.
                //
                // The floor is resolved over the WINDOW — the class fold picks its current heat
                // from the filtered slice, so that is the heat whose round owns the floor — and it
                // is resolved *inside* the fold, which already holds that slice. The pre-pass this
                // replaces needed a bare `&[Event]`, so it deep-cloned the whole window a second
                // time on every folded offset (#460 item 1).
                let window = crate::app::class_window_offsets(events, class);
                Some(ProjectionBody::LiveRaceState(with_heat_timing(
                    live_state_over_with_rounds(&window, rounds),
                    stored,
                )))
            }
            Scope::Heat { heat } => {
                // Only fold once the heat exists in the log; before that the scope has no
                // value to stream (the snapshot would 404).
                let scheduled = events
                    .iter()
                    .any(|e| matches!(e, Event::HeatScheduled { heat: h, .. } if h == heat));
                if !scheduled {
                    return None;
                }
                // The heat's phase/clock are its real log window; the race-go timing folds from the
                // full stored log.
                let window = crate::app::heat_window_offsets(events, heat);
                // The scope NAMES the heat, so the floor is that heat's round's — resolved through
                // the same helper `snapshot_heat` uses, against the same registry meta.
                let floor = crate::app::min_lap_micros_of(
                    crate::app::round_def_of_heat(events, heat, rounds).as_ref(),
                );
                // No defined-round filter: the scope names its heat (#439).
                Some(ProjectionBody::LiveRaceState(with_heat_timing(
                    live_state_over_with_floor(&window, floor, None),
                    stored,
                )))
            }
            Scope::Pilot { pilot, .. } => {
                let full =
                    lap_list_marshaled(events.iter().enumerate().map(|(i, e)| (i as u64, e)));
                let pilot_ref = CompetitorRef(pilot.0.clone());
                let competitors: Vec<_> = full
                    .competitors
                    .into_iter()
                    .filter(|c| c.competitor.competitor == pilot_ref)
                    .collect();
                if competitors.is_empty() {
                    return None;
                }
                Some(ProjectionBody::LapList(LapList { competitors }))
            }
        }
    }
}

/// `GET /events/{event_id}/stream` — upgrade to the change-stream WebSocket
/// (protocol.html §3, §9.1), scoped to one event's log (issue #72).
///
/// The handler resolves `event_id` to that event's [`AppState`] through the
/// [`EventRegistry`] (an unknown id → a typed 404 *before* the upgrade), then upgrades the
/// connection and hands it to [`run_stream`] against THAT log; auth (#44) and control
/// command handling (#45) layer on separately — this is the read stream only.
pub async fn stream_handler(
    ws: WebSocketUpgrade,
    State(registry): State<EventRegistry>,
    Path(event_id): Path<EventId>,
) -> Response {
    let state = match resolve_event(&registry, &event_id) {
        Ok(state) => state,
        // An unknown event id can't open a stream — reject the upgrade with the typed 404.
        Err(err) => return err.into_response(),
    };
    // The stream keeps the registry handle so it can re-read this event's rounds — the D26
    // min-lap floor lives there, not in the log (#409).
    let floors = RoundFloors {
        registry,
        event: event_id,
    };
    ws.on_upgrade(move |socket| run_stream(socket, state, floors))
}

/// Drive one subscribed change stream over an upgraded socket (protocol.html §3).
///
/// Reads the one [`SubscribeRequest`], then runs the replay-then-tail loop until the client
/// disconnects, the cursor is stale, or a send fails.
async fn run_stream(mut socket: WebSocket, state: AppState, floors: RoundFloors) {
    // 1. The client's single subscribe frame — it doubles as the connect message (§7),
    //    carrying the contract version and an optional read token.
    let request = match recv_subscribe(&mut socket).await {
        Ok(req) => req,
        Err(err) => {
            close_with(&mut socket, err).await;
            return;
        }
    };

    // 1a. Contract-version negotiation (#46, protocol.html §7, §9.7). The client states the
    //     version it was built against; if it falls outside the server's supported band we
    //     send the "too old / too new, please refresh" signal and close before streaming.
    //     An absent version means "unspecified" — treated as this build's own version.
    let client_version = request.contract_version.unwrap_or(crate::CONTRACT_VERSION);
    if !crate::is_supported_contract_version(client_version) {
        close_with(
            &mut socket,
            crate::ServerHello::refresh_error(client_version),
        )
        .await;
        return;
    }

    // 1b. Read auth (#44, protocol.html §5). Reads are open on the LAN, so an absent token
    //     is fine; a *present* token must be valid (a stale join-token is rejected rather
    //     than silently downgraded to anonymous).
    if let Err(err) = state.tokens().authenticate_read(request.token.as_deref()) {
        close_with(&mut socket, err).await;
        return;
    }

    let projection = ScopeProjection::of(&request.scope);
    // The resume cursor is a log offset (see the module docs); `None` / 0 means "from the
    // start of the log".
    let from = request.from.map(|c| c.seq).unwrap_or(0);

    // 2. Stale-cursor check against the bounded retained window.
    let tail = match state.read() {
        Ok((_, cursor)) => cursor.seq,
        Err(err) => {
            close_with(&mut socket, err).await;
            return;
        }
    };
    let oldest_replayable = tail.saturating_sub(RETAINED_WINDOW);
    if from > 0 && from < oldest_replayable {
        let _ = send_message(
            &mut socket,
            &StreamMessage::ReSnapshotRequired(ProtocolError::new(
                ErrorCode::StaleCursor,
                format!(
                    "resume cursor {from} is below the retained window \
                     (oldest replayable offset {oldest_replayable})"
                ),
            )),
        )
        .await;
        return;
    }

    // 3. Replay-then-tail. `Engine` folds the log forward from offset `from`, emitting a
    // fresh-value envelope each time the scoped projection changes, advancing the per-stream
    // sequence. `applied_offset` is how far into the log it has folded.
    //
    // `tail` — the same value the stale-cursor guard just judged `from` against — bounds the
    // **replay span** `from..tail` (#422): that span is history, already settled, and is
    // collapsed to one settled fold. Everything at or beyond `tail` arrived after this
    // subscription opened and is the live tail, streamed one envelope per changed offset
    // exactly as before. Passing the tail explicitly (rather than letting the engine infer
    // "whatever was on the log at my first fold") is what keeps that boundary deterministic:
    // an append racing the first fold is live, not replay.
    let mut engine = Engine::new(request.scope, projection, from, tail, floors);
    let appended = state.appended();

    loop {
        // Enrol the wakeup in the waiter list *before* reading the log so an append that
        // lands between the read and the await is not lost. `Notified::enable()` registers
        // the waiter immediately (rather than on first poll), so a `notify_waiters()` firing
        // any time after this line wakes this future — even one that fires before `select!`
        // polls it. Without this, an append in the gap between the read and the await would
        // wake nobody and the stream would park until the *next* append (a missed update).
        let mut wake = std::pin::pin!(appended.notified());
        wake.as_mut().enable();

        // Pull the current log (with `recorded_at`, for the race-clock timing) and fold any
        // new events into envelopes.
        let events = match state.read_stored() {
            Ok((events, _)) => events,
            Err(err) => {
                close_with(&mut socket, err).await;
                return;
            }
        };
        for message in engine.advance(&events) {
            if send_message(&mut socket, &message).await.is_err() {
                return; // client gone
            }
        }

        // Park until the next append (the pinned `wake`) or the client drops the socket.
        tokio::select! {
            _ = &mut wake => {}
            // Drain client frames so a close is observed promptly; the client sends
            // nothing else on the read stream.
            frame = socket.recv() => {
                match frame {
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => continue, // ignore stray frames, re-read the log
                    Some(Err(_)) => return,
                }
            }
        }
    }
}

/// The fold-and-sequence engine for one stream (protocol.html §3).
///
/// Holds the scope, its projection/encoding, the per-stream sequence counter, the log
/// offset it has folded up to, and the last projection value it emitted. [`advance`] folds
/// any newly-appended events and yields the envelopes whose projection value changed.
struct Engine {
    scope: Scope,
    projection: ScopeProjection,
    /// The next per-stream sequence to assign (starts at 1, §3).
    next_seq: u64,
    /// The log offset the engine has folded through (exclusive upper bound). Starts at the
    /// resume `from`; advances to the log length as events are consumed.
    applied_offset: u64,
    /// The last projection body emitted, to suppress re-emitting an unchanged fold.
    last_emitted: Option<ProjectionBody>,
    /// The end of the **replay span**: the log tail as it stood when this subscription opened
    /// (#422). Offsets in `from..replay_until` are settled history and collapse to one envelope;
    /// offsets at or beyond it are the live tail and stream one envelope per change.
    replay_until: u64,
    /// Whether the replay span has been dealt with (#422).
    ///
    /// `false` until the first [`advance`](Engine::advance) runs. Only that first call can face
    /// the span, and it collapses it; every later call sees only the live tail.
    caught_up: bool,
    /// Where the D26 min-lap floor comes from (#409) — re-read on every [`advance`], never
    /// captured as a resolved floor.
    floors: RoundFloors,
}

impl Engine {
    fn new(
        scope: Scope,
        projection: ScopeProjection,
        from: u64,
        replay_until: u64,
        floors: RoundFloors,
    ) -> Self {
        Self {
            scope,
            projection,
            next_seq: 1,
            applied_offset: from,
            // Seed with the projection *at* the resume point so the first envelope reflects
            // a change *after* `from`, not a re-send of what the snapshot already carried.
            last_emitted: None,
            replay_until,
            caught_up: false,
            floors,
        }
    }

    /// Fold any events appended since the last call into ordered envelopes.
    ///
    /// For each new offset `applied_offset..events.len()` it folds the scope over the prefix
    /// `0..=offset`; when the folded body differs from the last one emitted it produces one
    /// envelope (fresh value, the next sequence). Walking offset by offset keeps the
    /// per-stream sequence a faithful "one bump per projection change" and the order total.
    ///
    /// Every emission is driven by a **logged** offset. There is no out-of-band re-snapshot: the
    /// open-practice overlay that used to append one after the walk is gone (D5, reversed
    /// 2026-08-24 — practice passes are logged like every other format's). That overlay is what
    /// made #396's repeated lap callouts: `last_emitted` alternated between the pure-log body and
    /// the laps-spliced body, so *every* append — a `SignalHistory` chunk included — differed from
    /// the last value twice over and pushed two fresh `LiveRaceState` envelopes carrying the newest
    /// lap. With one body per offset, an append that does not move the projection emits nothing.
    ///
    /// # Scheduling a heat wakes the stream even when the body is unchanged
    ///
    /// A bare [`Event::HeatScheduled`] that *fills* a heat must NOT steal Live focus
    /// ([`live_state`] keeps `current_heat` on the staged/last-transitioned heat), and it often does
    /// not move `on_deck` either (that is the *earliest* still-scheduled heat — appending a third
    /// heat behind it leaves it unchanged). So for a `LiveRaceState` scope the folded body can be
    /// byte-identical across a `HeatScheduled` offset, and the plain change-suppression below would
    /// emit nothing — leaving every client's `/heats`-derived list (the Live heat picker, the
    /// Rounds & Heats list) stale until the next real transition. A scheduled heat is nonetheless a
    /// real, observable change to the event's heat set, so we **force one fresh-value envelope** at a
    /// `HeatScheduled` offset for the live-state scopes, re-sending the (possibly unchanged) body.
    /// The client dedups by per-stream `sequence`, not by content, so re-sending the same body is
    /// harmless; the extra envelope simply wakes consumers to re-read the heats list. `current_heat`
    /// is untouched, so this never steals focus (the `current-heat` proof stays green).
    /// # The min-lap floor is re-read here, not at subscribe (#409)
    ///
    /// The event's rounds are pulled from registry meta at the top of every `advance` — i.e. on
    /// every wake — and the floor is then derived *per prefix* inside [`ScopeProjection::fold`]
    /// (the current heat, and so the round that owns the floor, moves as the log crosses into the
    /// next round). Since every envelope is a fresh value re-folded from the whole prefix, an
    /// edited `min_lap_secs` re-applies to the run already on the log rather than leaving a
    /// half-floored tail behind: the next envelope carries the counts the heat snapshot would
    /// now give. What it cannot do is emit on its own — a meta edit appends nothing, so the new
    /// floor reaches subscribers on the next logged event (D26 freezes the floor once the round
    /// has raced, so an edit lands before the passes it would have re-judged).
    ///
    /// # The replay span is COLLAPSED, never replayed offset by offset (#422)
    ///
    /// The offset-by-offset walk above describes the **live tail** — the wake-driven case, where a
    /// call sees the one or two offsets appended since the last one. The *first* call is different:
    /// it faces `from..replay_until`, a span of history that had already settled before this
    /// subscription opened, and walking it emitted one fresh value per changed offset — a staircase
    /// of stale bodies ending at the current one.
    ///
    /// The span is bounded by the tail read at subscribe, **not** by "whatever is on the log when
    /// the first fold happens". Those differ: an append can land between the subscribe frame and
    /// the first fold, and it belongs to the live tail — a client that subscribed at the tail must
    /// see each subsequent change as its own envelope whether or not it won that race. Without the
    /// bound the number of envelopes such a client receives would depend on scheduling.
    ///
    /// That staircase is what made live lap counts step **backwards** after a reconnect. The client's
    /// resume cursor used to be a lower bound (see [`ChangeEnvelope::cursor`]), so a blip resubscribed
    /// from `tail - d` for some drift `d`; `d < RETAINED_WINDOW` is in-window, so this engine replayed
    /// rather than the guard rejecting, and every console's `LiveRaceState` was overwritten with an
    /// **older fold** — fewer laps — before climbing back through each intermediate one. To a Race
    /// Director watching the board a pilot lost laps and regained them, which is exactly what a
    /// marshal voiding a pass looks like.
    ///
    /// So the span is folded **once, at its end**, and emitted as a single settled envelope. This is
    /// sound precisely because every envelope is a [`Change::FreshValue`]: only the last body of a
    /// replayed span carries information, the earlier ones are pure waste. It also drops the walk's
    /// quadratic re-fold of the whole prefix per offset.
    ///
    /// What the collapse deliberately preserves:
    ///
    /// - **A real correction still shows.** A marshal's void is a logged append like any other; it
    ///   reaches a *connected* console as its own tail envelope, and it is folded into the settled
    ///   body of any span that contains it. Only the spurious staircase goes — a count that drops
    ///   because a pass was voided still drops.
    /// - **The stale-cursor guard is untouched.** A cursor below `tail - RETAINED_WINDOW` is still
    ///   answered with [`StreamMessage::ReSnapshotRequired`] before this engine is ever built.
    /// - **A schedule still wakes the stream.** If any offset in the span is an
    ///   [`Event::HeatScheduled`], the one collapsed envelope is emitted even when the settled body
    ///   equals what the client already had (the fill-no-steal case above).
    /// - **The crossing feed's contract.** [`LiveCrossing`](crate::live_state::LiveCrossing) entries
    ///   are identified by `pass_ref` and the feed is bounded to the most recent
    ///   [`MAX_LIVE_CROSSINGS`](crate::live_state::MAX_LIVE_CROSSINGS), oldest-dropped — so a
    ///   consumer holding a high-water mark reads the collapsed body exactly as it reads any
    ///   re-snapshot, and can only ever *miss* crossings the bound would already have trimmed.
    fn advance(&mut self, events: &[StoredEvent]) -> Vec<StreamMessage> {
        let mut out = Vec::new();
        let len = events.len() as u64;
        let rounds = self.floors.rounds();
        // Whether this scope folds the live race-state (Event/Class/Heat): only those carry the
        // heat set the `/heats` lists derive from, so only they need the schedule-wake re-emit.
        let live_state_scope = self.projection.kind == ProjectionKind::LiveRaceState;

        // On the *first* fold from a resume point > 0, seed `last_emitted` with the
        // projection value at `from` so we only emit changes strictly after the cursor
        // (the snapshot already carried the value at `from`). For a fresh subscribe
        // (`from == 0`) there is nothing prior, so the first non-empty fold is emitted.
        if self.last_emitted.is_none() && self.applied_offset > 0 {
            let prefix = &events[..(self.applied_offset as usize).min(events.len())];
            self.last_emitted = ScopeProjection::fold(&self.scope, prefix, &rounds);
        }

        // The replay span (see the doc comment): fold it once at its end rather than walking it.
        // It ends at the tail as it stood when the subscription opened — anything appended since
        // is the live tail and falls through to the offset-by-offset walk below.
        if !self.caught_up {
            self.caught_up = true;
            let span_end = self.replay_until.min(len);
            if self.applied_offset < span_end {
                let span = &events[self.applied_offset as usize..span_end as usize];
                // A schedule anywhere in the span must still wake the heats lists, even if the
                // settled body is byte-identical to what the client already holds.
                let scheduled_heat = live_state_scope
                    && span
                        .iter()
                        .any(|s| matches!(s.event, Event::HeatScheduled { .. }));
                self.applied_offset = span_end;
                let body =
                    ScopeProjection::fold(&self.scope, &events[..span_end as usize], &rounds);
                if let Some(body) = body
                    && (scheduled_heat || self.last_emitted.as_ref() != Some(&body))
                {
                    out.push(StreamMessage::Change(Box::new(self.envelope(body.clone()))));
                    self.last_emitted = Some(body);
                }
            }
        }

        while self.applied_offset < len {
            self.applied_offset += 1;
            let prefix = &events[..self.applied_offset as usize];
            // Whether the event newly folded at this offset schedules a heat — a real change to the
            // event's heat set that must wake the heats lists even when the rendered body is unchanged
            // (the fill-no-steal case; see the doc comment above).
            let scheduled_heat = live_state_scope
                && matches!(
                    prefix.last().map(|s| &s.event),
                    Some(Event::HeatScheduled { .. })
                );
            let body = ScopeProjection::fold(&self.scope, prefix, &rounds);
            if let Some(body) = body {
                if scheduled_heat || self.last_emitted.as_ref() != Some(&body) {
                    out.push(StreamMessage::Change(Box::new(self.envelope(body.clone()))));
                    self.last_emitted = Some(body);
                }
            }
        }
        out
    }

    /// Wrap a folded projection body in the next sequenced envelope.
    ///
    /// Every envelope is a [`Change::FreshValue`] today; the [`Encoding`] preference is
    /// recorded for #59 (see the module docs) but does not yet change the wire shape. The
    /// `kind` is taken from the body so a delta envelope (#59) and a fresh value name the
    /// same projection.
    ///
    /// The envelope's `cursor` is `applied_offset` — the log offset this body was folded
    /// **through**, and so the exact `from` a reconnect should present (#422). It is read here
    /// rather than passed in because `advance` has already moved `applied_offset` to the end of
    /// whatever it folded, collapsed span or single offset alike.
    fn envelope(&mut self, body: ProjectionBody) -> ChangeEnvelope {
        let sequence = Cursor::new(self.next_seq);
        self.next_seq += 1;
        let cursor = Cursor::new(self.applied_offset);
        let _ = self.projection.encoding; // wired for #59; fresh-value for now
        let projection = body.kind();
        debug_assert_eq!(
            projection, self.projection.kind,
            "a scope's folded body must match its declared projection kind"
        );
        ChangeEnvelope {
            sequence,
            cursor,
            projection,
            change: Change::FreshValue(body),
        }
    }
}

/// Receive and parse the client's single [`SubscribeRequest`] (protocol.html §3, §4).
///
/// The first text frame must be a JSON [`SubscribeRequest`]; anything else (a binary
/// frame, an early close, an unparseable body) is a [`ErrorCode::BadRequest`].
async fn recv_subscribe(
    socket: &mut WebSocket,
) -> Result<crate::scope::SubscribeRequest, ProtocolError> {
    match socket.recv().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str(&text).map_err(|e| {
            ProtocolError::new(ErrorCode::BadRequest, format!("malformed subscribe: {e}"))
        }),
        Some(Ok(Message::Binary(bytes))) => serde_json::from_slice(&bytes).map_err(|e| {
            ProtocolError::new(ErrorCode::BadRequest, format!("malformed subscribe: {e}"))
        }),
        Some(Ok(_)) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "expected a SubscribeRequest text frame first",
        )),
        Some(Err(e)) => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            format!("websocket error before subscribe: {e}"),
        )),
        None => Err(ProtocolError::new(
            ErrorCode::BadRequest,
            "connection closed before a SubscribeRequest was sent",
        )),
    }
}

/// Serialise and send one [`StreamMessage`] as a JSON text frame.
async fn send_message(socket: &mut WebSocket, message: &StreamMessage) -> Result<(), ()> {
    let json = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(json.into()))
        .await
        .map_err(|_| ())
}

/// Send a [`ProtocolError`] to the client, then close the socket (best-effort).
///
/// The full JSON error rides in a **text frame** (not the close frame): a WebSocket close
/// reason is a control frame capped at 123 bytes, which a descriptive error — a
/// version-mismatch naming the served band, a stale-cursor naming the window — readily
/// exceeds (`ControlFrameTooBig`). So the typed error is sent as data the client parses,
/// and the close frame carries only the short `POLICY` code and the error's machine code as
/// a tiny, always-in-bounds reason. The client reads the error frame, then sees the close.
async fn close_with(socket: &mut WebSocket, err: ProtocolError) {
    let json = serde_json::to_string(&err).unwrap_or_else(|_| err.message.clone());
    let _ = socket.send(Message::Text(json.into())).await;
    // A short, fixed reason that can never exceed the 123-byte control-frame limit.
    let reason = format!("{:?}", err.code);
    let _ = socket
        .send(Message::Close(Some(axum::extract::ws::CloseFrame {
            code: axum::extract::ws::close_code::POLICY,
            reason: reason.into(),
        })))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{HeatId, HeatTransition};

    /// Wrap a bare [`Event`] as a stored log entry (offset/timing irrelevant to the fold here).
    fn stored(event: Event) -> StoredEvent {
        StoredEvent {
            offset: 0,
            recorded_at: None,
            event,
        }
    }

    fn scheduled(id: &str) -> Event {
        Event::HeatScheduled {
            heat: HeatId(id.into()),
            lineup: vec![CompetitorRef("A".into()), CompetitorRef("B".into())],
            class: None,
            round: None,
            frequencies: Vec::new(),
            label: None,
        }
    }

    fn changed(id: &str, transition: HeatTransition) -> Event {
        Event::HeatStateChanged {
            heat: HeatId(id.into()),
            transition,
        }
    }

    /// An event-scope engine on an empty log: `from` and the replay span are both 0, so every
    /// offset these fixtures append afterwards is live tail, walked one at a time.
    fn event_engine() -> Engine {
        event_engine_from(0, 0)
    }

    /// An event-scope engine resuming from log offset `from` with the tail at `replay_until` —
    /// the shape a reconnect builds, where `from..replay_until` is the settled span to replay.
    fn event_engine_from(from: u64, replay_until: u64) -> Engine {
        // A registry holding one freshly-created, round-less event ⇒ no D26 floor, which is what
        // these fixtures always meant. The floor's own conformance proof lives in
        // `tests/min_lap_floor_conformance.rs`.
        let registry = EventRegistry::new(None).expect("in-memory registry");
        let event = registry
            .create(&crate::events::CreateEventRequest::named("Test Event"))
            .expect("create the test event")
            .id;
        let scope = Scope::Event {
            event: event.clone(),
        };
        let floors = RoundFloors { registry, event };
        Engine::new(
            scope.clone(),
            ScopeProjection::of(&scope),
            from,
            replay_until,
            floors,
        )
    }

    /// How many `Change` envelopes a batch of stream messages carries.
    fn change_count(msgs: &[StreamMessage]) -> usize {
        msgs.iter()
            .filter(|m| matches!(m, StreamMessage::Change(_)))
            .count()
    }

    /// The `LiveRaceState` of the nth `Change` in a batch.
    fn live_at(msgs: &[StreamMessage], n: usize) -> &crate::snapshot::LiveRaceState {
        match &msgs[n] {
            StreamMessage::Change(env) => match &env.change {
                Change::FreshValue(ProjectionBody::LiveRaceState(live)) => live,
                other => panic!("expected a LiveRaceState fresh value, got {other:?}"),
            },
            other => panic!("expected a Change envelope, got {other:?}"),
        }
    }

    /// The envelope's echoed resume cursor (the log offset the body was folded through).
    fn env_cursor(msgs: &[StreamMessage], n: usize) -> u64 {
        match &msgs[n] {
            StreamMessage::Change(env) => env.cursor.seq,
            other => panic!("expected a Change envelope, got {other:?}"),
        }
    }

    /// **#422 — the catch-up span is one settled envelope, not a staircase of stale folds.**
    ///
    /// A resume at offset 1 over a five-offset log used to walk offsets 1..5 and emit a fresh
    /// value per changed offset: Staged, Armed, Running — the client rendering each in turn. When
    /// the client was already *past* those (its cursor being a lower bound, not its true
    /// position), that staircase started BELOW where it stood, which is how a live lap count came
    /// to step backwards on a reconnect. Now the span folds once at its end.
    #[test]
    fn the_catch_up_span_collapses_to_one_settled_envelope() {
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Staged)),
            stored(changed("q-1", HeatTransition::Armed)),
            stored(changed("q-1", HeatTransition::Running)),
        ];

        let mut engine = event_engine_from(1, log.len() as u64);
        let out = engine.advance(&log);

        assert_eq!(
            change_count(&out),
            1,
            "the whole replayed span is one envelope, not one per changed offset"
        );
        // The one body is the SETTLED state at the tail — no intermediate phase is ever sent.
        assert_eq!(live_at(&out, 0).phase, crate::snapshot::HeatPhase::Running);
        // And it names the offset it was folded through, so the client's next `from` is exact.
        assert_eq!(env_cursor(&out, 0), log.len() as u64);

        // The live tail is unaffected: a later append still emits its own envelope.
        let mut later = log.clone();
        later.push(stored(changed("q-1", HeatTransition::Finished)));
        let tail = engine.advance(&later);
        assert_eq!(change_count(&tail), 1);
        assert_eq!(env_cursor(&tail, 0), later.len() as u64);
    }

    /// A `HeatScheduled` **inside** a collapsed span must still wake the heats lists (#422 must not
    /// swallow the fill-no-steal wake the plain change-suppression already needed an exception for).
    ///
    /// q-1 is staged and q-2 on deck at the resume point; the span then schedules q-3, which moves
    /// neither `current_heat` nor `on_deck`, so the settled body is byte-identical to what the
    /// client already holds. One envelope must still come out.
    #[test]
    fn a_schedule_inside_the_collapsed_span_still_wakes_the_stream() {
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Staged)),
            stored(scheduled("q-2")),
            stored(scheduled("q-3")),
        ];
        // Resume at offset 3: the span is the bare q-3 schedule alone.
        let mut engine = event_engine_from(3, log.len() as u64);
        let out = engine.advance(&log);

        assert_eq!(
            change_count(&out),
            1,
            "a schedule in the span wakes the stream even though the body did not move"
        );
        let live = live_at(&out, 0);
        assert_eq!(live.current_heat, Some(HeatId("q-1".into())));
        assert_eq!(live.on_deck, Some(HeatId("q-2".into())));
    }

    /// An append that **races the first fold** is live tail, not replay (#422).
    ///
    /// The subscribe frame arrives, the tail is read — and then two changes land before the engine
    /// gets to fold at all. Those are not history: a client subscribed at the tail must see each of
    /// them as its own envelope, exactly as it would have had it won the race. Bounding the span by
    /// the tail read at subscribe (rather than by the log length at the first fold) is what makes
    /// that independent of scheduling.
    #[test]
    fn appends_that_race_the_first_fold_are_live_tail_not_replay() {
        // Subscribed at the tail of a two-offset log…
        let mut engine = event_engine_from(2, 2);
        // …but by the time the first fold runs, two more offsets have landed.
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Staged)),
            stored(changed("q-1", HeatTransition::Armed)),
            stored(changed("q-1", HeatTransition::Running)),
        ];
        let out = engine.advance(&log);

        assert_eq!(
            change_count(&out),
            2,
            "post-subscribe appends stream one envelope per change, collapse or no collapse"
        );
        assert_eq!(live_at(&out, 0).phase, crate::snapshot::HeatPhase::Armed);
        assert_eq!(live_at(&out, 1).phase, crate::snapshot::HeatPhase::Running);
        assert_eq!(env_cursor(&out, 0), 3);
        assert_eq!(env_cursor(&out, 1), 4);
    }

    /// A resume from the **exact** offset an envelope echoed has nothing to replay (#422).
    ///
    /// This is the whole point of echoing the offset: a reconnecting client presents the cursor it
    /// really stands at, the engine seeds `last_emitted` with the identical fold, and the stream is
    /// silent until something genuinely new lands.
    #[test]
    fn a_resume_from_the_echoed_offset_emits_nothing() {
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Staged)),
            stored(changed("q-1", HeatTransition::Running)),
        ];
        let mut first = event_engine();
        let seen = first.advance(&log);
        let resume = env_cursor(&seen, change_count(&seen) - 1);
        assert_eq!(resume, log.len() as u64);

        let mut resumed = event_engine_from(resume, log.len() as u64);
        assert_eq!(
            change_count(&resumed.advance(&log)),
            0,
            "an exact resume cursor leaves nothing to replay"
        );
    }

    #[test]
    fn scheduling_a_heat_wakes_the_stream_even_when_the_body_is_unchanged() {
        // Reproduce the fill-no-steal staleness: q-1 is staged (current), q-2 is on deck. Scheduling
        // a THIRD heat (q-3) appends behind the on-deck heat, so `current_heat` (still q-1) and
        // `on_deck` (still q-2, the earliest scheduled) are unchanged — the folded `LiveRaceState`
        // body is byte-identical. The stream must still emit an envelope so heats lists re-read.
        let mut engine = event_engine();
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Staged)),
            stored(scheduled("q-2")),
        ];
        // Catch up to the current state; the picker would now show q-1 + q-2.
        let _ = engine.advance(&log);

        // Append q-3 — a bare schedule that does not move current/on-deck.
        let mut log3 = log.clone();
        log3.push(stored(scheduled("q-3")));
        let out = engine.advance(&log3);

        // Exactly one fresh-value envelope is emitted for the schedule (the wake), even though the
        // body did not change — so every console re-reads `/heats` and q-3 appears immediately.
        assert_eq!(change_count(&out), 1, "a schedule must wake the stream");
        match &out[0] {
            StreamMessage::Change(env) => match &env.change {
                Change::FreshValue(ProjectionBody::LiveRaceState(live)) => {
                    // current_heat is unchanged — no focus steal.
                    assert_eq!(live.current_heat, Some(HeatId("q-1".into())));
                    assert_eq!(live.on_deck, Some(HeatId("q-2".into())));
                }
                other => panic!("expected a LiveRaceState fresh value, got {other:?}"),
            },
            other => panic!("expected a Change envelope, got {other:?}"),
        }
    }

    #[test]
    fn a_non_schedule_unchanged_fold_still_emits_nothing() {
        // The wake is scoped to `HeatScheduled` offsets only — an offset that leaves the body
        // unchanged for any other reason must stay suppressed (no spurious envelopes).
        let mut engine = event_engine();
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Staged)),
        ];
        let _ = engine.advance(&log);
        // Re-advancing over the SAME log (no new offsets) emits nothing.
        let out = engine.advance(&log);
        assert_eq!(change_count(&out), 0);
    }

    /// **#396 regression — a signal append during a heat with laps must push nothing.**
    ///
    /// Practice repeated its lap callouts because its laps did not come from the log: the engine
    /// walked each offset folding the pure log (practice laps `0`) and then emitted a *second*,
    /// laps-spliced re-snapshot from the overlay. So `last_emitted` alternated between "no laps"
    /// and "N laps", and every append — a `SignalHistory` chunk included, ~2/s/seat — differed from
    /// the previous value twice over and pushed two fresh `LiveRaceState` envelopes carrying the
    /// newest lap. The console's callout detector folds *down* silently on a decrease and then
    /// announces on the next increase, so it re-announced the same lap on every signal tick.
    ///
    /// With practice laps on the log there is exactly one body per offset, so an append that does
    /// not move the projection emits nothing — the behaviour `timed_qual` always had.
    #[test]
    fn a_signal_append_during_a_running_heat_with_laps_emits_nothing() {
        use gridfpv_events::{AdapterId, GateIndex, Pass, SignalHistory, SourceTime};

        let pass = |at: i64, seq: u64| {
            stored(Event::Pass(Pass {
                adapter: AdapterId("sim".into()),
                competitor: CompetitorRef("A".into()),
                at: SourceTime::from_micros(at),
                sequence: Some(seq),
                gate: GateIndex::LAP,
                signal: None,
                heat: Some(HeatId("q-1".into())),
            }))
        };

        let mut engine = event_engine();
        let log = vec![
            stored(scheduled("q-1")),
            stored(changed("q-1", HeatTransition::Running)),
            pass(1_000_000, 0), // holeshot
            pass(4_000_000, 1), // lap 1
        ];
        let caught_up = engine.advance(&log);
        assert!(
            change_count(&caught_up) > 0,
            "the laps themselves are logged changes and DO push envelopes"
        );

        // Now the signal flood: a dense RSSI append that changes no lap and no phase.
        let mut with_signal = log.clone();
        with_signal.push(stored(Event::SignalHistory(SignalHistory {
            adapter: AdapterId("sim".into()),
            competitor: CompetitorRef("A".into()),
            times: vec![0, 1000, 2000],
            rssi: vec![50, 60, 70],
            base: 0,
        })));
        let out = engine.advance(&with_signal);
        assert_eq!(
            change_count(&out),
            0,
            "a signal append must not re-push a LiveRaceState carrying the newest lap (#396)"
        );

        // And a second one, in case the first happened to settle an alternation.
        let mut more_signal = with_signal.clone();
        more_signal.push(stored(Event::SignalHistory(SignalHistory {
            adapter: AdapterId("sim".into()),
            competitor: CompetitorRef("A".into()),
            times: vec![3000, 4000],
            rssi: vec![80, 90],
            base: 3,
        })));
        assert_eq!(change_count(&engine.advance(&more_signal)), 0);
    }
}
