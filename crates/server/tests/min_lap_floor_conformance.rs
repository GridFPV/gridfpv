//! **D26 conformance: one floor, every surface** (`docs/decisions.html#d26`, #409).
//!
//! D26 says the minimum-lap floor is *"applied identically in the lap list, the live view, and
//! every scoring path"*. That sentence is a claim about the whole system, and until #409 it was
//! **false**: the floor lives in registry meta (`RoundDef::min_lap_secs`), not in the log, so
//! every pure-log fold — the event- and class-scope snapshots, and all three live-state scopes on
//! the WebSocket change stream — counted a sub-floor echo pass the heat's lap list had suppressed.
//! An RD watching the live board saw one lap count and the marshaling list showed another.
//!
//! So this file holds ONE test, and it is not a test of the fix — it is the decision itself,
//! executed. Over a single log, under a single round's floor, it reads the lap count off **every
//! surface that reports one**:
//!
//! | surface | how it is read here |
//! |---|---|
//! | the lap list | `GET /snapshot/heat/{h}?projection=laps` |
//! | live, heat scope | `GET /snapshot/heat/{h}` and the heat-scope WS subscription |
//! | live, class scope | `GET /snapshot/class/{e}/{c}` and the class-scope WS subscription |
//! | live, event scope | `GET /snapshot/event/{e}` and the event-scope WS subscription |
//! | scoring, per heat | `GET /snapshot/heat/{h}?projection=result` |
//! | scoring, per round | `GET /rounds/{r}/standings` |
//!
//! …and asserts they are all the same number. Nine readings, one answer.
//!
//! **The log contains a genuine sub-floor echo**, because that is the only case in which these
//! surfaces *can* disagree — a log the floor does not touch would let a floorless fold pass this
//! test. The test states the divergent value explicitly (`UNFLOORED_LAPS`) and asserts the floored
//! answer differs from it, so it can never quietly become a tautology.
//!
//! It is a **pure protocol test**: an in-memory registry, the real `router`, a real WebSocket.
//! No Docker, no timer.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use futures_util::{SinkExt, StreamExt};
use gridfpv_engine::scoring::WinCondition;
use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, HeatId, HeatTransition, Pass, SourceTime,
};
use gridfpv_server::app::{AppState, router};
use gridfpv_server::classes::CreateClassRequest;
use gridfpv_server::events::{
    ChannelMode, CreateEventRequest, EventRegistry, MemberSlot, NewRoundReq, RoundDef, SeedingRule,
};
use gridfpv_server::pilots::CreatePilotRequest;
use gridfpv_server::round_engine::RoundStanding;
use gridfpv_server::scope::{ClassId, EventId, Scope, SubscribeRequest};
use gridfpv_server::snapshot::{LiveRaceState, ProjectionBody, Snapshot};
use gridfpv_server::stream::{Change, Cursor, StreamMessage};
use http_body_util::BodyExt;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tower::ServiceExt;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const SECOND: i64 = 1_000_000;

/// The round's floor: 10s, the value RotorHazard was configured with in the Audit Shakedown that
/// produced D26.
const MIN_LAP_SECS: u32 = 10;

/// What every surface must report for the echoing pilot once the floor is applied: the 1s echo is
/// suppressed, so their two real ~20s laps are the whole story.
const FLOORED_LAPS: u32 = 2;

/// What a **floorless** fold reports for the same pilot — the phantom 0-to-1s "lap" counted as a
/// third. This is the number the live view used to show while the lap list showed
/// [`FLOORED_LAPS`], and it is what makes this a real test rather than a tautology: if the two ever
/// coincide, the log has stopped exercising the floor.
const UNFLOORED_LAPS: u32 = 3;

/// The unaffected pilot: no echo, so the floor moves nothing. Present so the test proves the floor
/// is *applied*, not that some surface is uniformly under-counting.
const CLEAN_LAPS: u32 = 1;

// ---------------------------------------------------------------------------------------
// The rig: one event, one class, one floored round, one heat with an echo pass.
// ---------------------------------------------------------------------------------------

/// Everything the surfaces need addressing.
struct Rig {
    registry: EventRegistry,
    state: AppState,
    event: EventId,
    class: ClassId,
    round: RoundDef,
    heat: HeatId,
    /// The pilot whose gate echoed (their ref is their pilot id).
    echoing: CompetitorRef,
    /// The pilot with a clean run.
    clean: CompetitorRef,
    /// The log length once the passes are down — the offset the streams resume at, so the only
    /// envelopes they see carry the finish transitions.
    tail: u64,
}

/// Build the event/class/round/roster in the registry, then lay down the heat's log **up to and
/// including its passes** (the finish transitions are appended later, after the streams subscribe,
/// so each socket observes the finish rather than being handed it as history).
fn rig() -> Rig {
    let registry = EventRegistry::new(None).expect("in-memory registry");

    let class = registry
        .classes()
        .create(&CreateClassRequest {
            name: "Open".into(),
            ..Default::default()
        })
        .expect("class created");

    let echoing = registry
        .pilots()
        .create(&CreatePilotRequest {
            callsign: "Echo".into(),
            ..Default::default()
        })
        .expect("pilot created");
    let clean = registry
        .pilots()
        .create(&CreatePilotRequest {
            callsign: "Clean".into(),
            ..Default::default()
        })
        .expect("pilot created");

    let meta = registry
        .create(&CreateEventRequest {
            name: "Floor Conformance".into(),
            date: None,
            location: None,
            description: None,
            organizer: None,
        })
        .expect("event created");
    let event = meta.id.clone();

    registry
        .set_classes(&event, vec![class.id.clone()])
        .expect("class selected");
    registry
        .set_roster(&event, vec![echoing.id.clone(), clean.id.clone()])
        .expect("roster set");
    registry
        .set_class_membership(
            &event,
            class.id.clone(),
            vec![
                MemberSlot::new(echoing.id.clone()),
                MemberSlot::new(clean.id.clone()),
            ],
        )
        .expect("membership set");

    // The round that carries the D26 floor. Everything else is the plainest qualifying round the
    // validator accepts — the floor is the only interesting field.
    let round = registry
        .add_round(
            &event,
            NewRoundReq {
                label: "Qualifying".into(),
                classes: vec![class.id.clone()],
                format: "timed_qual".into(),
                params: BTreeMap::from([("rounds".to_string(), "1".to_string())]),
                win_condition: Some(WinCondition::Timed {
                    window_micros: 120 * SECOND,
                }),
                seeding: SeedingRule::FromRoster,
                time_limit_secs: Some(120),
                channel_mode: Some(ChannelMode::Static),
                // The one field this whole file is about.
                min_lap_secs: Some(MIN_LAP_SECS),
                staging_timer_secs: None,
                start_procedure: None,
                grace_window: None,
                protest_window: None,
            },
        )
        .expect("round added");

    let state = registry.resolve(&event).expect("event log");
    let heat = HeatId(format!("{}-h1", round.id.0));
    let echo_ref = CompetitorRef(echoing.id.0.clone());
    let clean_ref = CompetitorRef(clean.id.0.clone());

    let pass = |competitor: &CompetitorRef, at: i64, sequence: u64| {
        Event::Pass(Pass {
            adapter: AdapterId("sim".into()),
            competitor: competitor.clone(),
            at: SourceTime::from_micros(at),
            sequence: Some(sequence),
            gate: GateIndex::LAP,
            signal: None,
            heat: Some(heat.clone()),
        })
    };

    let log = vec![
        Event::HeatScheduled {
            heat: heat.clone(),
            lineup: vec![echo_ref.clone(), clean_ref.clone()],
            class: Some(class.id.clone()),
            round: Some(round.id.clone()),
            frequencies: Vec::new(),
            label: None,
        },
        Event::HeatStateChanged {
            heat: heat.clone(),
            transition: HeatTransition::Running,
        },
        // The echoing pilot: holeshot, then a **1s gate echo** — well under the 10s floor — then
        // two real ~20s laps. This is the Audit Shakedown's phantom lap, in miniature.
        pass(&echo_ref, 0, 0),
        pass(&echo_ref, SECOND, 1),
        pass(&echo_ref, 20 * SECOND, 2),
        pass(&echo_ref, 40 * SECOND, 3),
        // The clean pilot: holeshot and one honest lap. The floor must not move this.
        pass(&clean_ref, 0, 4),
        pass(&clean_ref, 25 * SECOND, 5),
    ];
    let tail = log.len() as u64;
    for event in log {
        state.append(event, None).expect("append");
    }

    Rig {
        tail,
        registry,
        state,
        event,
        class: class.id,
        round,
        heat,
        echoing: echo_ref,
        clean: clean_ref,
    }
}

/// Finish and finalize the heat — two appends, so a stream subscribed at the pre-finish tail
/// observes exactly two projection changes. Finalizing is also what lets the round's scoring
/// paths (`completed_heats`) see the heat at all.
fn finish_heat(rig: &Rig) {
    for transition in [HeatTransition::Finished, HeatTransition::Finalized] {
        rig.state
            .append(
                Event::HeatStateChanged {
                    heat: rig.heat.clone(),
                    transition,
                },
                None,
            )
            .expect("append");
    }
}

// ---------------------------------------------------------------------------------------
// Reading a lap count off each surface.
// ---------------------------------------------------------------------------------------

/// A surface's answer: competitor → laps completed. Every surface must produce the same map.
type Counts = BTreeMap<CompetitorRef, u32>;

/// The lap counts a [`LiveRaceState`] reports (the live board's own numbers).
fn counts_of_live(live: &LiveRaceState) -> Counts {
    live.progress
        .iter()
        .map(|p| (p.competitor.clone(), p.laps_completed))
        .collect()
}

/// `GET` a JSON body off the real router, with no network in the way.
async fn get_json<T: serde::de::DeserializeOwned>(registry: &EventRegistry, uri: &str) -> T {
    let response = router(registry.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        response.status().is_success(),
        "{uri} failed: {}",
        response.status()
    );
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{uri} body did not parse: {e}"))
}

/// The `LiveRaceState` of a snapshot response.
fn snapshot_live(snapshot: Snapshot) -> LiveRaceState {
    match snapshot.body {
        ProjectionBody::LiveRaceState(live) => live,
        other => panic!("expected a LiveRaceState, got {other:?}"),
    }
}

/// Serve the real router on an ephemeral port; the returned handle is dropped (aborting the task)
/// at test end.
async fn serve(registry: &EventRegistry) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(registry.clone());
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("ws://{addr}"), handle)
}

/// Subscribe one scope's change stream, resuming **at the current tail** so the socket emits
/// nothing until the finish transitions land.
async fn subscribe(base: &str, event: &EventId, scope: Scope, from: u64) -> Ws {
    let url = format!("{base}/events/{}/stream", event.0);
    let (mut ws, _) = connect_async(url).await.unwrap();
    let request = SubscribeRequest {
        scope,
        from: Some(Cursor::new(from)),
        contract_version: None,
        token: None,
    };
    ws.send(Message::text(serde_json::to_string(&request).unwrap()))
        .await
        .unwrap();
    ws
}

/// Read change envelopes until the stream falls silent, and return the `LiveRaceState` of the
/// **last** — the stream's settled value for that scope.
///
/// Deliberately not "read exactly N frames". How many envelopes a batch of appends produces is
/// not a floor question, and it is not fixed: a subscribe races the appends that follow it, and
/// whatever was already on the log when the server began serving this subscription is delivered
/// as one collapsed catch-up fold rather than one envelope per offset (#422). What this file
/// asserts — that every surface reports the same floored lap count — is a property of the
/// *settled* value, so settle first and read that.
async fn settled_live_of(ws: &mut Ws) -> LiveRaceState {
    let mut last = None;
    // The first frame may take a moment (the appends have to land and wake the stream); once one
    // has arrived, a short silence means the stream has settled.
    let mut patience = Duration::from_secs(5);
    while let Ok(frame) = tokio::time::timeout(patience, ws.next()).await {
        let frame = frame
            .expect("stream closed unexpectedly")
            .expect("websocket error");
        let message: StreamMessage = match frame {
            Message::Text(text) => serde_json::from_str(&text).expect("parse StreamMessage"),
            other => panic!("expected a text frame, got {other:?}"),
        };
        match message {
            StreamMessage::Change(envelope) => match envelope.change {
                Change::FreshValue(ProjectionBody::LiveRaceState(live)) => last = Some(live),
                other => panic!("expected a LiveRaceState fresh value, got {other:?}"),
            },
            other => panic!("expected a Change envelope, got {other:?}"),
        }
        patience = Duration::from_millis(300);
    }
    last.expect("timed out waiting for a stream frame")
}

// ---------------------------------------------------------------------------------------
// The decision, executed.
// ---------------------------------------------------------------------------------------

/// **D26** — *"applied identically in the lap list, the live view, and every scoring path"*.
///
/// One log, one floor, nine readings of the same number. This is the test that would have failed
/// the day the push path diverged (#409): the event-, class- and heat-scope live folds took no
/// floor at all, so with a sub-floor echo on the log they reported one lap MORE than the lap list
/// and the score. Anything that reintroduces a floorless live fold fails here.
#[tokio::test]
async fn the_min_lap_floor_is_identical_on_every_surface_that_reports_a_lap_count() {
    let rig = rig();
    let tail = rig.tail;

    // Subscribe all three live scopes at the tail, BEFORE the finish transitions, so each observes
    // exactly the two changes those transitions make.
    let (base, _server) = serve(&rig.registry).await;
    let mut event_ws = subscribe(
        &base,
        &rig.event,
        Scope::Event {
            event: rig.event.clone(),
        },
        tail,
    )
    .await;
    let mut class_ws = subscribe(
        &base,
        &rig.event,
        Scope::Class {
            event: rig.event.clone(),
            class: rig.class.clone(),
        },
        tail,
    )
    .await;
    let mut heat_ws = subscribe(
        &base,
        &rig.event,
        Scope::Heat {
            heat: rig.heat.clone(),
        },
        tail,
    )
    .await;

    finish_heat(&rig);

    let stream_event = settled_live_of(&mut event_ws).await;
    let stream_class = settled_live_of(&mut class_ws).await;
    let stream_heat = settled_live_of(&mut heat_ws).await;

    // --- The lap list: D26's reference surface, and the one that was always right. ---------
    let laps: Snapshot = get_json(
        &rig.registry,
        &format!(
            "/events/{}/snapshot/heat/{}?projection=laps",
            rig.event.0, rig.heat.0
        ),
    )
    .await;
    let lap_list = match laps.body {
        ProjectionBody::LapList(list) => list,
        other => panic!("expected a LapList, got {other:?}"),
    };
    let mut from_lap_list: Counts = Counts::new();
    for competitor in &lap_list.competitors {
        *from_lap_list
            .entry(competitor.competitor.competitor.clone())
            .or_insert(0) += competitor.lap_count() as u32;
    }

    // The floor is genuinely biting: the echo is gone from the lap list, and the answer differs
    // from what a floorless fold would say. Without this the whole comparison below could pass on
    // a log the floor never touches.
    assert_eq!(
        from_lap_list.get(&rig.echoing).copied(),
        Some(FLOORED_LAPS),
        "the lap list must suppress the sub-floor echo (D26)"
    );
    assert_ne!(
        FLOORED_LAPS, UNFLOORED_LAPS,
        "the fixture must contain an echo the floor actually removes"
    );
    assert_eq!(
        from_lap_list.get(&rig.clean).copied(),
        Some(CLEAN_LAPS),
        "the floor must not touch a clean run"
    );

    // --- Every scoring path. --------------------------------------------------------------
    let result: Snapshot = get_json(
        &rig.registry,
        &format!(
            "/events/{}/snapshot/heat/{}?projection=result",
            rig.event.0, rig.heat.0
        ),
    )
    .await;
    let from_heat_result: Counts = match result.body {
        ProjectionBody::HeatResult(result) => result
            .places
            .iter()
            .map(|p| (p.competitor.competitor.clone(), p.laps))
            .collect(),
        other => panic!("expected a HeatResult, got {other:?}"),
    };

    let standings: Vec<RoundStanding> = get_json(
        &rig.registry,
        &format!(
            "/events/{}/rounds/{}/standings",
            rig.event.0, rig.round.id.0
        ),
    )
    .await;
    assert!(
        !standings.is_empty(),
        "the round standings must have scored the finalized heat"
    );
    let from_round_standings: Counts = standings
        .iter()
        .map(|s| (s.competitor.clone(), s.laps))
        .collect();

    // --- Every live scope, over both transports. -------------------------------------------
    let snapshot_of = |uri: String| {
        let registry = rig.registry.clone();
        async move { snapshot_live(get_json::<Snapshot>(&registry, &uri).await) }
    };
    let snapshot_event = snapshot_of(format!("/events/{0}/snapshot/event/{0}", rig.event.0)).await;
    let snapshot_class = snapshot_of(format!(
        "/events/{0}/snapshot/class/{0}/{1}",
        rig.event.0, rig.class.0
    ))
    .await;
    let snapshot_heat = snapshot_of(format!(
        "/events/{}/snapshot/heat/{}",
        rig.event.0, rig.heat.0
    ))
    .await;

    // --- One answer. ------------------------------------------------------------------------
    let surfaces: Vec<(&str, Counts)> = vec![
        ("lap list (heat snapshot, ?projection=laps)", from_lap_list),
        (
            "scoring — heat result (?projection=result)",
            from_heat_result,
        ),
        ("scoring — round standings", from_round_standings),
        (
            "live — event scope, HTTP snapshot",
            counts_of_live(&snapshot_event),
        ),
        (
            "live — class scope, HTTP snapshot",
            counts_of_live(&snapshot_class),
        ),
        (
            "live — heat scope, HTTP snapshot",
            counts_of_live(&snapshot_heat),
        ),
        (
            "live — event scope, change stream",
            counts_of_live(&stream_event),
        ),
        (
            "live — class scope, change stream",
            counts_of_live(&stream_class),
        ),
        (
            "live — heat scope, change stream",
            counts_of_live(&stream_heat),
        ),
    ];

    let expected: Counts = Counts::from([
        (rig.echoing.clone(), FLOORED_LAPS),
        (rig.clean.clone(), CLEAN_LAPS),
    ]);
    for (name, counts) in &surfaces {
        assert_eq!(
            counts, &expected,
            "D26 (docs/decisions.html#d26): {name} disagrees about the lap count under a \
             {MIN_LAP_SECS}s min-lap floor. The floor must be applied identically in the lap \
             list, the live view, and every scoring path — see #409, where the change stream \
             and the event/class snapshots folded the log with no floor and counted the echo \
             pass as a {UNFLOORED_LAPS}rd lap."
        );
    }
}

/// The floor also reaches the **crossing feed** on the push path (#397 + #409): the echo is not
/// merely absent from the lap count, it is labelled `RejectedTooShort` — *"the gate fired and it
/// did not count"*. With no floor on the stream this disposition could never appear at all, which
/// is what made #397's most valuable signal dark on the surface an RD actually watches.
#[tokio::test]
async fn the_event_scope_stream_labels_the_sub_floor_echo_rejected() {
    use gridfpv_projection::CrossingDisposition;

    let rig = rig();
    let tail = rig.tail;
    let (base, _server) = serve(&rig.registry).await;
    let mut ws = subscribe(
        &base,
        &rig.event,
        Scope::Event {
            event: rig.event.clone(),
        },
        tail,
    )
    .await;
    finish_heat(&rig);
    let live = settled_live_of(&mut ws).await;

    let rejected: Vec<_> = live
        .crossings
        .iter()
        .filter(|c| c.disposition == CrossingDisposition::RejectedTooShort)
        .collect();
    assert_eq!(
        rejected.len(),
        1,
        "the event-scope stream must label the echo RejectedTooShort, not Counted"
    );
    assert_eq!(rejected[0].competitor, rig.echoing);
    assert_eq!(rejected[0].at, SourceTime::from_micros(SECOND));
}
