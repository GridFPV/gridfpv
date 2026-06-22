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
/// union (`"Analog" | "HDZero" | "DJI" | "Walksnail" | "Other"`). A pilot carries a **set** of
/// these (see [`Pilot::vtx_types`]) since many pilots run more than one video system; the
/// [`Other`](VtxType::Other) variant is the catch-all for anything not enumerated.
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
    /// Any other video system not enumerated above (catch-all).
    Other,
}

impl VtxType {
    /// The canonical display order of the variants, used to keep a pilot's [`vtx_types`](Pilot::vtx_types)
    /// set in a stable, deterministic order regardless of the order they were ticked.
    const ORDER: [VtxType; 5] = [
        VtxType::Analog,
        VtxType::HDZero,
        VtxType::DJI,
        VtxType::Walksnail,
        VtxType::Other,
    ];
}

/// Dedup a VTX list and return it in the canonical [`VtxType::ORDER`], so a pilot's
/// `vtx_types` is always a stable, duplicate-free set (issue #74).
fn normalize_vtx_types(vtx_types: &[VtxType]) -> Vec<VtxType> {
    VtxType::ORDER
        .iter()
        .copied()
        .filter(|kind| vtx_types.contains(kind))
        .collect()
}

/// Deserialize a [`Pilot::vtx_types`] set, tolerating **both** the new and the old on-disk shape
/// (issue #74 follow-up):
///
/// - the new shape — a JSON array of [`VtxType`] (`"vtx_types": ["Analog", "HDZero"]`) — loads as-is;
/// - the legacy shape — a single scalar `VtxType` (`"vtx_type": "Analog"`, reached through the field
///   `alias`) — migrates into a one-element set `[Analog]`.
///
/// In every case the result is normalized (deduped, canonical order) so demo data persisted under
/// the old single-value `vtx_type` key survives the upgrade rather than being dropped on load. A
/// JSON `null` (legacy unset) maps to the empty set.
fn deserialize_vtx_types<'de, D>(deserializer: D) -> Result<Vec<VtxType>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    /// Either the new array form or the old single-scalar form (or a legacy `null`).
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum VtxTypesCompat {
        /// The new shape: a set of VTX types.
        Many(Vec<VtxType>),
        /// The legacy shape: a single optional VTX type (`Some(X)` → `[X]`, `None` → `[]`).
        One(Option<VtxType>),
    }

    let raw = match VtxTypesCompat::deserialize(deserializer)? {
        VtxTypesCompat::Many(list) => list,
        VtxTypesCompat::One(Some(one)) => vec![one],
        VtxTypesCompat::One(None) => Vec::new(),
    };
    Ok(normalize_vtx_types(&raw))
}

/// One pilot in the application-level directory (issue #74).
///
/// The wire shape `GET /pilots` returns and the on-disk shape `pilots.json` persists: a stable
/// [`PilotId`] (auto-generated, never user-entered), a required `callsign`, and a little optional
/// metadata. The optional fields are all omitted from the wire when unset (`skip_serializing_if`)
/// so an entry with just a callsign serialises to a two-field object. Derives serde (its JSON *is*
/// both the wire and the persisted form) and `ts_rs::TS` so the frontend reads a generated `Pilot`
/// type.
///
/// # Directory fields (a survey of RotorHazard / MultiGP / FPVScores, #74/#120)
///
/// Beyond the core callsign + cloud-pull ids, the directory carries a small set of
/// **display/organizer** fields those systems converge on: a [`phonetic`](Pilot::phonetic)
/// pronunciation hint for voice callouts (RotorHazard), a [`team`](Pilot::team) / club name, a
/// [`color`](Pilot::color) for overlays/leaderboards, and a [`country`](Pilot::country) code.
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
    /// A **pronunciation hint** for the callsign, for voice callouts (RotorHazard carries this).
    /// Free-form (e.g. `"AK-ro AYS"`). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub phonetic: Option<String>,
    /// The pilot's **team / club** name, if recorded. Free-form. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub team: Option<String>,
    /// A **hex color** `#RRGGBB` for overlays / leaderboards, if recorded. Stored as a plain
    /// (normalized) string and lightly validated on create/update. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// The pilot's **country** as an ISO 3166-1 alpha-2 code (e.g. `US`, `GB`), if recorded. The
    /// **code only** — flags/names derive from it in the UI. Stored uppercase, lightly validated.
    /// Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub country: Option<String>,
    /// The pilot's **video-transmitter system(s)** (see [`VtxType`]). An FPV pilot always flies
    /// *some* video system, and many run more than one (e.g. Analog + HDZero), so this is a **set**
    /// rather than a single optional value. Empty means *unspecified*. Kept deduplicated and in a
    /// stable [`VtxType::ORDER`] on every create/update. Defaults empty (and, being a `Vec`, an
    /// empty set still serializes to `[]`).
    ///
    /// **Persisted-format note:** the old on-disk shape was a single optional scalar
    /// `vtx_type: VtxType`. A custom deserializer (see [`deserialize_vtx_types`]) accepts both the
    /// new `vtx_types: [..]` array and migrates a legacy `vtx_type: X` into `[X]`, so existing
    /// `pilots.json` rows load without data loss.
    #[serde(
        default,
        deserialize_with = "deserialize_vtx_types",
        alias = "vtx_type"
    )]
    pub vtx_types: Vec<VtxType>,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreatePilotRequest {
    /// The required callsign for the new pilot.
    pub callsign: String,
    /// Optional real name, stored on [`Pilot::name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// Optional pronunciation hint, stored on [`Pilot::phonetic`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub phonetic: Option<String>,
    /// Optional team / club, stored on [`Pilot::team`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub team: Option<String>,
    /// Optional hex color `#RRGGBB`, stored on [`Pilot::color`] (validated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub color: Option<String>,
    /// Optional ISO 3166-1 alpha-2 country code, stored on [`Pilot::country`] (validated).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub country: Option<String>,
    /// The pilot's video-transmitter system(s), stored (deduped, stable order) on
    /// [`Pilot::vtx_types`]. Defaults empty (unspecified).
    #[serde(default)]
    pub vtx_types: Vec<VtxType>,
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
/// never cleared). Each optional-metadata field is a three-state [`OptionalEdit`]: **absent** leaves
/// it unchanged ([`Keep`](OptionalEdit::Keep)), present **`null`** clears it
/// ([`Clear`](OptionalEdit::Clear)), and present **with a value** sets it ([`Set`](OptionalEdit::Set)).
/// The wire-level distinction between "field absent" and "field present and `null`" is what lets a
/// caller both leave-alone and clear — `#[serde(default)]` on each field maps an absent field to
/// `Keep` while a present `null`/value runs [`OptionalEdit`]'s deserializer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UpdatePilotRequest {
    /// A new callsign, or `None`/blank to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub callsign: Option<String>,
    /// A new real name: present value → set, present `null` → clear, absent → leave unchanged.
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub name: OptionalEdit<String>,
    /// A new pronunciation hint (set / clear / leave-unchanged, like [`name`](Self::name)).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub phonetic: OptionalEdit<String>,
    /// A new team / club (set / clear / leave-unchanged).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub team: OptionalEdit<String>,
    /// A new hex color `#RRGGBB` (set / clear / leave-unchanged; a set value is validated).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub color: OptionalEdit<String>,
    /// A new ISO 3166-1 alpha-2 country code (set / clear / leave-unchanged; a set value is
    /// validated and normalized uppercase).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub country: OptionalEdit<String>,
    /// A **full replacement** of the VTX set when present (absent leaves it unchanged; present `[]`
    /// clears it) — a present array replaces the stored set wholesale (deduped, stable order),
    /// rather than a per-value set/clear edit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub vtx_types: Option<Vec<VtxType>>,
    /// A new MultiGP id (set / clear / leave-unchanged).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub multigp_id: OptionalEdit<String>,
    /// A new Velocidrone id (set / clear / leave-unchanged).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub velocidrone_id: OptionalEdit<String>,
}

/// A **three-state** edit to an optional field: *leave unchanged*, *clear*, or *set* (issue #74).
///
/// `PUT /pilots/{id}` needs to tell three wire cases apart for each optional field:
///
/// - **field absent** ⇒ [`Keep`](OptionalEdit::Keep) — leave the stored value unchanged;
/// - **field present and `null`** ⇒ [`Clear`](OptionalEdit::Clear) — clear the stored value (`None`);
/// - **field present with a value** ⇒ [`Set`](OptionalEdit::Set) — set the stored value.
///
/// The naive `Option<Option<T>>` does **not** work over serde: a wire `null` deserializes the
/// **same as an absent field** (both yield the outer `None`), so a field could never be *cleared* —
/// only set or left alone. This enum fixes that by pairing a custom [`Deserialize`] (which only ever
/// runs when the field is *present*, mapping `null` → `Clear` and a value → `Set`) with
/// `#[serde(default)]` on each request field (an *absent* field never calls the deserializer and so
/// defaults to [`Keep`](OptionalEdit::Keep)). On the TS side it renders as `T | null` (the field's
/// own `?:` carries the absent/present distinction).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OptionalEdit<T> {
    /// The field was **absent** from the request — leave the stored value unchanged.
    #[default]
    Keep,
    /// The field was present and `null` — **clear** the stored value (set it to `None`).
    Clear,
    /// The field was present with a value — **set** the stored value to it.
    Set(T),
}

impl<T> Serialize for OptionalEdit<T>
where
    T: Serialize,
{
    /// Serializes back to the wire shape `T | null`: [`Set`](OptionalEdit::Set) is its value,
    /// [`Clear`](OptionalEdit::Clear) is `null`. [`Keep`](OptionalEdit::Keep) also serializes to
    /// `null` here, but in practice a `Keep` field is *skipped* entirely (see
    /// [`OptionalEdit::is_keep`]) so an absent field stays absent on round-trip.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            OptionalEdit::Set(value) => value.serialize(serializer),
            OptionalEdit::Clear | OptionalEdit::Keep => serializer.serialize_none(),
        }
    }
}

impl<'de, T> Deserialize<'de> for OptionalEdit<T>
where
    T: Deserialize<'de>,
{
    /// Only ever called when the field is **present** (an absent field defaults to
    /// [`Keep`](OptionalEdit::Keep) via `#[serde(default)]`), so this maps the two present cases:
    /// a JSON `null` → [`Clear`](OptionalEdit::Clear); any value → [`Set`](OptionalEdit::Set).
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(value) => OptionalEdit::Set(value),
            None => OptionalEdit::Clear,
        })
    }
}

// A hand-written `TS` impl (rather than `#[derive(TS)]`) because the derive cannot render this
// three-variant enum as the flat `T | null` the wire actually is. It mirrors what the previous
// `#[serde(transparent)] struct OptionalEdit<T>(Option<T>)` derive emitted — the same
// `bindings/OptionalEdit.ts` (`type OptionalEdit<T> = T | null;`) — so the TS shape is unchanged
// and gen-drift stays clean. The matching `export_bindings_optionaledit` test (see the tests module)
// stands in for the `#[ts(export)]`-generated export test the derive would otherwise produce.
impl<T: TS> TS for OptionalEdit<T> {
    type WithoutGenerics = OptionalEdit<ts_rs::Dummy>;
    type OptionInnerType = Self;

    fn name(cfg: &ts_rs::Config) -> String {
        format!("OptionalEdit<{}>", T::name(cfg))
    }

    fn inline(cfg: &ts_rs::Config) -> String {
        format!("{} | null", T::inline(cfg))
    }

    fn inline_flattened(cfg: &ts_rs::Config) -> String {
        Self::inline(cfg)
    }

    fn decl(_cfg: &ts_rs::Config) -> String {
        "type OptionalEdit<T> = T | null;".to_owned()
    }

    fn decl_concrete(cfg: &ts_rs::Config) -> String {
        Self::decl(cfg)
    }

    fn visit_dependencies(v: &mut impl ts_rs::TypeVisitor)
    where
        Self: 'static,
    {
        <T as TS>::visit_dependencies(v);
    }

    fn visit_generics(v: &mut impl ts_rs::TypeVisitor)
    where
        Self: 'static,
    {
        <T as TS>::visit_generics(v);
        v.visit::<T>();
    }

    fn output_path() -> Option<std::path::PathBuf> {
        // Matches the `#[ts(export_to = "bindings/")]` the rest of the wire types use; combined with
        // the workspace-root `TS_RS_EXPORT_DIR` (pinned by xtask), it lands at `bindings/OptionalEdit.ts`.
        Some(std::path::PathBuf::from("bindings/OptionalEdit.ts"))
    }
}

impl<T> OptionalEdit<T> {
    /// Whether this edit is [`Keep`](OptionalEdit::Keep) (the field was absent) — used as the
    /// per-field `skip_serializing_if` so an unchanged field stays *absent* on the wire rather than
    /// round-tripping to a `null` (which would mean *clear*). `pub(crate)` so sibling registries
    /// (e.g. the class directory, #84) reuse the same three-state edit on their own request types.
    pub(crate) fn is_keep(&self) -> bool {
        matches!(self, OptionalEdit::Keep)
    }
}

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
                PilotError::internal(format!("could not create data dir {}: {e}", dir.display()))
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
            return Err(PilotError::invalid("a pilot callsign is required"));
        }
        let color = normalize_color(&request.color)?;
        let country = normalize_country(&request.country)?;
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
            phonetic: normalize_optional(&request.phonetic),
            team: normalize_optional(&request.team),
            color,
            country,
            vtx_types: normalize_vtx_types(&request.vtx_types),
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
        // Validate the set-edits up front (before taking the lock / mutating) so a bad value is a
        // clean rejection that leaves the stored pilot untouched.
        let color_edit = validate_color_edit(&request.color)?;
        let country_edit = validate_country_edit(&request.country)?;

        let mut dir = self.write();
        let pilot = dir
            .pilots
            .get_mut(id)
            .ok_or_else(|| PilotError::not_found(format!("no pilot with id {:?}", id.0)))?;
        if let Some(callsign) = &request.callsign {
            let trimmed = callsign.trim();
            if !trimmed.is_empty() {
                pilot.callsign = trimmed.to_string();
            }
        }
        apply_string_edit(&mut pilot.name, &request.name);
        apply_string_edit(&mut pilot.phonetic, &request.phonetic);
        apply_string_edit(&mut pilot.team, &request.team);
        if let Some(value) = color_edit {
            pilot.color = value;
        }
        if let Some(value) = country_edit {
            pilot.country = value;
        }
        // VTX is a full-replacement set: a present array replaces the stored set (normalized);
        // `Some([])` clears it; absent (`None`) leaves it unchanged.
        if let Some(vtx_types) = &request.vtx_types {
            pilot.vtx_types = normalize_vtx_types(vtx_types);
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
            return Err(PilotError::not_found(format!(
                "no pilot with id {:?}",
                id.0
            )));
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
            .map_err(|e| PilotError::internal(format!("could not serialize pilots: {e}")))?;
        std::fs::write(pilots_path(dir), json)
            .map_err(|e| PilotError::internal(format!("could not persist pilots: {e}")))
    }
}

/// Apply an optional string edit to a stored field: [`Keep`](OptionalEdit::Keep) → unchanged;
/// [`Clear`](OptionalEdit::Clear) → cleared; [`Set`](OptionalEdit::Set) → set (trimmed, with a blank
/// value treated as a clear).
fn apply_string_edit(field: &mut Option<String>, edit: &OptionalEdit<String>) {
    match edit {
        OptionalEdit::Keep => {}
        OptionalEdit::Clear => *field = None,
        OptionalEdit::Set(value) => {
            *field = Some(value.trim().to_string()).filter(|s| !s.is_empty());
        }
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

/// An error mutating the pilot directory (a persistence failure, an unknown id, a missing callsign,
/// an invalid field value). Carries a [`PilotErrorKind`] so the HTTP layer can map a *validation*
/// failure to `400` and an *unknown id* to `404`.
#[derive(Debug, Clone)]
pub struct PilotError {
    /// What kind of failure this is (drives the HTTP status the handler picks).
    pub kind: PilotErrorKind,
    /// A human-readable message.
    pub message: String,
}

/// The class of a [`PilotError`], so a handler can pick the right status (issue #74).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PilotErrorKind {
    /// A bad request value (missing callsign, invalid color/country, …) → 400.
    Invalid,
    /// The addressed pilot id does not exist → 404.
    NotFound,
    /// A server-side persistence failure → 500.
    Internal,
}

impl PilotError {
    /// A validation / bad-request error (HTTP 400).
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: PilotErrorKind::Invalid,
            message: message.into(),
        }
    }

    /// An unknown-id error (HTTP 404).
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: PilotErrorKind::NotFound,
            message: message.into(),
        }
    }

    /// An internal / persistence error (HTTP 500).
    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: PilotErrorKind::Internal,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PilotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pilot directory error: {}", self.message)
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

/// Validate + normalize a **hex color** `#RRGGBB` (issue #74).
///
/// A blank/absent value is unset (`None`); otherwise the value must be a `#` followed by exactly six
/// ASCII hex digits (case-insensitive), normalized to an uppercase `#RRGGBB`. Anything else is a
/// validation [`PilotError`].
fn normalize_color(value: &Option<String>) -> Result<Option<String>, PilotError> {
    let Some(raw) = value.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let hex = raw.strip_prefix('#').unwrap_or("");
    if hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(Some(format!("#{}", hex.to_ascii_uppercase())))
    } else {
        Err(PilotError::invalid(format!(
            "invalid color {raw:?}: expected a hex color like #RRGGBB"
        )))
    }
}

/// Validate + normalize an **ISO 3166-1 alpha-2** country code (issue #74).
///
/// A blank/absent value is unset (`None`); otherwise the value must be exactly two ASCII letters,
/// normalized to uppercase (e.g. `gb` → `GB`). We deliberately do **not** check the code against a
/// full country table — that lives in the UI. Anything else is a validation [`PilotError`].
fn normalize_country(value: &Option<String>) -> Result<Option<String>, PilotError> {
    let Some(raw) = value.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if raw.len() == 2 && raw.bytes().all(|b| b.is_ascii_alphabetic()) {
        Ok(Some(raw.to_ascii_uppercase()))
    } else {
        Err(PilotError::invalid(format!(
            "invalid country {raw:?}: expected a two-letter ISO 3166-1 alpha-2 code"
        )))
    }
}

/// Validate a *set-or-clear* edit of a validated optional field (color/country), returning the
/// outer edit (issue #74): [`Keep`](OptionalEdit::Keep) ⇒ `None` (leave unchanged);
/// [`Clear`](OptionalEdit::Clear) ⇒ `Some(None)` (clear); [`Set`](OptionalEdit::Set) ⇒
/// `Some(normalized)` (set to the validated value). A set value that fails `normalize` is a
/// validation [`PilotError`].
fn validate_edit_with(
    edit: &OptionalEdit<String>,
    normalize: impl Fn(&Option<String>) -> Result<Option<String>, PilotError>,
) -> Result<Option<Option<String>>, PilotError> {
    match edit {
        OptionalEdit::Keep => Ok(None),
        OptionalEdit::Clear => Ok(Some(None)),
        OptionalEdit::Set(value) => Ok(Some(normalize(&Some(value.clone()))?)),
    }
}

/// Validate a color set-or-clear edit (see [`validate_edit_with`] / [`normalize_color`]).
fn validate_color_edit(edit: &OptionalEdit<String>) -> Result<Option<Option<String>>, PilotError> {
    validate_edit_with(edit, normalize_color)
}

/// Validate a country set-or-clear edit (see [`validate_edit_with`] / [`normalize_country`]).
fn validate_country_edit(
    edit: &OptionalEdit<String>,
) -> Result<Option<Option<String>>, PilotError> {
    validate_edit_with(edit, normalize_country)
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
            ..Default::default()
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
                // Multiple VTX types, deliberately out of canonical order + a duplicate, to prove
                // create dedups and reorders.
                vtx_types: vec![VtxType::HDZero, VtxType::Analog, VtxType::HDZero],
                multigp_id: Some("mgp-123".to_string()),
                velocidrone_id: Some("  ".to_string()), // blank → unset
                ..Default::default()
            })
            .unwrap();
        assert_eq!(pilot.name.as_deref(), Some("Zoe Oom"));
        // Deduped + canonical order (Analog before HDZero).
        assert_eq!(pilot.vtx_types, vec![VtxType::Analog, VtxType::HDZero]);
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
                vtx_types: vec![VtxType::Analog],
                ..Default::default()
            })
            .unwrap();

        // Set callsign + multigp_id, clear name, leave vtx_types unchanged (absent).
        let updated = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    callsign: Some("New".to_string()),
                    name: OptionalEdit::Clear, // explicit clear
                    multigp_id: OptionalEdit::Set("mgp-9".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.callsign, "New");
        assert_eq!(updated.name, None);
        assert_eq!(updated.vtx_types, vec![VtxType::Analog]); // absent → unchanged
        assert_eq!(updated.multigp_id.as_deref(), Some("mgp-9"));

        // A blank callsign is ignored (required field never cleared).
        let again = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    callsign: Some("   ".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(again.callsign, "New");
    }

    #[test]
    fn optional_edit_deserializes_the_three_wire_cases() {
        // A present value → Set; a present `null` → Clear; an absent field → Keep (the default).
        // This is the core of the fix: before, a wire `null` collapsed to the *absent* case and
        // could never clear a field.
        let set: UpdatePilotRequest = serde_json::from_str(r#"{"team":"X"}"#).unwrap();
        assert_eq!(set.team, OptionalEdit::Set("X".to_string()));

        let clear: UpdatePilotRequest = serde_json::from_str(r#"{"team":null}"#).unwrap();
        assert_eq!(clear.team, OptionalEdit::Clear);

        let absent: UpdatePilotRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.team, OptionalEdit::Keep);
    }

    /// Round-trip a JSON body through `update` (persisted to `pilots.json`) to prove the three wire
    /// cases — set, clear-via-`null`, leave-via-absent — apply to the stored pilot. The headline:
    /// **`color` and `country` can now be cleared** (their `#hex` / 2-letter validation rejects the
    /// empty string, so a wire `null` is the only path to clear them).
    #[test]
    fn wire_null_clears_color_and_country_through_persistence() {
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-pilots-clear-{}", short_suffix()));
        {
            let dir = PilotDirectory::new(Some(data_dir.clone())).unwrap();
            let created = dir
                .create(&CreatePilotRequest {
                    callsign: "Clearable".to_string(),
                    team: Some("Team Zoom".to_string()),
                    color: Some("#abcdef".to_string()),
                    country: Some("de".to_string()),
                    ..Default::default()
                })
                .unwrap();

            // `{"color":"#112233","country":"US"}` → both *set* (normalized uppercase); team absent
            // → unchanged.
            let set_body: UpdatePilotRequest =
                serde_json::from_str(r##"{"color":"#112233","country":"us"}"##).unwrap();
            let set = dir.update(&created.id, &set_body).unwrap();
            assert_eq!(set.color.as_deref(), Some("#112233"));
            assert_eq!(set.country.as_deref(), Some("US"));
            assert_eq!(set.team.as_deref(), Some("Team Zoom")); // absent → unchanged

            // `{"color":null,"country":null}` → both *cleared* (the case the old code could not
            // express for validated fields). team still absent → still unchanged.
            let clear_body: UpdatePilotRequest =
                serde_json::from_str(r#"{"color":null,"country":null}"#).unwrap();
            let cleared = dir.update(&created.id, &clear_body).unwrap();
            assert_eq!(cleared.color, None, "wire null must clear color");
            assert_eq!(cleared.country, None, "wire null must clear country");
            assert_eq!(cleared.team.as_deref(), Some("Team Zoom"));

            // The clear survives a restart (it was persisted to pilots.json, not just held in mem).
            let reopened = PilotDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.color, None);
            assert_eq!(got.country, None);
            assert_eq!(got.team.as_deref(), Some("Team Zoom"));
        }
        std::fs::remove_dir_all(&data_dir).ok();
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
                    ..Default::default()
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
                    phonetic: Some("PER sis-ted".to_string()),
                    team: Some("Team Zoom".to_string()),
                    color: Some("#ff0000".to_string()),
                    country: Some("gb".to_string()),
                    vtx_types: vec![VtxType::DJI, VtxType::Other],
                    multigp_id: Some("mgp-7".to_string()),
                    velocidrone_id: None,
                })
                .unwrap();

            let reopened = PilotDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.callsign, "Persisted");
            assert_eq!(got.name.as_deref(), Some("Per Sisted"));
            assert_eq!(got.phonetic.as_deref(), Some("PER sis-ted"));
            assert_eq!(got.team.as_deref(), Some("Team Zoom"));
            assert_eq!(got.color.as_deref(), Some("#FF0000")); // normalized uppercase
            assert_eq!(got.country.as_deref(), Some("GB")); // normalized uppercase
            assert_eq!(got.vtx_types, vec![VtxType::DJI, VtxType::Other]);
            assert_eq!(got.multigp_id.as_deref(), Some("mgp-7"));
            assert_eq!(got.velocidrone_id, None);
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn color_validation_accepts_hex_and_rejects_garbage() {
        let dir = PilotDirectory::new(None).unwrap();
        // Accepts #RRGGBB (any case), normalizes to uppercase.
        let ok = dir
            .create(&CreatePilotRequest {
                callsign: "Hexy".to_string(),
                color: Some("#AbCdEf".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(ok.color.as_deref(), Some("#ABCDEF"));

        // Rejects non-hex / wrong length / missing #.
        for bad in ["red", "#12345", "#1234567", "1188ff", "#zzzzzz"] {
            let err = dir
                .create(&CreatePilotRequest {
                    callsign: "Bad".to_string(),
                    color: Some(bad.to_string()),
                    ..Default::default()
                })
                .unwrap_err();
            assert_eq!(err.kind, PilotErrorKind::Invalid, "should reject {bad:?}");
        }

        // An update with a bad color is rejected and leaves the stored value untouched.
        let bad_update = dir
            .update(
                &ok.id,
                &UpdatePilotRequest {
                    color: OptionalEdit::Set("nope".to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert_eq!(bad_update.kind, PilotErrorKind::Invalid);
        assert_eq!(dir.get(&ok.id).unwrap().color.as_deref(), Some("#ABCDEF"));

        // Clearing the color works.
        let cleared = dir
            .update(
                &ok.id,
                &UpdatePilotRequest {
                    color: OptionalEdit::Clear,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(cleared.color, None);
    }

    #[test]
    fn country_validation_accepts_two_letters_and_rejects_others() {
        let dir = PilotDirectory::new(None).unwrap();
        let ok = dir
            .create(&CreatePilotRequest {
                callsign: "Yank".to_string(),
                country: Some("us".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(ok.country.as_deref(), Some("US"));

        for bad in ["USA", "U", "12", "U1", "g b"] {
            let err = dir
                .create(&CreatePilotRequest {
                    callsign: "Bad".to_string(),
                    country: Some(bad.to_string()),
                    ..Default::default()
                })
                .unwrap_err();
            assert_eq!(err.kind, PilotErrorKind::Invalid, "should reject {bad:?}");
        }

        // Update to a valid code (normalized) and then clear.
        let updated = dir
            .update(
                &ok.id,
                &UpdatePilotRequest {
                    country: OptionalEdit::Set("De".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.country.as_deref(), Some("DE"));
    }

    #[test]
    fn vtx_types_replace_dedup_and_clear_via_empty_array() {
        let dir = PilotDirectory::new(None).unwrap();
        let created = dir
            .create(&CreatePilotRequest {
                callsign: "Multi".to_string(),
                vtx_types: vec![VtxType::Analog, VtxType::HDZero],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(created.vtx_types, vec![VtxType::Analog, VtxType::HDZero]);

        // A present array fully replaces the set (and is deduped + reordered to canonical order).
        let replaced = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    vtx_types: Some(vec![VtxType::Other, VtxType::DJI, VtxType::Other]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(replaced.vtx_types, vec![VtxType::DJI, VtxType::Other]);

        // An absent vtx_types leaves the set unchanged.
        let unchanged = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    name: OptionalEdit::Set("Nom".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(unchanged.vtx_types, vec![VtxType::DJI, VtxType::Other]);

        // A present empty array clears the set (there is no "None" concept — empty = unspecified).
        let cleared = dir
            .update(
                &created.id,
                &UpdatePilotRequest {
                    vtx_types: Some(vec![]),
                    ..Default::default()
                },
            )
            .unwrap();
        assert!(cleared.vtx_types.is_empty());
    }

    #[test]
    fn loads_legacy_scalar_vtx_type_as_a_one_element_set() {
        // An existing pilots.json row written before this change carries the old scalar
        // `vtx_type: "HDZero"` (and no `vtx_types`). It must migrate to `vtx_types: ["HDZero"]`
        // rather than crashing the boot or losing the value.
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-pilots-legacy-{}", short_suffix()));
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            pilots_path(&data_dir),
            br#"[{"id":"legacy-1","callsign":"Legacy","vtx_type":"HDZero","attributes":{}}]"#,
        )
        .unwrap();

        let dir = PilotDirectory::new(Some(data_dir.clone())).unwrap();
        let got = dir.get(&PilotId("legacy-1".into())).unwrap();
        assert_eq!(got.callsign, "Legacy");
        assert_eq!(
            got.vtx_types,
            vec![VtxType::HDZero],
            "legacy scalar migrates"
        );

        // A legacy row that never set a VTX (no key at all) loads as the empty set.
        std::fs::write(
            pilots_path(&data_dir),
            br#"[{"id":"legacy-2","callsign":"NoVtx","attributes":{}}]"#,
        )
        .unwrap();
        let reopened = PilotDirectory::new(Some(data_dir.clone())).unwrap();
        let none = reopened.get(&PilotId("legacy-2".into())).unwrap();
        assert!(none.vtx_types.is_empty());

        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn loads_a_legacy_row_that_still_carries_a_dropped_attributes_field() {
        // Pilots created before the custom-attributes bag was removed still have an `"attributes"`
        // key on disk. The `Pilot` type no longer has the field; the row must still load (the stale
        // key is simply ignored — `Pilot` must NOT use `deny_unknown_fields`), with no migration.
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-pilots-attrs-{}", short_suffix()));
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(
            pilots_path(&data_dir),
            br#"[{"id":"legacy-attr","callsign":"OldAttr","vtx_types":["Analog"],"attributes":{"bib":"7","insurance":"AMA-123"}}]"#,
        )
        .unwrap();

        let dir = PilotDirectory::new(Some(data_dir.clone())).unwrap();
        let got = dir.get(&PilotId("legacy-attr".into())).unwrap();
        assert_eq!(got.callsign, "OldAttr");
        assert_eq!(got.vtx_types, vec![VtxType::Analog]);

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
