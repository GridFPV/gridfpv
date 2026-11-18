//! Classes as **application-level configuration** — the `ClassDirectory` and `Class`.
//!
//! A class is a *racing category in the Director's address book*: a name and a little optional
//! metadata (where it came from, a reference id, a description). Like the pilot directory
//! ([`PilotDirectory`](crate::pilots::PilotDirectory)), the Race Director maintains their classes
//! **once** at the application level (a persisted directory), outside any event.
//!
//! The [`ClassDirectory`] holds every configured [`Class`] behind a lock and **persists** the
//! user/Custom ones to `<GRIDFPV_DATA_DIR>/classes.json` (restored on boot; in-memory only when
//! no data dir is configured). On top of those it always seeds **9 code-defined built-in
//! classes** — the standard FPV classes (MultiGP / Five33 / FreedomSpec / Street League / UDL) —
//! with **fixed ids**, identical on every Director, so cross-event / cross-Director standings
//! aggregate with zero reconciliation. The built-ins are **read-only** (non-editable,
//! non-deletable) and are **never** written to `classes.json` (re-seeded on every boot). Users
//! create classes of their own (full CRUD), with an org tag if they type one.
//!
//! This is the directory of *which classes exist*. Which classes run at an event, and who races
//! in each, belongs to the event and is built on the primitives (`docs/primitives.md` §5.3).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::pilots::OptionalEdit;
use gridfpv_events::ClassId;

/// The file name (under the data dir) the class directory is persisted to (issue #84).
pub const CLASSES_FILE: &str = "classes.json";

/// The file name (under the data dir) the **hidden-set** is persisted to (hide/archive classes).
///
/// A small sidecar holding just the [`ClassId`]s the RD has hidden from the per-event picker. It is
/// kept *separate* from `classes.json` deliberately: the built-in classes are re-seeded on every
/// boot and never written to `classes.json`, so a `hidden` flag stored *on* a class would be lost
/// on restart. Persisting the hidden ids on their own — and applying them **after** the re-seed —
/// makes a hidden built-in (or custom class) survive a Director restart.
pub const HIDDEN_CLASSES_FILE: &str = "hidden_classes.json";

/// The file name (under the data dir) the **removed built-ins** are kept in.
///
/// Grid ships with the built-in classes, and a director may remove the ones their club never
/// flies. The built-ins are seeded on every start and never written to `classes.json`, so the
/// ones that were removed are remembered here and left out of the seed. Putting the built-ins
/// back (`POST /classes/builtins`) empties it.
pub const REMOVED_CLASSES_FILE: &str = "removed_classes.json";

/// The most characters an org name may have. It is shown as a small tag beside the class.
pub const ORG_MAX: usize = 24;

/// One class in the application-level directory (issue #84).
///
/// The wire shape `GET /classes` returns and the on-disk shape `classes.json` persists: a stable
/// [`ClassId`] (auto-generated, never user-entered), a required `name`, and a little optional
/// metadata, the org among it. The optional fields are omitted from the wire when
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
    /// The **org** whose class this is, such as `"MultiGP"` or a club's own name. Free text,
    /// shown as a tag beside the class. A built-in carries the org that defines it; the
    /// director types one, or none, for a class of their own. Omitted when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub org: Option<String>,
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
    /// re-seeded on every boot). A class the director made is never a built-in. Defaults to `false` and is omitted from the wire / disk when false, so a custom
    /// class's JSON is unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub builtin: bool,
    /// Whether this class is **hidden / archived** from the per-event class picker (hide/archive
    /// classes). A pure **visibility preference**, not an edit: the RD can hide a class they don't
    /// use (especially a built-in) so it stops cluttering the per-event picker, while it stays in
    /// the directory and the main Classes view (where it can be un-hidden). Because built-ins are
    /// re-seeded on every boot, this flag is **not** stored on the class itself — it is derived when
    /// `GET /classes` is built from a persisted set of hidden ids (see [`HIDDEN_CLASSES_FILE`]), so
    /// a hidden built-in survives a restart. Defaults to `false` and is omitted from the wire / disk
    /// when false, so a class's persisted JSON is unchanged (and the hidden state never lands in
    /// `classes.json`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
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
    /// The org that defines this class (shown as a tag).
    org: &'static str,
    /// A reference URL into the org's rules.
    reference: &'static str,
    /// A short canonical description.
    description: &'static str,
}

/// The 9 standard FPV racing classes seeded into **every** directory with **fixed ids** (issue
/// #84). Present on every Director identically so cross-event / cross-Director season standings
/// aggregate without reconciliation; carry their org as [`org`](Class::org) (a tag);
/// are read-only (non-editable, non-deletable); and are **not** persisted to `classes.json`.
const BUILTIN_CLASSES: &[BuiltinClass] = &[
    BuiltinClass {
        id: "mgp-open",
        name: "Open Class",
        org: "MultiGP",
        reference: "https://www.multigp.com/class-specifications/",
        description: "Open/unlimited 5–6\" class — no motor/prop/electronics limits.",
    },
    BuiltinClass {
        id: "mgp-pro-spec",
        name: "Pro Spec",
        org: "MultiGP",
        reference: "https://www.multigp.com/prospec/",
        description: "MultiGP's official 7\" spec class (Pro Spec frame, spec motor).",
    },
    BuiltinClass {
        id: "mgp-whoop",
        name: "Whoop Class",
        org: "MultiGP",
        reference: "https://www.multigp.com/class-specifications/",
        description: "1S indoor micro whoop (65mm ducted).",
    },
    BuiltinClass {
        id: "mgp-micro",
        name: "Micro Class",
        org: "MultiGP",
        reference: "https://www.multigp.com/class-specifications/microclass/",
        description: "3\" micro class (1404 motor, 3S).",
    },
    BuiltinClass {
        id: "five33-tiny-trainer",
        name: "Tiny Trainer",
        org: "Five33",
        reference: "https://flyfive33.com/pages/tt-spec",
        description: "Five33 3\" spec (Tiny Trainer frame, 1404, 3S).",
    },
    BuiltinClass {
        id: "freedom-spec",
        name: "Freedom Spec",
        org: "Freedom Spec",
        reference: "https://freedomspec.com/",
        description: "Open 5\" spec class with a standardized motor-RPM cap.",
    },
    BuiltinClass {
        id: "street-league",
        name: "Street League",
        org: "Street League",
        reference: "https://streetleague.io/spec",
        description: "7\" spec on firmware that equalizes motors via an RPM limiter.",
    },
    BuiltinClass {
        id: "udl-igniter",
        name: "Igniter",
        org: "UDL",
        reference: "https://undergrounddroneleague.com/igniter-legal-parts",
        description: "Indoor 1S 75mm-ducted spec, approved-parts-only, RPM-limited.",
    },
    BuiltinClass {
        id: "udl-shrieker",
        name: "Shrieker",
        org: "UDL",
        reference: "https://undergrounddroneleague.com/drone-types",
        description: "Fast 1S 65mm micro, near-open ruleset.",
    },
];

/// Build the 9 built-in [`Class`]es (issue #84) — the canonical, fixed-id, read-only classes seeded
/// into every directory. Each carries `builtin = true` and its org.
fn builtin_classes() -> Vec<Class> {
    BUILTIN_CLASSES
        .iter()
        .map(|b| Class {
            id: ClassId(b.id.to_string()),
            name: b.name.to_string(),
            org: Some(b.org.to_string()),
            reference: Some(b.reference.to_string()),
            description: Some(b.description.to_string()),
            builtin: true,
            // Visibility is layered on later from the persisted hidden-set, never seeded here.
            hidden: false,
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
    /// Optional org, stored on [`Class::org`]. At most [`ORG_MAX`] characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub org: Option<String>,
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
/// cleared). Each optional-metadata field is a
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
    /// A new org (set / clear / leave-unchanged, like [`reference`](Self::reference)).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub org: OptionalEdit<String>,
    /// A new reference id: present value → set, present `null` → clear, absent → leave unchanged.
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub reference: OptionalEdit<String>,
    /// A new description (set / clear / leave-unchanged, like [`reference`](Self::reference)).
    #[serde(default, skip_serializing_if = "OptionalEdit::is_keep")]
    #[ts(optional = nullable)]
    pub description: OptionalEdit<String>,
}

/// The body of `PUT /classes/{id}/hidden` — the new visibility for a class (hide/archive classes).
///
/// A one-field body: `hidden: true` tucks the class away from the per-event picker, `false` brings
/// it back. Applies to **built-in** and custom classes alike (hiding is a visibility preference, not
/// an edit), so it is valid even on a read-only built-in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetClassHiddenRequest {
    /// The desired visibility: `true` → hidden (archived from the picker), `false` → visible.
    pub hidden: bool,
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

/// The guarded interior: the class map, the hidden-set, and where the JSON files live.
struct Directory {
    /// `ClassId → Class`. A `BTreeMap` so listing is deterministic (id order). The stored `Class`
    /// values always carry `hidden = false`; visibility is layered on from [`hidden`](Self::hidden)
    /// when a `Class` is *served* (see [`Directory::view`]), so the hidden state never has to live
    /// on the class (and never leaks into `classes.json`).
    classes: BTreeMap<ClassId, Class>,
    /// The set of **hidden** class ids (hide/archive classes) — built-in or custom. Persisted to
    /// `<data_dir>/hidden_classes.json` and applied **after** the boot re-seed, so a hidden built-in
    /// survives a restart. An id in here that no longer names a class is harmless (ignored on read).
    hidden: BTreeSet<ClassId>,
    /// The built-in classes the director has removed. Kept in `removed_classes.json` and left
    /// out when the built-ins are seeded, so a removed one stays removed across a restart.
    removed: BTreeSet<ClassId>,
    /// Directory `classes.json` / `hidden_classes.json` are persisted under; `None` ⇒ in-memory
    /// only (no data dir).
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

        // The built-ins the director has removed. A bad or missing file means none.
        let removed = data_dir
            .as_deref()
            .and_then(read_removed_classes)
            .unwrap_or_default();

        // Seed the code-defined built-ins first (they are never read from disk), but for the
        // ones that were removed.
        for class in builtin_classes() {
            if !removed.contains(&class.id) {
                classes.insert(class.id.clone(), class);
            }
        }

        let mut hidden = BTreeSet::new();
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
                    // Hidden state lives in the sidecar, never on the class — clear any stray flag.
                    class.hidden = false;
                    classes.insert(class.id.clone(), class);
                }
            }
            // Restore the hidden-set **after** the re-seed so a hidden built-in survives a restart.
            // (A bad/missing file degrades to "nothing hidden" rather than failing to boot.)
            if let Some(ids) = read_hidden_classes(dir) {
                hidden = ids;
            }
        }

        Ok(Self {
            inner: Arc::new(RwLock::new(Directory {
                classes,
                hidden,
                removed,
                data_dir,
            })),
        })
    }

    /// Every class in id order — the `GET /classes` body. Each served [`Class`] carries its
    /// `hidden` flag derived from the persisted hidden-set (hide/archive classes), so the frontend
    /// can mark hidden classes in the main view and filter them out of the per-event picker.
    pub fn list(&self) -> Vec<Class> {
        let dir = self.read();
        dir.classes.values().map(|c| dir.view(c)).collect()
    }

    /// Whether a class with `id` exists. (A *hidden* class still exists; hiding only affects
    /// what a picker *offers*, never the validity of the id.)
    pub fn exists(&self, id: &ClassId) -> bool {
        self.read().classes.contains_key(id)
    }

    /// The [`Class`] for `id` (with its `hidden` flag applied), or `None`.
    pub fn get(&self, id: &ClassId) -> Option<Class> {
        let dir = self.read();
        dir.classes.get(id).map(|c| dir.view(c))
    }

    /// Hide or un-hide a class (hide/archive classes): add or remove `id` from the persisted
    /// hidden-set and return the updated [`Class`] (with its fresh `hidden` flag).
    ///
    /// Hiding is a **visibility preference**, not an edit — so it is allowed for **built-in**
    /// classes too (it is *not* a [`ReadOnly`](ClassErrorKind::ReadOnly) violation): the whole
    /// point is to let the RD tuck away the standard built-ins they don't run. An unknown id is a
    /// 404 ([`NotFound`](ClassErrorKind::NotFound)). The hidden-set is **persisted** on success, so
    /// the choice survives a restart — including the boot re-seed of the built-ins.
    pub fn set_hidden(&self, id: &ClassId, hidden: bool) -> Result<Class, ClassError> {
        let mut dir = self.write();
        if !dir.classes.contains_key(id) {
            return Err(ClassError::not_found(format!(
                "no class with id {:?}",
                id.0
            )));
        }
        let changed = if hidden {
            dir.hidden.insert(id.clone())
        } else {
            dir.hidden.remove(id)
        };
        if changed {
            dir.persist_hidden()?;
        }
        let class = dir.classes.get(id).expect("checked above");
        Ok(dir.view(class))
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
        let org = normalize_optional(&request.org);
        check_org(org.as_deref())?;
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
            org,
            reference: normalize_optional(&request.reference),
            description: normalize_optional(&request.description),
            builtin: false,
            // A freshly-created class is always visible; the RD hides it later if they choose.
            hidden: false,
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
        if let OptionalEdit::Set(org) = &request.org {
            check_org(Some(org.trim()))?;
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
        apply_string_edit(&mut class.org, &request.org);
        apply_string_edit(&mut class.reference, &request.reference);
        apply_string_edit(&mut class.description, &request.description);
        let updated = class.clone();
        dir.persist()?;
        // Re-apply the served `hidden` flag (an edit never changes visibility).
        Ok(dir.view(&updated))
    }

    /// Remove a class. An unknown id is a [`ClassError`]. The directory is **persisted** on
    /// success.
    ///
    /// A **built-in** class can be removed too: Grid ships with them, and a director drops the
    /// ones their club never flies. It still cannot be edited. A removed built-in is remembered
    /// so it does not come back at the next start; [`restore_builtins`](Self::restore_builtins)
    /// puts them all back.
    pub fn delete(&self, id: &ClassId) -> Result<(), ClassError> {
        let mut dir = self.write();
        if dir.classes.remove(id).is_none() {
            return Err(ClassError::not_found(format!(
                "no class with id {:?}",
                id.0
            )));
        }
        if is_builtin_id(id) {
            dir.removed.insert(id.clone());
            dir.persist_removed()?;
        } else {
            dir.persist()?;
        }
        // A deleted class drops out of the hidden-set too (so a later recreate isn't silently
        // hidden by a stale id, and the sidecar doesn't accumulate dangling ids).
        if dir.hidden.remove(id) {
            dir.persist_hidden()?;
        }
        Ok(())
    }

    /// How many built-in classes the director has removed.
    pub fn removed_builtins(&self) -> usize {
        self.read().removed.len()
    }

    /// Put back every built-in class that was removed. Answers the whole list, as
    /// [`list`](Self::list) does. With none removed it changes nothing.
    pub fn restore_builtins(&self) -> Result<Vec<Class>, ClassError> {
        {
            let mut dir = self.write();
            if !dir.removed.is_empty() {
                for class in builtin_classes() {
                    dir.classes.entry(class.id.clone()).or_insert(class);
                }
                dir.removed.clear();
                dir.persist_removed()?;
            }
        }
        Ok(self.list())
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Directory> {
        self.inner.read().expect("class directory lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Directory> {
        self.inner.write().expect("class directory lock poisoned")
    }
}

impl Directory {
    /// A served view of `class`: the stored class with its `hidden` flag set from the hidden-set
    /// (hide/archive classes). The stored `Class` always has `hidden = false`; this is the one place
    /// visibility is layered on, so `GET /classes` / `get` / `set_hidden` all report it consistently
    /// and the hidden state never has to live on the stored class.
    fn view(&self, class: &Class) -> Class {
        let mut out = class.clone();
        out.hidden = self.hidden.contains(&class.id);
        out
    }

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

    /// Keep the removed built-ins in `<data_dir>/removed_classes.json`. A no-op with no data dir.
    fn persist_removed(&self) -> Result<(), ClassError> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let ids: Vec<&ClassId> = self.removed.iter().collect();
        let json = serde_json::to_string_pretty(&ids).map_err(|e| {
            ClassError::internal(format!("could not serialize removed classes: {e}"))
        })?;
        std::fs::write(dir.join(REMOVED_CLASSES_FILE), json)
            .map_err(|e| ClassError::internal(format!("could not persist removed classes: {e}")))
    }

    /// Persist the **hidden-set** to `<data_dir>/hidden_classes.json` (hide/archive classes), a
    /// no-op with no data dir. Written as a plain JSON array of the hidden [`ClassId`]s — a tiny
    /// sidecar kept separate from `classes.json` so it can record hidden *built-in* ids (which never
    /// go in `classes.json`) and be applied **after** the boot re-seed.
    fn persist_hidden(&self) -> Result<(), ClassError> {
        let Some(dir) = &self.data_dir else {
            return Ok(());
        };
        let ids: Vec<&ClassId> = self.hidden.iter().collect();
        let json = serde_json::to_string_pretty(&ids).map_err(|e| {
            ClassError::internal(format!("could not serialize hidden classes: {e}"))
        })?;
        std::fs::write(hidden_classes_path(dir), json)
            .map_err(|e| ClassError::internal(format!("could not persist hidden classes: {e}")))
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

/// Read the removed built-ins from `<dir>/removed_classes.json`. Only ids of built-ins count.
fn read_removed_classes(dir: &Path) -> Option<BTreeSet<ClassId>> {
    let raw = std::fs::read_to_string(dir.join(REMOVED_CLASSES_FILE)).ok()?;
    let ids: BTreeSet<ClassId> = serde_json::from_str(&raw).ok()?;
    Some(ids.into_iter().filter(is_builtin_id).collect())
}

/// The file the hidden-set is persisted to under `dir`: `<dir>/hidden_classes.json`.
fn hidden_classes_path(dir: &Path) -> PathBuf {
    dir.join(HIDDEN_CLASSES_FILE)
}

/// Read the persisted hidden-set from `<dir>/hidden_classes.json`, or `None` if
/// absent/unreadable/corrupt (degrades to "nothing hidden" so the Director still boots).
fn read_hidden_classes(dir: &Path) -> Option<BTreeSet<ClassId>> {
    let raw = std::fs::read_to_string(hidden_classes_path(dir)).ok()?;
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
    /// An attempt to edit/delete a read-only **built-in** class → 400 (rejected).
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

/// Refuse an org name too long to be a tag.
fn check_org(org: Option<&str>) -> Result<(), ClassError> {
    match org {
        Some(org) if org.chars().count() > ORG_MAX => Err(ClassError::invalid(format!(
            "An org name can have {ORG_MAX} characters at most."
        ))),
        _ => Ok(()),
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
            assert_eq!(got.org.as_deref(), Some(spec.org));
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
        // The orgs are the expected spread, spelled as they are shown.
        let org = |id: &str| dir.get(&ClassId(id.into())).unwrap().org;
        assert_eq!(org("mgp-open").as_deref(), Some("MultiGP"));
        assert_eq!(org("five33-tiny-trainer").as_deref(), Some("Five33"));
        assert_eq!(org("freedom-spec").as_deref(), Some("Freedom Spec"));
        assert_eq!(org("street-league").as_deref(), Some("Street League"));
        assert_eq!(org("udl-igniter").as_deref(), Some("UDL"));
    }

    #[test]
    fn a_built_in_cannot_be_edited() {
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
    }

    fn removal_dir(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("gridfpv-classes-{name}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_built_in_can_be_removed_and_stays_removed_after_a_restart() {
        let data = removal_dir("removed");
        let open = ClassId("mgp-open".into());
        let whoop = ClassId("mgp-whoop".into());
        let dir = ClassDirectory::new(Some(data.clone())).unwrap();
        let all = dir.list().len();
        assert_eq!(dir.removed_builtins(), 0);
        dir.set_hidden(&open, true).unwrap();

        dir.delete(&open).unwrap();
        assert!(!dir.exists(&open));
        assert_eq!(dir.list().len(), all - 1);
        assert_eq!(dir.removed_builtins(), 1);
        // Removing it again is an unknown class, not a second removal.
        assert_eq!(
            dir.delete(&open).unwrap_err().kind,
            ClassErrorKind::NotFound
        );
        assert_eq!(dir.removed_builtins(), 1);
        // The other built-ins are untouched.
        assert!(dir.exists(&whoop));

        // The seed at the next start leaves it out.
        let again = ClassDirectory::new(Some(data.clone())).unwrap();
        assert!(!again.exists(&open));
        assert!(again.exists(&whoop));
        assert_eq!(again.removed_builtins(), 1);
        assert_eq!(again.list().len(), all - 1);
        std::fs::remove_dir_all(data).ok();
    }

    #[test]
    fn putting_the_built_ins_back_brings_every_removed_one_back_and_keeps_the_rest() {
        let data = removal_dir("restored");
        let dir = ClassDirectory::new(Some(data.clone())).unwrap();
        let all = dir.list();
        let mine = dir
            .create(&CreateClassRequest {
                name: "Club Spec".into(),
                ..Default::default()
            })
            .unwrap();
        let kept_hidden = ClassId("udl-igniter".into());
        dir.set_hidden(&kept_hidden, true).unwrap();
        for id in ["mgp-open", "mgp-whoop", "udl-shrieker"] {
            dir.delete(&ClassId(id.into())).unwrap();
        }
        assert_eq!(dir.removed_builtins(), 3);

        let back = dir.restore_builtins().unwrap();
        assert_eq!(back.len(), all.len() + 1);
        assert_eq!(dir.removed_builtins(), 0);
        // Each comes back as it shipped: built-in, not hidden, with its own name.
        let open = dir.get(&ClassId("mgp-open".into())).unwrap();
        assert!(open.builtin);
        assert!(!open.hidden);
        assert_eq!(open.name, "Open Class");
        // What the director made, and what they hid, is as it was.
        assert!(dir.exists(&mine.id));
        assert!(dir.get(&kept_hidden).unwrap().hidden);
        // Asked again with none removed, nothing changes.
        assert_eq!(dir.restore_builtins().unwrap().len(), all.len() + 1);

        // And they are back after a restart too.
        let again = ClassDirectory::new(Some(data.clone())).unwrap();
        assert_eq!(again.list().len(), all.len() + 1);
        assert_eq!(again.removed_builtins(), 0);
        std::fs::remove_dir_all(data).ok();
    }

    #[test]
    fn a_removed_list_that_is_broken_or_names_other_classes_removes_nothing_it_should_not() {
        let data = removal_dir("odd");
        let seeded = ClassDirectory::new(Some(data.clone())).unwrap();
        let mine = seeded
            .create(&CreateClassRequest {
                name: "Club Spec".into(),
                ..Default::default()
            })
            .unwrap();
        let all = seeded.list().len();
        for text in ["", "not json", "{}", "null", "[1,2]"] {
            std::fs::write(data.join(REMOVED_CLASSES_FILE), text).unwrap();
            let dir = ClassDirectory::new(Some(data.clone())).unwrap();
            assert_eq!(dir.list().len(), all, "{text}");
            assert_eq!(dir.removed_builtins(), 0, "{text}");
        }
        // A name that is not a built-in's is ignored: it removes no class of the director's.
        std::fs::write(
            data.join(REMOVED_CLASSES_FILE),
            serde_json::to_string(&[mine.id.0.as_str(), "no-such-class", "mgp-open"]).unwrap(),
        )
        .unwrap();
        let dir = ClassDirectory::new(Some(data.clone())).unwrap();
        assert!(dir.exists(&mine.id));
        assert!(!dir.exists(&ClassId("mgp-open".into())));
        assert_eq!(dir.removed_builtins(), 1);
        std::fs::remove_dir_all(data).ok();
    }

    #[test]
    fn a_class_made_with_no_org_has_none_and_says_nothing_of_it_on_the_wire() {
        let dir = ClassDirectory::new(None).unwrap();
        let class = dir.create(&req("Open")).unwrap();
        assert_eq!(class.org, None);
        let wire = serde_json::to_value(&class).unwrap();
        assert!(wire.get("org").is_none(), "{wire}");
        assert!(wire.get("source").is_none(), "{wire}");
    }

    #[test]
    fn an_org_is_free_text_trimmed_and_kept_short() {
        let dir = ClassDirectory::new(None).unwrap();
        let with = |org: &str| CreateClassRequest {
            name: "Club Spec".into(),
            org: Some(org.into()),
            ..Default::default()
        };
        // Anything the director types, tidied of the spaces around it.
        assert_eq!(
            dir.create(&with("  Tri-County FPV  "))
                .unwrap()
                .org
                .as_deref(),
            Some("Tri-County FPV")
        );
        // Blank is none.
        assert_eq!(dir.create(&with("   ")).unwrap().org, None);
        // As long as it may be, and no longer.
        let longest = "x".repeat(ORG_MAX);
        assert_eq!(
            dir.create(&with(&longest)).unwrap().org.as_deref(),
            Some(longest.as_str())
        );
        let before = dir.list().len();
        let err = dir.create(&with(&"x".repeat(ORG_MAX + 1))).unwrap_err();
        assert_eq!(err.kind, ClassErrorKind::Invalid);
        assert!(err.message.contains("24 characters"), "{}", err.message);
        assert_eq!(dir.list().len(), before, "a refused class is not made");

        // An edit can set it, change it, clear it, and leave it alone.
        let class = dir.create(&req("House")).unwrap();
        let edit = |body: &str| dir.update(&class.id, &serde_json::from_str(body).unwrap());
        assert_eq!(
            edit(r#"{"org":" MultiGP "}"#).unwrap().org.as_deref(),
            Some("MultiGP")
        );
        assert_eq!(
            edit(r#"{"name":"House 2"}"#).unwrap().org.as_deref(),
            Some("MultiGP")
        );
        assert_eq!(
            edit(r#"{"org":"Mine"}"#).unwrap().org.as_deref(),
            Some("Mine")
        );
        let too_long = format!(r#"{{"org":"{}"}}"#, "y".repeat(ORG_MAX + 1));
        assert_eq!(edit(&too_long).unwrap_err().kind, ClassErrorKind::Invalid);
        assert_eq!(dir.get(&class.id).unwrap().org.as_deref(), Some("Mine"));
        assert_eq!(edit(r#"{"org":null}"#).unwrap().org, None);
    }

    #[test]
    fn a_class_saved_by_an_older_grid_with_a_source_still_loads() {
        let data = removal_dir("old-source");
        std::fs::write(
            data.join(CLASSES_FILE),
            r#"[{"id":"house-ab12","name":"House","source":"Custom"},
                {"id":"club-cd34","name":"Club","source":"MultiGP","reference":"x"}]"#,
        )
        .unwrap();
        let dir = ClassDirectory::new(Some(data.clone())).unwrap();
        let house = dir.get(&ClassId("house-ab12".into())).unwrap();
        assert_eq!(house.name, "House");
        assert_eq!(house.org, None);
        // The old tag is not carried over; the class and the rest of it are.
        let club = dir.get(&ClassId("club-cd34".into())).unwrap();
        assert_eq!(club.org, None);
        assert_eq!(club.reference.as_deref(), Some("x"));
        std::fs::remove_dir_all(data).ok();
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
                org: Some("MultiGP".to_string()),
                reference: Some("  mgp-open  ".to_string()), // trimmed
                description: Some("  ".to_string()),         // blank → unset
            })
            .unwrap();
        assert_eq!(class.org.as_deref(), Some("MultiGP"));
        assert_eq!(class.reference.as_deref(), Some("mgp-open"));
        assert_eq!(class.description, None);
    }

    #[test]
    fn update_edits_name_source_and_optional_fields_with_clear_semantics() {
        let dir = ClassDirectory::new(None).unwrap();
        let created = dir
            .create(&CreateClassRequest {
                name: "Old".to_string(),
                org: None,
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
                    org: OptionalEdit::Set("MultiGP".to_string()),
                    reference: OptionalEdit::Clear, // explicit clear
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.name, "New");
        assert_eq!(updated.org.as_deref(), Some("MultiGP"));
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
                    org: Some("Other".to_string()),
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
            assert_eq!(cleared.org.as_deref(), Some("Other")); // absent → unchanged

            // The clear survives a restart (it was persisted to classes.json, not just in mem).
            let reopened = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.reference, None);
            assert_eq!(got.description, None);
            assert_eq!(got.org.as_deref(), Some("Other"));
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
                    org: Some("MultiGP".to_string()),
                    reference: Some("mgp-7".to_string()),
                    description: Some("the persisted class".to_string()),
                })
                .unwrap();

            let reopened = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let got = reopened.get(&created.id).unwrap();
            assert_eq!(got.name, "Persisted");
            assert_eq!(got.org.as_deref(), Some("MultiGP"));
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
            assert_eq!(created.org, None);

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

    // ── hide / archive classes ──────────────────────────────────────────────

    #[test]
    fn classes_default_to_visible_and_set_hidden_toggles_the_flag() {
        let dir = ClassDirectory::new(None).unwrap();
        let created = dir.create(&req("House Spec")).unwrap();
        // Fresh classes (and built-ins) are visible by default.
        assert!(!created.hidden);
        assert!(!dir.get(&ClassId("mgp-open".into())).unwrap().hidden);

        // Hide it, then un-hide it — the served flag and the list both reflect the change.
        let hidden = dir.set_hidden(&created.id, true).unwrap();
        assert!(hidden.hidden);
        assert!(dir.get(&created.id).unwrap().hidden);
        assert!(
            dir.list()
                .iter()
                .find(|c| c.id == created.id)
                .unwrap()
                .hidden
        );

        let shown = dir.set_hidden(&created.id, false).unwrap();
        assert!(!shown.hidden);
        assert!(!dir.get(&created.id).unwrap().hidden);
    }

    #[test]
    fn hiding_a_class_is_not_a_read_only_edit_so_built_ins_can_be_hidden() {
        let dir = ClassDirectory::new(None).unwrap();
        let id = ClassId("mgp-open".into());
        // A built-in cannot be edited/deleted (ReadOnly), but it CAN be hidden — visibility is a
        // preference, not an edit.
        let hidden = dir.set_hidden(&id, true).unwrap();
        assert!(hidden.hidden);
        assert!(hidden.builtin, "still a built-in, just hidden");
        assert!(dir.get(&id).unwrap().hidden);
        // Editing it is still rejected.
        assert_eq!(
            dir.update(
                &id,
                &UpdateClassRequest {
                    name: Some("X".into()),
                    ..Default::default()
                }
            )
            .unwrap_err()
            .kind,
            ClassErrorKind::ReadOnly
        );
    }

    #[test]
    fn set_hidden_rejects_an_unknown_id() {
        let dir = ClassDirectory::new(None).unwrap();
        let err = dir.set_hidden(&ClassId("nope".into()), true).unwrap_err();
        assert_eq!(err.kind, ClassErrorKind::NotFound);
    }

    #[test]
    fn a_hidden_built_in_persists_across_a_restart_surviving_the_reseed() {
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-classes-hide-{}", short_suffix()));
        {
            let dir = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let builtin = ClassId("mgp-open".into());
            let custom = dir.create(&req("House Spec")).unwrap();
            dir.set_hidden(&builtin, true).unwrap();
            dir.set_hidden(&custom.id, true).unwrap();

            // The sidecar holds the hidden ids; classes.json never carries the built-in id or a
            // `hidden` flag.
            let hidden_raw = std::fs::read_to_string(hidden_classes_path(&data_dir)).unwrap();
            assert!(hidden_raw.contains("mgp-open"));
            let classes_raw = std::fs::read_to_string(classes_path(&data_dir)).unwrap();
            assert!(
                !classes_raw.contains("mgp-open"),
                "built-in id must not leak into classes.json"
            );
            assert!(
                !classes_raw.contains("hidden"),
                "hidden state must not be persisted on the class in classes.json"
            );

            // Reopen: the built-ins are re-seeded, yet the hidden built-in (and custom) stay hidden.
            let reopened = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let bi = reopened.get(&builtin).unwrap();
            assert!(bi.builtin, "re-seeded built-in present");
            assert!(bi.hidden, "hidden built-in survives the re-seed");
            assert!(reopened.get(&custom.id).unwrap().hidden);

            // Un-hide the built-in and confirm it sticks across another restart.
            reopened.set_hidden(&builtin, false).unwrap();
            let again = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            assert!(!again.get(&builtin).unwrap().hidden);
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }

    #[test]
    fn deleting_a_class_drops_it_from_the_hidden_set() {
        let data_dir =
            std::env::temp_dir().join(format!("gridfpv-classes-hide-del-{}", short_suffix()));
        {
            let dir = ClassDirectory::new(Some(data_dir.clone())).unwrap();
            let created = dir.create(&req("Temp")).unwrap();
            dir.set_hidden(&created.id, true).unwrap();
            dir.delete(&created.id).unwrap();
            // The id is gone from the sidecar, so a same-named recreate is not silently hidden.
            let raw = std::fs::read_to_string(hidden_classes_path(&data_dir)).unwrap();
            assert!(!raw.contains(&created.id.0));
        }
        std::fs::remove_dir_all(&data_dir).ok();
    }
}
