//! Events as **first-class containers** — the `EventRegistry` and `EventMeta` (issue #72).
//!
//! An **event is the container** for a whole fact log: its heats, registrations, passes,
//! and marshaling adjudications all live in *one* event's append-only log, distinct from
//! every other event's. The flat single-log model (one Director = one event) is replaced
//! by a registry mapping each [`EventId`] to that event's own [`AppState`](crate::app::AppState)
//! — and therefore its own [`EventLog`]. Every read/realtime/control surface is rooted under
//! the event (`/events/{eventId}/…`); the registry resolves the id to the log that surface
//! serves (see [`crate::app::events_router`]).
//!
//! # Two physical realizations of one logical model
//!
//! The model is **backend-agnostic** — the registry stores an `AppState` over *any*
//! [`EventLog`] backend, so the same logical "an event owns a dense, per-event log" holds
//! across both realizations:
//!
//! - **Local (now):** each persistent event is its **own SQLite file** (one `log` table per
//!   file, dense offsets from 0 — exactly the existing [`SqliteLog`](gridfpv_storage::SqliteLog)
//!   schema). The built-in **Practice** event is an **in-memory** log
//!   ([`InMemoryLog`](gridfpv_storage::InMemoryLog)), non-persistent by design.
//! - **Cloud (v0.7 — NOT built here, kept compatible):** one Postgres DB with an `events`
//!   table and a shared `event_log` table keyed by `event_id` with a **composite primary key
//!   `(event_id, offset)`**, so each event's offset sequence stays **per-event dense** —
//!   identical to "one SQLite file per event" today. Reads are always event-scoped (the
//!   registry never serves across events), so the Postgres backend slots behind the same
//!   `EventLog` trait: an event-scoped log handle is a `WHERE event_id = ?` view whose
//!   `offset` column is dense within that event. No surface here reads the whole DB; every
//!   path resolves an event first, so the per-event-dense-offset invariant is the only thing
//!   the cloud mapping must preserve — and the composite PK gives it for free.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use gridfpv_engine::format::FormatRegistry;
use gridfpv_engine::scoring::WinCondition;
use gridfpv_events::RoundId;
use gridfpv_storage::{InMemoryLog, SqliteLog};

use crate::app::AppState;
use crate::auth::TokenStore;
use crate::classes::ClassDirectory;
use crate::pilots::PilotDirectory;
use crate::scope::{ClassId, EventId, PilotId};
use crate::timers::{MOCK_TIMER_ID, TimerId, TimerRegistry};

/// The reserved id of the always-present built-in **Practice** event.
///
/// Practice is seeded into every registry, backed by an in-memory (non-persistent) log:
/// the RD can run a sim race with nothing configured. Its id is reserved — [`EventRegistry::create`]
/// auto-generates ids and never collides with it.
pub const PRACTICE_EVENT_ID: &str = "practice";

/// The display name of the built-in Practice event.
pub const PRACTICE_EVENT_NAME: &str = "Practice";

/// The file name (under the data dir) the Director's active-event id is persisted to (issue
/// #90), so the selected event survives a Director restart.
pub const ACTIVE_EVENT_FILE: &str = "active-event";

/// The key, in an event's sidecar `meta` table, under which its [`EventMeta`] is persisted
/// as JSON (issue #111). Stored on create and re-written on every meta mutation
/// (`set_timers`/`set_primary_timer`/…) so a Director restart restores the latest config.
pub const EVENT_META_KEY: &str = "event_meta";

/// The file-name suffix (and the only files the boot-scan opens) of a persistent event's
/// SQLite log under the data dir: `<id>.sqlite` (issue #111).
const EVENT_DB_SUFFIX: &str = ".sqlite";

/// The metadata describing one event in the registry (issue #72).
///
/// The wire shape `GET /events` returns: a stable [`EventId`], a human display `name`, the
/// creation time, and whether the event is **persistent** (file-backed) or ephemeral (the
/// in-memory Practice event). Derives serde (its JSON *is* the wire form) and `ts_rs::TS`
/// so the frontend reads a generated `EventMeta` type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct EventMeta {
    /// The stable handle every per-event route is rooted under (`/events/{id}/…`).
    pub id: EventId,
    /// The human-readable display name (names are display-only; the id is authoritative).
    pub name: String,
    /// Creation time in **milliseconds since the Unix epoch** (a plain JSON number — bounded
    /// far below 2^53, rendered as a TS `number` not a `bigint`, matching every other integer
    /// on the wire). Practice is seeded at registry construction.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Whether the event's log is durable (a SQLite file) or ephemeral (the in-memory
    /// Practice log, `false`).
    pub persistent: bool,
    /// Optional **display date** of the event, as a free-form string (e.g. `"2026-06-20"` or
    /// `"Sat 20 Jun"`). A string, not an epoch — it is a *human label the RD types*, shown
    /// verbatim on the picker and (later) the context header; the authoritative machine
    /// timestamp is [`created_at`](Self::created_at). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    /// Optional venue / location label (e.g. `"Main field"`). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<String>,
    /// Optional free-text description / notes for the event. Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Optional organizer name (the running club / person). Omitted from the wire when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organizer: Option<String>,
    /// The application-level timers this event **selects** (issue #73) — the per-event reference
    /// into the app-level [`TimerRegistry`](crate::timers::TimerRegistry). Additive
    /// (`#[serde(default)]`) so an event persisted before #73 reads back with an empty list; new
    /// events and Practice default to `["mock"]` (the built-in Mock) so they work out of the
    /// box. The per-event source bridge runs the selected Sim timers; a selected RotorHazard is a
    /// reserved no-op stub (2b / #65).
    #[serde(default)]
    pub timers: Vec<TimerId>,
    /// The **primary** timer among the selection (issue #112) — redundant timers at one gate, one
    /// designated **primary** and the rest **alternates**. The per-event source bridge feeds **only
    /// the active source's** passes into the log (the primary while it's healthy; on a primary drop
    /// it fails over to the first healthy alternate; on primary recovery it switches back), so two
    /// timers at the same gate give redundancy without double-counting the same crossing.
    ///
    /// Additive (`#[serde(default)]`) so an event persisted before #112 reads back with `None`.
    /// When unset, the **first** selected timer is the effective primary (see
    /// [`EventMeta::effective_primary`]). Must name a timer that is in [`timers`](Self::timers); a
    /// primary not in the selection is ignored (the first selected timer is used instead).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub primary_timer: Option<TimerId>,
    /// The event's **roster** (issue #74) — the application-level [`Pilot`](crate::pilots::Pilot)s
    /// that race this event, by their [`PilotId`]. The per-event reference into the app-level
    /// [`PilotDirectory`](crate::pilots::PilotDirectory): a Director maintains their pilots once,
    /// and each event simply picks which of them race it (mirroring [`timers`](Self::timers)).
    ///
    /// Additive (`#[serde(default)]`) so an event persisted before #74 reads back with an empty
    /// roster; new events and Practice default to an **empty** roster. Channels (which frequency a
    /// roster pilot flies in a heat) are a separate concern (#117) and are not modelled here.
    #[serde(default)]
    pub roster: Vec<PilotId>,
    /// The application-level **classes** this event runs (issue #84) — the per-event reference into
    /// the app-level [`ClassDirectory`](crate::classes::ClassDirectory), by their [`ClassId`]. The
    /// per-event selection into the app-level class directory: a Director maintains their racing
    /// categories once, and each event simply picks which of them run at it (mirroring
    /// [`roster`](Self::roster) and [`timers`](Self::timers)).
    ///
    /// Additive (`#[serde(default)]`) so an event persisted before #84 reads back with an empty
    /// selection; new events and Practice default to an **empty** selection. This is the registry
    /// slice only — the rounds / phase engine a class later drives is a separate concern.
    #[serde(default)]
    pub classes: Vec<ClassId>,
    /// **Per-class membership** (race redesign Slice 1a) — which roster pilots race each
    /// [`class`](Self::classes). Each [`ClassMembership`] pairs one selected class with the
    /// [`PilotId`]s racing it; a roster pilot may be a member of several classes (or none).
    ///
    /// Distinct from the [`roster`](Self::roster) (who is *present at the event*) and from the
    /// [`classes`](Self::classes) selection (which categories *run at all*): membership is the
    /// finer join of the two — given the present pilots and the running classes, *who races
    /// which class*. Set per class through
    /// [`set_class_membership`](EventRegistry::set_class_membership).
    ///
    /// Additive (`#[serde(default)]`, omitted from the wire when empty) so an event persisted
    /// before Slice 1a reads back with no membership; new events and Practice default to an
    /// **empty** list. The whole field round-trips through the event's persisted meta (issue
    /// #115), so it is restart-safe for free.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub classes_membership: Vec<ClassMembership>,
    /// The event's **rounds** (race redesign Slice 2a) — the event-level, class-tagged, *dynamic*
    /// format-instances this event runs. A [`RoundDef`] scopes a format (a
    /// [`FormatRegistry`](gridfpv_engine::format::FormatRegistry) name) and its config to one or
    /// more eligible [`classes`](Self::classes), with a [`SeedingRule`] for how the field is drawn.
    /// Practice / qualifying rounds are added **as-you-go** through
    /// [`add_round`](EventRegistry::add_round); brackets (later slices) seed from a prior round's
    /// ranking via [`SeedingRule::FromRanking`].
    ///
    /// Additive (`#[serde(default)]`, omitted from the wire when empty) so an event persisted before
    /// Slice 2a reads back with no rounds; new events and Practice default to an **empty** list. The
    /// whole field round-trips through the event's persisted meta (issue #115), so it is restart-safe
    /// for free.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<RoundDef>,
}

/// One class's **membership** within an event (race redesign Slice 1a): the roster pilots that
/// race a single [`ClassId`].
///
/// Carried in [`EventMeta::classes_membership`] as a list, one entry per class with any members.
/// Derives serde (its JSON *is* the wire form) and `ts_rs::TS` so the frontend reads a generated
/// `ClassMembership` type for the per-class roster picker (the UI lands in Slice 1b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ClassMembership {
    /// The class these pilots race — one of the event's selected [`classes`](EventMeta::classes).
    pub class: ClassId,
    /// The roster pilots racing this class, in selection order, **each with an optional assigned
    /// channel** (race redesign Slice 7a). Each entry is a directory pilot that is also on the
    /// event's [`roster`](EventMeta::roster); its [`channel`](MemberSlot::channel) is the raw-MHz
    /// frequency the pilot flies in a *static*-channel-mode round (a fixed, per-membership channel,
    /// GQ-style).
    ///
    /// **Legacy-compatible:** older events persisted this as a bare `Vec<PilotId>`; the
    /// [`MemberSlot`] (de)serialises through a serde shim ([`member_slots`]) that accepts either
    /// shape — a plain pilot-id string (legacy) reads back as a [`MemberSlot`] with no channel — so
    /// pre-Slice-7a meta still loads and restart round-trips.
    #[serde(with = "member_slots")]
    #[ts(as = "Vec<MemberSlot>")]
    pub pilots: Vec<MemberSlot>,
}

/// One pilot's **slot** within a class's membership (race redesign Slice 7a): the directory pilot
/// plus the optional raw-MHz channel they fly in a *static*-channel-mode round.
///
/// The channel is the GQ-style **fixed, per-membership** assignment: in a [`ChannelMode::Static`]
/// round (time-trial / qualifying), every member flies their own channel and qual heats are
/// channel-balanced (one pilot per channel per heat). It is `None` until set, and is unused by a
/// [`ChannelMode::PerHeat`] round (the bracket path assigns channels per heat). Validated on set
/// against the event's **primary timer**'s `available_channels` — and that pool **can exceed** the
/// timer's `node_count`, so any channel in the pool is valid (node_count caps only pilots-per-heat).
///
/// Derives serde + `ts_rs::TS` so the frontend reads a generated `MemberSlot` for the Classes &
/// Roster channel picker (the UI is the Slice-7b follow-up).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct MemberSlot {
    /// The directory pilot racing this class.
    pub pilot: PilotId,
    /// The pilot's **fixed assigned channel** (raw MHz) for *static*-channel-mode rounds, or `None`
    /// when unassigned. Must be one of the event's **primary timer**'s `available_channels`
    /// (validated on set) — the pool may be larger than `node_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel: Option<u16>,
}

impl MemberSlot {
    /// A slot for `pilot` with no channel assigned yet (the legacy / freshly-added shape).
    pub fn new(pilot: PilotId) -> Self {
        Self {
            pilot,
            channel: None,
        }
    }
}

/// Serde shim letting [`ClassMembership::pilots`] (de)serialise as **either** the current
/// `Vec<MemberSlot>` *or* the legacy `Vec<PilotId>` (race redesign Slice 7a).
///
/// On read, each element is accepted as either a full `MemberSlot` object (`{ "pilot": …, "channel":
/// … }`) **or** a bare pilot-id string — a legacy `["acroace-1", …]` array loads as channel-less
/// slots, so pre-Slice-7a persisted meta round-trips. On write, the canonical `MemberSlot` form is
/// always emitted (a freshly-saved event is never legacy-shaped). Kept restart-safe for free since
/// it rides the existing meta JSON.
mod member_slots {
    use super::{MemberSlot, PilotId};
    use serde::Deserialize;
    use serde::de::{Deserializer, SeqAccess, Visitor};
    use serde::ser::Serializer;
    use std::fmt;

    /// One element of the legacy-or-current membership list: a bare pilot id (legacy) or a full slot.
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SlotOrId {
        /// The legacy shape — a bare pilot-id string; reads back as a channel-less slot.
        Id(PilotId),
        /// The current shape — a full `{ pilot, channel? }` object.
        Slot(MemberSlot),
    }

    impl From<SlotOrId> for MemberSlot {
        fn from(value: SlotOrId) -> Self {
            match value {
                SlotOrId::Id(pilot) => MemberSlot::new(pilot),
                SlotOrId::Slot(slot) => slot,
            }
        }
    }

    /// Always serialise the canonical `Vec<MemberSlot>` form.
    pub fn serialize<S: Serializer>(
        slots: &[MemberSlot],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(slots)
    }

    /// Deserialise a sequence whose elements are each a bare id (legacy) or a full slot.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<MemberSlot>, D::Error> {
        struct SlotsVisitor;

        impl<'de> Visitor<'de> for SlotsVisitor {
            type Value = Vec<MemberSlot>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a list of member slots or bare pilot ids")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut out = Vec::with_capacity(seq.size_hint().unwrap_or(0));
                while let Some(element) = seq.next_element::<SlotOrId>()? {
                    out.push(element.into());
                }
                Ok(out)
            }
        }

        deserializer.deserialize_seq(SlotsVisitor)
    }
}

/// One **round** within an event (race redesign Slice 2a): an event-level, class-tagged, *dynamic*
/// format-instance.
///
/// A round is a *format-instance* — a named, configured run of one
/// [`FormatRegistry`](gridfpv_engine::format::FormatRegistry) format, scoped to the eligible
/// [`classes`](Self::classes) it runs for, with a [`SeedingRule`] deciding how its field is drawn.
/// One eligible class is a **class round** (e.g. "Open Qualifying"); many/all classes is an
/// **open / practice** round. Rounds are added **as-you-go** (practice/quali) rather than
/// precomputed; later slices seed brackets from a prior round's ranking
/// ([`SeedingRule::FromRanking`]).
///
/// Carried in [`EventMeta::rounds`]. Derives serde (its JSON *is* the wire form) and `ts_rs::TS` so
/// the frontend reads a generated `RoundDef` type (the Rounds UI lands in Slice 2b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RoundDef {
    /// The stable, **auto-generated** handle for this round (a slug of the [`label`](Self::label)
    /// plus a short random suffix — mirroring the event/pilot id-gen). Never user-entered; the
    /// label is display-only. Referenced by a later round's [`SeedingRule::FromRanking`].
    pub id: RoundId,
    /// The human-readable label (e.g. `"Qualifying R1"`, `"Open Practice"`). Display-only; the
    /// [`id`](Self::id) is authoritative.
    pub label: String,
    /// The eligible [`classes`](EventMeta::classes) this round runs for. One class is a *class
    /// round*; many/all is an *open / practice* round. Each must be one of the event's selected
    /// classes.
    pub classes: Vec<ClassId>,
    /// The format this round runs — a known
    /// [`FormatRegistry`](gridfpv_engine::format::FormatRegistry) name (e.g. `"timed_qual"`,
    /// `"single_elim"`). Validated against [`FormatRegistry::standard`] on add/update.
    pub format: String,
    /// The format's config knobs (e.g. `rounds`, `advance`, `heat_size`), stored as-is as a
    /// string→string map — the same shape a `FormatConfig`'s params take. Stored verbatim with only
    /// light validation; format-specific interpretation is the engine's concern when the round runs
    /// (a later slice).
    pub params: BTreeMap<String, String>,
    /// How a heat in this round is won — the per-round scoring rule (the existing wire
    /// [`WinCondition`](gridfpv_engine::scoring::WinCondition)).
    pub win_condition: WinCondition,
    /// How this round's field is **seeded** (drawn). Defaults to [`SeedingRule::FromRoster`] (the
    /// eligible classes' membership, in roster order); a bracket round seeds from a prior round's
    /// ranking ([`SeedingRule::FromRanking`], consumed in a later slice).
    pub seeding: SeedingRule,
    /// How this round assigns **video channels** to its heats (race redesign Slice 7a). A
    /// [`ChannelMode::Static`] round (time-trial / qual, GQ-style) uses each member's *fixed*
    /// per-membership channel ([`MemberSlot::channel`]) and forms channel-balanced heats; a
    /// [`ChannelMode::PerHeat`] round (brackets) assigns channels per heat from the timer's pool
    /// (first-fit). Defaulted **by format** on create (`#[serde(default)]` so pre-Slice-7a meta
    /// reads back as [`ChannelMode::PerHeat`], the prior behaviour); RD-overridable.
    #[serde(default)]
    pub channel_mode: ChannelMode,
}

/// How a [`RoundDef`] assigns **video channels** to its heats (race redesign Slice 7a).
///
/// Two models, chosen by the round's competition shape:
///
/// - [`Static`](Self::Static) — **time-trial / qualifying** (GQ-style): every member has a *fixed*
///   channel assigned at membership ([`MemberSlot::channel`], drawn from the event's **primary
///   timer**'s `available_channels`). Heats are **channel-balanced** — one pilot per channel per
///   heat, ≤ `node_count` pilots per heat — so every member flies across the format's rounds.
/// - [`PerHeat`](Self::PerHeat) — **brackets**: the bracket decides matchups, so channels are
///   assigned **per heat** from the timer's pool (the existing first-fit allocation), each heat ≤
///   `node_count` pilots.
///
/// Defaulted by format on round-create (`timed_qual` / `round_robin` → [`Static`](Self::Static);
/// the elimination / multi-main formats → [`PerHeat`](Self::PerHeat)); the default `Default` impl is
/// [`PerHeat`](Self::PerHeat) so a round persisted before this field existed reads back with the
/// prior per-heat behaviour. Derives serde + `ts_rs::TS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum ChannelMode {
    /// Channel-balanced heats from each member's fixed, per-membership channel (time-trial / qual).
    Static,
    /// Per-heat channel assignment (brackets) — the default and the pre-Slice-7a behaviour.
    #[default]
    PerHeat,
}

impl ChannelMode {
    /// The **default channel mode for a format** (race redesign Slice 7a): the static, fixed-channel
    /// qualifying formats (`timed_qual`, `round_robin`) default to [`Static`](Self::Static); every
    /// other format (the elimination brackets, multi-main, zippyq) defaults to
    /// [`PerHeat`](Self::PerHeat). The RD can override the default per round.
    pub fn default_for_format(format: &str) -> Self {
        match format {
            "timed_qual" | "round_robin" => ChannelMode::Static,
            _ => ChannelMode::PerHeat,
        }
    }
}

/// How a [`RoundDef`]'s field is **seeded** (race redesign Slice 2a).
///
/// A round either draws its field straight from the eligible classes' roster membership
/// ([`FromRoster`](Self::FromRoster) — practice / first qualifying), or from a **prior round's
/// ranking** ([`FromRanking`](Self::FromRanking) — the bracket / cut case, the issue-#84 carry that
/// a later slice consumes). Derives serde + `ts_rs::TS`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum SeedingRule {
    /// Seed from the eligible classes' roster membership, in roster order. The default for
    /// practice and first-qualifying rounds.
    #[default]
    FromRoster,
    /// Seed from the **top-N** of a prior round's ranking — the bracket / cut seam (issue #84).
    /// The `source_round` must name another [`RoundDef`] in the same event's
    /// [`rounds`](EventMeta::rounds); `top_n` is how many advance. Stored now; *consumed* when the
    /// round actually runs (a later slice).
    FromRanking {
        /// The prior round this round seeds from — must exist in [`EventMeta::rounds`].
        source_round: RoundId,
        /// How many of the source ranking's top places advance into this round.
        top_n: usize,
    },
}

/// The body of `POST /events/{id}/rounds` — everything a caller supplies to add a round (race
/// redesign Slice 2a).
///
/// The **id is always auto-generated** (a slug of `label` plus a short random suffix), never
/// user-entered — mirroring the event/pilot create rule. The [`seeding`](Self::seeding) defaults to
/// [`SeedingRule::FromRoster`] when omitted. The route returns the created [`RoundDef`] (with its
/// generated [`id`](RoundDef::id)).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NewRoundReq {
    /// The display label for the new round (e.g. `"Qualifying R1"`).
    pub label: String,
    /// The eligible classes this round runs for. Each must be one of the event's selected classes.
    pub classes: Vec<ClassId>,
    /// The format this round runs — a known [`FormatRegistry`] name.
    pub format: String,
    /// The format's config knobs, stored verbatim.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// How a heat in this round is won.
    pub win_condition: WinCondition,
    /// How the round's field is seeded; defaults to [`SeedingRule::FromRoster`] when omitted.
    #[serde(default)]
    pub seeding: SeedingRule,
    /// How this round assigns channels (race redesign Slice 7a). Optional — **omit it** to take the
    /// format's default ([`ChannelMode::default_for_format`]); supply it to override (e.g. force a
    /// qual round per-heat). Additive on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel_mode: Option<ChannelMode>,
}

/// The body of `PUT /events/{id}/rounds/{round}` — the editable fields of an existing round (race
/// redesign Slice 2a). The round's [`id`](RoundDef::id) is the path segment and is **not** editable;
/// every other field is replaced wholesale. Same validation as the add path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct UpdateRoundReq {
    /// The new display label.
    pub label: String,
    /// The new eligible classes. Each must be one of the event's selected classes.
    pub classes: Vec<ClassId>,
    /// The new format — a known [`FormatRegistry`] name.
    pub format: String,
    /// The new format config knobs, stored verbatim.
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    /// The new win condition.
    pub win_condition: WinCondition,
    /// The new seeding rule; defaults to [`SeedingRule::FromRoster`] when omitted.
    #[serde(default)]
    pub seeding: SeedingRule,
    /// The new channel mode (race redesign Slice 7a). Optional — **omit it** to take the format's
    /// default ([`ChannelMode::default_for_format`]); supply it to override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel_mode: Option<ChannelMode>,
}

impl EventMeta {
    /// The event's **effective primary** timer (issue #112): the explicitly-set
    /// [`primary_timer`](Self::primary_timer) when it is present *and still in the selection*,
    /// else the **first** selected timer. `None` only when the event selects no timers at all.
    ///
    /// This is the single rule the source bridge and the API validation share so "the primary is
    /// the first selected timer unless overridden" holds everywhere. A stale `primary_timer` (it
    /// was deselected) gracefully degrades to the first selected timer rather than designating a
    /// timer the event no longer uses.
    pub fn effective_primary(&self) -> Option<TimerId> {
        match &self.primary_timer {
            Some(p) if self.timers.contains(p) => Some(p.clone()),
            _ => self.timers.first().cloned(),
        }
    }
}

/// The wire shape of `GET /active-event` — the **Director's currently-active event**, or
/// `null` when none is set (issue #90).
///
/// The active event is **Director (server-side) state**: there is exactly one Race Director
/// running one event, so the selected event lives on the Director, not in each browser. Every
/// client reads this on connect/reload to resume into the workspace (or fall to the picker when
/// `null`). The `event` field is the full [`EventMeta`] so a client renders the context header
/// without a second round-trip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ActiveEvent {
    /// The active event's metadata, or `null` when no event is active (→ the picker).
    pub event: Option<EventMeta>,
}

/// The body of `PUT /active-event` — the id of the event to make the Director's active one
/// (issue #90). The id must name a known event, else a typed 404 (`UnknownScope`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetActiveEventRequest {
    /// The event to make active.
    pub id: EventId,
}

/// The body of `PUT /events/{id}/roster` — the directory pilot ids that make up an event's
/// roster (issue #74). Each must name a pilot in the application-level
/// [`PilotDirectory`](crate::pilots::PilotDirectory), else a typed 404.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetEventRosterRequest {
    /// The directory pilots this event rosters, in selection order. Each must name a known pilot.
    pub pilot_ids: Vec<PilotId>,
}

/// The body of `PUT /events/{id}/classes` — the directory class ids that make up an event's
/// selection (issue #84). Each must name a class in the application-level
/// [`ClassDirectory`](crate::classes::ClassDirectory), else a typed 404.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetEventClassesRequest {
    /// The directory classes this event runs, in selection order. Each must name a known class.
    pub ids: Vec<ClassId>,
}

/// The body of `PUT /events/{id}/classes/{class}/membership` — the roster pilot ids that race a
/// single class (race redesign Slice 1a). The `class` is the path segment; each id here must name
/// a pilot in the [`PilotDirectory`](crate::pilots::PilotDirectory), else a typed 404. An empty
/// list clears the class's membership.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetClassMembershipRequest {
    /// The roster pilots that race this class, in selection order, **each with an optional assigned
    /// channel** (race redesign Slice 7a). Each must name a known pilot; each set
    /// [`channel`](MemberSlot::channel) must be one of the event's **primary timer**'s
    /// `available_channels` (which may exceed the timer's `node_count`).
    ///
    /// **Legacy-compatible:** an element may be a bare pilot-id string (the pre-Slice-7a wire shape),
    /// read as a channel-less slot — so an old client / persisted body still sets membership.
    #[serde(with = "member_slots")]
    #[ts(as = "Vec<MemberSlot>")]
    pub pilots: Vec<MemberSlot>,
}

/// The body of `POST /events` — the only thing a caller supplies when creating an event.
///
/// Just a display `name`; the **id is always auto-generated** (a slug of the name plus a
/// short random suffix), never user-entered, per the maintainer's rule. Keeping the id off
/// the wire means two events can share a name without colliding and a client can't squat a
/// reserved id (e.g. `practice`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct CreateEventRequest {
    /// The display name for the new event.
    pub name: String,
    /// Optional **display date** stored on the new event's [`EventMeta::date`] (see there).
    /// Omitted from the wire when unset — a name-only create stays a one-field body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    /// Optional venue / location, stored on [`EventMeta::location`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub location: Option<String>,
    /// Optional free-text description, stored on [`EventMeta::description`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
    /// Optional organizer name, stored on [`EventMeta::organizer`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub organizer: Option<String>,
}

/// One registered event: its metadata plus the [`AppState`] (its own log + append-notify,
/// the shared token store) every per-event surface serves against.
struct RegisteredEvent {
    meta: EventMeta,
    state: AppState,
}

/// The registry of all events on this Director (issue #72) — the backend-agnostic
/// `EventRegistry` the routing layer resolves an [`EventId`] through.
///
/// Maps each [`EventId`] to its [`AppState`] (and so its own [`EventLog`]). A built-in
/// **Practice** event ([`PRACTICE_EVENT_ID`], in-memory, non-persistent) is always present.
/// Created events get a file-backed [`SqliteLog`](gridfpv_storage::SqliteLog) under the
/// configured data dir (one file per event — the local realization of the per-event-dense
/// log; see the module docs for the Postgres mapping). Cloning shares the one registry (it
/// is `Arc<RwLock<…>>`), so it can be the axum router state cloned into every handler.
#[derive(Clone)]
pub struct EventRegistry {
    inner: Arc<RwLock<Registry>>,
}

/// The guarded interior: the event map, the shared token store, and where persistent event
/// DBs live.
struct Registry {
    /// `EventId → RegisteredEvent`. A `BTreeMap` so listing is deterministic (Practice is
    /// listed first explicitly regardless).
    events: BTreeMap<EventId, RegisteredEvent>,
    /// The one Director-wide auth authority, shared into every per-event [`AppState`].
    tokens: TokenStore,
    /// The Director-wide application-level **timer registry** (issue #73). Like the token store,
    /// it is one app-level authority the per-event selection ([`EventMeta::timers`]) references;
    /// it lives here so the single router state ([`EventRegistry`]) exposes it to the Timers API
    /// handlers without a second axum state type. Cloning shares the one registry.
    timers: TimerRegistry,
    /// The Director-wide application-level **pilot directory** (issue #74). Like the timer
    /// registry, it is one app-level authority the per-event roster ([`EventMeta::roster`])
    /// references; it lives here so the single router state ([`EventRegistry`]) exposes it to the
    /// Pilots API handlers without a second axum state type. Cloning shares the one directory.
    pilots: PilotDirectory,
    /// The Director-wide application-level **class directory** (issue #84). Like the pilot
    /// directory, it is one app-level authority the per-event class selection
    /// ([`EventMeta::classes`]) references; it lives here so the single router state
    /// ([`EventRegistry`]) exposes it to the Classes API handlers without a second axum state type.
    /// Cloning shares the one directory.
    classes: ClassDirectory,
    /// Directory persistent event SQLite files are created under; `None` ⇒ created events
    /// fall back to an in-memory log (no data dir configured — non-durable).
    data_dir: Option<PathBuf>,
    /// The Director's **currently-active event** (issue #90) — the one all clients resume
    /// into on connect/reload. `None` ⇒ the picker. Persisted to `<data_dir>/active-event`
    /// (when a data dir is configured) so it survives a Director restart; in-memory only with
    /// no data dir.
    active_event: Option<EventId>,
}

impl EventRegistry {
    /// Build a registry seeded with the built-in Practice event, persisting created events
    /// under `data_dir` when given.
    ///
    /// The Practice event is an in-memory, non-persistent log. When `data_dir` is `Some`,
    /// [`create`](EventRegistry::create) writes a SQLite file per event there; when `None`,
    /// created events fall back to an in-memory log (so the registry is still usable with no
    /// configured storage — useful in tests and an unconfigured Director).
    pub fn new(data_dir: Option<PathBuf>) -> Result<Self, RegistryError> {
        let tokens = TokenStore::new();
        let mut events = BTreeMap::new();

        // Build the Director-wide application-level timer registry (issue #73): the built-in
        // Mock (drawing its `laps`/`lap_ms` from the env defaults) plus any timers persisted
        // to `<data_dir>/timers.json`. Shares the same data dir as the events.
        let (sim_laps, sim_lap_ms) = sim_defaults();
        let timers = TimerRegistry::new(data_dir.clone(), sim_laps, sim_lap_ms)
            .map_err(|e| RegistryError(format!("could not build timer registry: {e}")))?;

        // Build the Director-wide application-level pilot directory (issue #74): the pilots
        // persisted to `<data_dir>/pilots.json` (empty on first boot — there is no built-in
        // pilot). Shares the same data dir as the events and timers.
        let pilots = PilotDirectory::new(data_dir.clone())
            .map_err(|e| RegistryError(format!("could not build pilot directory: {e}")))?;

        // Build the Director-wide application-level class directory (issue #84): the classes
        // persisted to `<data_dir>/classes.json` (empty on first boot — there is no built-in
        // class). Shares the same data dir as the events, timers, and pilots.
        let classes = ClassDirectory::new(data_dir.clone())
            .map_err(|e| RegistryError(format!("could not build class directory: {e}")))?;

        // Seed Practice: an in-memory (non-persistent) log, sharing the one token store.
        let practice_id = EventId(PRACTICE_EVENT_ID.to_string());
        let practice_state = AppState::with_tokens(InMemoryLog::new(), tokens.clone());
        events.insert(
            practice_id.clone(),
            RegisteredEvent {
                meta: EventMeta {
                    id: practice_id,
                    name: PRACTICE_EVENT_NAME.to_string(),
                    created_at: now_millis(),
                    persistent: false,
                    date: None,
                    location: None,
                    description: None,
                    organizer: None,
                    timers: default_timer_selection(),
                    primary_timer: None,
                    roster: Vec::new(),
                    classes: Vec::new(),
                    classes_membership: Vec::new(),
                    rounds: Vec::new(),
                },
                state: practice_state,
            },
        );

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                RegistryError(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            // Reload every previously-created event (issue #111): scan the data dir for the
            // per-event `<id>.sqlite` files and restore each event's `EventMeta` + its log into
            // the registry. Without this the registry only ever seeded Practice on boot, so
            // created events vanished on a Director restart (and the persisted active-event id
            // degraded to the picker because its event wasn't loaded). Practice stays the
            // built-in in-memory event, seeded above and never overwritten here.
            restore_persisted_events(dir, &tokens, &mut events);
        }

        // Restore the persisted active event (issue #90) on boot: read `<data_dir>/active-event`
        // if present and it still names a known event. A missing file, an unreadable one, or a
        // stale id (the event no longer exists) all degrade to `None` — the picker — rather than
        // failing to boot.
        let active_event = data_dir
            .as_deref()
            .and_then(read_persisted_active_event)
            .filter(|id| events.contains_key(id));

        Ok(Self {
            inner: Arc::new(RwLock::new(Registry {
                events,
                tokens,
                timers,
                pilots,
                classes,
                data_dir,
                active_event,
            })),
        })
    }

    /// The Director-wide application-level **timer registry** (issue #73) — the app-level
    /// authority the Timers API mutates and the per-event source bridge resolves selected timers
    /// through. Cloning shares the one registry.
    pub fn timers(&self) -> TimerRegistry {
        self.read().timers.clone()
    }

    /// The Director-wide application-level **pilot directory** (issue #74) — the app-level
    /// authority the Pilots API mutates and the per-event roster references. Cloning shares the one
    /// directory.
    pub fn pilots(&self) -> PilotDirectory {
        self.read().pilots.clone()
    }

    /// The Director-wide application-level **class directory** (issue #84) — the app-level authority
    /// the Classes API mutates and the per-event class selection references. Cloning shares the one
    /// directory.
    pub fn classes(&self) -> ClassDirectory {
        self.read().classes.clone()
    }

    /// The Director's currently-active event's [`EventMeta`] (issue #90), or `None` when no
    /// event is active (the picker). The single read every client makes on connect/reload to
    /// resume into the selected event.
    pub fn active(&self) -> Option<EventMeta> {
        let reg = self.read();
        reg.active_event
            .as_ref()
            .and_then(|id| reg.events.get(id))
            .map(|e| e.meta.clone())
    }

    /// Set the Director's active event (issue #90), returning its [`EventMeta`]. Validates the
    /// id names a known event, else [`RegistryError`] (the caller maps it to a typed 404). The
    /// new active id is **persisted** to `<data_dir>/active-event` (when a data dir is
    /// configured) so it survives a Director restart; with no data dir it is held in memory.
    pub fn set_active(&self, id: &EventId) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let meta = reg
            .events
            .get(id)
            .map(|e| e.meta.clone())
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        reg.active_event = Some(id.clone());
        // Persist best-effort; a write failure is logged-shaped (returned) but the in-memory
        // state is already updated so the live session is correct regardless.
        if let Some(dir) = reg.data_dir.clone() {
            write_persisted_active_event(&dir, id)
                .map_err(|e| RegistryError(format!("could not persist active event: {e}")))?;
        }
        Ok(meta)
    }

    /// Set an event's **selected timers** (issue #73), returning its updated [`EventMeta`].
    ///
    /// Validates the event exists (else a [`RegistryError`] the caller maps to a typed 404). The
    /// caller is responsible for validating each id names a known timer (the timer registry is a
    /// separate authority); this just records the selection on the event's meta. The selection is
    /// in-memory on the [`EventMeta`] (the event's own log/SQLite file holds its facts, not its
    /// config) — the source bridge reads it live when a heat goes Running.
    pub fn set_timers(&self, id: &EventId, ids: Vec<TimerId>) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        event.meta.timers = ids;
        // Drop a now-stale primary (issue #112): if the previously-designated primary is no longer
        // in the selection, clear it so [`EventMeta::effective_primary`] falls back to the first
        // selected timer rather than pointing at a deselected timer.
        if let Some(primary) = &event.meta.primary_timer {
            if !event.meta.timers.contains(primary) {
                event.meta.primary_timer = None;
            }
        }
        let meta = event.meta.clone();
        // Write the updated meta through to disk (issue #111) so a restart sees the latest
        // selection. Best-effort against a configured data dir for a persistent event.
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// Set an event's **primary** timer (issue #112), returning its updated [`EventMeta`].
    ///
    /// Designates which of the event's selected timers is the **primary** (the rest are
    /// alternates); the source bridge feeds only the active source's passes, preferring the primary
    /// (see [`EventMeta::effective_primary`]). Validates the event exists (else a [`RegistryError`]
    /// the caller maps to a typed 404). Passing `Some(id)` requires that id to be **in the event's
    /// current selection** (else a [`RegistryError`] — the caller maps it to a bad-request); passing
    /// `None` clears the override (the first selected timer becomes the effective primary).
    pub fn set_primary_timer(
        &self,
        id: &EventId,
        primary: Option<TimerId>,
    ) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        if let Some(primary) = &primary {
            if !event.meta.timers.contains(primary) {
                return Err(RegistryError(format!(
                    "primary timer {:?} is not in the event's selected timers",
                    primary.0
                )));
            }
        }
        event.meta.primary_timer = primary;
        let meta = event.meta.clone();
        // Write the updated meta through to disk (issue #111) so a restart sees the latest
        // primary designation.
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// Set an event's **roster** (issue #74), returning its updated [`EventMeta`].
    ///
    /// Replaces the event's roster wholesale with `pilot_ids`. Validates the event exists (else a
    /// [`RegistryError`] the caller maps to a typed 404); the caller is responsible for validating
    /// each id names a directory pilot (the pilot directory is a separate authority). The roster is
    /// recorded on the event's [`EventMeta`] and **written through** to the event's SQLite `meta`
    /// table (issue #115) so it survives a Director restart.
    pub fn set_roster(
        &self,
        id: &EventId,
        pilot_ids: Vec<PilotId>,
    ) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        event.meta.roster = pilot_ids;
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// Set an event's **class selection** (issue #84), returning its updated [`EventMeta`].
    ///
    /// Replaces the event's classes wholesale with `ids`. Validates the event exists (else a
    /// [`RegistryError`] the caller maps to a typed 404); the caller is responsible for validating
    /// each id names a directory class (the class directory is a separate authority). The selection
    /// is recorded on the event's [`EventMeta`] and **written through** to the event's SQLite `meta`
    /// table (issue #115) so it survives a Director restart — exactly the roster path.
    pub fn set_classes(&self, id: &EventId, ids: Vec<ClassId>) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        event.meta.classes = ids;
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// Set the **per-class membership** for one class (race redesign Slice 1a), returning the
    /// event's updated [`EventMeta`].
    ///
    /// Replaces *that class's* slot list wholesale with `pilots` (other classes' memberships are
    /// untouched), each [`MemberSlot`] carrying the pilot and its optional fixed channel (Slice 7a).
    /// An empty `pilots` removes the class's membership entry entirely (no empty entries are
    /// persisted). Validates the event exists (else a [`RegistryError`] the caller maps to a typed
    /// 404); the caller is responsible for validating that `class` names a directory class, each
    /// pilot id names a directory pilot, and each set channel is in the event's primary timer's
    /// available channels (the class/pilot/timer registries are separate authorities). The membership
    /// is recorded on the event's [`EventMeta`] and **written through**
    /// to the event's SQLite `meta` table (issue #115) so it survives a Director restart — exactly
    /// the roster/classes path.
    pub fn set_class_membership(
        &self,
        id: &EventId,
        class: ClassId,
        pilots: Vec<MemberSlot>,
    ) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        // Last-write-wins, set-membership semantics: replace the class's entry, drop it when the
        // new list is empty, so re-applying the same membership is idempotent.
        event.meta.classes_membership.retain(|m| m.class != class);
        if !pilots.is_empty() {
            event
                .meta
                .classes_membership
                .push(ClassMembership { class, pilots });
        }
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// Add a **round** to an event (race redesign Slice 2a), returning the created [`RoundDef`]
    /// (with its **generated** [`RoundId`]).
    ///
    /// The id is auto-generated — a slug of the request's `label` plus a short random suffix
    /// (mirroring the event/pilot id-gen) — retried on the (astronomically unlikely) collision with
    /// an existing round id. Validation (all [`RoundError::Invalid`], mapped to a 400):
    ///
    /// - each [`classes`](NewRoundReq::classes) entry exists in the class directory **and** is one
    ///   of the event's selected [`classes`](EventMeta::classes);
    /// - the [`format`](NewRoundReq::format) is a known [`FormatRegistry::standard`] name;
    /// - on [`SeedingRule::FromRanking`], the `source_round` names an existing round in this event.
    ///
    /// An unknown event is a [`RoundError::EventNotFound`] (→ 404). On success the round is appended
    /// to [`EventMeta::rounds`] and written through to the event's SQLite `meta` table (issue #115)
    /// so it survives a Director restart — exactly the classes/membership path.
    pub fn add_round(&self, id: &EventId, req: NewRoundReq) -> Result<RoundDef, RoundError> {
        let mut reg = self.write();
        let directory = reg.classes.clone();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RoundError::EventNotFound(id.0.clone()))?;

        validate_round_fields(
            &event.meta,
            &directory,
            &req.classes,
            &req.format,
            &req.seeding,
            None,
        )?;

        // Auto-generate a unique round id within this event: slug(label) + short suffix, retried on
        // the (astronomically unlikely) collision so the id is always fresh.
        let round_id = loop {
            let candidate = RoundId(format!("{}-{}", slugify(&req.label), short_suffix()));
            if !event.meta.rounds.iter().any(|r| r.id == candidate) {
                break candidate;
            }
        };

        // Default the channel mode **by format** when the request omits it (Slice 7a):
        // `timed_qual`/`round_robin` → Static, the bracket formats → PerHeat; an explicit
        // request value overrides.
        let channel_mode = req
            .channel_mode
            .unwrap_or_else(|| ChannelMode::default_for_format(&req.format));
        let round = RoundDef {
            id: round_id,
            label: req.label,
            classes: req.classes,
            format: req.format,
            params: req.params,
            win_condition: req.win_condition,
            seeding: req.seeding,
            channel_mode,
        };
        event.meta.rounds.push(round.clone());
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(round)
    }

    /// Replace an existing **round**'s editable fields (race redesign Slice 2a), returning the
    /// updated [`RoundDef`].
    ///
    /// The round's [`id`](RoundDef::id) is fixed (the path segment); every other field is replaced
    /// wholesale with `req`. Same validation as [`add_round`](Self::add_round): unknown event →
    /// [`RoundError::EventNotFound`] (404); unknown round id → [`RoundError::RoundNotFound`] (404);
    /// bad class / format / dangling seeding source → [`RoundError::Invalid`] (400). A
    /// [`SeedingRule::FromRanking`] may not name **this** round as its own source. Written through to
    /// disk (issue #115).
    pub fn update_round(
        &self,
        id: &EventId,
        round_id: &RoundId,
        req: UpdateRoundReq,
    ) -> Result<RoundDef, RoundError> {
        let mut reg = self.write();
        let directory = reg.classes.clone();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RoundError::EventNotFound(id.0.clone()))?;

        if !event.meta.rounds.iter().any(|r| &r.id == round_id) {
            return Err(RoundError::RoundNotFound(round_id.0.clone()));
        }
        validate_round_fields(
            &event.meta,
            &directory,
            &req.classes,
            &req.format,
            &req.seeding,
            Some(round_id),
        )?;

        // As with add: an omitted channel mode defaults by the (new) format; an explicit value
        // overrides. The round is replaced wholesale, so the mode is re-derived each update.
        let channel_mode = req
            .channel_mode
            .unwrap_or_else(|| ChannelMode::default_for_format(&req.format));
        let round = RoundDef {
            id: round_id.clone(),
            label: req.label,
            classes: req.classes,
            format: req.format,
            params: req.params,
            win_condition: req.win_condition,
            seeding: req.seeding,
            channel_mode,
        };
        if let Some(slot) = event.meta.rounds.iter_mut().find(|r| &r.id == round_id) {
            *slot = round.clone();
        }
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(round)
    }

    /// Remove a **round** from an event (race redesign Slice 2a), returning the event's updated
    /// [`EventMeta`].
    ///
    /// Unknown event → [`RoundError::EventNotFound`] (404); unknown round id →
    /// [`RoundError::RoundNotFound`] (404). Other rounds that seed from the removed round
    /// ([`SeedingRule::FromRanking`]) are **left as-is** (a dangling source is caught the next time
    /// that round is edited); pruning is a later-slice concern. Written through to disk (issue #115).
    pub fn remove_round(&self, id: &EventId, round_id: &RoundId) -> Result<EventMeta, RoundError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RoundError::EventNotFound(id.0.clone()))?;
        let before = event.meta.rounds.len();
        event.meta.rounds.retain(|r| &r.id != round_id);
        if event.meta.rounds.len() == before {
            return Err(RoundError::RoundNotFound(round_id.0.clone()));
        }
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// An event's **rounds** (race redesign Slice 2a), or `None` if no such event.
    pub fn rounds_of(&self, id: &EventId) -> Option<Vec<RoundDef>> {
        self.read().events.get(id).map(|e| e.meta.rounds.clone())
    }

    /// Add **one** pilot to an event's roster (issue #74), returning its updated [`EventMeta`].
    ///
    /// Idempotent — adding a pilot already on the roster is a no-op (no duplicate). Validates the
    /// event exists (else a [`RegistryError`] → 404); the caller validates the pilot id exists in
    /// the directory. Writes the updated meta through to disk (issue #115).
    pub fn add_to_roster(&self, id: &EventId, pilot: PilotId) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        if !event.meta.roster.contains(&pilot) {
            event.meta.roster.push(pilot);
        }
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// Remove **one** pilot from an event's roster (issue #74), returning its updated [`EventMeta`].
    ///
    /// Idempotent — removing a pilot not on the roster is a no-op. Validates the event exists (else
    /// a [`RegistryError`] → 404). Writes the updated meta through to disk (issue #115).
    pub fn remove_from_roster(
        &self,
        id: &EventId,
        pilot: &PilotId,
    ) -> Result<EventMeta, RegistryError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RegistryError(format!("no event with id {:?}", id.0)))?;
        event.meta.roster.retain(|p| p != pilot);
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(meta)
    }

    /// An event's full [`EventMeta`] (issue #112), or `None` if no such event — the source bridge
    /// reads it live to learn the selection *and* the effective primary in one consistent snapshot.
    pub fn meta_of(&self, id: &EventId) -> Option<EventMeta> {
        self.read().events.get(id).map(|e| e.meta.clone())
    }

    /// An event's currently-**selected timer ids** (issue #73), or `None` if no such event.
    ///
    /// The per-event source bridge reads this live when a heat goes Running to decide which
    /// timers to drive (resolving each id through the [`TimerRegistry`]).
    pub fn timers_of(&self, id: &EventId) -> Option<Vec<TimerId>> {
        self.read().events.get(id).map(|e| e.meta.timers.clone())
    }

    /// The shared [`TokenStore`] — the Director mints/pins its RD token through this so the
    /// one credential authenticates control on *every* event.
    pub fn tokens(&self) -> TokenStore {
        self.read().tokens.clone()
    }

    /// Resolve an [`EventId`] to that event's [`AppState`], or `None` if no such event.
    ///
    /// This is the registry's core operation: every per-event route resolves the id here and
    /// serves against the returned state's own log. An unknown id is the caller's cue to
    /// return a typed 404 (mirroring the `UnknownScope` pattern).
    pub fn resolve(&self, id: &EventId) -> Option<AppState> {
        self.read().events.get(id).map(|e| e.state.clone())
    }

    /// The metadata for every event, **Practice first**, then the rest in id order.
    ///
    /// The order is stable so `GET /events` is deterministic and the console can default to
    /// the first (Practice).
    pub fn list(&self) -> Vec<EventMeta> {
        let reg = self.read();
        let mut out = Vec::with_capacity(reg.events.len());
        let practice = EventId(PRACTICE_EVENT_ID.to_string());
        if let Some(p) = reg.events.get(&practice) {
            out.push(p.meta.clone());
        }
        for (id, ev) in &reg.events {
            if *id != practice {
                out.push(ev.meta.clone());
            }
        }
        out
    }

    /// Create a new persistent event from a [`CreateEventRequest`], returning its [`EventMeta`].
    ///
    /// The **id is auto-generated** — a slug of the request's `name` plus a short random
    /// suffix — so it is unique and never user-entered (names are display-only). The optional
    /// descriptive fields (`date`/`location`/`description`/`organizer`) are stored verbatim on
    /// the new event's meta. A file-backed [`SqliteLog`](gridfpv_storage::SqliteLog) is opened
    /// under the configured data dir (`<data_dir>/<id>.sqlite`); with no data dir configured the
    /// event falls back to an in-memory log so creation still succeeds. The new event shares the
    /// registry's token store, so the RD's token controls it immediately.
    pub fn create(&self, request: &CreateEventRequest) -> Result<EventMeta, RegistryError> {
        let name = request.name.as_str();
        let mut reg = self.write();

        // Auto-generate a unique id: slug + short random suffix, retried on the (astronomically
        // unlikely) collision so the id is always fresh and never the reserved `practice`.
        let id = loop {
            let candidate = EventId(format!("{}-{}", slugify(name), short_suffix()));
            if candidate.0 != PRACTICE_EVENT_ID && !reg.events.contains_key(&candidate) {
                break candidate;
            }
        };

        // Open the event's own log: a SQLite file per event under the data dir, else
        // in-memory (no configured storage).
        let (state, persistent) = match &reg.data_dir {
            Some(dir) => {
                let path = event_db_path(dir, &id);
                let log = SqliteLog::open(&path).map_err(|e| {
                    RegistryError(format!("could not open event log {}: {e}", path.display()))
                })?;
                (AppState::with_tokens(log, reg.tokens.clone()), true)
            }
            None => (
                AppState::with_tokens(InMemoryLog::new(), reg.tokens.clone()),
                false,
            ),
        };

        // Optional descriptive fields are stored verbatim, normalized so a blank string is
        // treated as "unset" (so an empty "Add details" field never persists an empty label).
        let meta = EventMeta {
            id: id.clone(),
            name: name.to_string(),
            created_at: now_millis(),
            persistent,
            date: normalize_optional(&request.date),
            location: normalize_optional(&request.location),
            description: normalize_optional(&request.description),
            organizer: normalize_optional(&request.organizer),
            timers: default_timer_selection(),
            primary_timer: None,
            roster: Vec::new(),
            classes: Vec::new(),
            classes_membership: Vec::new(),
            rounds: Vec::new(),
        };
        // Persist the freshly-built meta into the event's own SQLite `meta` table (issue
        // #111) so a Director restart can restore it. Only for a persistent (file-backed)
        // event — an in-memory event has nothing to persist to.
        if persistent {
            if let Some(dir) = reg.data_dir.clone() {
                persist_event_meta(&dir, &meta)?;
            }
        }

        reg.events.insert(
            id,
            RegisteredEvent {
                meta: meta.clone(),
                state,
            },
        );
        Ok(meta)
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, Registry> {
        self.inner.read().expect("event registry lock poisoned")
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, Registry> {
        self.inner.write().expect("event registry lock poisoned")
    }
}

/// The SQLite file an event's log lives in under `dir`: `<dir>/<id>.sqlite`.
fn event_db_path(dir: &Path, id: &EventId) -> PathBuf {
    dir.join(format!("{}{}", id.0, EVENT_DB_SUFFIX))
}

/// Persist an event's [`EventMeta`] into its own SQLite file's sidecar `meta` table (issue
/// #111), so a Director restart can restore it. Opens the event's `<dir>/<id>.sqlite` (WAL
/// allows this alongside the live `AppState` connection), serialises the meta to JSON, and
/// upserts it under [`EVENT_META_KEY`].
fn persist_event_meta(dir: &Path, meta: &EventMeta) -> Result<(), RegistryError> {
    let path = event_db_path(dir, &meta.id);
    let log = SqliteLog::open(&path).map_err(|e| {
        RegistryError(format!(
            "could not open event log {} to persist meta: {e}",
            path.display()
        ))
    })?;
    let json = serde_json::to_string(meta)
        .map_err(|e| RegistryError(format!("could not serialise event meta: {e}")))?;
    log.set_meta(EVENT_META_KEY, &json)
        .map_err(|e| RegistryError(format!("could not persist event meta: {e}")))?;
    Ok(())
}

/// Write an updated [`EventMeta`] through to its SQLite file when the event is persistent and
/// a data dir is configured (issue #111) — the shared tail of every meta mutation
/// (`set_timers`/`set_primary_timer`/…). A non-persistent event (in-memory, no data dir) is a
/// no-op: it has nothing to persist to and is gone on restart by design (Practice).
fn persist_meta_change(data_dir: Option<&Path>, meta: &EventMeta) -> Result<(), RegistryError> {
    match data_dir {
        Some(dir) if meta.persistent => persist_event_meta(dir, meta),
        _ => Ok(()),
    }
}

/// Restore every persisted event in `dir` into `events` on boot (issue #111).
///
/// Scans `<dir>` for `*.sqlite` files (each a created event's own log), and for each one opens
/// the log, reads its [`EventMeta`] back from the sidecar `meta` table, and rebuilds a
/// [`RegisteredEvent`] over that same on-disk log — so created events (and their metadata)
/// survive a Director restart. An entry that cannot be opened, has no persisted meta, or whose
/// meta cannot be parsed is **skipped** (logged-shaped, not fatal) so one bad file never blocks
/// boot. The reserved `practice` id is never produced here (Practice is the in-memory built-in,
/// seeded separately); a stray `practice.sqlite` is ignored so it can't shadow it.
fn restore_persisted_events(
    dir: &Path,
    tokens: &TokenStore,
    events: &mut BTreeMap<EventId, RegisteredEvent>,
) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        // No data dir yet (first boot) — nothing to restore.
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // Only event log files: a `<id>.sqlite`. Derive the id from the stem and skip
        // anything else (the `active-event` pointer, `timers.json`, WAL/SHM sidecars).
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(EVENT_DB_SUFFIX) else {
            continue;
        };
        // Never let a stray `practice.sqlite` shadow the built-in in-memory Practice event.
        if stem == PRACTICE_EVENT_ID || stem.is_empty() {
            continue;
        }
        let id = EventId(stem.to_string());

        // Open the event's own log and read its persisted meta back.
        let log = match SqliteLog::open(&path) {
            Ok(log) => log,
            Err(_) => continue, // unreadable file — skip, don't fail boot
        };
        let meta = match log.get_meta(EVENT_META_KEY) {
            Ok(Some(json)) => match serde_json::from_str::<EventMeta>(&json) {
                Ok(meta) => meta,
                Err(_) => continue, // unparseable meta — skip
            },
            // No persisted meta (a pre-#111 file, or a half-written create) — skip rather
            // than fabricate a name; without meta the event isn't safely reconstructable.
            Ok(None) | Err(_) => continue,
        };

        let state = AppState::with_tokens(log, tokens.clone());
        events.insert(id, RegisteredEvent { meta, state });
    }
}

/// The file the active-event id is persisted to under `dir` (issue #90).
fn active_event_path(dir: &Path) -> PathBuf {
    dir.join(ACTIVE_EVENT_FILE)
}

/// Read the persisted active-event id from `<dir>/active-event`, or `None` if the file is
/// absent/unreadable/blank. The id is validated against the live event set by the caller, so a
/// stale id here is harmless. The file holds just the id (trimmed).
fn read_persisted_active_event(dir: &Path) -> Option<EventId> {
    let raw = std::fs::read_to_string(active_event_path(dir)).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(EventId(trimmed.to_string()))
    }
}

/// Persist the active-event id to `<dir>/active-event` (issue #90), overwriting any prior value.
fn write_persisted_active_event(dir: &Path, id: &EventId) -> std::io::Result<()> {
    std::fs::write(active_event_path(dir), &id.0)
}

/// An error creating an event or its registry (a storage failure, a bad data dir).
#[derive(Debug, Clone)]
pub struct RegistryError(pub String);

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event registry error: {}", self.0)
    }
}

impl std::error::Error for RegistryError {}

/// An error adding/updating/removing a **round** (race redesign Slice 2a).
///
/// Distinguishes a missing event/round (the route maps to a typed **404**) from an invalid round
/// definition — a bad class, an unknown format, or a dangling seeding source — (a **400**). A
/// persistence failure folds into [`Invalid`](Self::Invalid) via the `From<RegistryError>`
/// conversion so the write-through path stays a single `?`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundError {
    /// No event with the given id (the inner `String` is the bad event id) — a typed 404.
    EventNotFound(String),
    /// No round with the given id in the event (the inner `String` is the bad round id) — a 404.
    RoundNotFound(String),
    /// The round definition is invalid (bad/unselected class, unknown format, or a dangling
    /// [`SeedingRule::FromRanking`] source) — a 400. The message names what was rejected.
    Invalid(String),
}

impl std::fmt::Display for RoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RoundError::EventNotFound(id) => write!(f, "no event with id {id:?}"),
            RoundError::RoundNotFound(id) => write!(f, "no round with id {id:?}"),
            RoundError::Invalid(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for RoundError {}

impl From<RegistryError> for RoundError {
    fn from(e: RegistryError) -> Self {
        RoundError::Invalid(e.0)
    }
}

/// Validate a round's class selection, format, and seeding against the event and the directories
/// (race redesign Slice 2a) — the shared check the add/update paths run.
///
/// Returns [`RoundError::Invalid`] when: a `classes` entry is unknown to the directory or is not one
/// of the event's selected [`classes`](EventMeta::classes); `format` is not a
/// [`FormatRegistry::standard`] name; or a [`SeedingRule::FromRanking`]'s `source_round` does not
/// name an existing round in this event (excluding `editing` — a round may not seed from itself).
fn validate_round_fields(
    meta: &EventMeta,
    directory: &ClassDirectory,
    classes: &[ClassId],
    format: &str,
    seeding: &SeedingRule,
    editing: Option<&RoundId>,
) -> Result<(), RoundError> {
    for class in classes {
        if !directory.exists(class) {
            return Err(RoundError::Invalid(format!(
                "no class with id {:?}",
                class.0
            )));
        }
        if !meta.classes.contains(class) {
            return Err(RoundError::Invalid(format!(
                "class {:?} is not selected by this event",
                class.0
            )));
        }
    }

    if !FormatRegistry::standard().contains(format) {
        return Err(RoundError::Invalid(format!("unknown format {format:?}")));
    }

    if let SeedingRule::FromRanking { source_round, .. } = seeding {
        if Some(source_round) == editing {
            return Err(RoundError::Invalid(
                "a round cannot seed from itself".to_string(),
            ));
        }
        if !meta.rounds.iter().any(|r| &r.id == source_round) {
            return Err(RoundError::Invalid(format!(
                "seeding source_round {:?} does not exist in this event",
                source_round.0
            )));
        }
    }

    Ok(())
}

/// The default per-event timer selection (issue #73): just the built-in **Mock**
/// ([`MOCK_TIMER_ID`]). New events and Practice select it so they run a sim race out of the box.
fn default_timer_selection() -> Vec<TimerId> {
    vec![TimerId(MOCK_TIMER_ID.to_string())]
}

/// The built-in Mock timer's default `laps`/`lap_ms` (issue #73), read from the same env
/// knobs the sim source uses (`GRIDFPV_SIM_LAPS` / `GRIDFPV_SIM_LAP_MS`), falling back to the
/// canonical defaults (5 laps @ 2500ms) when unset/unparseable — so the Mock timer's config
/// matches the env-driven sim exactly. Kept here (not in the app crate) to avoid a dependency
/// cycle; the values mirror `gridfpv_app::source::DEFAULT_SIM_LAPS`/`DEFAULT_SIM_LAP_MS`.
fn sim_defaults() -> (u32, u64) {
    let laps = std::env::var("GRIDFPV_SIM_LAPS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(5);
    let lap_ms = std::env::var("GRIDFPV_SIM_LAP_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(2500);
    (laps, lap_ms)
}

/// Current wall-clock time in milliseconds since the Unix epoch (creation timestamps).
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Trim an optional descriptive field, treating a blank/whitespace-only value as **unset**
/// (`None`) so an empty "Add details" input never persists an empty label.
fn normalize_optional(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Slugify a display name into the id-friendly stem: lowercase, ASCII alphanumerics kept,
/// every run of other characters collapsed to a single `-`, trimmed of leading/trailing
/// dashes. An empty/symbol-only name yields `event` so the id stem is never blank.
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
        "event".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A short random lowercase-alphanumeric suffix that makes an auto-generated id unique even
/// when two events share a name. Drawn from the OS CSPRNG (the same source the auth tokens
/// use) so it is unguessable and collision-resistant.
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
    use gridfpv_events::{CompetitorRef, Event, HeatId};

    /// A name-only create request (the common one-click path) for the tests.
    /// Wrap bare pilot ids into channel-less [`MemberSlot`]s — the membership shape the registry now
    /// takes (race redesign Slice 7a).
    fn slots(pilots: Vec<PilotId>) -> Vec<MemberSlot> {
        pilots.into_iter().map(MemberSlot::new).collect()
    }

    fn req(name: &str) -> CreateEventRequest {
        CreateEventRequest {
            name: name.to_string(),
            date: None,
            location: None,
            description: None,
            organizer: None,
        }
    }

    #[test]
    fn practice_is_always_present_and_first() {
        let reg = EventRegistry::new(None).unwrap();
        let list = reg.list();
        assert_eq!(list.first().unwrap().id.0, PRACTICE_EVENT_ID);
        assert_eq!(list.first().unwrap().name, PRACTICE_EVENT_NAME);
        assert!(!list.first().unwrap().persistent);
        // Practice resolves to a usable AppState.
        assert!(reg.resolve(&EventId(PRACTICE_EVENT_ID.into())).is_some());
    }

    #[test]
    fn unknown_event_does_not_resolve() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.resolve(&EventId("nope".into())).is_none());
    }

    #[test]
    fn create_auto_generates_a_unique_slug_id() {
        let reg = EventRegistry::new(None).unwrap();
        let a = reg.create(&req("Spring Cup 2026!")).unwrap();
        let b = reg.create(&req("Spring Cup 2026!")).unwrap();
        // Same name, distinct ids (the random suffix disambiguates); slug is name-derived.
        assert!(a.id.0.starts_with("spring-cup-2026-"));
        assert!(b.id.0.starts_with("spring-cup-2026-"));
        assert_ne!(a.id, b.id);
        // Both resolve, and they are listed after Practice.
        assert!(reg.resolve(&a.id).is_some());
        let ids: Vec<_> = reg.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids[0].0, PRACTICE_EVENT_ID);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn created_event_log_is_independent_of_practice() {
        let reg = EventRegistry::new(None).unwrap();
        let practice = reg.resolve(&EventId(PRACTICE_EVENT_ID.into())).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let created_state = reg.resolve(&created.id).unwrap();

        // Append a heat into the created event only.
        created_state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                },
                None,
            )
            .unwrap();

        // The created event's log has the heat; Practice's log is untouched (per-event dense).
        let (created_events, _) = created_state.read().unwrap();
        assert_eq!(created_events.len(), 1);
        let (practice_events, _) = practice.read().unwrap();
        assert_eq!(practice_events.len(), 0);
    }

    #[test]
    fn one_rd_token_controls_every_event() {
        let reg = EventRegistry::new(None).unwrap();
        let rd = reg.tokens().issue_rd_token();
        let created = reg.create(&req("Race Night")).unwrap();
        // The shared token store is the same instance behind every event's AppState.
        let practice = reg.resolve(&EventId(PRACTICE_EVENT_ID.into())).unwrap();
        let created_state = reg.resolve(&created.id).unwrap();
        assert!(practice.tokens().authenticate_control(Some(&rd)).is_ok());
        assert!(
            created_state
                .tokens()
                .authenticate_control(Some(&rd))
                .is_ok()
        );
    }

    #[test]
    fn slugify_collapses_and_trims() {
        assert_eq!(slugify("Spring Cup 2026!"), "spring-cup-2026");
        assert_eq!(slugify("  weird___name  "), "weird-name");
        assert_eq!(slugify("!!!"), "event");
        assert_eq!(slugify(""), "event");
    }

    #[test]
    fn active_event_defaults_to_none_then_set_and_read_back() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.active().is_none());

        // Setting to a known event returns its meta and reads back.
        let practice = EventId(PRACTICE_EVENT_ID.into());
        let meta = reg.set_active(&practice).unwrap();
        assert_eq!(meta.id, practice);
        assert_eq!(reg.active().map(|m| m.id), Some(practice));
    }

    #[test]
    fn set_active_rejects_an_unknown_event() {
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.set_active(&EventId("nope".into())).is_err());
        assert!(reg.active().is_none());
    }

    #[test]
    fn active_event_persists_across_a_restart_with_a_data_dir() {
        let dir = std::env::temp_dir().join(format!("gridfpv-active-test-{}", short_suffix()));
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted")).unwrap();
            reg.set_active(&created.id).unwrap();
            // A fresh registry over the SAME data dir restores the active event — the created
            // event's SQLite file (and its persisted meta) is reloaded on boot (issue #111), so
            // a created-id active pointer resolves rather than degrading to the picker.
            let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
            assert_eq!(reopened.active().map(|m| m.id), Some(created.id.clone()));

            // Persisting Practice (always present) survives the restart.
            reg.set_active(&EventId(PRACTICE_EVENT_ID.into())).unwrap();
            let reopened2 = EventRegistry::new(Some(dir.clone())).unwrap();
            assert_eq!(
                reopened2.active().map(|m| m.id.0),
                Some(PRACTICE_EVENT_ID.to_string())
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn created_events_and_their_metadata_survive_a_restart() {
        // The core #111 regression: a created event (with a name, descriptive fields, a timer
        // selection, and a primary) must be re-listed with its metadata intact, and its log, after
        // the Director restarts over the same data dir.
        let dir = std::env::temp_dir().join(format!("gridfpv-reload-test-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg
                .create(&CreateEventRequest {
                    name: "Spring Cup".to_string(),
                    date: Some("2026-06-20".to_string()),
                    location: Some("Main field".to_string()),
                    description: None,
                    organizer: Some("GridFPV Club".to_string()),
                })
                .unwrap();
            created_id = created.id.clone();

            // Give it a non-default timer selection + an explicit primary, then a log fact.
            let a = TimerId("rh-1".into());
            let b = TimerId(MOCK_TIMER_ID.into());
            reg.set_timers(&created.id, vec![a.clone(), b.clone()])
                .unwrap();
            reg.set_primary_timer(&created.id, Some(a.clone())).unwrap();
            reg.set_active(&created.id).unwrap();

            let state = reg.resolve(&created.id).unwrap();
            state
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("q-1".into()),
                        lineup: vec![CompetitorRef("A".into())],
                        class: None,
                        round: None,
                        frequencies: vec![],
                    },
                    None,
                )
                .unwrap();
        }

        // Simulate a Director restart: a brand-new registry over the SAME data dir.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();

        // The event is listed again (Practice first, then the created one) with its metadata.
        let restored = reopened
            .meta_of(&created_id)
            .expect("the created event should be reloaded on restart");
        assert_eq!(restored.name, "Spring Cup");
        assert!(restored.persistent);
        assert_eq!(restored.date.as_deref(), Some("2026-06-20"));
        assert_eq!(restored.location.as_deref(), Some("Main field"));
        assert_eq!(restored.organizer.as_deref(), Some("GridFPV Club"));
        assert_eq!(
            restored.timers,
            vec![TimerId("rh-1".into()), TimerId(MOCK_TIMER_ID.into())]
        );
        assert_eq!(restored.primary_timer, Some(TimerId("rh-1".into())));

        // It is in the public list, after Practice.
        let ids: Vec<_> = reopened.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids.first().map(|i| i.0.as_str()), Some(PRACTICE_EVENT_ID));
        assert!(ids.contains(&created_id));

        // Its log facts survived too.
        let state = reopened.resolve(&created_id).unwrap();
        let (events, _) = state.read().unwrap();
        assert_eq!(events.len(), 1);

        // And the active-event pointer restores to it (no longer degrades to the picker).
        assert_eq!(reopened.active().map(|m| m.id), Some(created_id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_events_default_to_an_empty_roster_and_set_add_remove_work() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Roster Event")).unwrap();
        assert!(event.roster.is_empty(), "a new event has an empty roster");

        let a = PilotId("acroace-1".into());
        let b = PilotId("zoom-2".into());

        // set_roster replaces wholesale.
        let meta = reg
            .set_roster(&event.id, vec![a.clone(), b.clone()])
            .unwrap();
        assert_eq!(meta.roster, vec![a.clone(), b.clone()]);

        // add is idempotent (no duplicate).
        let meta = reg.add_to_roster(&event.id, a.clone()).unwrap();
        assert_eq!(meta.roster, vec![a.clone(), b.clone()]);

        // add a fresh pilot appends.
        let c = PilotId("newbie-3".into());
        let meta = reg.add_to_roster(&event.id, c.clone()).unwrap();
        assert_eq!(meta.roster, vec![a.clone(), b.clone(), c.clone()]);

        // remove drops one; removing an absent one is a no-op.
        let meta = reg.remove_from_roster(&event.id, &b).unwrap();
        assert_eq!(meta.roster, vec![a.clone(), c.clone()]);
        let meta = reg.remove_from_roster(&event.id, &b).unwrap();
        assert_eq!(meta.roster, vec![a.clone(), c.clone()]);

        // unknown event → error.
        assert!(reg.set_roster(&EventId("nope".into()), vec![]).is_err());
        assert!(
            reg.add_to_roster(&EventId("nope".into()), a.clone())
                .is_err()
        );
        assert!(reg.remove_from_roster(&EventId("nope".into()), &a).is_err());
    }

    #[test]
    fn an_events_roster_persists_across_a_restart() {
        // The #115 meta mechanism must carry the additive roster through a Director restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-roster-test-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted Roster")).unwrap();
            created_id = created.id.clone();
            reg.set_roster(
                &created.id,
                vec![PilotId("acroace-1".into()), PilotId("zoom-2".into())],
            )
            .unwrap();
        }
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened.meta_of(&created_id).expect("event reloaded");
        assert_eq!(
            restored.roster,
            vec![PilotId("acroace-1".into()), PilotId("zoom-2".into())]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_events_default_to_an_empty_class_selection_and_set_works() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Class Event")).unwrap();
        assert!(event.classes.is_empty(), "a new event selects no classes");
        // Practice also defaults to an empty class selection.
        assert!(
            reg.meta_of(&EventId(PRACTICE_EVENT_ID.into()))
                .unwrap()
                .classes
                .is_empty()
        );

        let a = ClassId("open-1".into());
        let b = ClassId("spec-2".into());
        // set_classes replaces wholesale.
        let meta = reg
            .set_classes(&event.id, vec![a.clone(), b.clone()])
            .unwrap();
        assert_eq!(meta.classes, vec![a.clone(), b.clone()]);

        // unknown event → error.
        assert!(reg.set_classes(&EventId("nope".into()), vec![]).is_err());
    }

    #[test]
    fn an_events_class_selection_persists_across_a_restart() {
        // The #115 meta mechanism must carry the additive class selection through a restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-classes-reg-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted Classes")).unwrap();
            created_id = created.id.clone();
            reg.set_classes(
                &created.id,
                vec![ClassId("open-1".into()), ClassId("spec-2".into())],
            )
            .unwrap();
        }
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened.meta_of(&created_id).expect("event reloaded");
        assert_eq!(
            restored.classes,
            vec![ClassId("open-1".into()), ClassId("spec-2".into())]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn new_events_default_to_no_class_membership_and_set_replace_clear_work() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Membership Event")).unwrap();
        assert!(
            event.classes_membership.is_empty(),
            "a new event has no per-class membership"
        );

        let open = ClassId("open-1".into());
        let spec = ClassId("spec-2".into());
        let a = PilotId("acroace-1".into());
        let b = PilotId("zoom-2".into());
        let c = PilotId("newbie-3".into());

        // Set the Open class's membership.
        let meta = reg
            .set_class_membership(&event.id, open.clone(), slots(vec![a.clone(), b.clone()]))
            .unwrap();
        assert_eq!(meta.classes_membership.len(), 1);
        assert_eq!(meta.classes_membership[0].class, open);
        assert_eq!(
            meta.classes_membership[0].pilots,
            slots(vec![a.clone(), b.clone()])
        );

        // A second class gets its own entry; the first is untouched.
        let meta = reg
            .set_class_membership(&event.id, spec.clone(), slots(vec![c.clone()]))
            .unwrap();
        assert_eq!(meta.classes_membership.len(), 2);
        let open_entry = meta
            .classes_membership
            .iter()
            .find(|m| m.class == open)
            .unwrap();
        assert_eq!(open_entry.pilots, slots(vec![a.clone(), b.clone()]));

        // Re-setting one class replaces only that class's list (last-write-wins, no duplicate entry).
        let meta = reg
            .set_class_membership(&event.id, open.clone(), slots(vec![a.clone()]))
            .unwrap();
        assert_eq!(
            meta.classes_membership
                .iter()
                .filter(|m| m.class == open)
                .count(),
            1
        );
        let open_entry = meta
            .classes_membership
            .iter()
            .find(|m| m.class == open)
            .unwrap();
        assert_eq!(open_entry.pilots, slots(vec![a.clone()]));

        // An empty list clears the class's membership entry entirely.
        let meta = reg
            .set_class_membership(&event.id, open.clone(), vec![])
            .unwrap();
        assert!(meta.classes_membership.iter().all(|m| m.class != open));
        assert_eq!(meta.classes_membership.len(), 1, "only Spec remains");

        // unknown event → error.
        assert!(
            reg.set_class_membership(&EventId("nope".into()), open, vec![])
                .is_err()
        );
    }

    #[test]
    fn legacy_bare_pilot_id_membership_deserializes_as_channelless_slots() {
        // A pre-Slice-7a `classes_membership` persisted `pilots` as a bare `["acroace-1", …]`
        // array; it must still load (each as a channel-less MemberSlot), and re-serialize in the
        // canonical slot form — so an old event round-trips through restart.
        let legacy = r#"{"class":"open-1","pilots":["acroace-1","zoom-2"]}"#;
        let parsed: ClassMembership = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            parsed.pilots,
            vec![
                MemberSlot::new(PilotId("acroace-1".into())),
                MemberSlot::new(PilotId("zoom-2".into())),
            ]
        );

        // A mixed array (a bare id + a full slot with a channel) also loads.
        let mixed =
            r#"{"class":"open-1","pilots":["acroace-1",{"pilot":"zoom-2","channel":5658}]}"#;
        let parsed: ClassMembership = serde_json::from_str(mixed).unwrap();
        assert_eq!(parsed.pilots[0].channel, None);
        assert_eq!(parsed.pilots[1].channel, Some(5658));

        // Canonical re-serialization is the slot form; it round-trips back to the same value.
        let json = serde_json::to_string(&parsed).unwrap();
        let again: ClassMembership = serde_json::from_str(&json).unwrap();
        assert_eq!(again, parsed);
    }

    #[test]
    fn member_channels_persist_across_a_restart() {
        // A membership carrying per-pilot channels round-trips through the #115 meta mechanism.
        let dir = std::env::temp_dir().join(format!("gridfpv-member-chan-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted Channels")).unwrap();
            created_id = created.id.clone();
            reg.set_class_membership(
                &created.id,
                ClassId("open-1".into()),
                vec![
                    MemberSlot {
                        pilot: PilotId("acroace-1".into()),
                        channel: Some(5658),
                    },
                    MemberSlot {
                        pilot: PilotId("zoom-2".into()),
                        channel: Some(5695),
                    },
                ],
            )
            .unwrap();
        }
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened.meta_of(&created_id).expect("event reloaded");
        assert_eq!(restored.classes_membership[0].pilots[0].channel, Some(5658));
        assert_eq!(restored.classes_membership[0].pilots[1].channel, Some(5695));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn class_membership_persists_across_a_restart() {
        // The #115 meta mechanism must carry the additive per-class membership through a restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-membership-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted Membership")).unwrap();
            created_id = created.id.clone();
            reg.set_class_membership(
                &created.id,
                ClassId("open-1".into()),
                slots(vec![PilotId("acroace-1".into()), PilotId("zoom-2".into())]),
            )
            .unwrap();
        }
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened.meta_of(&created_id).expect("event reloaded");
        assert_eq!(restored.classes_membership.len(), 1);
        assert_eq!(
            restored.classes_membership[0].class,
            ClassId("open-1".into())
        );
        assert_eq!(
            restored.classes_membership[0].pilots,
            slots(vec![PilotId("acroace-1".into()), PilotId("zoom-2".into())])
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn create_persists_a_file_per_event_when_a_data_dir_is_set() {
        let dir = std::env::temp_dir().join(format!("gridfpv-reg-test-{}", short_suffix()));
        let reg = EventRegistry::new(Some(dir.clone())).unwrap();
        let created = reg.create(&req("Persisted")).unwrap();
        assert!(created.persistent);
        let path = event_db_path(&dir, &created.id);
        assert!(path.exists(), "an event DB file should be created");
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Rounds (race redesign Slice 2a) ------------------------------------

    /// Seed a directory class named `name` and return its generated [`ClassId`].
    fn seed_class(reg: &EventRegistry, name: &str) -> ClassId {
        reg.classes()
            .create(&crate::classes::CreateClassRequest {
                name: name.to_string(),
                source: Default::default(),
                reference: None,
                description: None,
            })
            .unwrap()
            .id
    }

    /// A minimal [`NewRoundReq`]: a `FromRoster` `timed_qual` round over `classes`.
    fn round_req(label: &str, classes: Vec<ClassId>) -> NewRoundReq {
        NewRoundReq {
            label: label.to_string(),
            classes,
            format: "timed_qual".to_string(),
            params: BTreeMap::new(),
            win_condition: WinCondition::BestLap,
            seeding: SeedingRule::FromRoster,
            channel_mode: None,
        }
    }

    #[test]
    fn add_round_generates_an_id_and_appends() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Rounds Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        let round = reg
            .add_round(&event.id, round_req("Qualifying R1", vec![open.clone()]))
            .unwrap();
        // The id is generated from the label slug + a suffix (not user-entered, never empty).
        assert!(
            round.id.0.starts_with("qualifying-r1-"),
            "got {:?}",
            round.id
        );
        assert_eq!(round.label, "Qualifying R1");
        assert_eq!(round.classes, vec![open.clone()]);
        assert_eq!(round.format, "timed_qual");
        assert_eq!(round.seeding, SeedingRule::FromRoster);

        // It is appended to the event's rounds list.
        let rounds = reg.rounds_of(&event.id).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].id, round.id);

        // A second round with the same label gets a distinct id.
        let round2 = reg
            .add_round(&event.id, round_req("Qualifying R1", vec![open]))
            .unwrap();
        assert_ne!(round.id, round2.id);
        assert_eq!(reg.rounds_of(&event.id).unwrap().len(), 2);
    }

    #[test]
    fn round_channel_mode_defaults_by_format_and_is_overridable() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Modes Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        // timed_qual / round_robin → Static; the bracket formats → PerHeat (RD-overridable).
        let cases = [
            ("timed_qual", ChannelMode::Static),
            ("round_robin", ChannelMode::Static),
            ("single_elim", ChannelMode::PerHeat),
            ("double_elim", ChannelMode::PerHeat),
            ("multi_main", ChannelMode::PerHeat),
            ("zippyq", ChannelMode::PerHeat),
        ];
        for (format, expected) in cases {
            assert_eq!(ChannelMode::default_for_format(format), expected);
            let mut req = round_req(format, vec![open.clone()]);
            req.format = format.to_string();
            let round = reg.add_round(&event.id, req).unwrap();
            assert_eq!(
                round.channel_mode, expected,
                "{format} should default to {expected:?}"
            );
        }

        // An explicit channel_mode overrides the format default (force a qual round per-heat).
        let mut req = round_req("timed_qual", vec![open]);
        req.channel_mode = Some(ChannelMode::PerHeat);
        let round = reg.add_round(&event.id, req).unwrap();
        assert_eq!(round.channel_mode, ChannelMode::PerHeat);
    }

    #[test]
    fn update_and_remove_a_round() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Rounds Event")).unwrap();
        let open = seed_class(&reg, "Open");
        let spec = seed_class(&reg, "Spec");
        reg.set_classes(&event.id, vec![open.clone(), spec.clone()])
            .unwrap();

        let round = reg
            .add_round(&event.id, round_req("Practice", vec![open.clone()]))
            .unwrap();

        // Update: replace fields wholesale, id is preserved.
        let updated = reg
            .update_round(
                &event.id,
                &round.id,
                UpdateRoundReq {
                    label: "Open Practice".to_string(),
                    classes: vec![open.clone(), spec.clone()],
                    format: "single_elim".to_string(),
                    params: BTreeMap::from([("advance".to_string(), "2".to_string())]),
                    win_condition: WinCondition::FirstToLaps { n: 5 },
                    seeding: SeedingRule::FromRoster,
                    channel_mode: None,
                },
            )
            .unwrap();
        assert_eq!(updated.id, round.id, "the id is not editable");
        assert_eq!(updated.label, "Open Practice");
        assert_eq!(updated.classes, vec![open, spec]);
        assert_eq!(updated.format, "single_elim");
        assert_eq!(updated.params.get("advance").map(String::as_str), Some("2"));

        // The list reflects the update (still one round).
        let rounds = reg.rounds_of(&event.id).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].format, "single_elim");

        // Remove: the round is gone; removing it again is a 404.
        let meta = reg.remove_round(&event.id, &round.id).unwrap();
        assert!(meta.rounds.is_empty());
        assert!(matches!(
            reg.remove_round(&event.id, &round.id),
            Err(RoundError::RoundNotFound(_))
        ));
    }

    #[test]
    fn round_validation_rejects_bad_format_class_and_seeding() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Rounds Event")).unwrap();
        let open = seed_class(&reg, "Open");
        let unselected = seed_class(&reg, "Spec");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        // Unknown event → EventNotFound (404).
        assert!(matches!(
            reg.add_round(&EventId("nope".into()), round_req("R", vec![open.clone()])),
            Err(RoundError::EventNotFound(_))
        ));

        // Unknown format → Invalid (400).
        let mut bad_format = round_req("R", vec![open.clone()]);
        bad_format.format = "no-such-format".to_string();
        assert!(matches!(
            reg.add_round(&event.id, bad_format),
            Err(RoundError::Invalid(_))
        ));

        // A `*-demo` fixture format is NOT a production format → Invalid.
        let mut demo_format = round_req("R", vec![open.clone()]);
        demo_format.format = "knockout-demo".to_string();
        assert!(matches!(
            reg.add_round(&event.id, demo_format),
            Err(RoundError::Invalid(_))
        ));

        // A class not in the directory → Invalid.
        assert!(matches!(
            reg.add_round(&event.id, round_req("R", vec![ClassId("ghost".into())])),
            Err(RoundError::Invalid(_))
        ));

        // A directory class the event does not select → Invalid.
        assert!(matches!(
            reg.add_round(&event.id, round_req("R", vec![unselected])),
            Err(RoundError::Invalid(_))
        ));

        // FromRanking with a dangling source_round → Invalid.
        let mut dangling = round_req("Bracket", vec![open.clone()]);
        dangling.seeding = SeedingRule::FromRanking {
            source_round: RoundId("does-not-exist".into()),
            top_n: 4,
        };
        assert!(matches!(
            reg.add_round(&event.id, dangling),
            Err(RoundError::Invalid(_))
        ));

        // FromRanking pointing at an existing round → ok (the #84 carry seam).
        let q = reg
            .add_round(&event.id, round_req("Qualifying", vec![open.clone()]))
            .unwrap();
        let mut bracket = round_req("Bracket", vec![open]);
        bracket.seeding = SeedingRule::FromRanking {
            source_round: q.id.clone(),
            top_n: 4,
        };
        let bracket = reg.add_round(&event.id, bracket).unwrap();
        assert_eq!(
            bracket.seeding,
            SeedingRule::FromRanking {
                source_round: q.id,
                top_n: 4
            }
        );

        // A round may not seed from itself (caught on update).
        let self_ref = reg.update_round(
            &event.id,
            &bracket.id,
            UpdateRoundReq {
                label: bracket.label.clone(),
                classes: bracket.classes.clone(),
                format: bracket.format.clone(),
                params: BTreeMap::new(),
                win_condition: bracket.win_condition,
                seeding: SeedingRule::FromRanking {
                    source_round: bracket.id.clone(),
                    top_n: 2,
                },
                channel_mode: None,
            },
        );
        assert!(matches!(self_ref, Err(RoundError::Invalid(_))));
    }

    #[test]
    fn rounds_persist_across_a_restart() {
        // The #115 meta mechanism must carry the additive rounds list through a Director restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-rounds-test-{}", short_suffix()));
        let created_id;
        let round_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Persisted Rounds")).unwrap();
            created_id = created.id.clone();
            let open = seed_class(&reg, "Open");
            reg.set_classes(&created.id, vec![open.clone()]).unwrap();
            let round = reg
                .add_round(&created.id, round_req("Qualifying R1", vec![open]))
                .unwrap();
            round_id = round.id.clone();
        }
        // Restart: a brand-new registry over the SAME data dir.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened.meta_of(&created_id).expect("event reloaded");
        assert_eq!(restored.rounds.len(), 1);
        assert_eq!(restored.rounds[0].id, round_id);
        assert_eq!(restored.rounds[0].label, "Qualifying R1");
        assert_eq!(restored.rounds[0].format, "timed_qual");
        assert_eq!(restored.rounds[0].seeding, SeedingRule::FromRoster);
        std::fs::remove_dir_all(&dir).ok();
    }
}
