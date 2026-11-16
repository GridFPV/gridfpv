//! The renderer against a real browser and a real ffmpeg: a race is flown, a clean
//! recording of it is put in place, and the overlay is drawn onto it.
//!
//! Local only (`#[ignore]`): it needs Chrome (or Edge, or Chromium), ffmpeg, and the
//! console built (`npm run build` in `frontend/apps/rd-console`).
//!
//! ```text
//! cargo test -p gridfpv-app --test render_live -- --ignored
//! ```
//!
//! - `GRIDFPV_BROWSER` and `GRIDFPV_FFMPEG` name the two programs when they are not where
//!   Grid looks by itself.
//! - `GRIDFPV_RENDER_DIR` is the folder the videos are made in, and left in, so the result
//!   can be looked at. Without it they go to a temporary folder that is removed.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use gridfpv_events::{
    AdapterId, Command, CompletionRule, EndCondition, Event, FirstLap, Format, GateIndex, HeatSeat,
    Pass, PhaseDef, PhaseId, PhaseKind, PilotId, RaceId, RaceRules, SeatRef, SeatSource,
};
use gridfpv_server::app::now_micros;
use gridfpv_server::control::with_event;
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::video::{
    RecordFrom, RenderState, RenderWhen, SaveIn, VideoSettingsRequest, log_video,
};
use gridfpv_server::view::{EventView, VideoOverlay};

async fn eventually<T>(what: &str, secs: u64, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn send(registry: &EventRegistry, event: &EventId, command: Command) {
    with_event(registry, event, |session| session.commands(vec![command])).unwrap();
}

fn view(registry: &EventRegistry, event: &EventId) -> EventView {
    with_event(registry, event, |session| {
        Ok(EventView::build(session.state(), session.now))
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a browser, ffmpeg and the built console"]
async fn the_overlay_is_drawn_onto_a_clean_recording() {
    let assets =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../frontend/apps/rd-console/dist");
    assert!(
        assets.join("index.html").is_file(),
        "build the console first: npm run build in frontend/apps/rd-console"
    );
    let browser = std::env::var("GRIDFPV_BROWSER").unwrap_or_default();
    let ffmpeg = std::env::var("GRIDFPV_FFMPEG").unwrap_or_default();
    let kept = std::env::var_os("GRIDFPV_RENDER_DIR").map(PathBuf::from);
    let dir = kept.clone().unwrap_or_else(|| {
        std::env::temp_dir().join(format!("gridfpv-render-live-{}", std::process::id()))
    });
    std::fs::create_dir_all(&dir).unwrap();

    // ── A Director, serving the console the browser will open ────────────────────────────
    let registry = EventRegistry::new(None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let app = gridfpv_app::director::build_app(registry.clone(), &assets);
    // The gate needs each caller's address, as the real Director gives it.
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });

    registry
        .video()
        .update(&VideoSettingsRequest {
            enabled: false,
            obs_address: "127.0.0.1:4455".into(),
            obs_password: None,
            tail_seconds: 1,
            trash_discarded: true,
            record_from: RecordFrom::Obs,
            clean_scene: String::new(),
            overlay_scene: String::new(),
            render_when: RenderWhen::Asked,
            browser_path: browser,
            ffmpeg_path: ffmpeg,
            save_in: SaveIn::Obs,
        })
        .unwrap();
    let renderer = gridfpv_app::render::spawn(registry.clone(), base);
    let tools = eventually("the two programs to be found", 10, || {
        let tools = registry.video().status().tools;
        (tools.browser.is_some() && tools.ffmpeg.is_some()).then_some(tools)
    })
    .await;

    // ── A race: a one second countdown, a hole shot and three laps ───────────────────────
    let event = registry
        .create(&CreateEventRequest::named("Render night"))
        .unwrap()
        .id;
    registry.set_active(&event).unwrap();
    let bench = AdapterId("bench".into());
    let seat = SeatRef("seat-1".into());
    let heat = PhaseDef {
        id: PhaseId("heat".into()),
        label: "Track 2".into(),
        parent: None,
        kind: PhaseKind::Heat {
            lineup: vec![HeatSeat {
                adapter: bench.clone(),
                seat: seat.clone(),
                source: SeatSource::Pilot(PilotId("ana".into())),
            }],
        },
        format: Some(Format {
            name: "Open".into(),
            race: RaceRules {
                end: EndCondition::Open,
                staging_micros: 1_000_000,
                min_lap_micros: None,
                first_lap: FirstLap::HoleShot,
                stop_after_all_finish_micros: None,
                max_micros: None,
            },
            completion: CompletionRule::DirectorDecides,
            ranking: None,
            points: Vec::new(),
        }),
        false_start_limit: None,
        display_hint: None,
    };
    send(
        &registry,
        &event,
        Command::Record(Box::new(Event::PhaseDefined { def: heat })),
    );
    let race = RaceId("heat#1".into());
    // The recording "begins" as the countdown does, as the recorder's would.
    let first_frame_at = now_micros();
    send(
        &registry,
        &event,
        Command::StartRace {
            heat: PhaseId("heat".into()),
        },
    );
    eventually("the race to start", 10, || {
        view(&registry, &event).races[0].started_at
    })
    .await;
    for wait_ms in [500, 1500, 1400, 1600] {
        tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        with_event(&registry, &event, |session| {
            session.observe(Event::Pass(Pass {
                adapter: bench.clone(),
                seat: seat.clone(),
                at: gridfpv_events::SourceTime {
                    micros: now_micros(),
                },
                sequence: None,
                gate: GateIndex::LAP,
                signal: None,
                race: None,
            }))
        })
        .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    send(&registry, &event, Command::StopRace { race: race.clone() });
    send(
        &registry,
        &event,
        Command::SaveRace {
            race: race.clone(),
            needs_marshal: false,
            note: None,
        },
    );
    assert_eq!(view(&registry, &event).races[0].seats[0].laps.len(), 3);

    // ── Its clean recording: nine seconds of a test picture, with sound ──────────────────
    let clean = dir.join("Render night Track 2 race 1 - Clean.mkv");
    let made = tokio::process::Command::new(tools.ffmpeg.as_deref().unwrap())
        .args(["-y", "-hide_banner", "-loglevel", "error"])
        .args(["-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=30"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440"])
        .args([
            "-t",
            "9",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-pix_fmt",
            "yuv420p",
        ])
        .args(["-c:a", "aac"])
        .arg(&clean)
        .status()
        .await
        .unwrap();
    assert!(made.success(), "ffmpeg could not make the test recording");
    log_video(
        &registry,
        &event,
        Event::VideoRecorded {
            race: race.clone(),
            file: clean.to_string_lossy().into_owned(),
            first_frame_at,
            clean: true,
            overlay_file: None,
            overlay_first_frame_at: None,
            laps_key: None,
        },
    );
    let video = view(&registry, &event).races[0].video.clone().unwrap();
    assert_eq!(video.overlay, VideoOverlay::Missing);

    // ── Drawn ────────────────────────────────────────────────────────────────────────────
    registry
        .video()
        .ask_render(&event, &race, "Render night Track 2 race 1");
    let started = Instant::now();
    eventually("the overlay to be drawn", 300, || {
        let job = registry.video().status().renders.into_iter().next()?;
        assert_ne!(job.state, RenderState::Failed, "{:?}", job.note);
        (job.state == RenderState::Done).then_some(())
    })
    .await;
    eprintln!("drawn in {:.1} s", started.elapsed().as_secs_f64());
    let video = view(&registry, &event).races[0].video.clone().unwrap();
    assert_eq!(video.overlay, VideoOverlay::Rendered);
    let out = dir.join("Render night Track 2 race 1 - Overlay (redrawn).mp4");
    assert_eq!(
        video.overlaid_file.as_deref(),
        Some(out.to_string_lossy().as_ref())
    );
    assert!(std::fs::metadata(&out).unwrap().len() > 10_000);
    assert!(!PathBuf::from(format!("{}.part", out.display())).exists());

    // ── A lap is thrown out afterwards: the video no longer matches, and says so ─────────
    let thrown = view(&registry, &event).races[0].seats[0].laps[1].crossing;
    send(
        &registry,
        &event,
        Command::Record(Box::new(Event::LapRemoved {
            race: race.clone(),
            crossing: thrown,
            reason: None,
            note: None,
        })),
    );
    let video = view(&registry, &event).races[0].video.clone().unwrap();
    assert_eq!(video.overlay, VideoOverlay::NeedsRender);
    assert_eq!(video.overlaid_file, None);

    renderer.abort();
    server.abort();
    if kept.is_none() {
        std::fs::remove_dir_all(dir).unwrap();
    }
}
