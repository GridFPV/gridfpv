//! The recorder against a real OBS: races are recorded, named, logged and dropped.
//!
//! Local only (`#[ignore]`): it needs an OBS with its WebSocket server on. grid-rh's
//! headless OBS container does (`make obs-up` there: port 4455, password `gridfpv`).
//!
//! ```text
//! GRIDFPV_OBS_ADDR=127.0.0.1:4455 GRIDFPV_OBS_PASS=gridfpv \
//!   cargo test -p gridfpv-app --test obs_live -- --ignored --test-threads=1
//! ```
//!
//! The scenes test also needs the Source Record plugin and two scenes, `Clean` and `Scene`
//! (or name them in `GRIDFPV_OBS_CLEAN` and `GRIDFPV_OBS_OVERLAY`).
//!
//! That OBS writes into a folder this test cannot see by the same path, which is the
//! "OBS on another machine" case: files are named and logged, and a discarded race's
//! recording is left where it is, with a note saying why.

use std::time::{Duration, Instant};

use serde_json::json;

use gridfpv_events::{
    AdapterId, Command, CompletionRule, EndCondition, Event, FirstLap, Format, HeatSeat, PhaseDef,
    PhaseId, PhaseKind, PilotId, RaceId, RaceRules, SeatRef, SeatSource,
};
use gridfpv_server::control::with_event;
use gridfpv_server::events::{CreateEventRequest, EventId, EventRegistry};
use gridfpv_server::video::{
    RecordFrom, RenderWhen, SaveIn, VideoRun, VideoRunState, VideoSettingsRequest,
};
use gridfpv_server::view::VideoOverlay;

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

fn run_of(registry: &EventRegistry, race: &RaceId) -> Option<VideoRun> {
    registry
        .video()
        .status()
        .runs
        .into_iter()
        .find(|run| &run.race == race)
}

fn events(registry: &EventRegistry, event: &EventId) -> Vec<Event> {
    registry
        .log(event)
        .unwrap()
        .lock()
        .unwrap()
        .read_all()
        .unwrap()
        .into_iter()
        .map(|stored| stored.event)
        .collect()
}

/// One heat, "Track 2", with one pilot and a one second countdown.
fn define_heat(registry: &EventRegistry, event: &EventId) {
    let heat = PhaseDef {
        id: PhaseId("heat".into()),
        label: "Track 2".into(),
        parent: None,
        kind: PhaseKind::Heat {
            lineup: vec![HeatSeat {
                adapter: AdapterId("bench".into()),
                seat: SeatRef("seat-1".into()),
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
        registry,
        event,
        Command::Record(Box::new(Event::PhaseDefined { def: heat })),
    );
}

/// `GRIDFPV_OBS_DATA_DIR`: a data folder OBS can see by the same path. Set, the recordings
/// go into Grid's own `videos` folder there; unset, they go where OBS records.
fn data_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("GRIDFPV_OBS_DATA_DIR").map(Into::into)
}

fn save_in() -> SaveIn {
    if data_dir().is_some() {
        SaveIn::Grid
    } else {
        SaveIn::Obs
    }
}

/// With a data folder, the file is in Grid's `videos` folder and really there.
fn assert_in_grid_folder(registry: &EventRegistry, file: &str) {
    if data_dir().is_none() {
        return;
    }
    let videos = registry.video().videos_dir().unwrap();
    assert!(
        std::path::Path::new(file).starts_with(&videos),
        "{file} is not in {}",
        videos.display()
    );
    assert!(std::path::Path::new(file).is_file(), "{file} is not there");
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an OBS with its WebSocket server on: set GRIDFPV_OBS_ADDR"]
async fn races_are_recorded_named_logged_and_dropped_by_a_real_obs() {
    let address = std::env::var("GRIDFPV_OBS_ADDR").unwrap_or_else(|_| "127.0.0.1:4455".into());
    let password = std::env::var("GRIDFPV_OBS_PASS").unwrap_or_default();

    let registry = EventRegistry::new(data_dir()).unwrap();
    // A name with characters OBS and Windows would trip on, to prove they are cleaned.
    let stamp = std::process::id();
    let event_name = format!("Live 100% {stamp}: night");
    let event = registry
        .create(&CreateEventRequest::named(&event_name))
        .unwrap()
        .id;
    registry.set_active(&event).unwrap();
    define_heat(&registry, &event);

    let recorder = gridfpv_app::video::spawn(registry.clone());

    // A wrong password is said in words, and the right one connects.
    let settings = |password: &str| VideoSettingsRequest {
        enabled: true,
        obs_address: address.clone(),
        obs_password: Some(password.to_owned()),
        tail_seconds: 1,
        trash_discarded: true,
        record_from: RecordFrom::Obs,
        clean_scene: String::new(),
        overlay_scene: String::new(),
        render_when: RenderWhen::Asked,
        browser_path: String::new(),
        ffmpeg_path: String::new(),
        save_in: save_in(),
    };
    if !password.is_empty() {
        registry
            .video()
            .update(&settings("not the password"))
            .unwrap();
        let problem = eventually("the wrong password to be refused", 15, || {
            registry.video().status().obs.problem
        })
        .await;
        assert!(problem.contains("password"), "{problem}");
    }
    registry.video().update(&settings(&password)).unwrap();
    let obs = eventually("OBS to connect", 15, || {
        let obs = registry.video().status().obs;
        obs.connected.then_some(obs)
    })
    .await;
    assert!(
        obs.version
            .as_deref()
            .unwrap_or_default()
            .starts_with("OBS "),
        "{obs:?}"
    );
    assert!(obs.record_folder.is_some() && !obs.recording, "{obs:?}");

    // ── Race 1: recorded from the countdown, stopped after the tail, kept at the save ────
    let first = RaceId("heat#1".into());
    send(
        &registry,
        &event,
        Command::StartRace {
            heat: PhaseId("heat".into()),
        },
    );
    let recording = eventually("the recording to start", 15, || {
        run_of(&registry, &first).filter(|run| run.state == VideoRunState::Recording)
    })
    .await;
    let name = format!("Live 100% {stamp}: night Track 2 race 1");
    assert_eq!(recording.name, name);
    assert!(registry.video().status().obs.recording);
    tokio::time::sleep(Duration::from_millis(2500)).await;
    send(
        &registry,
        &event,
        Command::StopRace {
            race: first.clone(),
        },
    );
    let written = eventually("the recording to be written", 15, || {
        run_of(&registry, &first).filter(|run| run.state == VideoRunState::WaitingForSave)
    })
    .await;
    // The file carries the race's name, cleaned of what a file name cannot hold.
    let file = written.file.clone().unwrap();
    let stem = format!("Live 100 {stamp} night Track 2 race 1");
    assert!(file.contains(&stem), "{file}");
    assert_in_grid_folder(&registry, &file);
    // The countdown was one second, and recording began as it did: the tone is in the
    // file, within a couple of seconds of its first frame.
    let start = written.start_at_ms.unwrap();
    assert!(
        (0..3000).contains(&start),
        "the tone is {start} ms into the file"
    );
    assert!(!registry.video().status().obs.recording);

    send(
        &registry,
        &event,
        Command::SaveRace {
            race: first.clone(),
            needs_marshal: false,
            note: None,
        },
    );
    eventually("the run to be saved", 10, || {
        run_of(&registry, &first).filter(|run| run.state == VideoRunState::Saved)
    })
    .await;
    let logged = events(&registry, &event);
    assert!(
        logged.iter().any(|fact| matches!(
            fact,
            Event::VideoRecorded { race, file: logged_file, .. } if race == &first && logged_file == &file
        )),
        "the recording is logged into the event"
    );
    // And the snapshot carries it on the race.
    let view = with_event(&registry, &event, |session| {
        Ok(gridfpv_server::view::EventView::build(
            session.state(),
            session.now,
        ))
    })
    .unwrap();
    assert_eq!(
        view.races[0]
            .video
            .as_ref()
            .map(|video| video.file.as_str()),
        Some(file.as_str())
    );

    // ── Race 2: called off in the countdown. The recording stops at once and is dropped ──
    let second = RaceId("heat#2".into());
    send(
        &registry,
        &event,
        Command::StartRace {
            heat: PhaseId("heat".into()),
        },
    );
    eventually("the second recording to start", 15, || {
        let run = run_of(&registry, &second)?;
        assert_ne!(run.state, VideoRunState::Failed, "{:?}", run.note);
        (run.state == VideoRunState::Recording).then_some(run)
    })
    .await;
    send(
        &registry,
        &event,
        Command::DiscardRace {
            race: second.clone(),
            reason: None,
            note: None,
        },
    );
    let dropped = eventually("the second run to be dropped", 15, || {
        run_of(&registry, &second).filter(|run| {
            matches!(
                run.state,
                VideoRunState::Trashed | VideoRunState::DiscardedKept
            )
        })
    })
    .await;
    assert!(!registry.video().status().obs.recording);
    // This OBS writes where the test cannot reach by the same path, so the file stays and
    // the run says why. (On the OBS machine itself it would be `Trashed`.)
    if dropped.state == VideoRunState::DiscardedKept {
        assert!(dropped.note.unwrap().contains("another machine"));
    }
    assert!(
        !events(&registry, &event).iter().any(|fact| matches!(
            fact,
            Event::VideoRecorded { race, .. } if race == &second
        )),
        "a discarded race's recording is never logged"
    );

    // ── Race 1 is discarded after all (a false start called late): its recording goes too ─
    send(
        &registry,
        &event,
        Command::DiscardRace {
            race: first.clone(),
            reason: None,
            note: None,
        },
    );
    eventually("the saved run to be dropped", 10, || {
        run_of(&registry, &first).filter(|run| {
            matches!(
                run.state,
                VideoRunState::Trashed | VideoRunState::DiscardedKept
            )
        })
    })
    .await;
    assert!(
        events(&registry, &event)
            .iter()
            .any(|fact| matches!(fact, Event::VideoRemoved { race } if race == &first))
    );

    // OBS's own file naming is back as it was, and nothing is left to put back.
    assert_eq!(registry.video().stored().obs_naming_before, None);
    // The same for its recording folder: OBS records where it did before the race.
    assert_eq!(registry.video().stored().obs_folder_before, None);
    let folder_now = eventually("OBS to connect again", 15, || {
        registry.video().status().obs.record_folder
    })
    .await;
    assert_eq!(Some(folder_now), obs.record_folder);

    // Switched off, the link to OBS stays (the scenes are read with recording off), and a
    // race that starts is not recorded.
    let mut off = settings(&password);
    off.enabled = false;
    registry.video().update(&off).unwrap();
    eventually("OBS to connect again", 15, || {
        registry.video().status().obs.connected.then_some(())
    })
    .await;
    let runs_before = registry.video().status().runs.len();
    send(
        &registry,
        &event,
        Command::StartRace {
            heat: PhaseId("heat".into()),
        },
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let status = registry.video().status();
    assert_eq!(status.runs.len(), runs_before, "{:?}", status.runs);
    assert!(!status.obs.recording);
    recorder.abort();
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs an OBS with Source Record and two scenes: set GRIDFPV_OBS_ADDR"]
async fn two_scenes_are_recorded_to_their_own_files_by_a_real_obs() {
    let address = std::env::var("GRIDFPV_OBS_ADDR").unwrap_or_else(|_| "127.0.0.1:4455".into());
    let password = std::env::var("GRIDFPV_OBS_PASS").unwrap_or_default();
    let clean_scene = std::env::var("GRIDFPV_OBS_CLEAN").unwrap_or_else(|_| "Clean".into());
    let overlay_scene = std::env::var("GRIDFPV_OBS_OVERLAY").unwrap_or_else(|_| "Scene".into());

    let registry = EventRegistry::new(data_dir()).unwrap();
    let stamp = std::process::id();
    let event = registry
        .create(&CreateEventRequest::named(format!("Scenes {stamp}")))
        .unwrap()
        .id;
    registry.set_active(&event).unwrap();
    define_heat(&registry, &event);

    let recorder = gridfpv_app::video::spawn(registry.clone());
    let settings = VideoSettingsRequest {
        enabled: true,
        obs_address: address.clone(),
        obs_password: Some(password.clone()),
        tail_seconds: 1,
        trash_discarded: true,
        record_from: RecordFrom::Scenes,
        clean_scene: clean_scene.clone(),
        overlay_scene: overlay_scene.clone(),
        render_when: RenderWhen::Asked,
        browser_path: String::new(),
        ffmpeg_path: String::new(),
        save_in: save_in(),
    };
    registry.video().update(&settings).unwrap();
    let obs = eventually("OBS to connect", 15, || {
        let obs = registry.video().status().obs;
        obs.connected.then_some(obs)
    })
    .await;
    // What the Video page's pickers are filled from.
    assert!(obs.source_record, "{obs:?}");
    assert!(
        obs.scenes.contains(&clean_scene) && obs.scenes.contains(&overlay_scene),
        "{obs:?}"
    );

    let race = RaceId("heat#1".into());
    send(
        &registry,
        &event,
        Command::StartRace {
            heat: PhaseId("heat".into()),
        },
    );
    eventually("the scenes to start recording", 20, || {
        let run = run_of(&registry, &race)?;
        assert_ne!(run.state, VideoRunState::Failed, "{:?}", run.note);
        (run.state == VideoRunState::Recording).then_some(run)
    })
    .await;
    // OBS's own recording is not touched: it stays free for whatever else it is doing.
    assert!(!registry.video().status().obs.recording);
    // Long enough for the recorder to look at the file twice: video is reaching it, so
    // nothing is said about a recording with no video.
    tokio::time::sleep(Duration::from_millis(5000)).await;
    let run = run_of(&registry, &race).unwrap();
    assert_eq!(
        (run.state, run.note.clone()),
        (VideoRunState::Recording, None)
    );
    send(&registry, &event, Command::StopRace { race: race.clone() });
    let written = eventually("the scenes to be written", 30, || {
        let run = run_of(&registry, &race)?;
        assert_ne!(run.state, VideoRunState::Failed, "{:?}", run.note);
        (run.state == VideoRunState::WaitingForSave).then_some(run)
    })
    .await;
    let file = written.file.unwrap();
    assert!(
        file.contains(&format!("Scenes {stamp} Track 2 race 1 - Clean.")),
        "{file}"
    );
    assert_in_grid_folder(&registry, &file);
    let start = written.start_at_ms.unwrap();
    assert!(
        (0..3000).contains(&start),
        "the tone is {start} ms into the file"
    );

    send(
        &registry,
        &event,
        Command::SaveRace {
            race: race.clone(),
            needs_marshal: false,
            note: None,
        },
    );
    eventually("the run to be saved", 10, || {
        run_of(&registry, &race).filter(|run| run.state == VideoRunState::Saved)
    })
    .await;
    // The fact says the file is clean, where the overlaid one is, and which laps that one
    // was filmed with.
    let logged = events(&registry, &event)
        .into_iter()
        .find_map(|fact| match fact {
            Event::VideoRecorded {
                race: of,
                file,
                clean,
                overlay_file,
                laps_key,
                ..
            } if of == race => Some((file, clean, overlay_file, laps_key)),
            _ => None,
        })
        .expect("the recording is logged");
    assert_eq!(logged.0, file);
    assert!(logged.1);
    let overlay = logged.2.expect("the overlay scene was recorded too");
    assert!(
        overlay.contains(&format!("Scenes {stamp} Track 2 race 1 - Overlay.")),
        "{overlay}"
    );
    assert!(logged.3.is_some());
    // The race has its overlay, as filmed, until a lap changes.
    let view = with_event(&registry, &event, |session| {
        Ok(gridfpv_server::view::EventView::build(
            session.state(),
            session.now,
        ))
    })
    .unwrap();
    let video = view.races[0].video.as_ref().unwrap();
    assert!(video.clean);
    assert_eq!(video.overlay, VideoOverlay::Filmed);
    assert_eq!(video.overlaid_file.as_deref(), Some(overlay.as_str()));
    recorder.abort();

    // ── The way out of a recording that will not stop ────────────────────────────────────
    // A recording is started on the clean scene and its filter is reset under it: the
    // recording ends, and the filter is back with the settings it had.
    let obs = gridfpv_app::obs::connect(&address, &password)
        .await
        .unwrap();
    let filter = format!("Grid record {clean_scene}");
    let named = json!({ "sourceName": clean_scene, "filterName": filter });
    let before = obs.call("GetSourceFilter", named.clone()).await.unwrap();
    obs.call(
        "CallVendorRequest",
        json!({
            "vendorName": "source-record",
            "requestType": "record_start",
            "requestData": { "source": clean_scene, "filter": filter, "filename": format!("Reset {stamp}") },
        }),
    )
    .await
    .unwrap();
    let active = |obs: gridfpv_app::obs::Obs, filter: String| async move {
        obs.call("GetOutputStatus", json!({ "outputName": filter }))
            .await
            .is_ok_and(|status| status["outputActive"] == true)
    };
    for _ in 0..50 {
        if active(obs.clone(), filter.clone()).await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(active(obs.clone(), filter.clone()).await);
    gridfpv_app::tape::scene_reset(&obs, &clean_scene, &filter)
        .await
        .unwrap();
    assert!(!active(obs.clone(), filter.clone()).await);
    let after = obs.call("GetSourceFilter", named).await.unwrap();
    for kept in ["encoder", "path", "rec_format"] {
        assert_eq!(
            after["filterSettings"][kept], before["filterSettings"][kept],
            "{kept}"
        );
    }
    // And it is not left set to record by itself.
    assert_eq!(after["filterSettings"]["record_mode"], 0);
}
