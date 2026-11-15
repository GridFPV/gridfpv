//! Starting and stopping one race's recording in OBS, the two ways there are.
//!
//! - **OBS's own recording** ([`RecordFrom::Obs`]): what its Start Recording button
//!   records. OBS names a file from its "filename formatting" setting when the recording
//!   starts, so that is set to the race's name just before, and OBS's own value is put back
//!   once the recording has stopped. The old value is kept in the video settings meanwhile,
//!   so it is put back even if the Director stops in between.
//!
//! - **A scene of its own** ([`RecordFrom::Scenes`]): exeldro's Source Record plugin, a
//!   filter on a scene that records that scene to its own file whatever OBS's own recording
//!   and stream are doing. The clean scene (the camera, no overlay) is what an overlay can
//!   be drawn onto afterwards. A second scene, with the live overlay on it, can be recorded
//!   alongside: that one is a finished video straight from OBS, right as long as the laps
//!   stand.
//!
//!   The plugin is driven through OBS's WebSocket "vendor requests" (vendor
//!   `source-record`): `record_start {source, filter, filename}` and `record_stop {source,
//!   filter}`. While it records, OBS lists the filter's output under the filter's name, so
//!   `GetOutputStatus` says when frames are flowing and when the file is closed. (Outputs
//!   of finished recordings stay in OBS's list, not active; the newest of a name answers.)
//!   Each scene's filter is named "Grid record ‹scene›" so the names never clash. Verified
//!   against OBS 32 with Source Record 0.4.8.
//!
//! Either way [`start`] returns once frames are really flowing, so the caller's clock at
//! that moment is the file's first frame, and [`stop`] returns once the file is closed.

use std::path::Path;
use std::time::Duration;

use gridfpv_server::video::{RecordFrom, StoredVideoSettings, VideoDesk};
use serde_json::{Value, json};

use crate::obs::Obs;

/// How long OBS is given to say a recording has really started.
const STARTED: Duration = Duration::from_millis(2500);
/// How long OBS is given to finish writing a recording after it is told to stop.
const STOPPED: Duration = Duration::from_secs(20);

/// The plugin's name to OBS's WebSocket server.
const VENDOR: &str = "source-record";
/// The kind of filter the plugin adds.
const FILTER_KIND: &str = "source_record_filter";

/// A recording in progress: what has to be stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tape {
    /// OBS's own recording.
    Own,
    /// One scene, or two, each by a Source Record filter.
    Scenes {
        clean: SceneTape,
        overlay: Option<SceneTape>,
    },
}

/// One scene being recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneTape {
    scene: String,
    filter: String,
    /// The folder the file goes into, as OBS knows it.
    folder: String,
    /// The file's name, without its extension.
    name: String,
}

/// What a finished recording left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// The recording: OBS's own, or the clean scene's.
    pub file: String,
    /// Whether `file` has no overlay in it.
    pub clean: bool,
    /// The overlay scene's recording, when one was made.
    pub overlay_file: Option<String>,
}

/// Whether OBS's own recording is running.
pub async fn own_recording(obs: &Obs) -> Result<bool, String> {
    let status = obs.call("GetRecordStatus", json!({})).await?;
    Ok(status["outputActive"] == true)
}

async fn naming(obs: &Obs) -> Result<String, String> {
    let got = obs
        .call(
            "GetProfileParameter",
            json!({ "parameterCategory": "Output", "parameterName": "FilenameFormatting" }),
        )
        .await?;
    Ok(got["parameterValue"]
        .as_str()
        .or(got["defaultParameterValue"].as_str())
        .unwrap_or("%CCYY-%MM-%DD %hh-%mm-%ss")
        .to_owned())
}

async fn set_naming(obs: &Obs, naming: &str) -> Result<(), String> {
    obs.call(
        "SetProfileParameter",
        json!({
            "parameterCategory": "Output",
            "parameterName": "FilenameFormatting",
            "parameterValue": naming,
        }),
    )
    .await
    .map(|_| ())
}

/// Put OBS's own file naming back, if the Director changed it for a race.
pub async fn restore_naming(obs: &Obs, desk: &VideoDesk) {
    if let Some(before) = desk.stored().obs_naming_before
        && set_naming(obs, &before).await.is_ok()
    {
        desk.remember_obs_naming(None);
    }
}

/// Wait for OBS to say its own recording reached `state`.
async fn own_state(
    events: &mut tokio::sync::broadcast::Receiver<crate::obs::ObsEvent>,
    state: &str,
    within: Duration,
) -> bool {
    tokio::time::timeout(within, async {
        loop {
            match events.recv().await {
                Ok(event)
                    if event.kind == "RecordStateChanged" && event.data["outputState"] == state =>
                {
                    return true;
                }
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(_) => return false,
            }
        }
    })
    .await
    .unwrap_or(false)
}

/// Start recording a race, under `name` (already safe as a file name). Returns once frames
/// are flowing. The error is a sentence for the director.
pub async fn start(
    obs: &Obs,
    desk: &VideoDesk,
    settings: &StoredVideoSettings,
    name: &str,
) -> Result<Tape, String> {
    match settings.record_from {
        RecordFrom::Obs => start_own(obs, desk, name).await.map(|()| Tape::Own),
        RecordFrom::Scenes => {
            if settings.clean_scene.is_empty() {
                return Err("No scene is picked to record. Pick one on the Video page.".into());
            }
            // The clean recording starts first and alone: its first frame is the moment
            // the caller notes. The overlaid one follows at once.
            let clean = scene_start(obs, &settings.clean_scene, &format!("{name} - Clean")).await?;
            let overlay = if settings.overlay_scene.is_empty() {
                None
            } else {
                match scene_start(obs, &settings.overlay_scene, &format!("{name} - Overlay")).await
                {
                    Ok(tape) => Some(tape),
                    Err(e) => {
                        // The clean recording is the one that matters. It carries on.
                        eprintln!("WARNING: the overlay scene is not being recorded: {e}");
                        None
                    }
                }
            };
            Ok(Tape::Scenes { clean, overlay })
        }
    }
}

async fn start_own(obs: &Obs, desk: &VideoDesk, name: &str) -> Result<(), String> {
    if own_recording(obs).await? {
        return Err(
            "OBS was already recording, so this race is inside that recording. Grid left it alone."
                .into(),
        );
    }
    // OBS's own naming, kept so it can be put back. If one is already remembered, the
    // race's name from an earlier recording is still in OBS: keep the older, true one.
    let before = match desk.stored().obs_naming_before {
        Some(before) => before,
        None => naming(obs).await?,
    };
    desk.remember_obs_naming(Some(before));
    set_naming(obs, name).await?;

    // OBS ignores a start that lands while an earlier recording is still stopping, and
    // says nothing. So wait for it to say the output is running, and ask again if not.
    let mut events = obs.events();
    for _ in 0..3 {
        if let Err(e) = obs.call("StartRecord", json!({})).await
            && !own_recording(obs).await.unwrap_or(false)
        {
            restore_naming(obs, desk).await;
            return Err(e);
        }
        if own_state(&mut events, "OBS_WEBSOCKET_OUTPUT_STARTED", STARTED).await
            || own_recording(obs).await.unwrap_or(false)
        {
            return Ok(());
        }
    }
    restore_naming(obs, desk).await;
    Err("OBS did not start recording.".into())
}

/// Stop a recording and say what it wrote. Returns once the file is closed.
pub async fn stop(obs: &Obs, desk: &VideoDesk, tape: Tape) -> Result<Written, String> {
    match tape {
        Tape::Own => {
            // StopRecord answers as soon as the stopping has begun. The file is only
            // finished, and OBS only ready to record again, once it says the output has
            // stopped. Waiting for that here is what lets the next race start a recording
            // straight away.
            let mut events = obs.events();
            let stopped = obs.call("StopRecord", json!({})).await;
            if stopped.is_ok() {
                own_state(&mut events, "OBS_WEBSOCKET_OUTPUT_STOPPED", STOPPED).await;
            }
            restore_naming(obs, desk).await;
            let answer = stopped?;
            Ok(Written {
                file: answer["outputPath"].as_str().unwrap_or_default().to_owned(),
                clean: false,
                overlay_file: None,
            })
        }
        Tape::Scenes { clean, overlay } => {
            // Both are told to stop before either is waited for, so they end together.
            let overlay_file = match overlay {
                Some(tape) => {
                    let asked = scene_ask_stop(obs, &tape).await;
                    let file = scene_stop(obs, &clean).await;
                    let overlay_file = match asked {
                        Ok(()) => scene_finish(obs, &tape).await.ok(),
                        Err(_) => None,
                    };
                    return Ok(Written {
                        file: file?,
                        clean: true,
                        overlay_file,
                    });
                }
                None => None,
            };
            Ok(Written {
                file: scene_stop(obs, &clean).await?,
                clean: true,
                overlay_file,
            })
        }
    }
}

/// One call to the Source Record plugin. Its answer says `success`, or `error` with why.
async fn vendor(obs: &Obs, request: &str, data: Value) -> Result<(), String> {
    let answer = obs
        .call(
            "CallVendorRequest",
            json!({ "vendorName": VENDOR, "requestType": request, "requestData": data }),
        )
        .await
        .map_err(|e| {
            if e.to_lowercase().contains("vendor") {
                "OBS does not have the Source Record plugin, which recording a scene needs. Install it in OBS (exeldro's Source Record).".to_owned()
            } else {
                e
            }
        })?;
    let said = &answer["responseData"];
    if said["success"] == true {
        Ok(())
    } else {
        Err(format!(
            "Source Record said no: {}.",
            said["error"].as_str().unwrap_or("it gave no reason")
        ))
    }
}

/// Whether OBS has the Source Record plugin. A stop for a source that does not exist is
/// harmless, and only a missing plugin answers that the vendor is unknown.
pub async fn has_source_record(obs: &Obs) -> bool {
    let probe = obs
        .call(
            "CallVendorRequest",
            json!({
                "vendorName": VENDOR,
                "requestType": "record_stop",
                "requestData": { "source": "\u{0}gridfpv-probe" },
            }),
        )
        .await;
    match probe {
        Ok(_) => true,
        Err(e) => !e.to_lowercase().contains("vendor"),
    }
}

/// OBS's scenes by name, in the order its own list shows them.
pub async fn scenes(obs: &Obs) -> Vec<String> {
    let Ok(list) = obs.call("GetSceneList", json!({})).await else {
        return Vec::new();
    };
    let mut names: Vec<String> = list["scenes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|scene| scene["sceneName"].as_str().map(str::to_owned))
        .collect();
    // OBS answers bottom first.
    names.reverse();
    names
}

/// The scene's Source Record filter, by name: the one it has, renamed, or one made now.
async fn scene_filter(obs: &Obs, scene: &str, folder: &str) -> Result<String, String> {
    let list = obs
        .call("GetSourceFilterList", json!({ "sourceName": scene }))
        .await
        .map_err(|_| {
            format!("OBS has no scene called \"{scene}\". Pick the scene again on the Video page.")
        })?;
    let found = list["filters"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|filter| filter["filterKind"] == FILTER_KIND)
        .and_then(|filter| filter["filterName"].as_str().map(str::to_owned));
    // Named for the scene. OBS lists a filter's recording under the filter's name, and two
    // scenes whose filters share a name ("Source Record", the plugin's own) cannot be told
    // apart there. So a filter the scene already has keeps its settings and takes this name.
    let filter = format!("Grid record {scene}");
    if let Some(found) = found {
        if found != filter {
            obs.call(
                "SetSourceFilterName",
                json!({ "sourceName": scene, "filterName": found, "newFilterName": filter }),
            )
            .await?;
        }
        // The folder is set every time. The plugin's own default is not OBS's recording
        // folder, and a file would otherwise land where nobody looks.
        obs.call(
            "SetSourceFilterSettings",
            json!({
                "sourceName": scene,
                "filterName": filter,
                "filterSettings": { "path": folder },
                "overlay": true,
            }),
        )
        .await?;
        return Ok(filter);
    }
    obs.call(
        "CreateSourceFilter",
        json!({
            "sourceName": scene,
            "filterName": filter,
            "filterKind": FILTER_KIND,
            "filterSettings": { "path": folder },
        }),
    )
    .await?;
    Ok(filter)
}

/// Whether a scene's Source Record output is running. OBS lists it while it records; gone
/// from the list means stopped.
async fn scene_active(obs: &Obs, filter: &str) -> bool {
    obs.call("GetOutputStatus", json!({ "outputName": filter }))
        .await
        .is_ok_and(|status| status["outputActive"] == true)
}

async fn scene_start(obs: &Obs, scene: &str, name: &str) -> Result<SceneTape, String> {
    // Where the files go: the folder the scene's filter already has, else OBS's own.
    let own = obs
        .call("GetRecordDirectory", json!({}))
        .await?
        .get("recordDirectory")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let list = obs
        .call("GetSourceFilterList", json!({ "sourceName": scene }))
        .await
        .map_err(|_| {
            format!("OBS has no scene called \"{scene}\". Pick the scene again on the Video page.")
        })?;
    let folder = list["filters"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|filter| filter["filterKind"] == FILTER_KIND)
        .and_then(|filter| filter["filterSettings"]["path"].as_str())
        .filter(|path| !path.is_empty())
        .map_or(own, str::to_owned);
    let filter = scene_filter(obs, scene, &folder).await?;
    if scene_active(obs, &filter).await {
        return Err(format!(
            "The scene \"{scene}\" was already being recorded, so this race is inside that recording. Grid left it alone."
        ));
    }
    vendor(
        obs,
        "record_start",
        json!({ "source": scene, "filter": filter, "filename": name }),
    )
    .await?;
    // The plugin starts its output a moment after it answers. Frames are flowing, and the
    // file's time is zero, once the output is listed as active.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !scene_active(obs, &filter).await {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("The scene \"{scene}\" did not start recording."));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(SceneTape {
        scene: scene.to_owned(),
        filter,
        folder,
        name: name.to_owned(),
    })
}

async fn scene_ask_stop(obs: &Obs, tape: &SceneTape) -> Result<(), String> {
    vendor(
        obs,
        "record_stop",
        json!({ "source": tape.scene, "filter": tape.filter }),
    )
    .await
}

/// Wait for a scene's file to be closed, and say where it is.
async fn scene_finish(obs: &Obs, tape: &SceneTape) -> Result<String, String> {
    let deadline = tokio::time::Instant::now() + STOPPED;
    while scene_active(obs, &tape.filter).await {
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "The scene \"{}\" did not stop recording.",
                tape.scene
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // The plugin picks the extension. When the folder is on this machine the file is looked
    // for; when it is not, the extension is the one the filter is set to.
    if let Some(found) = find_file(Path::new(&tape.folder), &tape.name) {
        return Ok(found);
    }
    let settings = obs
        .call(
            "GetSourceFilter",
            json!({ "sourceName": tape.scene, "filterName": tape.filter }),
        )
        .await
        .unwrap_or(Value::Null);
    let extension = settings["filterSettings"]["rec_format"]
        .as_str()
        .filter(|format| !format.is_empty())
        .unwrap_or("mkv");
    Ok(join(&tape.folder, &format!("{}.{extension}", tape.name)))
}

async fn scene_stop(obs: &Obs, tape: &SceneTape) -> Result<String, String> {
    scene_ask_stop(obs, tape).await?;
    scene_finish(obs, tape).await
}

/// The file in `folder` called `name` with any extension, if the folder is on this machine.
fn find_file(folder: &Path, name: &str) -> Option<String> {
    let prefix = format!("{name}.");
    std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.starts_with(&prefix) && !file.ends_with(".tmp"))
        })
        .map(|path| path.to_string_lossy().into_owned())
}

/// A folder and a file name joined the way the folder itself is written, so a path on a
/// Windows OBS reads right even when the Director is on another machine.
fn join(folder: &str, file: &str) -> String {
    let separator = if folder.contains('\\') { '\\' } else { '/' };
    format!("{}{separator}{file}", folder.trim_end_matches(['/', '\\']))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_joined_the_way_its_folder_is_written() {
        assert_eq!(join("C:\\Videos", "a.mkv"), "C:\\Videos\\a.mkv");
        assert_eq!(join("C:\\Videos\\", "a.mkv"), "C:\\Videos\\a.mkv");
        assert_eq!(join("/recordings", "a.mkv"), "/recordings/a.mkv");
        assert_eq!(join("/recordings/", "a.mkv"), "/recordings/a.mkv");
    }

    #[test]
    fn a_scenes_file_is_found_by_name_whatever_its_extension() {
        let dir = std::env::temp_dir().join(format!("gridfpv-tape-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Night race 1 - Clean.mkv"), b"v").unwrap();
        // A longer name that starts the same is another race's file.
        std::fs::write(dir.join("Night race 10 - Clean.mkv"), b"v").unwrap();
        std::fs::write(dir.join("Night race 2 - Clean.mkv.tmp"), b"v").unwrap();
        assert_eq!(
            find_file(&dir, "Night race 1 - Clean").as_deref(),
            Some(
                dir.join("Night race 1 - Clean.mkv")
                    .to_string_lossy()
                    .as_ref()
            )
        );
        // Still being written: not a file yet.
        assert_eq!(find_file(&dir, "Night race 2 - Clean"), None);
        assert_eq!(find_file(Path::new("/nowhere/at/all"), "x"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
