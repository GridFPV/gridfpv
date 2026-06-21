//! Pilots as **application-level configuration** — the `PilotDirectory` and `Pilot` (issue #74).
//!
//! A pilot is a *racer in the Director's address book*: a callsign and a little optional metadata
//! (real name, VTX type, external service ids). The model parallels the timer model
//! ([`TimerRegistry`](crate::timers::TimerRegistry)): the Race Director maintains their pilots
//! **once** at the application level (a persisted directory) and each event simply builds a
//! **roster** of which directory pilots race it (see [`EventMeta::roster`](crate::events::EventMeta::roster)).
//! Type a pilot in once, and every new event just picks them.
//!
//! # Two pieces, mirroring timers
//!
//! - **App-level directory (this module).** The [`PilotDirectory`] holds every configured
//!   [`Pilot`] behind a lock and **persists** them to `<GRIDFPV_DATA_DIR>/pilots.json`
//!   (restored on boot; in-memory only when no data dir is configured). Unlike timers there is
//!   no built-in pilot — a fresh Director starts with an empty directory.
//! - **Per-event roster (`crate::events`).** Each [`EventMeta`](crate::events::EventMeta) carries
//!   a `roster: Vec<PilotId>` of the directory pilots that event races; new events default to an
//!   **empty** roster.
//!
//! Channels (which pilot flies which frequency in a given heat) are **not** modelled here — that
//! is a separate concern (#117). The directory is purely *who exists*; the roster is *who races
//! this event*.
//!
//! # Cloud-pull future hook (#74)
//!
//! The optional [`multigp_id`](Pilot::multigp_id) / [`velocidrone_id`](Pilot::velocidrone_id)
//! fields are deliberately carried (and persisted) now so a later **cloud-pull** — importing a
//! chapter's roster from MultiGP, or matching Velocidrone racers — has a stable place to record
//! the external identity it resolved a directory pilot from, without a schema change.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::scope::PilotId;

/// The file name (under the data dir) the pilot directory is persisted to (issue #74).
pub const PILOTS_FILE: &str = "pilots.json";

/// The kind of **video transmitter** a pilot flies (issue #74).
///
/// A small closed enum the directory records so the RD can group / display pilots by their video
/// system. Externally tagged (the default serde enum representation) so it maps to a TS string
/// union (`"Analog" | "HDZero" | "DJI" | "Walksnail"`). Optional on a [`Pilot`] — a directory
/// entry need not state a VTX type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum VtxType {
    /// Analog video (the classic 5.8GHz analog system).
    Analog,
    /// HDZero digital video.
    HDZero,
    /// DJI digital video (O3/O4/Air units).
    DJI,
    /// Walksnail Avatar digital video.
    Walksnail,
}

/// One pilot in the application-level directory (issue #74).
///
/// The wire shape `GET /pilots` returns and the on-disk shape `pilots.json` persists: a stable
/// [`PilotId`] (auto-generated, never user-entered), a required `callsign`, and a little optional
/// metadata. The optional fields are all omitted from the wire when unset (`skip_serializing_if`)
/// so an entry with just a callsign serialises to a two-field object. Derives serde (its JSON *is*
/// both the wire and the persisted form) and `ts_rs::TS` so the frontend reads a generated `Pilot`
/// type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Pilot {
    /// The stable handle a roster references and the API addresses (`PUT /pilots/{id}`). The same
    /// [`PilotId`] the log's registration binding uses — directory and log never disagree on what a
    /// pilot id is.
    pub id: PilotId,
    /// The pilot's **callsign** — the required display handle (their racing name). The directory's
    /// one mandatory field; everything else is optional metadata.
    pub callsign: String,
    /// The pilot's real / full name, if recorded. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// The pilot's video-transmitter type, if recorded (see [`VtxType`]). Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vtx_type: Option<VtxType>,
    /// The pilot's **MultiGP** pilot id, if known — a forward hook for a later cloud-pull import
    /// (#74). A free-form string. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub multigp_id: Option<String>,
    /// The pilot's **Velocidrone** id, if known — a forward hook for matching a Velocidrone racer
    /// (#74). A free-form string. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub velocidrone_id: Option<String>,
}

/// The body of `POST /pilots` — the fields a caller supplies to create a pilot (issue #74).
///
/// The `callsign` is required; everything else is optional. The **id is auto-generated**
/// server-side (a slug of the callsign + a short random suffix), never user-entered, mirroring
/// `POST /timers` / `POST /events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreatePilotRequest {
    /// The required callsign for the new pilot.
    pub callsign: String,
    /// Optional real name, stored on [`Pilot::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// Optional video-transmitter type, stored on [`Pilot::vtx_type`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vtx_type: Option<VtxType>,
    /// Optional MultiGP id, stored on [`Pilot::multigp_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub multigp_id: Option<String>,
    /// Optional Velocidrone id, stored on [`Pilot::velocidrone_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub velocidrone_id: Option<String>,
}

/// The body of `PUT /pilots/{id}` — the editable fields of a pilot (issue #74).
///
/// Every field is optional so a partial edit is a one-field body; the id is fixed (it is in the
/// path). A present `callsign` replaces it (a blank one is ignored — the callsign is required and
/// never cleared). For the optional metadata, a present field **replaces** the stored value
/// (including with `null` to clear it); an absent field leaves it unchanged. The wire-level
/// distinction between "field absent" and "field present and null" is what lets a caller both
/// leave-alone and clear.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UpdatePilotRequest {
    /// A new callsign, or `None`/blank to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub callsign: Option<String>,
    /// A new real name (`Some(value)` to set, `Some(null)` to clear, absent to leave unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<OptionalEdit<String>>,
    /// A new VTX type (set / clear / leave-unchanged, like [`name`](Self::name)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vtx_type: Option<OptionalEdit<VtxType>>,
    /// A new MultiGP id (set / clear / leave-unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub multigp_id: Option<OptionalEdit<String>>,
    /// A new Velocidrone id (set / clear / leave-unchanged).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub velocidrone_id: Option<OptionalEdit<String>>,
}

/// An edit to an **optional** field that distinguishes *clear it* from *leave it unchanged*
/// (issue #74).
///
/// In `PUT /pilots/{id}` an absent field means "leave unchanged"; a present field means "apply
/// this", where the value can itself be `null` to **clear** the stored value. Wrapping each
/// optional edit in this type makes that two-level optionality explicit (`Option<OptionalEdit<T>>`:
/// the outer `Option` is absent/present, the inner is the new value or `null`). On the TS side it
/// is just `T | null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(transparent)]
#[ts(export, export_to = "bindings/")]
pub struct OptionalEdit<T>(pub Option<T>);

/// The application-level directory of all configured pilots (issue #74).
///
/// Maps each [`PilotId`] to its [`Pilot`]. The set is **persisted** to `<data_dir>/pilots.json`
/// (restored on boot) so the RD's address book survives a Director restart; with no data dir
/// configured it is in-memory only. Cloning shares the one directory (`Arc<RwLock<…>>`), so it is
/// reached through the axum router state ([`EventRegistry`](crate::events::EventRegistry)) exactly
/// like the [`TimerRegistry`](crate::timers::TimerRegistry).
#[derive(Clone)]
pub struct PilotDirectory {
    inner: Arc<RwLock<Directory>>,
}

/// The guarded interior: the pilot map and where `pilots.json` lives.
struct Directory {
    /// `PilotId → Pilot`. A `BTreeMap` so listing is deterministic (id order).
    pilots: BTreeMap<PilotId, Pilot>,
    /// Directory `pilots.json` is persisted under; `None` ⇒ in-memory only (no data dir).
    data_dir: Option<PathBuf>,
}

impl PilotDirectory {
    /// Build a directory, restoring `<data_dir>/pilots.json` when a data dir is given.
    ///
    /// Starts empty (unlike timers, there is no built-in pilot). When `data_dir` is `Some` and a
    /// `pilots.json` already exists, the saved pilots are restored (an unreadable/corrupt file
    /// degrades to an empty directory rather than failing to boot). When `data_dir` is `None` the
    /// directory is in-memory only.
    pub fn new(data_dir: Option<PathBuf>) -> Result<Self, PilotError> {
        let mut pilots = BTreeMap::new();

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                PilotError(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            if let Some(restored) = read_persisted_pilots(dir) {
                for pilot in restored {
                    pilots.insert(pilot.id.clone(), pilot);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Directory { pilots, data_dir })),
        })
    }

    /// Every pilot in id order — the `GET /pilots` body.
    pub fn list(&self) -> Vec<Pilot> {
        self.read().pilots.values().cloned().collect()
    }

    /// Whether a pilot with `id` exists — the per-event roster validates each id through this.
    pub fn exists(&self, id: &PilotId) -> bool {
        self.read().pilots.contains_key(id)
    }

    /// The [`Pilot`] for `id`, or `None`.
    pub fn get(&self, id: &PilotId) -> Option<Pilot> {
        self.read().pilots.get(id).cloned()
    }

    /// Create a pilot from a [`CreatePilotRequest`], returning it (issue #74).
    ///
    /// The **id is auto-generated** — a slug of the `callsign` + a short random suffix — so it is
    /// unique and never user-entered. The optional metadata is stored verbatim (trimmed, with a
    /// blank treated as unset). The directory is **persisted** on success.
    pub fn create(&self, request: &CreatePilotRequest) -> Result<Pilot, PilotError> {
        let callsign = request.callsign.trim();
        if callsign.is_empty() {
            return Err(PilotError("a pilot callsign is required".to_string()));
        }
        let mut dir = self.write();
        let id = loop {
            let candidate = PilotId(format!("{}-{}", slugify(callsign), short_suffix()));
            if !dir.pilots.contains_key(&candidate) {
                break candidate;
            }
        };
        let pilot = Pilot {
            id: id.clone(),
            callsign: callsign.to_string(),
            name: normalize_optional(&request.name),
            vtx_type: request.vtx_type,
            multigp_id: normalize_optional(&request.multigp_id),
            velocidrone_id: normalize_optional(&request.velocidrone_id),
        };
        dir.pilots.insert(id, pilot.clone());
        dir.persist()?;
        Ok(pilot)
    }

    /// Edit a pilot's fields (issue #74), returning the updated [`Pilot`].
    ///
    /// A present `callsign` replaces it (a blank one is ignored — the callsign is required). Each
    /// optional metadata field is a three-way edit: absent → unchanged; present `Some(value)` →
    /// set (trimmed; a blank string clears it); present `null` → cleared. An unknown id is a
    /// [`PilotError`]. The directory is **persisted** on success.
    pub fn update(&self, id: &PilotId, request: &UpdatePilotRequest) -> Result<Pilot, PilotError> {
        let mut dir = self.write();
        let pilot = dir
            .pilots
            .get_mut(id)
            .ok_or_else(|| PilotError(format!("no pilot with id {:?}", id.0)))?;
        if let Some(callsign) = &request.callsign {
            let trimmed = callsign.trim();
            if !trimmed.is_empty() {
                pilot.callsign = trimmed.to_string();
            }
        }
        apply_string_edit(&mut pilot.name, &request.name);
        if let Some(edit) = &request.vtx_type {
            pilot.vtx_type = edit.0;
        }
        apply_string_edit(&mut pilot.multigp_id, &request.multigp_id);
        apply_string_edit(&mut pilot.velocidrone_id, &request.velocidrone_id);
        let updated = pilot.clone();
        dir.persist()?;
        Ok(updated)
    }

    /// Delete a pilot (issue #74). An unknown id is a [`PilotError`]. The directory is
    /// **persisted** on success. (Removing a pilot does not touch any event roster that still
    /// names them; a stale roster id is harmless — see the events module.)
    pub fn delete(&self, id: &PilotId) -> Result<(), PilotError> {
        let mut dir = self.write();
        if dir.pilots.remove(id).is_none() {
            return Err(PilotError(format!("no pilot with id {:?}", id.0)));
        }
        dir.persist()?;
        Ok(())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Directory> {
        self.inner.read().expect("pilot directory lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Directory> {
        self.inner.write().expect("pilot directory lock poisoned")
    }
}

impl Directory {
    /// Persist the pilot set to `<data_dir>/pilots.json` (issue #74), a no-op with no data dir.
    fn persist(&self) -> Result<(), PilotError> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let pilots: Vec<&Pilot> = self.pilots.values().collect();
        let json = serde_json::to_string_pretty(&pilots)
            .map_err(|e| PilotError(format!("could not serialize pilots: {e}")))?;
        std::fs::write(pilots_path(dir), json)
            .map_err(|e| PilotError(format!("could not persist pilots: {e}")))
    }
}

/// Apply an optional string edit to a stored field: absent → unchanged; present → set (trimmed,
/// blank ⇒ cleared) or cleared (`null`).
fn apply_string_edit(field: &mut Option<String>, edit: &Option<OptionalEdit<String>>) {
    if let Some(OptionalEdit(value)) = edit {
        *field = value
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
}

/// The file the pilot set is persisted to under `dir`: `<dir>/pilots.json`.
fn pilots_path(dir: &Path) -> PathBuf {
    dir.join(PILOTS_FILE)
}

/// Read the persisted pilots from `<dir>/pilots.json`, or `None` if absent/unreadable/corrupt.
/// A bad file degrades to "no persisted pilots" so the Director still boots.
fn read_persisted_pilots(dir: &Path) -> Option<Vec<Pilot>> {
    let raw = std::fs::read_to_string(pilots_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// An error mutating the pilot directory (a persistence failure, an unknown id, a missing callsign).
#[derive(Debug, Clone)]
pub struct PilotError(pub String);

impl std::fmt::Display for PilotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pilot directory error: {}", self.0)
    }
}

impl std::error::Error for PilotError {}

/// Trim an optional field, treating a blank/whitespace-only value as **unset** (`None`).
fn normalize_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Slugify a callsign into an id-friendly stem (the same rule as the event/timer registries):
/// lowercase ASCII alphanumerics kept, every other run collapsed to a single `-`, trimmed of
/// dashes; an empty/symbol-only callsign yields `pilot`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            slug.push('-');
            prev_dash = true;
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        "pilot".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A short random lowercase-alphanumeric suffix making an auto-generated id unique (same source as
/// the event/timer registries — the OS CSPRNG).
fn short_suffix() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("OS CSPRNG available");
    bytes
        .iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(callsign: &str) -> CreatePilotRequest {
        CreatePilotRequest {
            callsign: callsign.to_string(),
            name: None,
            vtx_type: None,
            multigp_id: None,
            velocidrone_id: None,
        }
    }

    #[test]
    fn a_fresh_directory_is_empty() {
        let dir = PilotDirectory::new(None).unwrap();
        assert!(dir.list().is_empty());
    }

    #[test]
    fn create_auto_generates_a_unique_slug_id() {
        let dir = PilotDirectory::new(None).unwrap();
        let a = dir.create(&req("Acro Ace!")).unwrap();
        let b = dir.create(&req("Acro Ace!")).unwrap();
        assert!(a.id.0.starts_with("acro-ace-"));
        assert_ne!(a.id, b.id);
        assert_eq!(a.callsign, "Acro Ace!");
        // Both listed.
        let ids: Vec<_> = dir.list().into_iter().map(|p| p.id).collect();
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn create_requires_a_callsign() {
        let dir = PilotDirectory::new(None).unwrap();
        assert!(dir.create(&req("   ")).is_err());
        assert!(dir.list().is_empty());
    }

    #[test]
    fn create_stores_optional_metadata_including_cloud_pull_ids() {
        let dir = PilotDirectory::new(None).unwrap();
        let pilot = dir
            .create(&CreatePilotRequest {
                callsign: "Zoom".to_string(),
                name: Some("Zoe Oom".to_string()),
                vtx_type: Some(VtxType::HDZero),
                multigp_id: Some("mgp-123".to_string()),
                velocidrone_id: Some("  ".to_string()), // blank → unset
            })
            .unwrap();
        assert_eq!(pilot.name.as_deref(), Some("Zoe Oom"));
        assert_eq!(pilot.vtx_type, Some(VtxType::HDZero));
        assert_eq!(pilot.multigp_id.as_deref(), Some("mgp-123"));
        assert_eq!(pilot.velocidrone_id, None);
    }

    #[test]
    fn update_edits_callsign_and_optional_fields_with_clear_semantics() {
        let dir = PilotDirectory::new(None).unwrap();
        let created = dir
            .create(&CreatePilotRequest {
                callsign: "Old".to_string(),
                name: Some("Real Name".to_string()),
                vtx_type: Some(VtxType::Analog),
                multigp_id: None,
                velocidrone_id: None,
            })
            .unwrap();

        // Set callsign + multigp_id, clear name, leave vtx_type unchanged.
        let updated = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    callsign: Some("New".to_string()),
                    name: Some(OptionalEdit(None)), // explicit clear
                    vtx_type: None,                 // unchanged
                    multigp_id: Some(OptionalEdit(Some("mgp-9".to_string()))),
                    velocidrone_id: None,
                },
            )
            .unwrap();
        assert_eq!(updated.callsign, "New");
        assert_eq!(updated.name, None);
        assert_eq!(updated.vtx_type, Some(VtxType::Analog));
        assert_eq!(updated.multigp_id.as_deref(), Some("mgp-9"));

        // A blank callsign is ignored (required field never cleared).
        let again = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    callsign: Some("   ".to_string()),
                    name: None,
                    vtx_type: None,
                    multigp_id: None,
                    velocidrone_id: None,
                },
            )
            .unwrap();
        assert_eq!(again.callsign, "New");
    }

    #[test]
    fn update_and_delete_reject_unknown_ids() {
        let dir = PilotDirectory::new(None).unwrap();
        let unknown = PilotId("nope".into());
        assert!(
            dir.update(
                &unknown,
                &UpdatePilotRequest {
                    callsign: Some("X".into()),
                    name: None,
                    vtx_type: None,
                    multigp_id: None,
                    velocidrone_id: None,
                },
            )
            .is_err()
        );
        assert!(dir.delete(&unknown).is_err());
    }

    #[test]
    fn delete_removes_a_created_pilot() {
        let dir = PilotDirectory::new(None).unwrap();
        let created = dir.create(&req("Temp")).unwrap();
        assert!(dir.exists(&created.id));
        dir.delete(&created.id).unwrap();
        assert!(!dir.exists(&created.id));
        assert!(dir.delete(&created.id).is_err());
    }

    #[test]
    fn pilots_persist_across_a_restart_with_a_data_dir() {
        let data_dir = std::env::temp_dir().join(format!("gridfpv-pilots-test-{}", short_suffix()));
        {
            let dir = PilotDirectory::new(Some(data_dir.clone())).unwrap();
            let created = dir
                .create(&CreatePilotRequest {
                    callsign: "Persisted".to_string(),
                    name: Some("Per Sisted".to_string()),
                    vtx_type: Some(VtxType::DJI),
                    multigp_id: Some("mgp-7".to_string()),
                    velocidrone_id: None,
                })
                .unwrap();

            let reopened = PilotDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.callsign, "Persisted");
            assert_eq!(got.name.as_deref(), Some("Per Sisted"));
            assert_eq!(got.vtx_type, Some(VtxType::DJI));
            assert_eq!(got.multigp_id.as_deref(), Some("mgp-7"));
            assert_eq!(got.velocidrone_id, None);
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn a_corrupt_pilots_file_degrades_to_an_empty_directory() {
        let data_dir = std::env::temp_dir().join(format!("gridfpv-pilots-bad-{}", short_suffix()));
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(pilots_path(&data_dir), b"not json at all").unwrap();
        let dir = PilotDirectory::new(Some(data_dir.clone())).unwrap();
        assert!(dir.list().is_empty());
        std::fs::remove_dir_all(&data_dir).ok();
    }
}
