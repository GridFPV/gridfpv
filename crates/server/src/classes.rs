//! Classes as **application-level configuration** — the `ClassDirectory` and `Class` (issue #84).
//!
//! A class is a *racing category in the Director's address book*: a name and a little optional
//! metadata (where it came from, a reference id, a description). The model parallels the pilot
//! directory ([`PilotDirectory`](crate::pilots::PilotDirectory)): the Race Director maintains
//! their classes **once** at the application level (a persisted directory) and each event simply
//! builds a **selection** of which directory classes run at it (see
//! [`EventMeta::classes`](crate::events::EventMeta::classes)). Type a class in once, and every new
//! event just picks them.
//!
//! # Two pieces, mirroring pilots
//!
//! - **App-level directory (this module).** The [`ClassDirectory`] holds every configured
//!   [`Class`] behind a lock and **persists** the user/Custom ones to
//!   `<GRIDFPV_DATA_DIR>/classes.json` (restored on boot; in-memory only when no data dir is
//!   configured). On top of those it always seeds **9 code-defined built-in classes** (issue #84) —
//!   the standard FPV classes (MultiGP / Five33 / FreedomSpec / Street League / UDL) — with **fixed
//!   ids**, identical on every Director, so cross-event / cross-Director standings aggregate with
//!   zero reconciliation. The built-ins are **read-only** (non-editable, non-deletable, like the
//!   Mock timer) and are **never** written to `classes.json` (re-seeded on every boot). Users only
//!   ever create [`Custom`](ClassSource::Custom) classes (full CRUD).
//! - **Per-event selection (`crate::events`).** Each [`EventMeta`](crate::events::EventMeta)
//!   carries a `classes: Vec<ClassId>` of the directory classes that run at that event; new events
//!   default to an **empty** selection.
//!
//! This is the **registry slice only** (issue #84's lineage): the directory of *which classes
//! exist* plus the per-event selection of *which run here*. The rounds / phase engine that a class
//! later drives is a separate concern and is not modelled here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::pilots::OptionalEdit;
use crate::scope::ClassId;

/// The file name (under the data dir) the class directory is persisted to (issue #84).
pub const CLASSES_FILE: &str = "classes.json";

/// Where a [`Class`] came from (issue #84).
///
/// A small closed enum the directory records so the RD can tell a canonical built-in class (e.g. a
/// MultiGP spec class) from one they typed in themselves. Externally tagged (the default serde enum
/// representation) so it maps to a TS string union (`"MultiGP" | "Five33" | … | "Custom" |
/// "Other"`), exactly like [`VtxType`](crate::pilots::VtxType). The org variants name the standard
/// FPV racing leagues / spec bodies the built-in classes carry as their provenance (shown as a
/// badge). Defaults to [`Custom`](ClassSource::Custom) — a class the RD created by hand; users only
/// ever create `Custom` classes. [`Other`](ClassSource::Other) is the catch-all for any provenance
/// not enumerated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ClassSource {
    /// Imported from / aligned with **MultiGP** (a GQ / spec class).
    MultiGP,
    /// Aligned with **Five33** (flyfive33.com spec classes, e.g. Tiny Trainer).
    Five33,
    /// Aligned with the **Freedom Spec** open-spec class (freedomspec.com).
    FreedomSpec,
    /// Aligned with **Street League** (streetleague.io spec).
    StreetLeague,
    /// Aligned with the **Underground Drone League** (UDL, undergrounddroneleague.com).
    UDL,
    /// A **custom** class the RD created by hand (the default). The only source a user creates.
    #[default]
    Custom,
    /// Any other provenance not enumerated above (catch-all).
    Other,
}

/// One class in the application-level directory (issue #84).
///
/// The wire shape `GET /classes` returns and the on-disk shape `classes.json` persists: a stable
/// [`ClassId`] (auto-generated, never user-entered), a required `name`, a [`source`](Class::source)
/// provenance, and a little optional metadata. The optional fields are omitted from the wire when
/// unset (`skip_serializing_if`). Derives serde (its JSON *is* both the wire and the persisted form)
/// and `ts_rs::TS` so the frontend reads a generated `Class` type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct Class {
    /// The stable handle a per-event selection references and the API addresses
    /// (`PUT /classes/{id}`). The same [`ClassId`] the scope/log layer uses — directory and scope
    /// never disagree on what a class id is.
    pub id: ClassId,
    /// The class's **name** — the required display label (e.g. `"Open"`, `"Spec 5\""`). The
    /// directory's one mandatory field; everything else is optional metadata.
    pub name: String,
    /// Where this class came from (see [`ClassSource`]). Defaults to
    /// [`Custom`](ClassSource::Custom).
    #[serde(default)]
    pub source: ClassSource,
    /// An optional **reference** id/handle into the source system (e.g. a MultiGP class id), if
    /// recorded. Free-form. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reference: Option<String>,
    /// An optional free-text description / notes for the class. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Whether this is a **code-defined built-in** class (issue #84) — one of the canonical
    /// standard FPV classes seeded into every directory with a fixed id, so cross-event /
    /// cross-Director standings aggregate with zero reconciliation. Built-ins are **read-only**:
    /// they cannot be edited or deleted, and are **not** persisted to `classes.json` (they are
    /// re-seeded on every boot). A user-created [`Custom`](ClassSource::Custom) class is never a
    /// built-in. Defaults to `false` and is omitted from the wire / disk when false, so a custom
    /// class's JSON is unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builtin: bool,
}

/// `skip_serializing_if` helper: the `builtin` flag is omitted from the wire/disk when false, so a
/// user/Custom class round-trips its original shape.
fn is_false(value: &bool) -> bool {
    !*value
}

/// One code-defined built-in class spec (issue #84): a fixed id + its canonical fields. Turned into
/// a [`Class`] (with `builtin = true`) by [`builtin_classes`].
struct BuiltinClass {
    /// The fixed, stable slug id — identical on every Director so standings aggregate.
    id: &'static str,
    /// The canonical display name.
    name: &'static str,
    /// The real org this class belongs to (shown as a source badge).
    source: ClassSource,
    /// A reference URL into the org's rules.
    reference: &'static str,
    /// A short canonical description.
    description: &'static str,
}

/// The 9 standard FPV racing classes seeded into **every** directory with **fixed ids** (issue
/// #84). Present on every Director identically so cross-event / cross-Director season standings
/// aggregate without reconciliation; carry their real org as [`source`](Class::source) (a badge);
/// are read-only (non-editable, non-deletable); and are **not** persisted to `classes.json`.
const BUILTIN_CLASSES: &[BuiltinClass] = &[
    BuiltinClass {
        id: "mgp-open",
        name: "Open Class",
        source: ClassSource::MultiGP,
        reference: "https://www.multigp.com/class-specifications/",
        description: "Open/unlimited 5–6\" class — no motor/prop/electronics limits.",
    },
    BuiltinClass {
        id: "mgp-pro-spec",
        name: "Pro Spec",
        source: ClassSource::MultiGP,
        reference: "https://www.multigp.com/prospec/",
        description: "MultiGP's official 7\" spec class (Pro Spec frame, spec motor).",
    },
    BuiltinClass {
        id: "mgp-whoop",
        name: "Whoop Class",
        source: ClassSource::MultiGP,
        reference: "https://www.multigp.com/class-specifications/",
        description: "1S indoor micro whoop (65mm ducted).",
    },
    BuiltinClass {
        id: "mgp-micro",
        name: "Micro Class",
        source: ClassSource::MultiGP,
        reference: "https://www.multigp.com/class-specifications/microclass/",
        description: "3\" micro class (1404 motor, 3S).",
    },
    BuiltinClass {
        id: "five33-tiny-trainer",
        name: "Tiny Trainer",
        source: ClassSource::Five33,
        reference: "https://flyfive33.com/pages/tt-spec",
        description: "Five33 3\" spec (Tiny Trainer frame, 1404, 3S).",
    },
    BuiltinClass {
        id: "freedom-spec",
        name: "Freedom Spec",
        source: ClassSource::FreedomSpec,
        reference: "https://freedomspec.com/",
        description: "Open 5\" spec class with a standardized motor-RPM cap.",
    },
    BuiltinClass {
        id: "street-league",
        name: "Street League",
        source: ClassSource::StreetLeague,
        reference: "https://streetleague.io/spec",
        description: "7\" spec on firmware that equalizes motors via an RPM limiter.",
    },
    BuiltinClass {
        id: "udl-igniter",
        name: "Igniter",
        source: ClassSource::UDL,
        reference: "https://undergrounddroneleague.com/igniter-legal-parts",
        description: "Indoor 1S 75mm-ducted spec, approved-parts-only, RPM-limited.",
    },
    BuiltinClass {
        id: "udl-shrieker",
        name: "Shrieker",
        source: ClassSource::UDL,
        reference: "https://undergrounddroneleague.com/drone-types",
        description: "Fast 1S 65mm micro, near-open ruleset.",
    },
];

/// Build the 9 built-in [`Class`]es (issue #84) — the canonical, fixed-id, read-only classes seeded
/// into every directory. Each carries `builtin = true` and its real org as the source.
fn builtin_classes() -> Vec<Class> {
    BUILTIN_CLASSES
        .iter()
        .map(|b| Class {
            id: ClassId(b.id.to_string()),
            name: b.name.to_string(),
            source: b.source,
            reference: Some(b.reference.to_string()),
            description: Some(b.description.to_string()),
            builtin: true,
        })
        .collect()
}

/// Whether `id` is one of the fixed built-in class ids (issue #84) — built-ins are recognized by
/// their reserved id set, which is how the edit/delete guards and the persistence filter know not
/// to touch them.
fn is_builtin_id(id: &ClassId) -> bool {
    BUILTIN_CLASSES.iter().any(|b| b.id == id.0)
}

/// The body of `POST /classes` — the fields a caller supplies to create a class (issue #84).
///
/// The `name` is required; everything else is optional. The **id is auto-generated** server-side (a
/// slug of the name + a short random suffix), never user-entered, mirroring `POST /pilots`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateClassRequest {
    /// The required name for the new class.
    pub name: String,
    /// The class's provenance, stored on [`Class::source`]. Defaults to
    /// [`Custom`](ClassSource::Custom).
    #[serde(default)]
    pub source: ClassSource,
    /// Optional reference id, stored on [`Class::reference`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub reference: Option<String>,
    /// Optional description, stored on [`Class::description`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
}

/// The body of `PUT /classes/{id}` — the editable fields of a class (issue #84).
///
/// Every field is optional so a partial edit is a one-field body; the id is fixed (it is in the
/// path). A present `name` replaces it (a blank one is ignored — the name is required and never
/// cleared). A present `source` replaces the provenance. Each optional-metadata field is a
/// three-state [`OptionalEdit`]: **absent** leaves it unchanged ([`Keep`](OptionalEdit::Keep)),
/// present **`null`** clears it ([`Clear`](OptionalEdit::Clear)), and present **with a value** sets
/// it ([`Set`](OptionalEdit::Set)) — exactly the `UpdatePilotRequest` semantics.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UpdateClassRequest {
    /// A new name, or `None`/blank to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<String>,
    /// A new provenance, or `None` to leave it unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<ClassSource>,
    /// A new reference id: present value → set, present `null` → clear, absent → leave unchanged.
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub reference: OptionalEdit<String>,
    /// A new description (set / clear / leave-unchanged, like [`reference`](Self::reference)).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub description: OptionalEdit<String>,
}

/// The application-level directory of all configured classes (issue #84).
///
/// Maps each [`ClassId`] to its [`Class`]. The set is **persisted** to `<data_dir>/classes.json`
/// (restored on boot) so the RD's category list survives a Director restart; with no data dir
/// configured it is in-memory only. Cloning shares the one directory (`Arc<RwLock<…>>`), so it is
/// reached through the axum router state ([`EventRegistry`](crate::events::EventRegistry)) exactly
/// like the [`PilotDirectory`](crate::pilots::PilotDirectory).
#[derive(Clone)]
pub struct ClassDirectory {
    inner: Arc<RwLock<Directory>>,
}

/// The guarded interior: the class map and where `classes.json` lives.
struct Directory {
    /// `ClassId → Class`. A `BTreeMap` so listing is deterministic (id order).
    classes: BTreeMap<ClassId, Class>,
    /// Directory `classes.json` is persisted under; `None` ⇒ in-memory only (no data dir).
    data_dir: Option<PathBuf>,
}

impl ClassDirectory {
    /// Build a directory seeded with the 9 read-only built-in classes, restoring any user/Custom
    /// classes from `<data_dir>/classes.json` when a data dir is given (issue #84).
    ///
    /// The built-ins are **always present** with their fixed ids — re-seeded on every boot, never
    /// read from disk — so every Director carries the same canonical classes. When `data_dir` is
    /// `Some` and a `classes.json` already exists, the saved **user** classes are restored on top
    /// (an unreadable/corrupt file degrades to just the built-ins rather than failing to boot); any
    /// stale entry that collides with a built-in id is ignored (the code-defined built-in wins).
    /// When `data_dir` is `None` the directory is in-memory only (still seeded with the built-ins).
    pub fn new(data_dir: Option<PathBuf>) -> Result<Self, ClassError> {
        let mut classes = BTreeMap::new();

        // Always seed the code-defined built-ins first (they are never read from disk).
        for class in builtin_classes() {
            classes.insert(class.id.clone(), class);
        }

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                ClassError::internal(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            if let Some(restored) = read_persisted_classes(dir) {
                for mut class in restored {
                    // A persisted entry can never be a built-in (we never write them); ignore any
                    // stale id collision and force the flag off for restored user classes.
                    if is_builtin_id(&class.id) {
                        continue;
                    }
                    class.builtin = false;
                    classes.insert(class.id.clone(), class);
                }
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Directory { classes, data_dir })),
        })
    }

    /// Every class in id order — the `GET /classes` body.
    pub fn list(&self) -> Vec<Class> {
        self.read().classes.values().cloned().collect()
    }

    /// Whether a class with `id` exists — the per-event selection validates each id through this.
    pub fn exists(&self, id: &ClassId) -> bool {
        self.read().classes.contains_key(id)
    }

    /// The [`Class`] for `id`, or `None`.
    pub fn get(&self, id: &ClassId) -> Option<Class> {
        self.read().classes.get(id).cloned()
    }

    /// Create a class from a [`CreateClassRequest`], returning it (issue #84).
    ///
    /// The **id is auto-generated** — a slug of the `name` + a short random suffix — so it is
    /// unique and never user-entered. The optional metadata is stored verbatim (trimmed, with a
    /// blank treated as unset). The directory is **persisted** on success.
    pub fn create(&self, request: &CreateClassRequest) -> Result<Class, ClassError> {
        let name = request.name.trim();
        if name.is_empty() {
            return Err(ClassError::invalid("a class name is required"));
        }
        let mut dir = self.write();
        let id = loop {
            let candidate = ClassId(format!("{}-{}", slugify(name), short_suffix()));
            if !is_builtin_id(&candidate) && !dir.classes.contains_key(&candidate) {
                break candidate;
            }
        };
        let class = Class {
            id: id.clone(),
            name: name.to_string(),
            source: request.source,
            reference: normalize_optional(&request.reference),
            description: normalize_optional(&request.description),
            builtin: false,
        };
        dir.classes.insert(id, class.clone());
        dir.persist()?;
        Ok(class)
    }

    /// Edit a class's fields (issue #84), returning the updated [`Class`].
    ///
    /// A present `name` replaces it (a blank one is ignored — the name is required). A present
    /// `source` replaces the provenance. Each optional metadata field is a three-way edit: absent →
    /// unchanged; present `Some(value)` → set (trimmed; a blank string clears it); present `null` →
    /// cleared. A **built-in** class is read-only — editing one is rejected (issue #84). An unknown
    /// id is a [`ClassError`]. The directory is **persisted** on success.
    pub fn update(&self, id: &ClassId, request: &UpdateClassRequest) -> Result<Class, ClassError> {
        if is_builtin_id(id) {
            return Err(ClassError::read_only(format!(
                "the built-in class {:?} is read-only and cannot be edited",
                id.0
            )));
        }
        let mut dir = self.write();
        let class = dir
            .classes
            .get_mut(id)
            .ok_or_else(|| ClassError::not_found(format!("no class with id {:?}", id.0)))?;
        if let Some(name) = &request.name {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                class.name = trimmed.to_string();
            }
        }
        if let Some(source) = request.source {
            class.source = source;
        }
        apply_string_edit(&mut class.reference, &request.reference);
        apply_string_edit(&mut class.description, &request.description);
        let updated = class.clone();
        dir.persist()?;
        Ok(updated)
    }

    /// Delete a class (issue #84). A **built-in** class is read-only and cannot be deleted (it is
    /// always present); attempting to is rejected — mirroring the Mock timer's protected delete. An
    /// unknown id is a [`ClassError`]. The directory is **persisted** on success. (Removing a class
    /// does not touch any event selection that still names it; a stale selection id is harmless —
    /// see the events module.)
    pub fn delete(&self, id: &ClassId) -> Result<(), ClassError> {
        if is_builtin_id(id) {
            return Err(ClassError::read_only(format!(
                "the built-in class {:?} is read-only and cannot be deleted",
                id.0
            )));
        }
        let mut dir = self.write();
        if dir.classes.remove(id).is_none() {
            return Err(ClassError::not_found(format!(
                "no class with id {:?}",
                id.0
            )));
        }
        dir.persist()?;
        Ok(())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Directory> {
        self.inner.read().expect("class directory lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Directory> {
        self.inner.write().expect("class directory lock poisoned")
    }
}

impl Directory {
    /// Persist the **user/Custom** classes to `<data_dir>/classes.json` (issue #84), a no-op with no
    /// data dir. The code-defined built-ins are **never** written — they are re-seeded on every boot
    /// — so the persisted file holds only the classes the RD created.
    fn persist(&self) -> Result<(), ClassError> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let classes: Vec<&Class> = self
            .classes
            .values()
            .filter(|c| !c.builtin && !is_builtin_id(&c.id))
            .collect();
        let json = serde_json::to_string_pretty(&classes)
            .map_err(|e| ClassError::internal(format!("could not serialize classes: {e}")))?;
        std::fs::write(classes_path(dir), json)
            .map_err(|e| ClassError::internal(format!("could not persist classes: {e}")))
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

/// The file the class set is persisted to under `dir`: `<dir>/classes.json`.
fn classes_path(dir: &Path) -> PathBuf {
    dir.join(CLASSES_FILE)
}

/// Read the persisted classes from `<dir>/classes.json`, or `None` if absent/unreadable/corrupt.
/// A bad file degrades to "no persisted classes" so the Director still boots.
fn read_persisted_classes(dir: &Path) -> Option<Vec<Class>> {
    let raw = std::fs::read_to_string(classes_path(dir)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// An error mutating the class directory (a persistence failure, an unknown id, a missing name).
/// Carries a [`ClassErrorKind`] so the HTTP layer can map a *validation* failure to `400` and an
/// *unknown id* to `404`.
#[derive(Debug, Clone)]
pub struct ClassError {
    /// What kind of failure this is (drives the HTTP status the handler picks).
    pub kind: ClassErrorKind,
    /// A human-readable message.
    pub message: String,
}

/// The class of a [`ClassError`], so a handler can pick the right status (issue #84).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassErrorKind {
    /// A bad request value (missing name, …) → 400.
    Invalid,
    /// An attempt to edit/delete a read-only **built-in** class → 400 (rejected, like the Mock
    /// timer's protected delete).
    ReadOnly,
    /// The addressed class id does not exist → 404.
    NotFound,
    /// A server-side persistence failure → 500.
    Internal,
}

impl ClassError {
    /// A validation / bad-request error (HTTP 400).
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: ClassErrorKind::Invalid,
            message: message.into(),
        }
    }

    /// A read-only error: an attempt to edit/delete a built-in class (HTTP 400).
    fn read_only(message: impl Into<String>) -> Self {
        Self {
            kind: ClassErrorKind::ReadOnly,
            message: message.into(),
        }
    }

    /// An unknown-id error (HTTP 404).
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: ClassErrorKind::NotFound,
            message: message.into(),
        }
    }

    /// An internal / persistence error (HTTP 500).
    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: ClassErrorKind::Internal,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ClassError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "class directory error: {}", self.message)
    }
}

impl std::error::Error for ClassError {}

/// Trim an optional field, treating a blank/whitespace-only value as **unset** (`None`).
fn normalize_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Slugify a name into an id-friendly stem (the same rule as the pilot/event registries):
/// lowercase ASCII alphanumerics kept, every other run collapsed to a single `-`, trimmed of
/// dashes; an empty/symbol-only name yields `class`.
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
        "class".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A short random lowercase-alphanumeric suffix making an auto-generated id unique (same source as
/// the pilot/event registries — the OS CSPRNG).
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

    fn req(name: &str) -> CreateClassRequest {
        CreateClassRequest {
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// The number of code-defined built-in classes seeded into every directory.
    const BUILTIN_COUNT: usize = 9;

    /// The non-built-in (user/Custom) classes in id order.
    fn user_classes(dir: &ClassDirectory) -> Vec<Class> {
        dir.list().into_iter().filter(|c| !c.builtin).collect()
    }

    #[test]
    fn a_fresh_directory_holds_only_the_built_ins() {
        let dir = ClassDirectory::new(None).unwrap();
        let list = dir.list();
        assert_eq!(list.len(), BUILTIN_COUNT);
        assert!(list.iter().all(|c| c.builtin));
        assert!(user_classes(&dir).is_empty());
    }

    #[test]
    fn the_nine_built_ins_are_present_with_fixed_ids_orgs_and_metadata() {
        let dir = ClassDirectory::new(None).unwrap();
        // Every fixed id is present, flagged built-in, and carries its real org + reference + desc.
        for spec in BUILTIN_CLASSES {
            let id = ClassId(spec.id.to_string());
            let got = dir
                .get(&id)
                .unwrap_or_else(|| panic!("missing {}", spec.id));
            assert!(got.builtin, "{} must be flagged built-in", spec.id);
            assert_eq!(got.name, spec.name);
            assert_eq!(got.source, spec.source);
            assert_eq!(got.reference.as_deref(), Some(spec.reference));
            assert_eq!(got.description.as_deref(), Some(spec.description));
        }
        // The expected fixed-id set, exactly.
        let ids: std::collections::BTreeSet<_> = dir.list().into_iter().map(|c| c.id.0).collect();
        for expected in [
            "mgp-open",
            "mgp-pro-spec",
            "mgp-whoop",
            "mgp-micro",
            "five33-tiny-trainer",
            "freedom-spec",
            "street-league",
            "udl-igniter",
            "udl-shrieker",
        ] {
            assert!(ids.contains(expected), "missing built-in id {expected}");
        }
        // The org sources are the expected spread.
        assert_eq!(
            dir.get(&ClassId("mgp-open".into())).unwrap().source,
            ClassSource::MultiGP
        );
        assert_eq!(
            dir.get(&ClassId("five33-tiny-trainer".into()))
                .unwrap()
                .source,
            ClassSource::Five33
        );
        assert_eq!(
            dir.get(&ClassId("freedom-spec".into())).unwrap().source,
            ClassSource::FreedomSpec
        );
        assert_eq!(
            dir.get(&ClassId("street-league".into())).unwrap().source,
            ClassSource::StreetLeague
        );
        assert_eq!(
            dir.get(&ClassId("udl-igniter".into())).unwrap().source,
            ClassSource::UDL
        );
    }

    #[test]
    fn a_built_in_cannot_be_edited_or_deleted() {
        let dir = ClassDirectory::new(None).unwrap();
        let id = ClassId("mgp-open".into());

        let edit = dir.update(
            &id,
            &UpdateClassRequest {
                name: Some("Renamed".into()),
                ..Default::default()
            },
        );
        let err = edit.unwrap_err();
        assert_eq!(err.kind, ClassErrorKind::ReadOnly);
        // The built-in is untouched.
        assert_eq!(dir.get(&id).unwrap().name, "Open Class");

        let del = dir.delete(&id);
        assert_eq!(del.unwrap_err().kind, ClassErrorKind::ReadOnly);
        assert!(dir.exists(&id));
    }

    #[test]
    fn class_source_defaults_to_custom() {
        assert_eq!(ClassSource::default(), ClassSource::Custom);
        let dir = ClassDirectory::new(None).unwrap();
        let class = dir.create(&req("Open")).unwrap();
        assert_eq!(class.source, ClassSource::Custom);
    }

    #[test]
    fn create_auto_generates_a_unique_slug_id() {
        let dir = ClassDirectory::new(None).unwrap();
        let a = dir.create(&req("Spec 5\"!")).unwrap();
        let b = dir.create(&req("Spec 5\"!")).unwrap();
        assert!(a.id.0.starts_with("spec-5-"));
        assert_ne!(a.id, b.id);
        assert_eq!(a.name, "Spec 5\"!");
        let ids: Vec<_> = dir.list().into_iter().map(|c| c.id).collect();
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn create_requires_a_name() {
        let dir = ClassDirectory::new(None).unwrap();
        let err = dir.create(&req("   ")).unwrap_err();
        assert_eq!(err.kind, ClassErrorKind::Invalid);
        assert!(user_classes(&dir).is_empty());
    }

    #[test]
    fn create_stores_source_and_optional_metadata() {
        let dir = ClassDirectory::new(None).unwrap();
        let class = dir
            .create(&CreateClassRequest {
                name: "Open".to_string(),
                source: ClassSource::MultiGP,
                reference: Some("  mgp-open  ".to_string()), // trimmed
                description: Some("  ".to_string()),         // blank → unset
            })
            .unwrap();
        assert_eq!(class.source, ClassSource::MultiGP);
        assert_eq!(class.reference.as_deref(), Some("mgp-open"));
        assert_eq!(class.description, None);
    }

    #[test]
    fn update_edits_name_source_and_optional_fields_with_clear_semantics() {
        let dir = ClassDirectory::new(None).unwrap();
        let created = dir
            .create(&CreateClassRequest {
                name: "Old".to_string(),
                source: ClassSource::Custom,
                reference: Some("ref-1".to_string()),
                description: Some("desc".to_string()),
            })
            .unwrap();

        // Set name + source, clear reference, leave description unchanged (absent).
        let updated = dir
            .update(
                &created.id,
                &UpdateClassRequest {
                    name: Some("New".to_string()),
                    source: Some(ClassSource::MultiGP),
                    reference: OptionalEdit::Clear, // explicit clear
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.name, "New");
        assert_eq!(updated.source, ClassSource::MultiGP);
        assert_eq!(updated.reference, None);
        assert_eq!(updated.description.as_deref(), Some("desc")); // absent → unchanged

        // A blank name is ignored (required field never cleared).
        let again = dir
            .update(
                &created.id,
                &UpdateClassRequest {
                    name: Some("   ".to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(again.name, "New");
    }

    #[test]
    fn optional_edit_deserializes_the_three_wire_cases() {
        // A present value → Set; a present `null` → Clear; an absent field → Keep (the default).
        let set: UpdateClassRequest = serde_json::from_str(r#"{"reference":"X"}"#).unwrap();
        assert_eq!(set.reference, OptionalEdit::Set("X".to_string()));

        let clear: UpdateClassRequest = serde_json::from_str(r#"{"reference":null}"#).unwrap();
        assert_eq!(clear.reference, OptionalEdit::Clear);

        let absent: UpdateClassRequest = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.reference, OptionalEdit::Keep);
    }

    /// Round-trip a JSON body through `update` (persisted to `classes.json`) to prove a wire `null`
    /// clears `reference`/`description` — the [`OptionalEdit`] behavior — and that the clear
    /// survives a restart.
    #[test]
    fn wire_null_clears_reference_and_description_through_persistence() {
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-classes-clear-{}", short_suffix()));
        {
            let dir = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let created = dir
                .create(&CreateClassRequest {
                    name: "Clearable".to_string(),
                    source: ClassSource::Other,
                    reference: Some("ref-x".to_string()),
                    description: Some("a description".to_string()),
                })
                .unwrap();

            // `{"reference":null,"description":null}` → both cleared; source absent → unchanged.
            let clear_body: UpdateClassRequest =
                serde_json::from_str(r#"{"reference":null,"description":null}"#).unwrap();
            let cleared = dir.update(&created.id, &clear_body).unwrap();
            assert_eq!(cleared.reference, None, "wire null must clear reference");
            assert_eq!(
                cleared.description, None,
                "wire null must clear description"
            );
            assert_eq!(cleared.source, ClassSource::Other); // absent → unchanged

            // The clear survives a restart (it was persisted to classes.json, not just in mem).
            let reopened = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.reference, None);
            assert_eq!(got.description, None);
            assert_eq!(got.source, ClassSource::Other);
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn update_and_delete_reject_unknown_ids() {
        let dir = ClassDirectory::new(None).unwrap();
        let unknown = ClassId("nope".into());
        assert!(
            dir.update(
                &unknown,
                &UpdateClassRequest {
                    name: Some("X".into()),
                    ..Default::default()
                },
            )
            .is_err()
        );
        assert!(dir.delete(&unknown).is_err());
    }

    #[test]
    fn delete_removes_a_created_class() {
        let dir = ClassDirectory::new(None).unwrap();
        let created = dir.create(&req("Temp")).unwrap();
        assert!(dir.exists(&created.id));
        dir.delete(&created.id).unwrap();
        assert!(!dir.exists(&created.id));
        assert!(dir.delete(&created.id).is_err());
    }

    #[test]
    fn classes_persist_across_a_restart_with_a_data_dir() {
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-classes-test-{}", short_suffix()));
        {
            let dir = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let created = dir
                .create(&CreateClassRequest {
                    name: "Persisted".to_string(),
                    source: ClassSource::MultiGP,
                    reference: Some("mgp-7".to_string()),
                    description: Some("the persisted class".to_string()),
                })
                .unwrap();

            let reopened = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.name, "Persisted");
            assert_eq!(got.source, ClassSource::MultiGP);
            assert_eq!(got.reference.as_deref(), Some("mgp-7"));
            assert_eq!(got.description.as_deref(), Some("the persisted class"));
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn a_corrupt_classes_file_degrades_to_just_the_built_ins() {
        let data_dir = std::env::temp_dir().join(format!("gridfpv-classes-bad-{}", short_suffix()));
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(classes_path(&data_dir), b"not json at all").unwrap();
        let dir = ClassDirectory::new(Some(data_dir.clone())).unwrap();
        // The built-ins are still seeded; only user classes are missing.
        assert_eq!(dir.list().len(), BUILTIN_COUNT);
        assert!(user_classes(&dir).is_empty());
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn built_ins_are_not_written_to_classes_json_only_user_classes_are() {
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-classes-persist-{}", short_suffix()));
        {
            let dir = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            // A user/Custom class is full CRUD + persists…
            let created = dir
                .create(&CreateClassRequest {
                    name: "House Spec".to_string(),
                    ..Default::default()
                })
                .unwrap();
            assert!(!created.builtin);
            assert_eq!(created.source, ClassSource::Custom);

            // …but the on-disk file holds ONLY the user class — never a built-in.
            let raw = std::fs::read_to_string(classes_path(&data_dir)).unwrap();
            let on_disk: Vec<Class> = serde_json::from_str(&raw).unwrap();
            assert_eq!(on_disk.len(), 1, "classes.json holds only the user class");
            assert_eq!(on_disk[0].id, created.id);
            assert!(!on_disk[0].builtin);
            // None of the fixed built-in ids leaked to disk.
            for spec in BUILTIN_CLASSES {
                assert!(
                    !raw.contains(&format!("\"{}\"", spec.id)),
                    "built-in id {} must not be persisted",
                    spec.id
                );
            }

            // A reopen re-seeds the built-ins and restores the one user class.
            let reopened = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            assert_eq!(reopened.list().len(), BUILTIN_COUNT + 1);
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.name, "House Spec");
            assert!(!got.builtin);
            // The built-ins are still all present.
            assert!(reopened.get(&ClassId("mgp-open".into())).unwrap().builtin);
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }
}
