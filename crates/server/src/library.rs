//! Saved formats and saved seat profiles: two lists that outlive events
//! (`docs/primitives.md` §3.2, §5.6).
//!
//! - A **saved format** is a format kept by name so it can be picked again. Setting one on a
//!   phase copies it into the event, so editing or deleting a saved format never changes a
//!   phase that already uses it.
//! - A **seat profile** is a saved set of one timer's settings: each seat's channel and
//!   whether it is on, the thresholds, the exit hold and the lap cooldown. Applying one
//!   writes it to the timer. Each timer remembers which profile was applied last.
//!
//! | route | what |
//! |-------|------|
//! | `GET /formats`, `POST /formats` | the saved formats; save a new one |
//! | `PUT /formats/{id}`, `DELETE /formats/{id}` | change or remove one |
//! | `GET /timers/{id}/profiles`, `POST /timers/{id}/profiles` | a timer's profiles; save a new one |
//! | `PUT /timers/{id}/profiles/{pid}`, `DELETE …` | change or remove one |
//! | `POST /timers/{id}/profiles/{pid}/apply` | write a profile to its timer |
//!
//! Both lists are small JSON files in the data directory. With no data directory they live
//! in memory only.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use gridfpv_events::{
    AdapterId, CompletionRule, EndCondition, FirstLap, Format, Measure, RaceRules, RankingRule,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::auth::ControlAuth;
use crate::error::{ErrorCode, ProtocolError};
use crate::events::EventRegistry;
use crate::timers::TimerConfig;

/// File name of the saved formats under the data directory.
pub const FORMATS_FILE: &str = "formats.json";
/// File name of the saved seat profiles under the data directory.
pub const PROFILES_FILE: &str = "profiles.json";

/// A format kept by name. The name is the format's own (`format.name`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SavedFormat {
    /// A handle for the saved format. Never shown.
    pub id: String,
    pub format: Format,
}

/// A saved set of one timer's settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SeatProfile {
    /// A handle for the profile. Never shown.
    pub id: String,
    pub name: String,
    pub config: TimerConfig,
}

/// What a profile is saved from: its name and the settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SeatProfileRequest {
    pub name: String,
    pub config: TimerConfig,
}

/// One timer's profiles, and which one was applied last.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct TimerProfiles {
    /// The profile last applied to the timer, or saved from it. The timer may have been
    /// changed since: a console compares the profile's settings with the timer's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub active: Option<String>,
    /// The profiles, in name order.
    pub profiles: Vec<SeatProfile>,
}

/// Why a library call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LibraryError {
    #[error("It needs a name.")]
    NoName,
    #[error("There is already one called {0:?}. Pick another name.")]
    NameTaken(String),
    #[error("There is no such format.")]
    NoFormat,
    #[error("There is no such profile.")]
    NoProfile,
    #[error("There is no such timer.")]
    NoTimer,
    #[error("Could not save it: {0}")]
    Io(String),
}

impl From<LibraryError> for ProtocolError {
    fn from(error: LibraryError) -> Self {
        let code = match error {
            LibraryError::NoName => ErrorCode::BadRequest,
            LibraryError::NameTaken(_) => ErrorCode::Refused,
            LibraryError::NoFormat | LibraryError::NoProfile | LibraryError::NoTimer => {
                ErrorCode::UnknownScope
            }
            LibraryError::Io(_) => ErrorCode::Internal,
        };
        ProtocolError::new(code, error.to_string())
    }
}

#[derive(Default)]
struct Inner {
    formats: Vec<SavedFormat>,
    profiles: BTreeMap<AdapterId, TimerProfiles>,
    data_dir: Option<PathBuf>,
}

/// The saved formats and seat profiles. Cloning shares the one library.
#[derive(Clone, Default)]
pub struct Library {
    inner: Arc<RwLock<Inner>>,
}

const SECOND: i64 = 1_000_000;

/// The formats a new Director starts with. They are ordinary saved formats: each can be
/// changed or deleted.
fn starting_formats() -> Vec<SavedFormat> {
    let rules = |end, staging_micros| RaceRules {
        end,
        staging_micros,
        min_lap_micros: None,
        first_lap: FirstLap::HoleShot,
        stop_after_all_finish_micros: None,
        max_micros: None,
    };
    let saved = |id: &str, format| SavedFormat {
        id: id.to_owned(),
        format,
    };
    vec![
        saved(
            "practice",
            Format {
                name: "Practice".into(),
                // No countdown: practice starts the moment the director presses Start.
                race: rules(EndCondition::Open, 0),
                completion: CompletionRule::DirectorDecides,
                ranking: None,
                points: Vec::new(),
            },
        ),
        saved(
            "best-3-consecutive",
            Format {
                name: "Best 3 consecutive".into(),
                race: rules(EndCondition::Open, 5 * SECOND),
                completion: CompletionRule::DirectorDecides,
                ranking: Some(RankingRule {
                    sort: vec![Measure::BestConsecutive(3)],
                }),
                points: Vec::new(),
            },
        ),
        saved(
            "first-to-3",
            Format {
                name: "First to 3 laps".into(),
                race: rules(EndCondition::FirstToLaps(3), 5 * SECOND),
                completion: CompletionRule::SavedRaces(1),
                ranking: Some(RankingRule {
                    sort: vec![Measure::LastFinish],
                }),
                points: Vec::new(),
            },
        ),
        saved(
            "timed-2-min",
            Format {
                name: "2 minutes, most laps".into(),
                race: rules(
                    EndCondition::Timed {
                        micros: 120 * SECOND,
                        grace_micros: 10 * SECOND,
                    },
                    5 * SECOND,
                ),
                completion: CompletionRule::SavedRaces(1),
                ranking: Some(RankingRule {
                    sort: vec![Measure::Laps, Measure::LastFinish],
                }),
                points: Vec::new(),
            },
        ),
    ]
}

fn read_file<T: for<'de> Deserialize<'de>>(dir: &std::path::Path, name: &str) -> Option<T> {
    let bytes = std::fs::read(dir.join(name)).ok()?;
    match serde_json::from_slice(&bytes) {
        Ok(value) => Some(value),
        Err(e) => {
            // A file that cannot be read is left where it is and the list starts over in
            // memory; the next save replaces it.
            eprintln!("WARNING: could not read {name}: {e}");
            None
        }
    }
}

fn write_file<T: Serialize>(
    dir: &Option<PathBuf>,
    name: &str,
    value: &T,
) -> Result<(), LibraryError> {
    let Some(dir) = dir else {
        return Ok(());
    };
    let io = |e: std::io::Error| LibraryError::Io(e.to_string());
    std::fs::create_dir_all(dir).map_err(io)?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| LibraryError::Io(e.to_string()))?;
    // Write beside the file, then swap it in, so a crash never leaves half a file.
    let scratch = dir.join(format!("{name}.tmp"));
    std::fs::write(&scratch, bytes).map_err(io)?;
    std::fs::rename(&scratch, dir.join(name)).map_err(io)
}

fn tail() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("OS CSPRNG available");
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

impl Library {
    /// The library kept under `data_dir`. A Director with no saved formats yet starts with
    /// the built-in set.
    pub fn new(data_dir: Option<PathBuf>) -> Self {
        let formats = data_dir
            .as_deref()
            .and_then(|dir| read_file(dir, FORMATS_FILE))
            .unwrap_or_else(starting_formats);
        let profiles = data_dir
            .as_deref()
            .and_then(|dir| read_file(dir, PROFILES_FILE))
            .unwrap_or_default();
        Self {
            inner: Arc::new(RwLock::new(Inner {
                formats,
                profiles,
                data_dir,
            })),
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Change the formats, and keep the change only if it could be saved.
    fn change_formats<R>(
        &self,
        change: impl FnOnce(&mut Vec<SavedFormat>) -> Result<R, LibraryError>,
    ) -> Result<R, LibraryError> {
        let mut inner = self.write();
        let mut formats = inner.formats.clone();
        let out = change(&mut formats)?;
        write_file(&inner.data_dir, FORMATS_FILE, &formats)?;
        inner.formats = formats;
        Ok(out)
    }

    /// Change the profiles, and keep the change only if it could be saved.
    fn change_profiles<R>(
        &self,
        change: impl FnOnce(&mut BTreeMap<AdapterId, TimerProfiles>) -> Result<R, LibraryError>,
    ) -> Result<R, LibraryError> {
        let mut inner = self.write();
        let mut profiles = inner.profiles.clone();
        let out = change(&mut profiles)?;
        write_file(&inner.data_dir, PROFILES_FILE, &profiles)?;
        inner.profiles = profiles;
        Ok(out)
    }

    /// The saved formats, in the order they were saved.
    pub fn formats(&self) -> Vec<SavedFormat> {
        self.read().formats.clone()
    }

    /// Save a new format. Its name must be its own.
    pub fn create_format(&self, format: Format) -> Result<SavedFormat, LibraryError> {
        self.change_formats(|formats| {
            let format = named(format, formats, None)?;
            let saved = SavedFormat {
                id: format!("format-{}", tail()),
                format,
            };
            formats.push(saved.clone());
            Ok(saved)
        })
    }

    /// Replace a saved format.
    pub fn update_format(&self, id: &str, format: Format) -> Result<SavedFormat, LibraryError> {
        self.change_formats(|formats| {
            let format = named(format, formats, Some(id))?;
            let saved = formats
                .iter_mut()
                .find(|saved| saved.id == id)
                .ok_or(LibraryError::NoFormat)?;
            saved.format = format;
            Ok(saved.clone())
        })
    }

    /// Remove a saved format. Phases that were given it keep their copy.
    pub fn delete_format(&self, id: &str) -> Result<(), LibraryError> {
        self.change_formats(|formats| {
            let before = formats.len();
            formats.retain(|saved| saved.id != id);
            if formats.len() == before {
                return Err(LibraryError::NoFormat);
            }
            Ok(())
        })
    }

    /// One timer's profiles.
    pub fn profiles(&self, timer: &AdapterId) -> TimerProfiles {
        self.read().profiles.get(timer).cloned().unwrap_or_default()
    }

    /// Save a new profile for a timer. It becomes the timer's active profile: it is saved
    /// from what the timer is set to.
    pub fn create_profile(
        &self,
        timer: &AdapterId,
        request: SeatProfileRequest,
    ) -> Result<SeatProfile, LibraryError> {
        self.change_profiles(|all| {
            let of_timer = all.entry(timer.clone()).or_default();
            let name = profile_name(&request.name, of_timer, None)?;
            let profile = SeatProfile {
                id: format!("profile-{}", tail()),
                name,
                config: request.config,
            };
            of_timer.profiles.push(profile.clone());
            of_timer.profiles.sort_by_key(|p| p.name.to_lowercase());
            of_timer.active = Some(profile.id.clone());
            Ok(profile)
        })
    }

    /// Replace a profile's name and settings.
    pub fn update_profile(
        &self,
        timer: &AdapterId,
        id: &str,
        request: SeatProfileRequest,
    ) -> Result<SeatProfile, LibraryError> {
        self.change_profiles(|all| {
            let of_timer = all.get_mut(timer).ok_or(LibraryError::NoProfile)?;
            let name = profile_name(&request.name, of_timer, Some(id))?;
            let profile = of_timer
                .profiles
                .iter_mut()
                .find(|profile| profile.id == id)
                .ok_or(LibraryError::NoProfile)?;
            profile.name = name;
            profile.config = request.config;
            let out = profile.clone();
            of_timer.profiles.sort_by_key(|p| p.name.to_lowercase());
            Ok(out)
        })
    }

    /// Remove a profile. The timer keeps the settings it has.
    pub fn delete_profile(&self, timer: &AdapterId, id: &str) -> Result<(), LibraryError> {
        self.change_profiles(|all| {
            let of_timer = all.get_mut(timer).ok_or(LibraryError::NoProfile)?;
            let before = of_timer.profiles.len();
            of_timer.profiles.retain(|profile| profile.id != id);
            if of_timer.profiles.len() == before {
                return Err(LibraryError::NoProfile);
            }
            if of_timer.active.as_deref() == Some(id) {
                of_timer.active = None;
            }
            Ok(())
        })
    }

    /// Note that a profile was written to its timer.
    pub fn set_active(&self, timer: &AdapterId, id: &str) -> Result<(), LibraryError> {
        self.change_profiles(|all| {
            let of_timer = all.get_mut(timer).ok_or(LibraryError::NoProfile)?;
            if !of_timer.profiles.iter().any(|profile| profile.id == id) {
                return Err(LibraryError::NoProfile);
            }
            of_timer.active = Some(id.to_owned());
            Ok(())
        })
    }

    /// Forget a removed timer's profiles.
    pub fn forget_timer(&self, timer: &AdapterId) -> Result<(), LibraryError> {
        self.change_profiles(|all| {
            all.remove(timer);
            Ok(())
        })
    }
}

/// `format` with its name trimmed, if no other saved format has that name.
fn named(
    mut format: Format,
    formats: &[SavedFormat],
    except: Option<&str>,
) -> Result<Format, LibraryError> {
    let name = format.name.trim().to_owned();
    if name.is_empty() {
        return Err(LibraryError::NoName);
    }
    let taken = formats
        .iter()
        .any(|saved| Some(saved.id.as_str()) != except && same_name(&saved.format.name, &name));
    if taken {
        return Err(LibraryError::NameTaken(name));
    }
    format.name = name;
    Ok(format)
}

/// A profile name, trimmed, if no other profile of the timer has it.
fn profile_name(
    name: &str,
    of_timer: &TimerProfiles,
    except: Option<&str>,
) -> Result<String, LibraryError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LibraryError::NoName);
    }
    let taken = of_timer
        .profiles
        .iter()
        .any(|profile| Some(profile.id.as_str()) != except && same_name(&profile.name, name));
    if taken {
        return Err(LibraryError::NameTaken(name.to_owned()));
    }
    Ok(name.to_owned())
}

/// `GET /formats` — the saved formats. An open read.
pub async fn list_formats(State(registry): State<EventRegistry>) -> Json<Vec<SavedFormat>> {
    Json(registry.library().formats())
}

/// `POST /formats` — save a new format, RD-gated.
pub async fn create_format(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Json(format): Json<Format>,
) -> Result<Json<SavedFormat>, ProtocolError> {
    Ok(Json(registry.library().create_format(format)?))
}

/// `PUT /formats/{format_id}` — replace a saved format, RD-gated.
pub async fn update_format(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(format_id): Path<String>,
    Json(format): Json<Format>,
) -> Result<Json<SavedFormat>, ProtocolError> {
    Ok(Json(registry.library().update_format(&format_id, format)?))
}

/// `DELETE /formats/{format_id}` — remove a saved format, RD-gated.
pub async fn delete_format(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(format_id): Path<String>,
) -> Result<StatusCode, ProtocolError> {
    registry.library().delete_format(&format_id)?;
    Ok(StatusCode::OK)
}

fn known_timer(registry: &EventRegistry, timer: &AdapterId) -> Result<(), LibraryError> {
    registry
        .timers()
        .get(timer)
        .map(|_| ())
        .ok_or(LibraryError::NoTimer)
}

/// `GET /timers/{timer_id}/profiles` — a timer's saved profiles. An open read.
pub async fn list_profiles(
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
) -> Result<Json<TimerProfiles>, ProtocolError> {
    known_timer(&registry, &timer_id)?;
    Ok(Json(registry.library().profiles(&timer_id)))
}

/// `POST /timers/{timer_id}/profiles` — save settings as a new profile, RD-gated. Nothing
/// is written to the timer: the settings sent are the ones it already has.
pub async fn create_profile(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path(timer_id): Path<AdapterId>,
    Json(body): Json<SeatProfileRequest>,
) -> Result<Json<SeatProfile>, ProtocolError> {
    known_timer(&registry, &timer_id)?;
    Ok(Json(registry.library().create_profile(&timer_id, body)?))
}

/// `PUT /timers/{timer_id}/profiles/{profile_id}` — rename a profile or replace its
/// settings, RD-gated. Nothing is written to the timer.
pub async fn update_profile(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((timer_id, profile_id)): Path<(AdapterId, String)>,
    Json(body): Json<SeatProfileRequest>,
) -> Result<Json<SeatProfile>, ProtocolError> {
    known_timer(&registry, &timer_id)?;
    Ok(Json(registry.library().update_profile(
        &timer_id,
        &profile_id,
        body,
    )?))
}

/// `DELETE /timers/{timer_id}/profiles/{profile_id}` — remove a profile, RD-gated. The
/// timer keeps the settings it has.
pub async fn delete_profile(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((timer_id, profile_id)): Path<(AdapterId, String)>,
) -> Result<StatusCode, ProtocolError> {
    known_timer(&registry, &timer_id)?;
    registry.library().delete_profile(&timer_id, &profile_id)?;
    Ok(StatusCode::OK)
}

/// `POST /timers/{timer_id}/profiles/{profile_id}/apply` — write a profile to its timer,
/// RD-gated. Answers with the settings as the timer reports them afterwards, and the
/// profile becomes the timer's active one. A write the timer did not take is `Refused`,
/// and the active profile is left as it was.
pub async fn apply_profile(
    _auth: ControlAuth,
    State(registry): State<EventRegistry>,
    Path((timer_id, profile_id)): Path<(AdapterId, String)>,
) -> Result<Json<TimerConfig>, ProtocolError> {
    known_timer(&registry, &timer_id)?;
    let profile = registry
        .library()
        .profiles(&timer_id)
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or(LibraryError::NoProfile)?;
    let written = crate::timers::apply_config(&registry, &timer_id, &profile.config).await?;
    registry.library().set_active(&timer_id, &profile_id)?;
    Ok(Json(written))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timers::{TimerSeatConfig, TimerThreshold};
    use gridfpv_events::SeatRef;

    fn config(freq_mhz: u32) -> TimerConfig {
        TimerConfig {
            seats: vec![TimerSeatConfig {
                seat: SeatRef("0".into()),
                freq_mhz,
                enabled: true,
                threshold: None,
            }],
            threshold: TimerThreshold::Auto {
                snr_enter: 150,
                hysteresis: 50,
            },
            lap_cooldown_ms: Some(2000),
            exit_hold_ms: Some(80),
        }
    }

    fn gate() -> AdapterId {
        AdapterId("gate".into())
    }

    #[test]
    fn a_new_library_has_the_starting_formats_and_a_format_needs_its_own_name() {
        let library = Library::new(None);
        let names: Vec<String> = library
            .formats()
            .into_iter()
            .map(|saved| saved.format.name)
            .collect();
        assert_eq!(
            names,
            [
                "Practice",
                "Best 3 consecutive",
                "First to 3 laps",
                "2 minutes, most laps"
            ]
        );
        let mut copy = library.formats()[0].format.clone();
        // The same name in another case is still the same name.
        copy.name = " practice ".into();
        assert_eq!(
            library.create_format(copy.clone()),
            Err(LibraryError::NameTaken("practice".into()))
        );
        copy.name = "  ".into();
        assert_eq!(
            library.create_format(copy.clone()),
            Err(LibraryError::NoName)
        );
        copy.name = " Warm up ".into();
        let saved = library.create_format(copy.clone()).unwrap();
        assert_eq!(saved.format.name, "Warm up");
        // A format can keep its own name when it is changed.
        let mut changed = saved.format.clone();
        changed.race.staging_micros = 3 * SECOND;
        assert_eq!(
            library
                .update_format(&saved.id, changed)
                .unwrap()
                .format
                .race
                .staging_micros,
            3 * SECOND
        );
        library.delete_format(&saved.id).unwrap();
        assert_eq!(
            library.delete_format(&saved.id),
            Err(LibraryError::NoFormat)
        );
        assert_eq!(library.formats().len(), 4);
    }

    #[test]
    fn formats_and_profiles_are_kept_in_the_data_directory() {
        let dir = std::env::temp_dir().join(format!("gridfpv-library-{}", tail()));
        let library = Library::new(Some(dir.clone()));
        let first = library.formats()[0].id.clone();
        library.delete_format(&first).unwrap();
        let profile = library
            .create_profile(
                &gate(),
                SeatProfileRequest {
                    name: "Garage".into(),
                    config: config(5658),
                },
            )
            .unwrap();
        let again = Library::new(Some(dir.clone()));
        assert_eq!(again.formats().len(), 3);
        assert_eq!(
            again.profiles(&gate()),
            TimerProfiles {
                active: Some(profile.id.clone()),
                profiles: vec![profile],
            }
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_timers_profiles_have_their_own_names_and_one_is_active() {
        let library = Library::new(None);
        let request = |name: &str, freq| SeatProfileRequest {
            name: name.into(),
            config: config(freq),
        };
        let garage = library
            .create_profile(&gate(), request("Garage", 5658))
            .unwrap();
        assert_eq!(
            library.create_profile(&gate(), request("garage", 5800)),
            Err(LibraryError::NameTaken("garage".into()))
        );
        // Another timer may use the name.
        library
            .create_profile(&AdapterId("other".into()), request("Garage", 5800))
            .unwrap();
        let field = library
            .create_profile(&gate(), request("Field", 5800))
            .unwrap();
        let profiles = library.profiles(&gate());
        // In name order; the one saved last is active.
        assert_eq!(
            profiles
                .profiles
                .iter()
                .map(|p| p.name.as_str())
                .collect::<Vec<_>>(),
            ["Field", "Garage"]
        );
        assert_eq!(profiles.active, Some(field.id.clone()));
        library.set_active(&gate(), &garage.id).unwrap();
        assert_eq!(
            library.set_active(&gate(), "nope"),
            Err(LibraryError::NoProfile)
        );
        // Renaming keeps the id; deleting the active one leaves none active.
        let renamed = library
            .update_profile(&gate(), &garage.id, request("Home", 5732))
            .unwrap();
        assert_eq!(
            (renamed.id.as_str(), renamed.config.seats[0].freq_mhz),
            (garage.id.as_str(), 5732)
        );
        library.delete_profile(&gate(), &garage.id).unwrap();
        assert_eq!(library.profiles(&gate()).active, None);
        library.forget_timer(&gate()).unwrap();
        assert!(library.profiles(&gate()).profiles.is_empty());
    }
}
