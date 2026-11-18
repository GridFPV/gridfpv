//! A copy of everything Grid keeps, to carry to another computer, and bringing one back in.
//!
//! A copy is one zip file: the pilots, classes, formats, seat profiles and timers, which
//! event is open, and every event's own log. It leaves out what belongs to one computer:
//! the video setup (the OBS address and password, where the browser and ffmpeg are), the
//! log files and the recordings themselves.
//!
//! Bringing a copy in **replaces** what this computer has. It is done in two steps so that
//! no open file is ever swapped under a running Director (Windows refuses that outright):
//!
//! 1. [`stage`] checks the file and keeps it as [`PENDING_FILE`] in the data folder.
//! 2. [`apply_waiting`] runs when the Director next starts, before anything is opened. It
//!    moves what is there now into a folder under [`BACKUPS_DIR`], then unpacks the copy.
//!
//! Nothing is deleted by either step.

use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::app::now_micros;
use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::{ACTIVE_EVENT_FILE, EventRegistry};

/// Where copies made on this computer are written, under the data folder.
pub const EXPORTS_DIR: &str = "exports";
/// Where what was here before a copy came in is kept, under the data folder.
pub const BACKUPS_DIR: &str = "backups";
/// A copy that was brought in and waits for the next start.
pub const PENDING_FILE: &str = "import-pending.zip";
/// A waiting copy that could not be used, kept for a look.
pub const FAILED_FILE: &str = "import-failed.zip";
/// The largest copy that is accepted, in bytes.
pub const MAX_IMPORT_BYTES: usize = 1 << 30;

/// The note inside a copy that says what it is.
const MANIFEST: &str = "grid-data.json";
/// The one layout of a copy there is so far.
const FORMAT: u32 = 1;
/// Event logs sit under this folder inside a copy.
const EVENTS_PREFIX: &str = "events/";
const EVENT_SUFFIX: &str = ".sqlite";
/// How many copies made here are kept before the oldest is removed.
const EXPORTS_KEPT: usize = 5;
/// How every SQLite file begins.
const SQLITE_MAGIC: &[u8] = b"SQLite format 3\0";

/// The plain files that go into a copy, when they exist.
pub const KEPT_FILES: [&str; 9] = [
    crate::pilots::PILOTS_FILE,
    crate::classes::CLASSES_FILE,
    crate::classes::HIDDEN_CLASSES_FILE,
    crate::classes::REMOVED_CLASSES_FILE,
    crate::library::FORMATS_FILE,
    crate::library::PROFILES_FILE,
    crate::timers::TIMERS_FILE,
    crate::overlay::OVERLAY_FILE,
    ACTIVE_EVENT_FILE,
];

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    format: u32,
    /// The Grid build that made the copy.
    version: String,
    /// When it was made, in microseconds since 1970.
    made_at: i64,
}

/// What a copy holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../bindings/")]
pub struct DataCopy {
    /// How many events are in it.
    pub events: u32,
    /// When it was made, in microseconds since 1970.
    #[ts(type = "number")]
    pub made_at: i64,
    /// The Grid build that made it.
    pub version: String,
}

/// Where Grid keeps its data, and whether a copy waits to be brought in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../bindings/")]
pub struct DataInfo {
    /// The data folder on the Director's computer. Absent when Grid keeps nothing on disk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub folder: Option<String>,
    /// A copy that was brought in and takes over when Grid is next started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub waiting: Option<DataCopy>,
}

/// A copy that was just made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "../../../bindings/")]
pub struct DataExport {
    /// The file's name, which `GET /data/export/{name}` serves.
    pub name: String,
    /// Where the file is on the Director's computer.
    pub file: String,
    /// What is in it.
    pub copy: DataCopy,
}

/// Why a copy could not be made or brought in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupError {
    #[error("Grid is not keeping its data in a folder, so there is nothing to copy.")]
    NoFolder,
    #[error("That file is not a copy made by Grid. {0}")]
    NotACopy(String),
    #[error("A race is on. Bring the copy in when it is over.")]
    RaceOn,
    #[error("There is no copy with that name.")]
    NoSuchExport,
    #[error("{0}")]
    Io(String),
}

impl From<BackupError> for ProtocolError {
    fn from(err: BackupError) -> Self {
        let code = match err {
            BackupError::NoFolder | BackupError::RaceOn => ErrorCode::Refused,
            BackupError::NotACopy(_) => ErrorCode::BadRequest,
            BackupError::NoSuchExport => ErrorCode::UnknownScope,
            BackupError::Io(_) => ErrorCode::Internal,
        };
        ProtocolError::new(code, err.to_string())
    }
}

fn io(what: &str, err: impl std::fmt::Display) -> BackupError {
    BackupError::Io(format!("Could not {what}: {err}"))
}

// ── Making a copy ────────────────────────────────────────────────────────────

/// Make a copy of everything in `dir`, as a zip under its [`EXPORTS_DIR`]. Safe while the
/// Director runs: each event's log is copied through SQLite, not read off the disk.
pub fn export(dir: &Path, now: i64) -> Result<DataExport, BackupError> {
    let exports = dir.join(EXPORTS_DIR);
    std::fs::create_dir_all(&exports).map_err(|e| io("make the folder for copies", e))?;
    let name = format!("grid-data-{}.zip", stamp(now));
    let file = exports.join(&name);
    let part = exports.join(format!("{name}.part"));

    let made = write_copy(dir, &exports, &part, now);
    let copy = match made {
        Ok(copy) => copy,
        Err(err) => {
            let _ = std::fs::remove_file(&part);
            return Err(err);
        }
    };
    std::fs::rename(&part, &file).map_err(|e| io("save the copy", e))?;
    prune(&exports);
    Ok(DataExport {
        name,
        file: file.to_string_lossy().into_owned(),
        copy,
    })
}

fn write_copy(dir: &Path, scratch: &Path, part: &Path, now: i64) -> Result<DataCopy, BackupError> {
    let out = std::fs::File::create(part).map_err(|e| io("write the copy", e))?;
    let mut zip = zip::ZipWriter::new(out);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .large_file(true);
    let zip_err = |e: zip::result::ZipError| io("write the copy", e);

    let manifest = Manifest {
        format: FORMAT,
        version: crate::BUILD_VERSION.to_string(),
        made_at: now,
    };
    zip.start_file(MANIFEST, options).map_err(zip_err)?;
    zip.write_all(&serde_json::to_vec_pretty(&manifest).unwrap_or_default())
        .map_err(|e| io("write the copy", e))?;

    for name in KEPT_FILES {
        let Ok(bytes) = std::fs::read(dir.join(name)) else {
            continue;
        };
        zip.start_file(name, options).map_err(zip_err)?;
        zip.write_all(&bytes).map_err(|e| io("write the copy", e))?;
    }

    let mut events = 0u32;
    for log in event_logs(dir) {
        let Some(log_name) = log.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // A whole, settled copy of the log, taken through SQLite so a write that is under
        // way is either all in or all out.
        let snap = scratch.join(format!(".copying-{log_name}"));
        let _ = std::fs::remove_file(&snap);
        gridfpv_storage::snapshot(&log, &snap).map_err(|e| io("copy an event", e))?;
        let bytes = std::fs::read(&snap);
        let _ = std::fs::remove_file(&snap);
        let bytes = bytes.map_err(|e| io("copy an event", e))?;
        zip.start_file(format!("{EVENTS_PREFIX}{log_name}"), options)
            .map_err(zip_err)?;
        zip.write_all(&bytes).map_err(|e| io("write the copy", e))?;
        events += 1;
    }
    zip.finish().map_err(zip_err)?;
    Ok(DataCopy {
        events,
        made_at: now,
        version: manifest.version,
    })
}

/// Every event log in the data folder, in name order.
fn event_logs(dir: &Path) -> Vec<PathBuf> {
    let mut logs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_suffix(EVENT_SUFFIX))
                .is_some_and(safe_id)
        })
        .collect();
    logs.sort();
    logs
}

/// Whether `name` is one a copy made here carries: nothing that could name another folder.
fn export_name(name: &str) -> bool {
    name.strip_prefix("grid-data-")
        .and_then(|rest| rest.strip_suffix(".zip"))
        .is_some_and(|stamp| {
            !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_digit() || c == '-')
        })
}

/// Keep the newest few copies made here; a copy is meant to be carried away, not stored.
fn prune(exports: &Path) {
    let mut copies: Vec<PathBuf> = std::fs::read_dir(exports)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(export_name)
        })
        .collect();
    // The name carries the time, so name order is age order.
    copies.sort();
    let extra = copies.len().saturating_sub(EXPORTS_KEPT);
    for old in copies.into_iter().take(extra) {
        let _ = std::fs::remove_file(old);
    }
}

// ── Bringing a copy in ───────────────────────────────────────────────────────

/// An event id as Grid makes them: letters, digits, `-` and `_`. Anything else in a copy
/// could be a path to somewhere it has no business writing.
fn safe_id(stem: &str) -> bool {
    !stem.is_empty()
        && stem.len() <= 200
        && stem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Where one entry of a copy belongs in the data folder, or `None` for one that does not
/// belong in a copy at all.
fn place_of(entry: &str) -> Option<String> {
    if KEPT_FILES.contains(&entry) {
        return Some(entry.to_string());
    }
    let log = entry.strip_prefix(EVENTS_PREFIX)?;
    log.strip_suffix(EVENT_SUFFIX)
        .is_some_and(safe_id)
        .then(|| log.to_string())
}

/// The files of a copy: where each goes in the data folder, and what is in it.
type Unpacked = Vec<(String, Vec<u8>)>;

/// Read a copy: its note, and every file with where it goes. Refuses anything that is not
/// exactly what [`export`] writes.
fn read_copy(bytes: &[u8]) -> Result<(DataCopy, Unpacked), BackupError> {
    let not = |why: &str| BackupError::NotACopy(why.to_string());
    let mut zip =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| not("It is not a zip file."))?;
    let mut manifest: Option<Manifest> = None;
    let mut files = Vec::new();
    let mut events = 0u32;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|_| not("It could not be read."))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry
            .name()
            .map_err(|_| not("It could not be read."))?
            .to_string();
        let mut content = Vec::new();
        entry
            .read_to_end(&mut content)
            .map_err(|_| not("It could not be read."))?;
        if name == MANIFEST {
            manifest = serde_json::from_slice(&content).ok();
            continue;
        }
        let Some(place) = place_of(&name) else {
            return Err(not("It holds a file Grid does not know."));
        };
        if name.starts_with(EVENTS_PREFIX) {
            if !content.starts_with(SQLITE_MAGIC) {
                return Err(not("One of its events is damaged."));
            }
            events += 1;
        }
        files.push((place, content));
    }
    let manifest = manifest.ok_or_else(|| not("It does not say what it is."))?;
    if manifest.format != FORMAT {
        return Err(not("It was made by a newer Grid than this one."));
    }
    Ok((
        DataCopy {
            events,
            made_at: manifest.made_at,
            version: manifest.version,
        },
        files,
    ))
}

/// What is in a copy, without changing anything.
pub fn inspect(bytes: &[u8]) -> Result<DataCopy, BackupError> {
    read_copy(bytes).map(|(copy, _)| copy)
}

/// Check a copy and keep it to take over at the next start. Replaces a copy already waiting.
pub fn stage(dir: &Path, bytes: &[u8]) -> Result<DataCopy, BackupError> {
    let copy = inspect(bytes)?;
    std::fs::create_dir_all(dir).map_err(|e| io("keep the copy", e))?;
    let part = dir.join(format!("{PENDING_FILE}.part"));
    std::fs::write(&part, bytes).map_err(|e| io("keep the copy", e))?;
    std::fs::rename(&part, dir.join(PENDING_FILE)).map_err(|e| io("keep the copy", e))?;
    Ok(copy)
}

/// The copy that waits for the next start, if one does.
pub fn waiting(dir: &Path) -> Option<DataCopy> {
    let bytes = std::fs::read(dir.join(PENDING_FILE)).ok()?;
    inspect(&bytes).ok()
}

/// Forget the copy that waits. Nothing else changes.
pub fn cancel(dir: &Path) -> Result<(), BackupError> {
    match std::fs::remove_file(dir.join(PENDING_FILE)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io("forget the copy", err)),
    }
}

/// What [`apply_waiting`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub copy: DataCopy,
    /// The folder that now holds what was here before.
    pub kept_in: PathBuf,
}

/// Bring in the copy that waits, if one does. Call this **before** anything in `dir` is
/// opened. What was there is moved into a folder under [`BACKUPS_DIR`]; nothing is deleted.
///
/// A waiting file that is not a good copy is set aside as [`FAILED_FILE`] and the data is
/// left as it was.
pub fn apply_waiting(dir: &Path, now: i64) -> Option<Result<Applied, BackupError>> {
    let pending = dir.join(PENDING_FILE);
    let bytes = std::fs::read(&pending).ok()?;
    let result = apply(dir, &bytes, now);
    if result.is_ok() {
        let _ = std::fs::remove_file(&pending);
    } else {
        let _ = std::fs::rename(&pending, dir.join(FAILED_FILE));
    }
    Some(result)
}

fn apply(dir: &Path, bytes: &[u8], now: i64) -> Result<Applied, BackupError> {
    let (copy, files) = read_copy(bytes)?;
    let kept_in = dir
        .join(BACKUPS_DIR)
        .join(format!("before-import-{}", stamp(now)));
    std::fs::create_dir_all(&kept_in).map_err(|e| io("make the backup folder", e))?;

    // Move out what is here now: the plain files, and every event log with the two files
    // SQLite may keep beside it.
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for path in current_files(dir) {
            let Some(name) = path.file_name() else {
                continue;
            };
            let to = kept_in.join(name);
            std::fs::rename(&path, &to).map_err(|e| io("move the old data aside", e))?;
            moved.push((path, to));
        }
        for (place, content) in &files {
            std::fs::write(dir.join(place), content).map_err(|e| io("unpack the copy", e))?;
        }
        Ok(())
    })();
    if let Err(err) = result {
        // Put everything back as it was: remove what was unpacked, then return what moved.
        for (place, _) in &files {
            let _ = std::fs::remove_file(dir.join(place));
        }
        for (from, to) in moved {
            let _ = std::fs::rename(to, from);
        }
        return Err(err);
    }
    Ok(Applied { copy, kept_in })
}

/// Everything in the data folder that a copy replaces.
fn current_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                return false;
            };
            path.is_file()
                && (KEPT_FILES.contains(&name)
                    || [EVENT_SUFFIX, ".sqlite-wal", ".sqlite-shm"]
                        .iter()
                        .any(|suffix| name.strip_suffix(suffix).is_some_and(safe_id)))
        })
        .collect()
}

// ── The time in a file name ──────────────────────────────────────────────────

/// `2026-11-15-203007` for a moment in microseconds since 1970 (UTC).
fn stamp(micros: i64) -> String {
    let seconds = micros.div_euclid(1_000_000);
    let (days, sod) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 to (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ── HTTP ─────────────────────────────────────────────────────────────────────

fn folder(registry: &EventRegistry) -> Result<PathBuf, BackupError> {
    registry.data_dir().ok_or(BackupError::NoFolder)
}

/// `GET /data` — where the data is kept, and the copy that waits, if any. An open read.
pub async fn get_data(State(registry): State<EventRegistry>) -> Json<DataInfo> {
    let dir = registry.data_dir();
    Json(DataInfo {
        waiting: dir.as_deref().and_then(waiting),
        folder: dir.map(|dir| {
            std::fs::canonicalize(&dir)
                .unwrap_or(dir)
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_string()
        }),
    })
}

/// `POST /data/export` — make a copy of everything, RD-gated. Answers with the file's name
/// and where it is on the Director's computer.
pub async fn post_export(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
) -> Result<Json<DataExport>, ProtocolError> {
    let dir = folder(&registry)?;
    let mut made = export(&dir, now_micros())?;
    if let Ok(full) = std::fs::canonicalize(&made.file) {
        made.file = full
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_string();
    }
    Ok(Json(made))
}

/// `GET /data/export/{name}` — a copy made by `POST /data/export`, as a download.
pub async fn get_export(
    State(registry): State<EventRegistry>,
    UrlPath(name): UrlPath<String>,
) -> Result<Response, ProtocolError> {
    let dir = folder(&registry)?;
    if !export_name(&name) {
        return Err(BackupError::NoSuchExport.into());
    }
    let bytes =
        std::fs::read(dir.join(EXPORTS_DIR).join(&name)).map_err(|_| BackupError::NoSuchExport)?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// `POST /data/import` — bring in a copy, RD-gated. The body is the zip file itself. The
/// copy is checked and kept; it takes over when Grid is next started. Refused while a race
/// is on.
pub async fn post_import(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    body: Bytes,
) -> Result<Json<DataCopy>, ProtocolError> {
    let dir = folder(&registry)?;
    if registry.race_on() {
        return Err(BackupError::RaceOn.into());
    }
    Ok(Json(stage(&dir, &body)?))
}

/// `DELETE /data/import` — forget the copy that waits, RD-gated.
pub async fn delete_import(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
) -> Result<StatusCode, ProtocolError> {
    cancel(&folder(&registry)?)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{CreateEventRequest, EventMeta};

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gridfpv-backup-{tag}-{}-{}",
            std::process::id(),
            now_micros()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn new_event(registry: &EventRegistry, name: &str) -> EventMeta {
        registry
            .create(&CreateEventRequest {
                name: name.into(),
                ..Default::default()
            })
            .unwrap()
    }

    #[test]
    fn the_stamp_reads_as_a_date_and_time() {
        assert_eq!(stamp(0), "1970-01-01-000000");
        // 2026-11-15 20:30:07 UTC.
        assert_eq!(stamp(1_794_774_607_000_000), "2026-11-15-203007");
    }

    #[test]
    fn a_copy_carries_everything_to_another_computer() {
        let (here, there) = (scratch("here"), scratch("there"));
        let (first, second) = {
            let registry = EventRegistry::new(Some(here.clone())).unwrap();
            let first = new_event(&registry, "Friday Whoops");
            let second = new_event(&registry, "Sunday Finals");
            registry.set_active(&second.id).unwrap();
            std::fs::write(here.join(crate::pilots::PILOTS_FILE), b"[]").unwrap();
            std::fs::write(here.join(crate::video::VIDEO_FILE), b"{}").unwrap();
            // Made while the events are open, as it is from the console.
            let made = export(&here, 1_794_774_607_000_000).unwrap();
            assert_eq!(made.name, "grid-data-2026-11-15-203007.zip");
            assert_eq!(made.copy.events, 2);
            (first, second)
        };
        let bytes = std::fs::read(
            here.join(EXPORTS_DIR)
                .join("grid-data-2026-11-15-203007.zip"),
        )
        .unwrap();

        // The other computer has an event of its own, and its own video setup.
        {
            let registry = EventRegistry::new(Some(there.clone())).unwrap();
            new_event(&registry, "Old Practice");
            std::fs::write(there.join(crate::video::VIDEO_FILE), b"{\"mine\":1}").unwrap();
            assert_eq!(stage(&there, &bytes).unwrap().events, 2);
            assert_eq!(waiting(&there).unwrap().events, 2);
            // Nothing changes until the next start.
            assert_eq!(registry.list().len(), 1);
        }

        // The next start brings it in.
        let registry = EventRegistry::new(Some(there.clone())).unwrap();
        let names: Vec<String> = registry.list().into_iter().map(|e| e.name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&first.name) && names.contains(&second.name));
        assert_eq!(registry.active().map(|e| e.id), Some(second.id));
        assert!(waiting(&there).is_none());
        assert!(there.join(crate::pilots::PILOTS_FILE).is_file());
        // The video setup is this computer's own and is left alone.
        assert_eq!(
            std::fs::read(there.join(crate::video::VIDEO_FILE)).unwrap(),
            b"{\"mine\":1}"
        );
        // What was here before is kept, not deleted.
        let kept: Vec<PathBuf> = std::fs::read_dir(there.join(BACKUPS_DIR))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();
        assert_eq!(kept.len(), 1);
        let old_logs = std::fs::read_dir(&kept[0])
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "sqlite"))
            .count();
        assert_eq!(old_logs, 1);

        drop(registry);
        std::fs::remove_dir_all(&here).ok();
        std::fs::remove_dir_all(&there).ok();
    }

    fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    const NOTE: &[u8] = br#"{"format":1,"version":"test","made_at":5}"#;

    #[test]
    fn a_file_that_is_not_a_copy_is_refused() {
        assert!(matches!(inspect(b"hello"), Err(BackupError::NotACopy(_))));
        // A zip with no note saying what it is.
        assert!(inspect(&zip_of(&[("pilots.json", b"[]")])).is_err());
        // A file Grid does not know, and one that points out of the folder.
        assert!(inspect(&zip_of(&[(MANIFEST, NOTE), ("video.json", b"{}")])).is_err());
        assert!(inspect(&zip_of(&[(MANIFEST, NOTE), ("../pilots.json", b"[]")])).is_err());
        assert!(
            inspect(&zip_of(&[
                (MANIFEST, NOTE),
                ("events/../../evil.sqlite", SQLITE_MAGIC)
            ]))
            .is_err()
        );
        // An event that is not a SQLite file.
        assert!(inspect(&zip_of(&[(MANIFEST, NOTE), ("events/a-1.sqlite", b"nope")])).is_err());
        // A copy from a newer layout.
        let newer: &[u8] = br#"{"format":2,"version":"x","made_at":5}"#;
        assert!(inspect(&zip_of(&[(MANIFEST, newer)])).is_err());
        // A good one, with nothing but pilots in it.
        let good = inspect(&zip_of(&[(MANIFEST, NOTE), ("pilots.json", b"[]")])).unwrap();
        assert_eq!((good.events, good.made_at), (0, 5));
    }

    #[test]
    fn a_bad_waiting_file_is_set_aside_and_the_data_left_alone() {
        let dir = scratch("bad");
        std::fs::write(dir.join(crate::pilots::PILOTS_FILE), b"[]").unwrap();
        std::fs::write(dir.join(PENDING_FILE), b"not a zip").unwrap();
        assert!(matches!(apply_waiting(&dir, 0), Some(Err(_))));
        assert!(dir.join(crate::pilots::PILOTS_FILE).is_file());
        assert!(!dir.join(PENDING_FILE).exists());
        assert!(dir.join(FAILED_FILE).is_file());
        // With nothing waiting there is nothing to do.
        assert!(apply_waiting(&dir, 0).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_waiting_copy_can_be_forgotten() {
        let dir = scratch("cancel");
        stage(&dir, &zip_of(&[(MANIFEST, NOTE)])).unwrap();
        assert!(waiting(&dir).is_some());
        cancel(&dir).unwrap();
        assert!(waiting(&dir).is_none());
        // Forgetting twice is fine.
        cancel(&dir).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_newest_copies_made_here_are_kept() {
        let dir = scratch("prune");
        for minute in 0..(EXPORTS_KEPT as i64 + 3) {
            export(&dir, minute * 60_000_000).unwrap();
        }
        let left = std::fs::read_dir(dir.join(EXPORTS_DIR)).unwrap().count();
        assert_eq!(left, EXPORTS_KEPT);
        assert!(
            dir.join(EXPORTS_DIR)
                .join("grid-data-1970-01-01-000700.zip")
                .is_file()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
