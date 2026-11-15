//! The recorder: has OBS record each race of the active event.
//!
//! [`spawn`] starts one task. While video is switched on in the settings it keeps a
//! connection to OBS ([`crate::obs`]) and watches the active event:
//!
//! | The race | The recorder |
//! |---|---|
//! | begins its countdown, or starts | names the file after the race, then starts OBS's recording |
//! | is stopped, or saved | records the tail (a few seconds more), then stops the recording |
//! | is called off or discarded while recording | stops the recording at once |
//! | is saved | logs the recording into the event (`VideoRecorded`) |
//! | is discarded | moves the recording to `trash/` beside it, when the file can be reached |
//!
//! A saved race that is discarded later (a false start called late) has its recording
//! moved to the trash too.
//!
//! **OBS already recording.** If OBS is recording when a race begins (the whole day is
//! being recorded by hand), the recorder leaves it alone and says so on the run. It never
//! stops a recording it did not start.
//!
//! **What records.** OBS's own recording, or one or two scenes of their own through the
//! Source Record plugin. How each is started, stopped and named is in [`crate::tape`].
//!
//! What to do next is decided by [`decide`], which is pure and tested alone. The rest of
//! this module is the talking to OBS around it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gridfpv_events::{Event, RaceId};
use gridfpv_server::events::{EventId, EventRegistry};
use gridfpv_server::video::{
    RaceState, VideoRun, VideoRunState, WatchedRace, current_race, log_video, race_now,
};
use serde_json::json;
use tokio::task::JoinHandle;

use crate::obs::{self, Obs};
use crate::tape::{self, Tape, Written};

/// How often the recorder looks at the race.
const TICK: Duration = Duration::from_millis(200);
/// How long to wait before trying OBS again.
const RETRY: Duration = Duration::from_secs(5);
/// How many saved runs are watched for a late discard.
const SAVED_WATCHED: usize = 20;

fn now_micros() -> i64 {
    gridfpv_server::app::now_micros()
}

/// The recording the recorder is in the middle of.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Active {
    event: EventId,
    race: RaceId,
    name: String,
    /// The Director's clock when OBS said the recording had started: the file's first frame.
    first_frame_at: i64,
    /// The race's start on the Director's clock, as last seen.
    starts_at: Option<i64>,
    phase: Phase,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Phase {
    /// OBS is recording the race.
    Recording,
    /// The race is over; recording goes on until this moment on the Director's clock.
    Tail { until: i64 },
    /// The recording is written. Waiting for the race to be saved or discarded.
    Written(Written),
}

/// What the recorder should do now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Decision {
    Nothing,
    /// Start recording this race.
    Begin(WatchedRace),
    /// The race is over: keep recording until `until`, then stop.
    StartTail {
        until: i64,
    },
    /// Stop the recording now.
    Stop,
    /// The race was saved: keep the recording.
    Keep,
    /// The race was discarded, or is gone: the recording is not wanted.
    Drop,
}

/// Decide what to do, from the recording in hand, the race the event has on now
/// (`current`), the race being recorded as it stands now (`mine`), the Director's clock,
/// and the tail to record after a race.
fn decide(
    active: Option<&Active>,
    current: Option<&WatchedRace>,
    mine: Option<&WatchedRace>,
    now: i64,
    tail_micros: i64,
) -> Decision {
    let Some(active) = active else {
        // Nothing in hand: a race that is counting down or running wants a recording. One
        // that is already stopped was missed, and is left alone.
        return match current {
            Some(race) if matches!(race.state, RaceState::Staging | RaceState::Running) => {
                Decision::Begin(race.clone())
            }
            _ => Decision::Nothing,
        };
    };
    let state = mine.map(|race| race.state);
    match &active.phase {
        Phase::Recording => match state {
            Some(RaceState::Staging | RaceState::Running) => Decision::Nothing,
            // Stopped, or saved without this tick seeing the stop: record the tail.
            Some(RaceState::Stopped | RaceState::Saved) => Decision::StartTail {
                until: now + tail_micros,
            },
            // Called off, discarded, or the event is gone: nothing more to record.
            Some(RaceState::Discarded) | None => Decision::Stop,
        },
        Phase::Tail { until } => match state {
            Some(RaceState::Discarded) | None => Decision::Stop,
            _ if now >= *until => Decision::Stop,
            _ => Decision::Nothing,
        },
        Phase::Written { .. } => match state {
            Some(RaceState::Saved) => Decision::Keep,
            Some(RaceState::Discarded) | None => Decision::Drop,
            _ => Decision::Nothing,
        },
    }
}

/// A race's name as a file name OBS will take as it is. OBS reads `%` as the start of a
/// date code, and a few characters cannot be in a file name on Windows; each becomes a
/// space, and runs of spaces become one.
fn file_name_for(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '%' | '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = joined.trim_matches('.').trim();
    if trimmed.is_empty() {
        "race".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Where a recording goes when its race is discarded: a `trash` folder beside it.
fn trash_path(file: &Path) -> Option<PathBuf> {
    Some(file.parent()?.join("trash").join(file.file_name()?))
}

/// Move a recording to the trash. `Ok(None)` when the file is not on this machine (OBS
/// runs somewhere else), so there is nothing here to move.
fn move_to_trash(file: &str) -> Result<Option<String>, String> {
    let path = Path::new(file);
    if !path.is_file() {
        return Ok(None);
    }
    let target = trash_path(path).ok_or("the recording has no folder")?;
    let moved = (|| {
        std::fs::create_dir_all(target.parent().expect("a trash path has a parent"))?;
        std::fs::rename(path, &target)
    })();
    match moved {
        Ok(()) => Ok(Some(target.to_string_lossy().into_owned())),
        Err(e) => Err(e.to_string()),
    }
}

/// Start having OBS record the races of the active event. Runs until aborted.
pub fn spawn(registry: EventRegistry) -> JoinHandle<()> {
    tokio::spawn(async move {
        let desk = registry.video();
        let mut changed = desk.watch();
        loop {
            let settings = desk.stored();
            if !settings.enabled {
                desk.report_obs(|obs| *obs = Default::default());
                if changed.changed().await.is_err() {
                    return;
                }
                continue;
            }
            match obs::connect(&settings.obs_address, &settings.obs_password).await {
                Ok(obs) => {
                    let mut recorder = Recorder {
                        registry: registry.clone(),
                        obs,
                        active: None,
                        tape: None,
                        saved: Vec::new(),
                    };
                    recorder.hello().await;
                    // Until OBS goes away or the settings change.
                    tokio::select! {
                        () = recorder.run() => {
                            desk.report_obs(|obs| {
                                *obs = Default::default();
                                obs.problem = Some("OBS closed the connection. Trying again.".into());
                            });
                        }
                        result = changed.changed() => {
                            if result.is_err() { return; }
                            // A recording in hand is finished before the settings are read again.
                            recorder.settle().await;
                            continue;
                        }
                    }
                }
                Err(problem) => {
                    desk.report_obs(|obs| {
                        *obs = Default::default();
                        obs.problem = Some(problem);
                    });
                }
            }
            tokio::select! {
                () = tokio::time::sleep(RETRY) => {}
                result = changed.changed() => if result.is_err() { return; }
            }
        }
    })
}

struct Recorder {
    registry: EventRegistry,
    obs: Obs,
    active: Option<Active>,
    /// What is recording the race in hand, while it is being recorded.
    tape: Option<Tape>,
    /// Saved runs still watched for a late discard: the event, the race, its name, and
    /// what was recorded.
    saved: Vec<(EventId, RaceId, String, Written)>,
}

impl Recorder {
    /// Say what answered and what it has, and put OBS's own file naming back if a Director
    /// that stopped in the middle of a recording left the race's name there.
    async fn hello(&self) {
        let desk = self.registry.video();
        let version = self.obs.call("GetVersion", json!({})).await.ok().map(|v| {
            format!(
                "OBS {} on {}",
                v["obsVersion"].as_str().unwrap_or("?"),
                v["platform"].as_str().unwrap_or("?")
            )
        });
        let folder = self
            .obs
            .call("GetRecordDirectory", json!({}))
            .await
            .ok()
            .and_then(|v| v["recordDirectory"].as_str().map(str::to_owned));
        let recording = tape::own_recording(&self.obs).await.unwrap_or(false);
        let scenes = tape::scenes(&self.obs).await;
        let source_record = tape::has_source_record(&self.obs).await;
        desk.report_obs(|obs| {
            obs.connected = true;
            obs.problem = None;
            obs.version = version;
            obs.record_folder = folder;
            obs.recording = recording;
            obs.scenes = scenes;
            obs.source_record = source_record;
        });
        if !recording {
            tape::restore_naming(&self.obs, &desk).await;
        }
    }

    /// Look at the race every tick, for as long as OBS is there.
    async fn run(&mut self) {
        let mut ticks: u32 = 0;
        loop {
            tokio::time::sleep(TICK).await;
            if !self.obs.is_open() {
                return;
            }
            ticks = ticks.wrapping_add(1);
            // About every two seconds: is OBS still there, and is it recording? And has a
            // saved race been discarded since?
            if ticks.is_multiple_of(10) {
                match tape::own_recording(&self.obs).await {
                    Ok(recording) => self
                        .registry
                        .video()
                        .report_obs(|obs| obs.recording = recording),
                    Err(_) => return,
                }
                // Scenes come and go in OBS: keep the list for the pickers fresh.
                if ticks.is_multiple_of(50) {
                    let scenes = tape::scenes(&self.obs).await;
                    self.registry.video().report_obs(|obs| obs.scenes = scenes);
                }
                self.late_discards();
            }
            self.step().await;
        }
    }

    /// One look at the race, and whatever it calls for.
    async fn step(&mut self) {
        let settings = self.registry.video().stored();
        let current = current_race(&self.registry);
        let mine = self
            .active
            .as_ref()
            .and_then(|active| race_now(&self.registry, &active.event, &active.race));
        if let (Some(active), Some(mine)) = (self.active.as_mut(), mine.as_ref()) {
            active.starts_at = mine.starts_at.or(active.starts_at);
        }
        let decision = decide(
            self.active.as_ref(),
            current.as_ref(),
            mine.as_ref(),
            now_micros(),
            i64::from(settings.tail_seconds) * 1_000_000,
        );
        match decision {
            Decision::Nothing => {}
            Decision::Begin(race) => self.begin(race).await,
            Decision::StartTail { until } => {
                if let Some(active) = self.active.as_mut() {
                    active.phase = Phase::Tail { until };
                    let run = run_of(active, VideoRunState::Finishing, None, None);
                    self.registry.video().report_run(run);
                }
            }
            Decision::Stop => self.stop().await,
            Decision::Keep => self.keep(mine.as_ref()),
            Decision::Drop => self.discard(settings.trash_discarded),
        }
    }

    /// A run that never got a recording, with the reason.
    fn failed(&self, race: &WatchedRace, note: impl Into<String>) {
        self.registry.video().report_run(VideoRun {
            event: race.event.clone(),
            race: race.race.clone(),
            name: race.name.clone(),
            state: VideoRunState::Failed,
            file: None,
            start_at_ms: None,
            note: Some(note.into()),
        });
    }

    /// Start recording the race, under its name.
    async fn begin(&mut self, race: WatchedRace) {
        // A race already listed was handled, well or badly: it is not tried again each tick.
        let desk = self.registry.video();
        let listed = desk
            .status()
            .runs
            .iter()
            .any(|run| run.event == race.event && run.race == race.race);
        if listed {
            return;
        }
        let settings = desk.stored();
        let started = tape::start(&self.obs, &desk, &settings, &file_name_for(&race.name)).await;
        // The clock at this moment is where the file begins.
        let first_frame_at = now_micros();
        let tape = match started {
            Ok(tape) => tape,
            Err(e) => return self.failed(&race, e),
        };
        let active = Active {
            event: race.event.clone(),
            race: race.race.clone(),
            name: race.name.clone(),
            first_frame_at,
            starts_at: race.starts_at,
            phase: Phase::Recording,
        };
        desk.report_run(run_of(&active, VideoRunState::Recording, None, None));
        if tape == Tape::Own {
            desk.report_obs(|obs| obs.recording = true);
        }
        self.tape = Some(tape);
        self.active = Some(active);
    }

    /// Stop the recording and note what it wrote.
    async fn stop(&mut self) {
        let (Some(mut active), Some(tape)) = (self.active.take(), self.tape.take()) else {
            return;
        };
        let desk = self.registry.video();
        let own = tape == Tape::Own;
        let stopped = tape::stop(&self.obs, &desk, tape).await;
        if own {
            desk.report_obs(|obs| obs.recording = false);
        }
        match stopped {
            Ok(written) => {
                let file = written.file.clone();
                active.phase = Phase::Written(written);
                desk.report_run(run_of(
                    &active,
                    VideoRunState::WaitingForSave,
                    Some(file),
                    None,
                ));
                self.active = Some(active);
            }
            Err(e) => desk.report_run(run_of(
                &active,
                VideoRunState::Failed,
                None,
                Some(format!("OBS did not stop the recording cleanly: {e}")),
            )),
        }
    }

    /// The race was saved: log the recording into its event, with the mark of the laps it
    /// was filmed with.
    fn keep(&mut self, mine: Option<&WatchedRace>) {
        let Some(active) = self.active.take() else {
            return;
        };
        let Phase::Written(written) = &active.phase else {
            return;
        };
        log_video(
            &self.registry,
            &active.event,
            Event::VideoRecorded {
                race: active.race.clone(),
                file: written.file.clone(),
                first_frame_at: active.first_frame_at,
                clean: written.clean,
                overlay_file: written.overlay_file.clone(),
                laps_key: mine.map(|race| race.laps_key.clone()),
            },
        );
        self.registry.video().report_run(run_of(
            &active,
            VideoRunState::Saved,
            Some(written.file.clone()),
            None,
        ));
        self.saved.insert(
            0,
            (
                active.event.clone(),
                active.race.clone(),
                active.name.clone(),
                written.clone(),
            ),
        );
        self.saved.truncate(SAVED_WATCHED);
    }

    /// The race was discarded: the recording goes to the trash, when it can.
    fn discard(&mut self, trash: bool) {
        let Some(active) = self.active.take() else {
            return;
        };
        let Phase::Written(written) = &active.phase else {
            return;
        };
        let (state, file, note) = discarded(written, trash);
        self.registry
            .video()
            .report_run(run_of(&active, state, Some(file), note));
    }

    /// A saved race that has since been discarded: its recording goes to the trash too.
    fn late_discards(&mut self) {
        let trash = self.registry.video().stored().trash_discarded;
        let registry = self.registry.clone();
        self.saved.retain(|(event, race, name, written)| {
            let now = race_now(&registry, event, race);
            if now.as_ref().is_some_and(|r| r.state == RaceState::Saved) {
                return true;
            }
            let (state, moved, note) = discarded(written, trash);
            if now.is_some() {
                log_video(&registry, event, Event::VideoRemoved { race: race.clone() });
            }
            registry.video().report_run(VideoRun {
                event: event.clone(),
                race: race.clone(),
                name: name.clone(),
                state,
                file: Some(moved),
                start_at_ms: None,
                note,
            });
            false
        });
    }

    /// Finish whatever is in hand before the recorder is torn down: a recording still
    /// running is stopped, so OBS is never left recording a race nobody is watching.
    async fn settle(&mut self) {
        if matches!(
            self.active.as_ref().map(|active| &active.phase),
            Some(Phase::Recording | Phase::Tail { .. })
        ) {
            self.stop().await;
        }
    }
}

/// A run as the status lists it, from the recording in hand.
fn run_of(
    active: &Active,
    state: VideoRunState,
    file: Option<String>,
    note: Option<String>,
) -> VideoRun {
    VideoRun {
        event: active.event.clone(),
        race: active.race.clone(),
        name: active.name.clone(),
        state,
        file,
        start_at_ms: active
            .starts_at
            .map(|starts_at| (starts_at - active.first_frame_at) / 1000),
        note,
    }
}

/// What becomes of a discarded race's recording: where it is now, and what to say. The
/// overlay scene's recording, when there is one, goes the same way.
fn discarded(written: &Written, trash: bool) -> (VideoRunState, String, Option<String>) {
    let file = written.file.as_str();
    if !trash {
        return (VideoRunState::DiscardedKept, file.to_owned(), None);
    }
    // Best effort: the clean file below decides what the run says.
    if let Some(overlay) = &written.overlay_file {
        let _ = move_to_trash(overlay);
    }
    // An overlay drawn onto it since goes too.
    let redrawn = crate::render::overlaid_path(file);
    if redrawn.is_file() {
        let _ = move_to_trash(&redrawn.to_string_lossy());
    }
    match move_to_trash(file) {
        Ok(Some(moved)) => (VideoRunState::Trashed, moved, None),
        Ok(None) => (
            VideoRunState::DiscardedKept,
            file.to_owned(),
            Some(
                "The recording is on another machine, so Grid could not move it to the trash."
                    .into(),
            ),
        ),
        Err(e) => (
            VideoRunState::DiscardedKept,
            file.to_owned(),
            Some(format!(
                "The recording could not be moved to the trash: {e}"
            )),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: i64 = 1_000_000;
    const TAIL: i64 = 3 * S;

    fn race(state: RaceState) -> WatchedRace {
        WatchedRace {
            event: EventId("night".into()),
            race: RaceId("heat#1".into()),
            name: "Night Heat race 1".into(),
            state,
            starts_at: Some(10 * S),
            laps_key: "0000000000000000".into(),
        }
    }

    fn own(file: &str) -> Written {
        Written {
            file: file.into(),
            clean: false,
            overlay_file: None,
        }
    }

    fn active(phase: Phase) -> Active {
        Active {
            event: EventId("night".into()),
            race: RaceId("heat#1".into()),
            name: "Night Heat race 1".into(),
            first_frame_at: 5 * S,
            starts_at: Some(10 * S),
            phase,
        }
    }

    #[test]
    fn a_race_that_counts_down_or_runs_gets_a_recording_and_a_stopped_one_is_left_alone() {
        for state in [RaceState::Staging, RaceState::Running] {
            let on = race(state);
            assert_eq!(
                decide(None, Some(&on), None, 0, TAIL),
                Decision::Begin(on.clone())
            );
        }
        // Stopped before the recorder saw it (OBS was not connected yet): too late.
        assert_eq!(
            decide(None, Some(&race(RaceState::Stopped)), None, 0, TAIL),
            Decision::Nothing
        );
        assert_eq!(decide(None, None, None, 0, TAIL), Decision::Nothing);
    }

    #[test]
    fn a_stop_or_a_save_records_the_tail_and_then_stops() {
        let recording = active(Phase::Recording);
        for state in [RaceState::Staging, RaceState::Running] {
            assert_eq!(
                decide(Some(&recording), None, Some(&race(state)), 20 * S, TAIL),
                Decision::Nothing
            );
        }
        for state in [RaceState::Stopped, RaceState::Saved] {
            assert_eq!(
                decide(Some(&recording), None, Some(&race(state)), 20 * S, TAIL),
                Decision::StartTail { until: 23 * S }
            );
        }
        let tail = active(Phase::Tail { until: 23 * S });
        let stopped = race(RaceState::Stopped);
        assert_eq!(
            decide(Some(&tail), None, Some(&stopped), 22 * S, TAIL),
            Decision::Nothing
        );
        assert_eq!(
            decide(Some(&tail), None, Some(&stopped), 23 * S, TAIL),
            Decision::Stop
        );
    }

    #[test]
    fn a_race_called_off_or_discarded_stops_the_recording_at_once() {
        let gone = race(RaceState::Discarded);
        for phase in [Phase::Recording, Phase::Tail { until: 99 * S }] {
            assert_eq!(
                decide(Some(&active(phase.clone())), None, Some(&gone), 0, TAIL),
                Decision::Stop
            );
            // The event itself was deleted.
            assert_eq!(
                decide(Some(&active(phase)), None, None, 0, TAIL),
                Decision::Stop
            );
        }
    }

    #[test]
    fn a_written_recording_waits_for_the_save_or_the_discard() {
        let written = active(Phase::Written(own("/rec/a.mkv")));
        assert_eq!(
            decide(
                Some(&written),
                None,
                Some(&race(RaceState::Stopped)),
                0,
                TAIL
            ),
            Decision::Nothing
        );
        assert_eq!(
            decide(Some(&written), None, Some(&race(RaceState::Saved)), 0, TAIL),
            Decision::Keep
        );
        assert_eq!(
            decide(
                Some(&written),
                None,
                Some(&race(RaceState::Discarded)),
                0,
                TAIL
            ),
            Decision::Drop
        );
        assert_eq!(decide(Some(&written), None, None, 0, TAIL), Decision::Drop);
    }

    #[test]
    fn a_race_name_becomes_a_file_name_obs_takes_as_it_is() {
        assert_eq!(
            file_name_for("RaceGOW 6 Track 2 race 7"),
            "RaceGOW 6 Track 2 race 7"
        );
        // "%" starts a date code in OBS; the rest cannot be in a Windows file name.
        assert_eq!(
            file_name_for("100% Open: A/B \"finals\"? <3>|*\\"),
            "100 Open A B finals 3"
        );
        assert_eq!(file_name_for("  Club\tNight\n heat  "), "Club Night heat");
        assert_eq!(file_name_for("..."), "race");
        assert_eq!(file_name_for(""), "race");
    }

    #[test]
    fn a_discarded_recording_goes_to_a_trash_folder_beside_it() {
        let dir = std::env::temp_dir().join(format!("gridfpv-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("Night Heat race 1.mkv");
        std::fs::write(&file, b"video").unwrap();
        let path = file.to_string_lossy().into_owned();

        // Trash switched off: the file stays, with nothing to say about it.
        assert_eq!(
            discarded(&own(&path), false),
            (VideoRunState::DiscardedKept, path.clone(), None)
        );
        assert!(file.is_file());

        let (state, moved, note) = discarded(&own(&path), true);
        assert_eq!((state, note), (VideoRunState::Trashed, None));
        assert_eq!(
            Path::new(&moved),
            dir.join("trash").join("Night Heat race 1.mkv")
        );
        assert!(!file.exists() && Path::new(&moved).is_file());

        // A file that is not on this machine is left alone, and the run says why.
        let (state, kept, note) = discarded(&own("/nowhere/at/all.mkv"), true);
        assert_eq!(state, VideoRunState::DiscardedKept);
        assert_eq!(kept, "/nowhere/at/all.mkv");
        assert!(note.unwrap().contains("another machine"));

        // Two files, a clean one and one with the overlay: both go.
        let clean = dir.join("Night race 2 - Clean.mkv");
        let overlay = dir.join("Night race 2 - Overlay.mkv");
        std::fs::write(&clean, b"video").unwrap();
        std::fs::write(&overlay, b"video").unwrap();
        let redrawn = dir.join("Night race 2 - Overlay (redrawn).mp4");
        std::fs::write(&redrawn, b"video").unwrap();
        let both = Written {
            file: clean.to_string_lossy().into_owned(),
            clean: true,
            overlay_file: Some(overlay.to_string_lossy().into_owned()),
        };
        assert_eq!(discarded(&both, true).0, VideoRunState::Trashed);
        assert!(!clean.exists() && !overlay.exists() && !redrawn.exists());
        assert!(
            dir.join("trash")
                .join("Night race 2 - Overlay.mkv")
                .is_file()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
