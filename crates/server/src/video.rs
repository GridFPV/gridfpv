//! Video: the Director has OBS record each race.
//!
//! OBS does the capturing. The Director only tells it when: it starts a recording when a
//! race begins its countdown, stops it a few seconds after the race is stopped, and names
//! the file after the race before the recording starts. A race that is discarded has its
//! recording moved to a `trash` folder beside it, when the Director can reach the file.
//!
//! This module is the part every client sees: the settings, the status, and the routes. The
//! connection to OBS and the recorder that drives it live in the app crate
//! (`app::obs`, `app::video`); they read the settings from here and report their status
//! here.
//!
//! | route | what |
//! |-------|------|
//! | `GET /video/settings`, `PUT /video/settings` | where OBS is and how races are recorded |
//! | `GET /video/status` | whether OBS is connected, what is recording, and this session's runs |
//!
//! **The password never leaves the Director.** Reads are open on the network, so the
//! settings a client reads say only whether one is set.
//!
//! **Where the files are.** Talking to OBS works over the network. Moving a file does not:
//! the trash only works when the Director runs on the machine OBS records on. When it does
//! not, the recording of a discarded race is left where it is, and the status says so.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::State;
pub use gridfpv_engine::race::RaceState;
use gridfpv_events::RaceId;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::control::with_event;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{EventId, EventRegistry};

/// File name of the video settings under the data directory.
pub const VIDEO_FILE: &str = "video.json";

/// How many finished runs the status keeps. Older ones drop off the list; their files and
/// the facts in the event's log are untouched.
const RUNS_KEPT: usize = 50;

/// The settings as they are stored. The password is here and nowhere else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredVideoSettings {
    #[serde(default)]
    pub enabled: bool,
    /// Where OBS's WebSocket server is: `host:port`.
    #[serde(default = "default_obs_address")]
    pub obs_address: String,
    #[serde(default)]
    pub obs_password: String,
    /// How long to keep recording after a race is stopped, in seconds.
    #[serde(default = "default_tail")]
    pub tail_seconds: u32,
    /// Move a discarded race's recording to a `trash` folder beside it.
    #[serde(default = "yes")]
    pub trash_discarded: bool,
    /// What records each race.
    #[serde(default)]
    pub record_from: RecordFrom,
    /// With [`RecordFrom::Scenes`]: the scene with the camera and no overlay.
    #[serde(default)]
    pub clean_scene: String,
    /// With [`RecordFrom::Scenes`]: a second scene, with the live overlay on it, recorded
    /// alongside. Empty for none.
    #[serde(default)]
    pub overlay_scene: String,
    /// When the overlay is drawn onto a clean recording.
    #[serde(default)]
    pub render_when: RenderWhen,
    /// The browser that draws the overlay's frames (Chrome or Edge). Empty: look for one.
    #[serde(default)]
    pub browser_path: String,
    /// ffmpeg, which puts the frames on the video. Empty: `ffmpeg` on the PATH.
    #[serde(default)]
    pub ffmpeg_path: String,
    /// OBS's own file naming, as it was before the Director changed it for a race. Kept so
    /// it can be put back even if the Director stopped in the middle of a recording.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obs_naming_before: Option<String>,
}

/// What records each race.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum RecordFrom {
    /// OBS's own recording: what its Start Recording button records.
    #[default]
    Obs,
    /// A scene of its own, recorded by the Source Record plugin, whatever OBS's own
    /// recording and stream are doing. The scene has no overlay, so one can be drawn onto
    /// the recording afterwards.
    Scenes,
}

/// When the overlay is drawn onto a clean recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum RenderWhen {
    /// Only when asked, from the Video page.
    #[default]
    Asked,
    /// By itself, whenever a recorded race has no overlaid video that matches its laps.
    Always,
}

fn default_obs_address() -> String {
    "127.0.0.1:4455".to_owned()
}
fn default_tail() -> u32 {
    3
}
fn yes() -> bool {
    true
}

impl Default for StoredVideoSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            obs_address: default_obs_address(),
            obs_password: String::new(),
            tail_seconds: default_tail(),
            trash_discarded: true,
            record_from: RecordFrom::Obs,
            clean_scene: String::new(),
            overlay_scene: String::new(),
            render_when: RenderWhen::Asked,
            browser_path: String::new(),
            ffmpeg_path: String::new(),
            obs_naming_before: None,
        }
    }
}

/// The video settings as a client reads them. The password is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct VideoSettings {
    /// Whether the Director records races at all.
    pub enabled: bool,
    /// Where OBS's WebSocket server is: `host:port`.
    pub obs_address: String,
    /// Whether a password for OBS is stored.
    pub has_password: bool,
    /// How long to keep recording after a race is stopped, in seconds.
    pub tail_seconds: u32,
    /// Move a discarded race's recording to a `trash` folder beside it.
    pub trash_discarded: bool,
    /// What records each race.
    pub record_from: RecordFrom,
    /// With `Scenes`: the scene with the camera and no overlay.
    pub clean_scene: String,
    /// With `Scenes`: a second scene, with the live overlay on it. Empty for none.
    pub overlay_scene: String,
    /// When the overlay is drawn onto a clean recording.
    pub render_when: RenderWhen,
    /// The browser that draws the overlay's frames. Empty: the Director looks for one.
    pub browser_path: String,
    /// ffmpeg. Empty: `ffmpeg` on the PATH.
    pub ffmpeg_path: String,
}

/// What `PUT /video/settings` takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct VideoSettingsRequest {
    pub enabled: bool,
    pub obs_address: String,
    /// A new password for OBS. Left out, the stored one is kept. An empty one clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub obs_password: Option<String>,
    pub tail_seconds: u32,
    pub trash_discarded: bool,
    #[serde(default)]
    pub record_from: RecordFrom,
    #[serde(default)]
    pub clean_scene: String,
    #[serde(default)]
    pub overlay_scene: String,
    #[serde(default)]
    pub render_when: RenderWhen,
    #[serde(default)]
    pub browser_path: String,
    #[serde(default)]
    pub ffmpeg_path: String,
}

/// Where a run's recording stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum VideoRunState {
    /// OBS is recording the race.
    Recording,
    /// The race is stopped; the last seconds are still being recorded.
    Finishing,
    /// The file is written. The race is not saved or discarded yet.
    WaitingForSave,
    /// The race was saved. The recording is kept under the race's name.
    Saved,
    /// The race was discarded and its recording moved to the trash.
    Trashed,
    /// The race was discarded. The recording is still where OBS wrote it.
    DiscardedKept,
    /// The race was not recorded, or its recording could not be finished. `note` says why.
    Failed,
}

/// One race the Director recorded, or tried to, since it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct VideoRun {
    /// The event the race is in.
    pub event: EventId,
    pub race: RaceId,
    /// What the race is called, and so the recording: "Club Night Track 2 race 4".
    pub name: String,
    pub state: VideoRunState,
    /// The recording, as OBS named it: a path on the machine OBS runs on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub file: Option<String>,
    /// Where the start tone is in the recording, in milliseconds from its first frame.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub start_at_ms: Option<i64>,
    /// Anything a director should know about this run, as a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// The Director's link to OBS right now.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ObsStatus {
    pub connected: bool,
    /// Why it is not connected, as a sentence for the director.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub problem: Option<String>,
    /// What answered: "OBS 31.0.2 on windows".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Whether OBS is recording right now, by anyone's doing.
    pub recording: bool,
    /// The folder OBS records into, on its own machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub record_folder: Option<String>,
    /// OBS's scenes, by name, for picking which to record.
    #[serde(default)]
    pub scenes: Vec<String>,
    /// Whether OBS has the Source Record plugin, which recording a scene needs.
    #[serde(default)]
    pub source_record: bool,
}

/// Where one render stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum RenderState {
    /// Waiting its turn.
    Queued,
    /// Being drawn: this many frames out of a hundred are done.
    Rendering {
        percent: u8,
    },
    Done,
    /// It could not be made. `note` on the job says why.
    Failed,
}

/// One race whose overlay is being drawn onto its recording, or was, or is waiting to be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RenderJob {
    pub event: EventId,
    pub race: RaceId,
    /// What the race is called.
    pub name: String,
    pub state: RenderState,
    /// Why it failed, as a sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub note: Option<String>,
}

/// What `POST /video/render` takes: one race, or every race of the event that needs one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RenderRequest {
    pub event: EventId,
    /// The race to render. Left out: every race of the event with a clean recording and no
    /// overlaid video that matches its laps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub race: Option<RaceId>,
}

/// What `GET /video/status` answers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct VideoStatus {
    pub obs: ObsStatus,
    /// The runs since the Director started, newest first.
    pub runs: Vec<VideoRun>,
    /// The renders asked for since the Director started, newest first.
    #[serde(default)]
    pub renders: Vec<RenderJob>,
    /// The programs a render needs, as found on this machine.
    #[serde(default)]
    pub tools: RenderTools,
}

/// The two programs that draw an overlay onto a recording, as found on the Director's
/// machine. Each is the path found, or absent when there is none.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RenderTools {
    /// Chrome, Edge or Chromium: it draws the overlay's pictures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub browser: Option<String>,
    /// ffmpeg: it puts the pictures onto the video.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ffmpeg: Option<String>,
    /// Whether Grid knows where to download ffmpeg for this kind of computer.
    #[serde(default)]
    pub can_fetch: bool,
    /// ffmpeg is being downloaded right now.
    #[serde(default)]
    pub fetching: bool,
    /// Why the last download did not work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub fetch_problem: Option<String>,
}

/// Why a settings change was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VideoError {
    #[error("OBS's address looks like 192.168.1.20:4455: a host, a colon, and a port.")]
    BadAddress,
    #[error("The tail after a race can be at most 60 seconds.")]
    TailTooLong,
    #[error("Pick the scene to record. It is the one with the camera and no overlay.")]
    NoCleanScene,
    #[error("The clean scene and the overlay scene must be two different scenes.")]
    SameScene,
    #[error("There is no such event.")]
    NoEvent,
    #[error("That race has no clean recording to draw an overlay onto.")]
    NothingToRender,
    #[error("Could not save the video settings: {0}")]
    Io(String),
}

impl From<VideoError> for ProtocolError {
    fn from(error: VideoError) -> Self {
        let code = match error {
            VideoError::BadAddress
            | VideoError::TailTooLong
            | VideoError::NoCleanScene
            | VideoError::SameScene => ErrorCode::BadRequest,
            VideoError::NoEvent => ErrorCode::UnknownScope,
            VideoError::NothingToRender => ErrorCode::Refused,
            VideoError::Io(_) => ErrorCode::Internal,
        };
        ProtocolError::new(code, error.to_string())
    }
}

struct Inner {
    settings: StoredVideoSettings,
    status: VideoStatus,
    data_dir: Option<PathBuf>,
    /// The director asked for ffmpeg to be downloaded, and nobody has started yet.
    ffmpeg_asked: bool,
}

/// The video settings and status. Cloning shares the one desk.
#[derive(Clone)]
pub struct VideoDesk {
    inner: Arc<RwLock<Inner>>,
    /// Counts up each time the settings change, so the recorder knows to read them again.
    changed: watch::Sender<u64>,
    /// Counts up each time a render is asked for, so the renderer knows to look.
    asked: watch::Sender<u64>,
}

impl VideoDesk {
    /// The desk kept under `data_dir`.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let settings = data_dir
            .as_deref()
            .and_then(|dir| std::fs::read(dir.join(VIDEO_FILE)).ok())
            .and_then(|bytes| match serde_json::from_slice(&bytes) {
                Ok(settings) => Some(settings),
                Err(e) => {
                    eprintln!("WARNING: could not read {VIDEO_FILE}: {e}");
                    None
                }
            })
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                settings,
                status: VideoStatus::default(),
                data_dir,
                ffmpeg_asked: false,
            })),
            changed: watch::Sender::new(0),
            asked: watch::Sender::new(0),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// The settings as stored, password included. For the recorder only.
    pub fn stored(&self) -> StoredVideoSettings {
        self.read().settings.clone()
    }

    /// The settings as a client reads them.
    pub fn settings(&self) -> VideoSettings {
        let stored = self.stored();
        VideoSettings {
            enabled: stored.enabled,
            obs_address: stored.obs_address,
            has_password: !stored.obs_password.is_empty(),
            tail_seconds: stored.tail_seconds,
            trash_discarded: stored.trash_discarded,
            record_from: stored.record_from,
            clean_scene: stored.clean_scene,
            overlay_scene: stored.overlay_scene,
            render_when: stored.render_when,
            browser_path: stored.browser_path,
            ffmpeg_path: stored.ffmpeg_path,
        }
    }

    /// A receiver that changes each time the settings do.
    pub fn watch(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    /// Change the stored settings, and keep the change only if it could be saved.
    fn store(
        &self,
        change: impl FnOnce(&mut StoredVideoSettings),
        announce: bool,
    ) -> Result<(), VideoError> {
        let mut inner = self.write();
        let mut settings = inner.settings.clone();
        change(&mut settings);
        if settings == inner.settings {
            return Ok(());
        }
        if let Some(dir) = &inner.data_dir {
            let io = |e: std::io::Error| VideoError::Io(e.to_string());
            std::fs::create_dir_all(dir).map_err(io)?;
            let bytes =
                serde_json::to_vec_pretty(&settings).map_err(|e| VideoError::Io(e.to_string()))?;
            // Write beside the file, then swap it in, so a crash never leaves half a file.
            let scratch = dir.join(format!("{VIDEO_FILE}.tmp"));
            std::fs::write(&scratch, bytes).map_err(io)?;
            std::fs::rename(&scratch, dir.join(VIDEO_FILE)).map_err(io)?;
        }
        inner.settings = settings;
        drop(inner);
        if announce {
            self.changed.send_modify(|n| *n += 1);
        }
        Ok(())
    }

    /// Take new settings from a client.
    pub fn update(&self, request: &VideoSettingsRequest) -> Result<VideoSettings, VideoError> {
        let address = request.obs_address.trim();
        let well_formed = address
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !well_formed {
            return Err(VideoError::BadAddress);
        }
        if request.tail_seconds > 60 {
            return Err(VideoError::TailTooLong);
        }
        let clean_scene = request.clean_scene.trim();
        let overlay_scene = request.overlay_scene.trim();
        if request.record_from == RecordFrom::Scenes {
            if clean_scene.is_empty() {
                return Err(VideoError::NoCleanScene);
            }
            if clean_scene == overlay_scene {
                return Err(VideoError::SameScene);
            }
        }
        self.store(
            |settings| {
                settings.enabled = request.enabled;
                settings.obs_address = address.to_owned();
                if let Some(password) = &request.obs_password {
                    settings.obs_password = password.clone();
                }
                settings.tail_seconds = request.tail_seconds;
                settings.trash_discarded = request.trash_discarded;
                settings.record_from = request.record_from;
                settings.clean_scene = clean_scene.to_owned();
                settings.overlay_scene = overlay_scene.to_owned();
                settings.render_when = request.render_when;
                settings.browser_path = request.browser_path.trim().to_owned();
                settings.ffmpeg_path = request.ffmpeg_path.trim().to_owned();
            },
            true,
        )?;
        Ok(self.settings())
    }

    /// Remember OBS's own file naming while the Director has changed it for a race, or
    /// forget it (`None`) once it is put back. Not a change the recorder needs to hear.
    pub fn remember_obs_naming(&self, naming: Option<String>) {
        if let Err(e) = self.store(|settings| settings.obs_naming_before = naming, false) {
            eprintln!("WARNING: {e}");
        }
    }

    /// The status as it is now.
    pub fn status(&self) -> VideoStatus {
        self.read().status.clone()
    }

    /// Change what is known about the link to OBS.
    pub fn report_obs(&self, change: impl FnOnce(&mut ObsStatus)) {
        change(&mut self.write().status.obs);
    }

    /// Add a run, or change the one already listed for that race. The newest is first.
    pub fn report_run(&self, run: VideoRun) {
        let mut inner = self.write();
        let runs = &mut inner.status.runs;
        match runs
            .iter_mut()
            .find(|known| known.event == run.event && known.race == run.race)
        {
            Some(known) => *known = run,
            None => {
                runs.insert(0, run);
                runs.truncate(RUNS_KEPT);
            }
        }
    }
}

impl VideoDesk {
    /// Say which of the programs a render needs were found.
    pub fn report_tools(&self, tools: RenderTools) {
        self.write().status.tools = tools;
    }

    /// Where a downloaded ffmpeg is kept: the `tools` folder of the data folder.
    pub fn tools_dir(&self) -> Option<PathBuf> {
        self.read().data_dir.as_ref().map(|dir| dir.join("tools"))
    }

    /// Ask for ffmpeg to be downloaded. Answers `false` when Grid has nowhere to get it
    /// for this kind of computer, or nowhere to keep it.
    pub fn ask_ffmpeg(&self) -> bool {
        {
            let mut inner = self.write();
            if !inner.status.tools.can_fetch || inner.data_dir.is_none() {
                return false;
            }
            inner.ffmpeg_asked = true;
        }
        self.asked.send_modify(|n| *n += 1);
        true
    }

    /// Whether ffmpeg was asked for since the last time this was called.
    pub fn take_ffmpeg_ask(&self) -> bool {
        std::mem::take(&mut self.write().ffmpeg_asked)
    }

    /// A receiver that changes each time a render is asked for.
    pub fn watch_renders(&self) -> watch::Receiver<u64> {
        self.asked.subscribe()
    }

    /// Ask for a race's overlay to be drawn onto its recording. A race already waiting or
    /// being drawn is not listed twice.
    pub fn ask_render(&self, event: &EventId, race: &RaceId, name: &str) {
        let mut inner = self.write();
        let jobs = &mut inner.status.renders;
        let busy = jobs.iter().any(|job| {
            &job.event == event
                && &job.race == race
                && matches!(
                    job.state,
                    RenderState::Queued | RenderState::Rendering { .. }
                )
        });
        if busy {
            return;
        }
        jobs.retain(|job| !(&job.event == event && &job.race == race));
        jobs.insert(
            0,
            RenderJob {
                event: event.clone(),
                race: race.clone(),
                name: name.to_owned(),
                state: RenderState::Queued,
                note: None,
            },
        );
        jobs.truncate(RUNS_KEPT);
        drop(inner);
        self.asked.send_modify(|n| *n += 1);
    }

    /// The render that has waited longest, if any is waiting.
    pub fn next_render(&self) -> Option<RenderJob> {
        self.read()
            .status
            .renders
            .iter()
            .rev()
            .find(|job| job.state == RenderState::Queued)
            .cloned()
    }

    /// Change where a render stands.
    pub fn report_render(
        &self,
        event: &EventId,
        race: &RaceId,
        state: RenderState,
        note: Option<String>,
    ) {
        let mut inner = self.write();
        if let Some(job) = inner
            .status
            .renders
            .iter_mut()
            .find(|job| &job.event == event && &job.race == race)
        {
            job.state = state;
            job.note = note;
        }
    }
}

/// A race as the recorder needs to see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedRace {
    pub event: EventId,
    pub race: RaceId,
    /// What the race is called: "‹event› ‹heat› race N".
    pub name: String,
    pub state: RaceState,
    /// The start on the Director's clock, in microseconds: the tone while counting down,
    /// the logged start once running.
    pub starts_at: Option<i64>,
    /// The mark of the race's laps as they stand now (`view::laps_key`).
    pub laps_key: String,
}

/// What a race is called for its recording: the event, the heat with the phases above it,
/// and the race's number in the heat. "Club Night Qualifying Heat 2 race 1".
fn race_name(
    event: &str,
    state: &gridfpv_engine::state::EventState,
    race: &gridfpv_engine::race::Race,
) -> String {
    let mut labels = Vec::new();
    let mut at = Some(race.setup.heat.clone());
    // Bounded, so a loop in a bad log cannot hang the recorder.
    for _ in 0..16 {
        let Some(def) = at.as_ref().and_then(|id| state.tree.get(id)) else {
            break;
        };
        labels.push(def.label.clone());
        at = def.parent.clone();
    }
    labels.reverse();
    let number = state
        .races
        .of_heat(&race.setup.heat)
        .position(|other| other.setup.race == race.setup.race)
        .map_or(1, |index| index + 1);
    format!("{event} {} race {number}", labels.join(" "))
}

fn watched(
    event: &crate::events::EventMeta,
    state: &gridfpv_engine::state::EventState,
    race: &gridfpv_engine::race::Race,
) -> WatchedRace {
    WatchedRace {
        event: event.id.clone(),
        race: race.setup.race.clone(),
        name: race_name(&event.name, state, race),
        state: race.state,
        starts_at: race.started_at.or(race.tone_at),
        laps_key: crate::view::race_laps_key(state, race),
    }
}

/// The race of the active event that is counting down, running, or stopped and not yet
/// saved or discarded. `None` when there is no such race, or no active event.
pub fn current_race(registry: &EventRegistry) -> Option<WatchedRace> {
    let event = registry.active()?;
    with_event(registry, &event.id, |session| {
        let state = session.state();
        Ok(state
            .races
            .undecided()
            .map(|race| watched(&event, state, race)))
    })
    .ok()
    .flatten()
}

/// One race as it stands now, by id. `None` when the event or the race is gone.
pub fn race_now(registry: &EventRegistry, event: &EventId, race: &RaceId) -> Option<WatchedRace> {
    let meta = registry.meta_of(event)?;
    with_event(registry, event, |session| {
        let state = session.state();
        Ok(state
            .races
            .get(race)
            .map(|found| watched(&meta, state, found)))
    })
    .ok()
    .flatten()
}

/// What a render is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderSource {
    pub event: EventId,
    pub race: RaceId,
    /// What the race is called.
    pub name: String,
    /// The clean recording.
    pub file: String,
    /// Where the race's start is in the recording, in microseconds from its first frame.
    pub start_in_file_micros: i64,
    /// The mark of the laps the render will show.
    pub laps_key: String,
}

/// What to render a race from, if it has a clean recording and a start. `Err` says, as a
/// sentence, why it cannot be rendered.
pub fn render_source(
    registry: &EventRegistry,
    event: &EventId,
    race: &RaceId,
) -> Result<RenderSource, VideoError> {
    let meta = registry.meta_of(event).ok_or(VideoError::NoEvent)?;
    with_event(registry, event, |session| {
        let state = session.state();
        Ok((|| {
            let found = state.races.get(race)?;
            let video = state.videos.get(race).filter(|video| video.clean)?;
            let starts_at = found.started_at.or(found.tone_at)?;
            Some(RenderSource {
                event: event.clone(),
                race: race.clone(),
                name: race_name(&meta.name, state, found),
                file: video.file.clone(),
                start_in_file_micros: starts_at - video.first_frame_at,
                laps_key: crate::view::race_laps_key(state, found),
            })
        })())
    })
    .ok()
    .flatten()
    .ok_or(VideoError::NothingToRender)
}

/// The saved races of an event that have a clean recording and no overlaid video matching
/// their laps as they stand: the ones a render would do something for. Oldest first.
pub fn races_needing_render(registry: &EventRegistry, event: &EventId) -> Vec<(RaceId, String)> {
    let Some(meta) = registry.meta_of(event) else {
        return Vec::new();
    };
    with_event(registry, event, |session| {
        let state = session.state();
        Ok(state
            .races
            .iter()
            .filter(|race| race.state == RaceState::Saved)
            .filter_map(|race| {
                let video = state.videos.get(&race.setup.race)?;
                let key = crate::view::race_laps_key(state, race);
                let (overlay, _) = crate::view::overlay_state(video, &key);
                matches!(
                    overlay,
                    crate::view::VideoOverlay::Missing | crate::view::VideoOverlay::NeedsRender
                )
                .then(|| (race.setup.race.clone(), race_name(&meta.name, state, race)))
            })
            .collect())
    })
    .unwrap_or_default()
}

/// Log a fact about a race's recording into its event.
pub fn log_video(registry: &EventRegistry, event: &EventId, fact: gridfpv_events::Event) {
    if let Err(e) = with_event(registry, event, |session| session.observe(fact)) {
        eprintln!("WARNING: could not log a recording: {}", e.message);
    }
}

/// `GET /video/settings` — the video settings, without the password. An open read.
pub async fn get_settings(State(registry): State<EventRegistry>) -> Json<VideoSettings> {
    Json(registry.video().settings())
}

/// `PUT /video/settings` — change the video settings, RD-gated.
pub async fn put_settings(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<VideoSettingsRequest>,
) -> Result<Json<VideoSettings>, ProtocolError> {
    Ok(Json(registry.video().update(&body)?))
}

/// `POST /video/render` — ask for the overlay to be drawn onto a race's clean recording,
/// or onto every race of the event that needs it. RD-gated. Answers with how many were
/// asked for. The drawing happens in the background; `GET /video/status` follows it.
pub async fn post_render(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(body): Json<RenderRequest>,
) -> Result<Json<usize>, ProtocolError> {
    let desk = registry.video();
    let asked = match &body.race {
        Some(race) => {
            let source = render_source(&registry, &body.event, race)?;
            desk.ask_render(&body.event, race, &source.name);
            1
        }
        None => {
            if registry.meta_of(&body.event).is_none() {
                return Err(VideoError::NoEvent.into());
            }
            let races = races_needing_render(&registry, &body.event);
            for (race, name) in &races {
                desk.ask_render(&body.event, race, name);
            }
            races.len()
        }
    };
    Ok(Json(asked))
}

/// `POST /video/ffmpeg` — download ffmpeg and keep it in Grid's data folder. RD-gated. The
/// download runs in the background; `GET /video/status` says when it is there.
pub async fn post_ffmpeg(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
) -> Result<axum::http::StatusCode, ProtocolError> {
    if registry.video().ask_ffmpeg() {
        Ok(axum::http::StatusCode::ACCEPTED)
    } else {
        Err(ProtocolError::new(
            ErrorCode::Refused,
            "Grid cannot download ffmpeg for this kind of computer. Install ffmpeg yourself.",
        ))
    }
}

/// `GET /video/status` — the link to OBS and this session's runs. An open read.
pub async fn get_status(State(registry): State<EventRegistry>) -> Json<VideoStatus> {
    Json(registry.video().status())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> VideoSettingsRequest {
        VideoSettingsRequest {
            enabled: true,
            obs_address: " 192.168.1.20:4455 ".into(),
            obs_password: Some("secret".into()),
            tail_seconds: 4,
            trash_discarded: false,
            record_from: RecordFrom::Obs,
            clean_scene: String::new(),
            overlay_scene: String::new(),
            render_when: RenderWhen::Asked,
            browser_path: String::new(),
            ffmpeg_path: String::new(),
        }
    }

    #[test]
    fn settings_start_off_and_never_give_the_password_back() {
        let desk = VideoDesk::new(None);
        assert_eq!(
            desk.settings(),
            VideoSettings {
                enabled: false,
                obs_address: "127.0.0.1:4455".into(),
                has_password: false,
                tail_seconds: 3,
                trash_discarded: true,
                record_from: RecordFrom::Obs,
                clean_scene: String::new(),
                overlay_scene: String::new(),
                render_when: RenderWhen::Asked,
                browser_path: String::new(),
                ffmpeg_path: String::new(),
            }
        );
        let mut watch = desk.watch();
        watch.mark_unchanged();
        let saved = desk.update(&request()).unwrap();
        assert_eq!(saved.obs_address, "192.168.1.20:4455");
        assert!(saved.has_password && saved.enabled && !saved.trash_discarded);
        assert_eq!(desk.stored().obs_password, "secret");
        // The recorder is told.
        assert!(watch.has_changed().unwrap());
        // Nothing a client reads holds the password.
        assert!(
            !serde_json::to_string(&desk.settings())
                .unwrap()
                .contains("secret")
        );
    }

    #[test]
    fn a_password_left_out_is_kept_and_an_empty_one_clears_it() {
        let desk = VideoDesk::new(None);
        desk.update(&request()).unwrap();
        let mut kept = request();
        kept.obs_password = None;
        kept.tail_seconds = 6;
        assert!(desk.update(&kept).unwrap().has_password);
        assert_eq!(desk.stored().obs_password, "secret");
        let mut cleared = request();
        cleared.obs_password = Some(String::new());
        assert!(!desk.update(&cleared).unwrap().has_password);
    }

    #[test]
    fn a_bad_address_or_a_long_tail_is_refused_and_nothing_changes() {
        let desk = VideoDesk::new(None);
        for address in ["", "obs", "obs:", ":4455", "obs:port"] {
            let mut bad = request();
            bad.obs_address = address.into();
            assert_eq!(desk.update(&bad), Err(VideoError::BadAddress), "{address}");
        }
        let mut long = request();
        long.tail_seconds = 61;
        assert_eq!(desk.update(&long), Err(VideoError::TailTooLong));
        assert!(!desk.settings().enabled);
    }

    #[test]
    fn settings_are_kept_in_the_data_directory_with_the_naming_to_put_back() {
        let dir = std::env::temp_dir().join(format!("gridfpv-video-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let desk = VideoDesk::new(Some(dir.clone()));
        desk.update(&request()).unwrap();
        let mut watch = desk.watch();
        watch.mark_unchanged();
        desk.remember_obs_naming(Some("%CCYY-%MM-%DD %hh-%mm-%ss".into()));
        // Remembering the naming is the recorder's own note: it is not told about it.
        assert!(!watch.has_changed().unwrap());
        let again = VideoDesk::new(Some(dir.clone()));
        assert_eq!(again.stored(), desk.stored());
        assert_eq!(
            again.stored().obs_naming_before.as_deref(),
            Some("%CCYY-%MM-%DD %hh-%mm-%ss")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recording_scenes_needs_a_clean_scene_that_is_not_also_the_overlay_scene() {
        let desk = VideoDesk::new(None);
        let mut scenes = request();
        scenes.record_from = RecordFrom::Scenes;
        assert_eq!(desk.update(&scenes), Err(VideoError::NoCleanScene));
        scenes.clean_scene = " Clean ".into();
        scenes.overlay_scene = "Clean".into();
        assert_eq!(desk.update(&scenes), Err(VideoError::SameScene));
        scenes.overlay_scene = " Program ".into();
        let saved = desk.update(&scenes).unwrap();
        assert_eq!(
            (saved.clean_scene.as_str(), saved.overlay_scene.as_str()),
            ("Clean", "Program")
        );
        // With OBS's own recording the scene names are not looked at.
        let mut own = request();
        own.clean_scene = String::new();
        assert!(desk.update(&own).is_ok());
    }

    #[test]
    fn a_render_is_asked_for_once_and_taken_oldest_first() {
        let desk = VideoDesk::new(None);
        let event = EventId("night".into());
        let race = |n: u32| RaceId(format!("heat#{n}"));
        let mut asked = desk.watch_renders();
        asked.mark_unchanged();
        desk.ask_render(&event, &race(1), "Night Heat race 1");
        desk.ask_render(&event, &race(2), "Night Heat race 2");
        // Already waiting: not listed twice.
        desk.ask_render(&event, &race(1), "Night Heat race 1");
        assert!(asked.has_changed().unwrap());
        assert_eq!(desk.status().renders.len(), 2);
        assert_eq!(desk.next_render().unwrap().race, race(1));
        desk.report_render(
            &event,
            &race(1),
            RenderState::Rendering { percent: 40 },
            None,
        );
        assert_eq!(desk.next_render().unwrap().race, race(2));
        desk.report_render(
            &event,
            &race(1),
            RenderState::Failed,
            Some("ffmpeg is not installed.".into()),
        );
        // A finished or failed one can be asked for again, and takes the place of the old.
        desk.ask_render(&event, &race(1), "Night Heat race 1");
        let renders = desk.status().renders;
        assert_eq!(renders.len(), 2);
        assert_eq!(
            (
                renders[0].race.clone(),
                renders[0].state.clone(),
                renders[0].note.clone()
            ),
            (race(1), RenderState::Queued, None)
        );
    }

    #[test]
    fn a_run_is_listed_once_newest_first_and_follows_its_state() {
        let desk = VideoDesk::new(None);
        let run = |race: &str, state| VideoRun {
            event: EventId("night".into()),
            race: RaceId(race.into()),
            name: format!("Night {race}"),
            state,
            file: None,
            start_at_ms: None,
            note: None,
        };
        desk.report_run(run("heat#1", VideoRunState::Recording));
        desk.report_run(run("heat#2", VideoRunState::Recording));
        desk.report_run(run("heat#1", VideoRunState::Saved));
        let runs = desk.status().runs;
        assert_eq!(runs.len(), 2);
        assert_eq!(
            (runs[0].race.0.as_str(), runs[0].state),
            ("heat#2", VideoRunState::Recording)
        );
        assert_eq!(runs[1].state, VideoRunState::Saved);
    }
}
