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

use gridfpv_engine::format::{FormatRegistry, OpenPractice};
use gridfpv_engine::heat::{GraceWindow, ProtestWindow};
use gridfpv_engine::scoring::WinCondition;
use gridfpv_events::RoundId;
use gridfpv_storage::{InMemoryLog, SqliteLog};

use crate::app::AppState;
use crate::auth::TokenStore;
use crate::classes::ClassDirectory;
use crate::pilots::PilotDirectory;
use crate::round_engine;
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
    ///
    /// **Open practice does no scoring**, so a win condition is not *required* for an open-practice
    /// round: the create/update requests make this field optional ([`NewRoundReq::win_condition`])
    /// and an omitted condition stores an inert [`default_win_condition`] here. The field stays a
    /// plain [`WinCondition`] (not an `Option`) so every scoring path is unchanged — for a
    /// non-open-practice round the stored condition is the one the RD chose; for an open-practice
    /// round it is never consulted (the heat ends on the [`time_limit_secs`](Self::time_limit_secs)
    /// or the RD's `ForceEnd`).
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
    /// The **staging timer** for this round, in seconds (heat-lifecycle Slice 2). *Informational
    /// only* — there is **no** auto-advance out of `Staged`; the console displays it as a staging
    /// countdown (Slice 3). Defaults to [`default_staging_timer_secs`] (300s = 5 min). Additive
    /// (`#[serde(default)]`) so pre-Slice-2 meta reads back with the default.
    #[serde(default = "default_staging_timer_secs")]
    pub staging_timer_secs: u32,
    /// The **start procedure** for this round (heat-lifecycle Slice 2) — how the heat auto-advances
    /// `Armed → Running`. The runtime picks a randomized delay in the procedure's window once, logs
    /// it ([`Event::HeatStarting`](gridfpv_events::Event::HeatStarting)), and fires the transition
    /// then. Defaults to a sane randomized delay ([`StartProcedure::default`]). Additive.
    #[serde(default)]
    pub start_procedure: StartProcedure,
    /// The **grace window** for late crossings after the win condition is met (heat-lifecycle
    /// Slice 2). The runtime holds the heat in `Running` for this long after the race-end criterion
    /// before appending the auto `Running → Unofficial`, so trailing pilots' final laps still count.
    /// Defaults to [`default_grace_window`] (a bounded few seconds — *not* `UntilScored`, so the
    /// auto-completion actually fires). Additive.
    #[serde(default = "default_grace_window")]
    pub grace_window: GraceWindow,
    /// The **protest window** for the provisional → official lifecycle (marshaling Slice 5,
    /// marshaling.html §3.3) — an optional, **OFF-by-default auto-official timer**. When set to
    /// [`ProtestWindow::After`], the runtime auto-finalizes the heat (`Unofficial → Final`) once the
    /// window elapses from the race-end instant; the RD can always finalize early or correct during
    /// the window, and `Revert` re-opens a finalized result. The default [`ProtestWindow::Off`] is
    /// today's behaviour — **manual `Finalize` only**, nothing auto-finalizes.
    ///
    /// Per-round so it can vary by phase (e.g. a protest window on the mains, none on practice).
    /// Additive (`#[serde(default)]`) so a round persisted before this field reads back as `Off`.
    #[serde(default)]
    pub protest_window: ProtestWindow,
    /// The **minimum lap time** floor, in seconds (D26) — GridFPV-native, because timers are
    /// dumb pass emitters and GridFPV owns lap semantics: a raw pass that would close a lap
    /// shorter than this (a gate reflection, a double-detection) is AUTO-SUPPRESSED by the
    /// corrected-passes fold — visible on the marshaling lap list as a struck removal-record
    /// row with a Restore override (marshal-created passes — inserts, re-times — are exempt:
    /// an explicit ruling always outranks the floor). `None`/`0` = off (rounds predating the
    /// field keep their scored results bit-identical).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub min_lap_secs: Option<u32>,
    /// The **practice duration** for an open-practice round, in seconds (open-practice refinement).
    /// When set, the runtime clock **auto-ends the practice** (`Running → Unofficial`) once the
    /// heat's elapsed running time reaches this limit — independent of any win condition (the time is
    /// the *only* end condition for an open-practice heat). When unset (`None`), the practice runs
    /// until the RD ends it (`ForceEnd`). E.g. `3600` ends a one-hour practice on its own.
    ///
    /// Additive (`#[serde(default)]`, omitted from the wire when unset) so a round persisted before
    /// this field reads back with `None`. Only consulted for an open-practice heat; a normal round
    /// keeps ending on its win condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub time_limit_secs: Option<u32>,
}

/// The **inert default win condition** stored on a round whose create/update request omitted one
/// (open-practice refinement): a [`WinCondition::BestLap`].
///
/// Open practice does no scoring, so the stored condition is never consulted for an open-practice
/// round — it ends on its [`time_limit_secs`](RoundDef::time_limit_secs) or the RD's `ForceEnd`.
/// Keeping [`RoundDef::win_condition`] a plain (non-`Option`) [`WinCondition`] means every scoring
/// path is unchanged; this just gives the field a harmless value when the form supplies none.
pub fn default_win_condition() -> WinCondition {
    WinCondition::BestLap
}

/// The default [`RoundDef::staging_timer_secs`] — **300s (5 minutes)** of staging (heat-lifecycle
/// Slice 2). Informational only (no auto-advance); the console renders it as a staging countdown.
pub fn default_staging_timer_secs() -> u32 {
    300
}

/// The default [`RoundDef::grace_window`] — a **bounded 30-second** window after the win condition
/// is met (heat-lifecycle Slice 2).
///
/// Deliberately a [`GraceWindow::Duration`], **not** the open-ended [`GraceWindow::UntilScored`]:
/// the runtime's completion clock must eventually fire the `Running → Unofficial` auto-transition,
/// so the grace window has to close on its own. Thirty seconds comfortably covers a trailing pilot
/// finishing the lap (or the last lap of a multi-lap final) they were on when the leader met the
/// criterion, while still ending the heat on its own. RD-configurable per round.
pub fn default_grace_window() -> GraceWindow {
    GraceWindow::Duration { micros: 30_000_000 }
}

/// The **start procedure** that drives a heat's `Armed → Running` auto-transition (heat-lifecycle
/// Slice 2).
///
/// An **extensible** enum (today the one [`RandomizedDelay`](Self::randomized-delay) mode): the
/// runtime reads it when a heat enters `Armed`, picks a delay in the procedure's window **once**,
/// writes it to the log as a fact ([`Event::HeatStarting`](gridfpv_events::Event::HeatStarting)),
/// and schedules the `Running` transition for then. Randomization happens only in the runtime at
/// emission time, never in the fold, so a replay reads the logged delay and reproduces the start
/// exactly (race-engine.html §6). Derives serde + `ts_rs::TS`.
///
/// Serialized **internally tagged** on `mode` (e.g. `{ "mode": "randomized-delay", ... }`) so a
/// future mode (a fixed countdown, an external arm-tone trigger) is an additive variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "mode")]
#[ts(export, export_to = "bindings/")]
pub enum StartProcedure {
    /// **Randomized hold-then-go** (the FPV "arm… and… go" with a random hold): the runtime
    /// picks a delay uniformly in `[min_delay_ms, max_delay_ms]` and starts the race then. This is
    /// the canonical FPV start where pilots are armed and the go-tone comes after an
    /// unpredictable hold.
    #[serde(rename = "randomized-delay")]
    RandomizedDelay {
        /// The shortest the runtime will hold before `Running`, in milliseconds.
        #[ts(type = "number")]
        min_delay_ms: u32,
        /// The longest the runtime will hold before `Running`, in milliseconds. Must be ≥
        /// `min_delay_ms` (the runtime clamps a mis-ordered pair to a point delay defensively).
        #[ts(type = "number")]
        max_delay_ms: u32,
        /// Optional start-tone cue config for the console (heat-lifecycle Slice 3 renders/plays it;
        /// stored here now). Absent ⇒ the console uses its default tone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        tone: Option<StartTone>,
    },
}

impl Default for StartProcedure {
    /// A sane default randomized delay — a **2000–5000ms** hold before the go (heat-lifecycle
    /// Slice 2), the canonical FPV start window.
    fn default() -> Self {
        StartProcedure::RandomizedDelay {
            min_delay_ms: 2000,
            max_delay_ms: 5000,
            tone: None,
        }
    }
}

/// The start-tone cue for a [`StartProcedure`] (heat-lifecycle Slice 2) — stored config the console
/// uses to play the go-tone (the audio UX lands in Slice 3). Kept minimal and additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct StartTone {
    /// The tone frequency in hertz (e.g. `880`). Absent fields default in the console.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub hz: Option<u32>,
    /// The tone duration in milliseconds (e.g. `400`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub ms: Option<u32>,
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
/// a later slice consumes), or — for the casual **open-practice** format — from a set of active
/// **channels** ([`AllChannels`](Self::AllChannels)) rather than pilots. Derives serde + `ts_rs::TS`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum SeedingRule {
    /// Seed from the eligible classes' roster membership, in roster order. The default for
    /// practice and first-qualifying rounds.
    #[default]
    FromRoster,
    /// Seed from the **top-N** of one or more prior rounds' aggregated ranking — the bracket / cut
    /// seam (issue #51 multi-select, issue #84 carry). Each entry of `source_rounds` must name
    /// another [`RoundDef`] in the same event's [`rounds`](EventMeta::rounds); `top_n` is how many
    /// advance. When several rounds are named, the field is seeded from the **best-per-pilot
    /// ranking aggregated across those rounds** (see the round engine's `round_field`).
    ///
    /// **Serde back-compat:** an older stored round wrote a single `source_round: "x"` string;
    /// the enum's hand-written [`Deserialize`](SeedingRule#impl-Deserialize) reads either the
    /// legacy `source_round` key (→ `source_rounds: ["x"]`) or the current `source_rounds` array.
    /// Purely additive — no data migration.
    FromRanking {
        /// The prior rounds this round seeds from — each must exist in [`EventMeta::rounds`].
        /// Aggregated best-per-pilot when more than one is named. Always at least one entry.
        source_rounds: Vec<RoundId>,
        /// How many of the aggregated ranking's top places advance into this round.
        top_n: usize,
    },
    /// Seed from the **heat winners** of a prior bracket-level round — the **bracket
    /// advancement** carry (decisions D13, #217). The field is each completed heat's
    /// **advancing** competitor(s) in `source_round`, taken **in heat order** (heat 0's winner
    /// first, then heat 1's, …), plus any bye competitor the level advanced. This is exactly how
    /// a single-elimination bracket advances **round-to-round** under the level-per-round model: a
    /// level is one round, and the *next* level is a new round seeded from the prior level's
    /// winners — no intra-round bracket-walking.
    ///
    /// "Winner" means the heat's **advancing set** under the source round's format (the heat's top
    /// half — head-to-head advances one, a 4-up heat advances two), so a 4-up bracket carries the
    /// right competitors forward. The order is the source level's
    /// [`round_ranking`](crate::round_engine::round_ranking) advancers prefix, which a single-elim
    /// level already lists winners-first in heat order. Unlike [`FromRanking`](Self::FromRanking)
    /// (which takes a *top-N* slice of an aggregated ranking) this takes **exactly the winners** —
    /// however many heats the level had — so the next level's size follows the bracket, not a
    /// fixed `top_n`. Single source only (a single-elim level feeds exactly one next level);
    /// double-elimination's cross-bracket losers-of feed is a separate, deferred design (D13).
    FromHeatWinners {
        /// The prior bracket-level round whose heat winners seed this round — must exist in
        /// [`EventMeta::rounds`].
        source_round: RoundId,
    },
    /// Seed from a **slice** of one or more prior rounds' aggregated ranking — the multi-main /
    /// consolation seam (MultiGP multi-main). Like [`FromRanking`](Self::FromRanking) the field is
    /// the **best-per-pilot ranking aggregated across `source_rounds`** (see the round engine's
    /// `resolve_seeding`), but rather than the top-N it takes the window `skip+1 ..= skip+take` of
    /// that ranking — e.g. a C-main seeded from qual seeds 13–20 is `skip: 12, take: 8`. Each entry
    /// of `source_rounds` must name another [`RoundDef`] in the same event; `take` must be `> 0`.
    ///
    /// **Serde back-compat:** mirrors [`FromRanking`](Self::FromRanking)'s lenient body — the
    /// enum's hand-written [`Deserialize`](SeedingRule#impl-Deserialize) reads either the current
    /// `source_rounds` array or a legacy single `source_round` string. Purely additive.
    FromRankingRange {
        /// The prior rounds this round seeds from — each must exist in [`EventMeta::rounds`].
        /// Aggregated best-per-pilot when more than one is named. Always at least one entry.
        source_rounds: Vec<RoundId>,
        /// How many of the aggregated ranking's leading places to **skip** before taking — the
        /// 0-based start of the window (seed 13 is `skip: 12`).
        skip: usize,
        /// How many places to take after the skip — the window width. Must be `> 0`.
        take: usize,
    },
    /// Seed from the **union of sub-sources** — the composition primitive a real MultiGP multi-main
    /// wires per-main brackets bottom-up with (e.g. a B-main = the next six qual seeds **plus** the
    /// top two out of the C-main final). Each sub-rule in `sources` is resolved independently and
    /// the results are **concatenated in order**, de-duplicated keeping each competitor's **first**
    /// occurrence — so a competitor that two sources both name is seeded once, at the earlier
    /// source's position. `sources` must be non-empty; sub-rules may themselves be `Combine`
    /// (serde handles the self-reference), bounded by a nesting-depth cap at add/update.
    Combine {
        /// The sub-rules whose fields are concatenated (in order) then de-duplicated first-wins.
        /// Each is resolved exactly as a standalone seeding rule. Always at least one entry.
        sources: Vec<SeedingRule>,
    },
    /// Seed from a set of active **channels** rather than pilots — the **open-practice** seeding
    /// (open-practice format). The field builder lays each node index out as a `node-{i}`
    /// [`CompetitorRef`](gridfpv_events::CompetitorRef) (the timer-seat handle the timer emits
    /// passes for), and the one open heat runs over those channels with per-channel laps tracked
    /// **live in memory, not logged**. An open-practice round is `format: "open_practice"` +
    /// `seeding: AllChannels { channels }`; its [`classes`](RoundDef::classes) may be empty (it is
    /// not a class round). Additive variant — pre-existing rounds (`FromRoster`/`FromRanking`) read
    /// back unchanged.
    AllChannels {
        /// The active channels as **node indices** (the timer-seat indices the RD made live), laid
        /// out as `node-{i}` competitor refs by the field builder, in this order.
        channels: Vec<usize>,
    },
}

/// Hand-written [`Deserialize`] for [`SeedingRule`] that mirrors serde's default externally-tagged
/// representation while accepting **both** the current `source_rounds: [...]` shape and the legacy
/// single `source_round: "x"` shape for [`FromRanking`](SeedingRule::FromRanking) (issue #51
/// back-compat). The legacy string is lifted to a one-element `source_rounds` — additive, no data
/// migration. Every other variant deserializes exactly as the derive would.
impl<'de> Deserialize<'de> for SeedingRule {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        // The body of the `FromRanking` variant, accepting either `source_rounds` (current) or the
        // legacy `source_round` single string. `deny_unknown_fields` keeps a typo from silently
        // seeding from an empty set; exactly one of the two source keys must be present.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct FromRankingBody {
            #[serde(default)]
            source_rounds: Option<Vec<RoundId>>,
            #[serde(default)]
            source_round: Option<RoundId>,
            top_n: usize,
        }

        // `FromHeatWinners` is single-source (`source_round`), but for consistency with
        // `FromRanking` (which takes both) we also accept a one-element `source_rounds` array — so a
        // caller who pluralises by habit isn't rejected. Exactly one source must resolve.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct FromHeatWinnersBody {
            #[serde(default)]
            source_round: Option<RoundId>,
            #[serde(default)]
            source_rounds: Option<Vec<RoundId>>,
        }

        // `FromRankingRange` mirrors `FromRanking`'s lenient body (current `source_rounds` array or
        // legacy single `source_round` string) and adds the `skip` / `take` window.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct FromRankingRangeBody {
            #[serde(default)]
            source_rounds: Option<Vec<RoundId>>,
            #[serde(default)]
            source_round: Option<RoundId>,
            skip: usize,
            take: usize,
        }

        // An untagged shadow of the externally-tagged enum. `FromRanking` / `FromRankingRange` /
        // `FromHeatWinners` carry the lenient bodies; the other variants reuse the same field shapes
        // as `SeedingRule` so they round-trip 1:1. `Combine`'s `sources` is `Vec<SeedingRule>` — the
        // recursive self-reference dispatches back through this same hand-written `Deserialize`.
        #[derive(Deserialize)]
        enum Shadow {
            FromRoster,
            FromRanking(FromRankingBody),
            FromRankingRange(FromRankingRangeBody),
            FromHeatWinners(FromHeatWinnersBody),
            Combine { sources: Vec<SeedingRule> },
            AllChannels { channels: Vec<usize> },
        }

        match Shadow::deserialize(deserializer)? {
            Shadow::FromRoster => Ok(SeedingRule::FromRoster),
            Shadow::FromHeatWinners(body) => {
                let source_round = match (body.source_round, body.source_rounds) {
                    // Canonical singular form wins.
                    (Some(single), _) => single,
                    // A one-element `source_rounds` is lifted to the single source.
                    (None, Some(rounds)) if rounds.len() == 1 => {
                        rounds.into_iter().next().expect("len == 1")
                    }
                    (None, Some(_)) => {
                        return Err(D::Error::custom(
                            "FromHeatWinners seeding takes a single `source_round` (a one-element `source_rounds` is also accepted)",
                        ));
                    }
                    (None, None) => {
                        return Err(D::Error::custom(
                            "FromHeatWinners seeding requires `source_round` (or a one-element `source_rounds`)",
                        ));
                    }
                };
                Ok(SeedingRule::FromHeatWinners { source_round })
            }
            Shadow::AllChannels { channels } => Ok(SeedingRule::AllChannels { channels }),
            Shadow::Combine { sources } => Ok(SeedingRule::Combine { sources }),
            Shadow::FromRankingRange(body) => {
                let source_rounds = match (body.source_rounds, body.source_round) {
                    // Current shape — the explicit list wins.
                    (Some(rounds), _) => rounds,
                    // Legacy shape — lift the single source to a one-element list.
                    (None, Some(single)) => vec![single],
                    (None, None) => {
                        return Err(D::Error::custom(
                            "FromRankingRange seeding requires `source_rounds` (or legacy `source_round`)",
                        ));
                    }
                };
                Ok(SeedingRule::FromRankingRange {
                    source_rounds,
                    skip: body.skip,
                    take: body.take,
                })
            }
            Shadow::FromRanking(body) => {
                let source_rounds = match (body.source_rounds, body.source_round) {
                    // Current shape — the explicit list wins.
                    (Some(rounds), _) => rounds,
                    // Legacy shape — lift the single source to a one-element list.
                    (None, Some(single)) => vec![single],
                    (None, None) => {
                        return Err(D::Error::custom(
                            "FromRanking seeding requires `source_rounds` (or legacy `source_round`)",
                        ));
                    }
                };
                Ok(SeedingRule::FromRanking {
                    source_rounds,
                    top_n: body.top_n,
                })
            }
        }
    }
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
    /// How a heat in this round is won. **Optional** (open-practice refinement): an open-practice
    /// round does no scoring, so the form is not forced to supply one — **omit it** to store the
    /// inert [`default_win_condition`] ([`WinCondition::BestLap`]). A normal round supplies its
    /// chosen condition. Additive on the wire (a pre-existing client always sends it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub win_condition: Option<WinCondition>,
    /// How the round's field is seeded; defaults to [`SeedingRule::FromRoster`] when omitted.
    #[serde(default)]
    pub seeding: SeedingRule,
    /// The **practice duration** in seconds for an open-practice round (open-practice refinement).
    /// Optional — omit (or leave blank) for **no time limit** (the RD ends the practice with
    /// `ForceEnd`); supply it to have the runtime auto-end the practice at the limit. Stored on
    /// [`RoundDef::time_limit_secs`]. Additive on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub time_limit_secs: Option<u32>,
    /// How this round assigns channels (race redesign Slice 7a). Optional — **omit it** to take the
    /// format's default ([`ChannelMode::default_for_format`]); supply it to override (e.g. force a
    /// qual round per-heat). Additive on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel_mode: Option<ChannelMode>,
    /// The round's staging timer in seconds (heat-lifecycle Slice 2). Optional — omit for the
    /// [`default_staging_timer_secs`] (300). Informational only (no auto-advance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub staging_timer_secs: Option<u32>,
    /// The round's start procedure (heat-lifecycle Slice 2). Optional — omit for the
    /// [`StartProcedure::default`] randomized 2000–5000ms delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub start_procedure: Option<StartProcedure>,
    /// The round's grace window (heat-lifecycle Slice 2). Optional — omit for the
    /// [`default_grace_window`] (a bounded 3s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub grace_window: Option<GraceWindow>,
    /// The round's protest window (marshaling Slice 5). Optional — omit for the default
    /// [`ProtestWindow::Off`] (manual finalize only); supply [`ProtestWindow::After`] to arm the
    /// auto-official timer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub protest_window: Option<ProtestWindow>,
    /// Minimum lap time floor in seconds (D26); omitted/0 ⇒ off.
    #[serde(default)]
    #[ts(optional)]
    pub min_lap_secs: Option<u32>,
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
    /// The new win condition. **Optional** (open-practice refinement): omit it to store the inert
    /// [`default_win_condition`] (an open-practice round does no scoring).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub win_condition: Option<WinCondition>,
    /// The new seeding rule; defaults to [`SeedingRule::FromRoster`] when omitted.
    #[serde(default)]
    pub seeding: SeedingRule,
    /// The new practice duration in seconds (open-practice refinement). Optional — omit for **no
    /// time limit**. Stored on [`RoundDef::time_limit_secs`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub time_limit_secs: Option<u32>,
    /// The new channel mode (race redesign Slice 7a). Optional — **omit it** to take the format's
    /// default ([`ChannelMode::default_for_format`]); supply it to override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub channel_mode: Option<ChannelMode>,
    /// The new staging timer in seconds (heat-lifecycle Slice 2). Optional — omit for the
    /// [`default_staging_timer_secs`] (300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub staging_timer_secs: Option<u32>,
    /// The new start procedure (heat-lifecycle Slice 2). Optional — omit for the
    /// [`StartProcedure::default`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub start_procedure: Option<StartProcedure>,
    /// The new grace window (heat-lifecycle Slice 2). Optional — omit for the
    /// [`default_grace_window`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub grace_window: Option<GraceWindow>,
    /// The new protest window (marshaling Slice 5). Optional — omit for the default
    /// [`ProtestWindow::Off`] (manual finalize only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub protest_window: Option<ProtestWindow>,
    /// Minimum lap time floor in seconds (D26); omitted/0 ⇒ off.
    #[serde(default)]
    #[ts(optional)]
    pub min_lap_secs: Option<u32>,
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

/// Whether a heat in `state` means **a race is under way on the timer** — the shared refusal
/// predicate behind both the round-edit freeze (#387) and the timer restart (#386).
///
/// The four phases a race actually occupies: the countdown has begun (`Staged`), the gate is open
/// (`Armed`), pilots are flying (`Running`), or passes are recorded but not yet official
/// (`Unofficial`). `Scheduled` is not begun and `Final` is done, so neither is in progress —
/// callers that need to be stricter than "a race is under way" (the round-edit freeze also refuses
/// a `Scheduled` heat the RD has *loaded* in Live control, whose channels may already have been
/// read off) layer that on top rather than widening this.
fn is_racing_phase(state: gridfpv_engine::heat::HeatState) -> bool {
    use gridfpv_engine::heat::HeatState;
    matches!(
        state,
        HeatState::Staged | HeatState::Armed | HeatState::Running | HeatState::Unofficial
    )
}

/// What a round's heats say about how far its config may still move (release-hardening; the
/// in-progress refusal is #387) — the answer
/// [`round_heat_facts`](EventRegistry::round_heat_facts) folds off the event's log in ONE pass.
#[derive(Debug, Default)]
struct RoundHeatFacts {
    /// Whether ANY heat in the log is tagged with this round.
    has_heats: bool,
    /// Whether any of them has left `Scheduled` (staged / raced / scored). Scoring re-derives from
    /// the round's CURRENT config on every read, so editing a raced round's scoring fields would
    /// silently rewrite already-official results — [`EventRegistry::update_round`] rejects that.
    raced: bool,
    /// The **friendly name** of the first heat of this round that is *in progress* — staged, armed,
    /// running, or unofficial, or still `Scheduled` but loaded on the timer. While one exists the
    /// round cannot be edited at all (#387): re-materializing its heats would swap a lineup out
    /// from under the timer, and re-tuning mid-heat is worse.
    ///
    /// It carries the **name**, not the id: it goes straight into an RD-facing refusal, and a raw
    /// id must never reach a user (repo display rule). `Final` heats are deliberately absent — the
    /// raced-freeze already covers them, so there is no gap between "in progress" and "raced".
    in_progress: Option<String>,
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
            .map_err(|e| RegistryError::io(format!("could not build timer registry: {e}")))?;

        // Build the Director-wide application-level pilot directory (issue #74): the pilots
        // persisted to `<data_dir>/pilots.json` (empty on first boot — there is no built-in
        // pilot). Shares the same data dir as the events and timers.
        let pilots = PilotDirectory::new(data_dir.clone())
            .map_err(|e| RegistryError::io(format!("could not build pilot directory: {e}")))?;

        // Build the Director-wide application-level class directory (issue #84): the classes
        // persisted to `<data_dir>/classes.json` (empty on first boot — there is no built-in
        // class). Shares the same data dir as the events, timers, and pilots.
        let classes = ClassDirectory::new(data_dir.clone())
            .map_err(|e| RegistryError::io(format!("could not build class directory: {e}")))?;

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
                RegistryError::io(format!("could not create data dir {}: {e}", dir.display()))
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        reg.active_event = Some(id.clone());
        // Persist best-effort; a write failure is logged-shaped (returned) but the in-memory
        // state is already updated so the live session is correct regardless.
        if let Some(dir) = reg.data_dir.clone() {
            write_persisted_active_event(&dir, id)
                .map_err(|e| RegistryError::io(format!("could not persist active event: {e}")))?;
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        event.meta.timers = dedup_preserving_order(ids);
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        if let Some(primary) = &primary {
            if !event.meta.timers.contains(primary) {
                return Err(RegistryError::invalid(format!(
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        event.meta.roster = dedup_preserving_order(pilot_ids);
        // Membership is the finer roster × classes join, so a pilot dropped from the roster must
        // not linger in any class's membership (#336) — a stale slot would still be seated by
        // FillRound/ScheduleHeat, bypassing the roster/membership validation the membership PUT
        // enforces. Surviving members keep their slots (and their assigned channels) untouched.
        prune_membership_to_roster(&mut event.meta);
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        event.meta.classes = dedup_preserving_order(ids);
        // A deselected class's membership goes with it (#336): membership only means anything for
        // a class the event runs, and a stale entry would still field pilots if the class were
        // reselected later under different assumptions (or be resolved by a round that still
        // names it). Memberships of the surviving selection are untouched.
        let selected = event.meta.classes.clone();
        event
            .meta
            .classes_membership
            .retain(|m| selected.contains(&m.class));
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
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
    /// - on [`SeedingRule::FromRanking`], each `source_rounds` entry names an existing round in this
    ///   event.
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

        // The effective win condition (an omitted one stores the inert default) — used both to
        // validate the round can end and to build the `RoundDef` below.
        let win_condition = req.win_condition.unwrap_or_else(default_win_condition);
        // Default the channel mode **by format** when the request omits it (Slice 7a):
        // `timed_qual`/`round_robin` → Static, the bracket formats → PerHeat; an explicit
        // request value overrides. Resolved *before* validation so the Static/seeding rule can run.
        let channel_mode = req
            .channel_mode
            .unwrap_or_else(|| ChannelMode::default_for_format(&req.format));
        validate_round_fields(
            &event.meta,
            &directory,
            &req.classes,
            &req.format,
            &req.seeding,
            channel_mode,
            &win_condition,
            req.time_limit_secs,
            None,
        )?;
        validate_round_params(&req.format, &req.params)?;
        validate_min_lap(req.min_lap_secs)?;

        // Auto-generate a unique round id within this event: slug(label) + short suffix, retried on
        // the (astronomically unlikely) collision so the id is always fresh.
        let round_id = loop {
            let candidate = RoundId(format!("{}-{}", slugify(&req.label), short_suffix()));
            if !event.meta.rounds.iter().any(|r| r.id == candidate) {
                break candidate;
            }
        };

        let round = RoundDef {
            id: round_id,
            label: req.label,
            classes: req.classes,
            format: req.format,
            params: req.params,
            // The effective win condition computed + validated above (omitted ⇒ inert default).
            win_condition,
            seeding: req.seeding,
            channel_mode,
            // Heat-lifecycle Slice 2 configs: omitted request fields take their documented defaults.
            staging_timer_secs: req
                .staging_timer_secs
                .unwrap_or_else(default_staging_timer_secs),
            start_procedure: req.start_procedure.unwrap_or_default(),
            grace_window: req.grace_window.unwrap_or_else(default_grace_window),
            // The protest window (marshaling Slice 5): omitted ⇒ `Off` (manual finalize only).
            protest_window: req.protest_window.unwrap_or_default(),
            // The optional open-practice duration (open-practice refinement): carried through as-is.
            min_lap_secs: req.min_lap_secs.filter(|s| *s > 0),
            time_limit_secs: req.time_limit_secs,
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
    /// The **freeze / refusal probe** for round config: fold the event's log once and report the
    /// [`RoundHeatFacts`] for `round_id`.
    fn round_heat_facts(&self, id: &EventId, round_id: &RoundId) -> RoundHeatFacts {
        use gridfpv_engine::heat::{HeatState, heat_state};

        let mut facts = RoundHeatFacts::default();
        let Some(state) = self.resolve(id) else {
            return facts;
        };
        let Ok((events, _cursor)) = state.read() else {
            return facts;
        };
        // The round's own definition, for naming its heats the way the console does.
        let round = self
            .read()
            .events
            .get(id)
            .and_then(|e| e.meta.rounds.iter().find(|r| &r.id == round_id).cloned());
        let on_timer = round_engine::heat_on_timer(&events);
        for heat in round_engine::scheduled_round_heats(&events, round_id) {
            facts.has_heats = true;
            let Some(heat_state) = heat_state(&events, &heat) else {
                continue;
            };
            if heat_state != HeatState::Scheduled {
                facts.raced = true;
            }
            // Countdown begun / gate open / racing / passes recorded but not yet official — plus,
            // stricter than [`is_racing_phase`], a still-`Scheduled` heat the RD has loaded in Live
            // control: it may be on deck with its channels already read off, so it is off limits to
            // a round edit too.
            let in_progress =
                is_racing_phase(heat_state) || (heat_state == HeatState::Scheduled && on_timer.as_ref() == Some(&heat));
            if in_progress && facts.in_progress.is_none() {
                facts.in_progress = Some(match &round {
                    Some(round) => round_engine::heat_display_name(round, &events, &heat),
                    None => heat.0.clone(),
                });
            }
        }
        facts
    }

    /// The **friendly name** of a heat that is *in progress* on `timer` right now (#386), or `None`
    /// when no race is under way on it — the refusal probe behind restarting a RotorHazard timer.
    ///
    /// Restarting RotorHazard re-executes the RD's timing hardware, so it must be refused outright
    /// while a race is on it, not merely confirmed. "On it" is any event that **selects** this
    /// timer (not just the active one: a heat can be driven through a non-active event's bridge),
    /// and "in progress" is [`is_racing_phase`] — the same `Staged`/`Armed`/`Running`/`Unofficial`
    /// set the round-edit freeze refuses on.
    ///
    /// Returns the heat's **name**, never its id: it goes straight into an RD-facing refusal (repo
    /// display rule). A heat tagged to a round is named the way the console names it
    /// ([`round_engine::heat_display_name`]); an untagged free-text heat falls back to its RD-typed
    /// label, and a heat with neither to the generic "a heat" — a raw id is never emitted.
    pub fn heat_in_progress_on_timer(&self, timer: &TimerId) -> Option<String> {
        use gridfpv_engine::heat::heat_state;
        use gridfpv_events::Event;

        // Snapshot the candidate events (id + their rounds) and release the registry lock BEFORE
        // resolving/reading a log — `resolve` takes the same lock.
        let candidates: Vec<(EventId, Vec<RoundDef>)> = {
            let reg = self.read();
            reg.events
                .values()
                .filter(|e| e.meta.timers.contains(timer))
                .map(|e| (e.meta.id.clone(), e.meta.rounds.clone()))
                .collect()
        };
        for (event_id, rounds) in candidates {
            let Some(state) = self.resolve(&event_id) else {
                continue;
            };
            let Ok((events, _cursor)) = state.read() else {
                continue;
            };
            // Every heat the log ever scheduled, with the round it was tagged to, in first-scheduled
            // order (a re-schedule of the same id must not be considered twice).
            let mut heats: Vec<(gridfpv_events::HeatId, Option<RoundId>)> = Vec::new();
            for event in &events {
                if let Event::HeatScheduled { heat, round, .. } = event {
                    if !heats.iter().any(|(h, _)| h == heat) {
                        heats.push((heat.clone(), round.clone()));
                    }
                }
            }
            for (heat, round_id) in heats {
                if !heat_state(&events, &heat).is_some_and(is_racing_phase) {
                    continue;
                }
                let round = round_id
                    .as_ref()
                    .and_then(|r| rounds.iter().find(|def| &def.id == r));
                return Some(match round {
                    Some(round) => round_engine::heat_display_name(round, &events, &heat),
                    // Untagged (a free-text heat): its RD-typed label if it has one, else a generic
                    // phrase. Never the raw id.
                    None => events
                        .iter()
                        .rev()
                        .find_map(|e| match e {
                            Event::HeatScheduled {
                                heat: h,
                                label: Some(label),
                                ..
                            } if h == &heat && !label.trim().is_empty() => {
                                Some(label.trim().to_string())
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| "a heat".to_string()),
                });
            }
        }
        None
    }

    pub fn update_round(
        &self,
        id: &EventId,
        round_id: &RoundId,
        req: UpdateRoundReq,
    ) -> Result<RoundDef, RoundError> {
        // Probe the log BEFORE taking the registry write lock (the log has its own mutex).
        let facts = self.round_heat_facts(id, round_id);
        let mut reg = self.write();
        let directory = reg.classes.clone();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| RoundError::EventNotFound(id.0.clone()))?;

        let Some(existing) = event
            .meta
            .rounds
            .iter()
            .find(|r| &r.id == round_id)
            .cloned()
        else {
            return Err(RoundError::RoundNotFound(round_id.0.clone()));
        };
        // A round with a heat IN PROGRESS cannot be edited AT ALL (user decision, 2026-08-24,
        // #387). Editing a round re-materializes its still-`Scheduled` heats below; doing that to a
        // heat that is staged/armed/running — or one the RD has loaded in Live control — would swap
        // its lineup and frequencies out from under the timer, and re-tuning a heat mid-race is
        // worse. Refuse and name the heat instead. Once it reaches `Final` the raced-freeze below
        // takes over, so there is no gap between "in progress" and "raced".
        if let Some(heat) = &facts.in_progress {
            return Err(RoundError::Invalid(format!(
                "this round has a heat in progress ({heat}) — finalize or reset it before editing \
                 the round"
            )));
        }
        let win_condition = req.win_condition.unwrap_or_else(default_win_condition);
        // As with add: an omitted channel mode defaults by the (new) format; an explicit value
        // overrides. The round is replaced wholesale, so the mode is re-derived each update.
        // Resolved *before* validation so the Static/seeding rule can run.
        let channel_mode = req
            .channel_mode
            .unwrap_or_else(|| ChannelMode::default_for_format(&req.format));
        validate_round_fields(
            &event.meta,
            &directory,
            &req.classes,
            &req.format,
            &req.seeding,
            channel_mode,
            &win_condition,
            req.time_limit_secs,
            Some(round_id),
        )?;
        validate_round_params(&req.format, &req.params)?;
        validate_min_lap(req.min_lap_secs)?;

        // A RACED round's scoring-defining config is FROZEN (user-approved policy): scoring
        // re-derives from the round's current config, so editing these would silently re-score
        // already-official heats (a config-side bypass of the Final lock), and re-seeding would
        // rewrite a bracket chain. Still editable on a raced round: label, staging timer, start
        // procedure, grace window, protest window, time limit — and the `rounds` param (heats
        // per pilot), which only extends future fills.
        if facts.raced {
            let effective_channel_mode = channel_mode;
            let mut frozen: Vec<&str> = Vec::new();
            if req.format != existing.format {
                frozen.push("format");
            }
            if req.classes != existing.classes {
                frozen.push("classes");
            }
            if win_condition != existing.win_condition {
                frozen.push("win condition");
            }
            if req.seeding != existing.seeding {
                frozen.push("seeding");
            }
            if effective_channel_mode != existing.channel_mode {
                frozen.push("channel mode");
            }
            // The min-lap floor suppresses passes from the scored chain — editing it would
            // silently re-score raced heats, so it freezes with the win condition.
            if req.min_lap_secs.filter(|s| *s > 0) != existing.min_lap_secs {
                frozen.push("min lap time");
            }
            // Params: only `rounds` (heats per pilot) may change once raced.
            let differs_beyond_rounds = {
                let mut a = req.params.clone();
                let mut b = existing.params.clone();
                a.remove("rounds");
                b.remove("rounds");
                a != b
            };
            if differs_beyond_rounds {
                frozen.push("format params (other than rounds)");
            }
            if !frozen.is_empty() {
                return Err(RoundError::Invalid(format!(
                    "this round has raced heats — its {} can no longer change (label, staging, \
                     start procedure, grace, protest window, race time, and the rounds count \
                     stay editable)",
                    frozen.join(", ")
                )));
            }
        }

        let round = RoundDef {
            id: round_id.clone(),
            label: req.label,
            classes: req.classes,
            format: req.format,
            params: req.params,
            // The effective win condition computed + validated above (omitted ⇒ inert default).
            win_condition,
            seeding: req.seeding,
            channel_mode,
            // Heat-lifecycle Slice 2 configs: replaced wholesale, defaulting an omitted field.
            staging_timer_secs: req
                .staging_timer_secs
                .unwrap_or_else(default_staging_timer_secs),
            start_procedure: req.start_procedure.unwrap_or_default(),
            grace_window: req.grace_window.unwrap_or_else(default_grace_window),
            // The protest window (marshaling Slice 5): omitted ⇒ `Off` (manual finalize only).
            protest_window: req.protest_window.unwrap_or_default(),
            // The min-lap floor (D26): normalized so 0 and omitted are the same OFF.
            min_lap_secs: req.min_lap_secs.filter(|s| *s > 0),
            // The optional open-practice duration (open-practice refinement): replaced wholesale.
            time_limit_secs: req.time_limit_secs,
        };
        if let Some(slot) = event.meta.rounds.iter_mut().find(|r| &r.id == round_id) {
            *slot = round.clone();
        }
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        let timers = reg.timers.clone();
        // Release the registry write lock BEFORE touching the log: the command lock is always
        // taken ahead of the log mutex, and no other write path holds the registry across either.
        drop(reg);

        // RE-MATERIALIZE the round's already-scheduled heats (#387). A scheduled heat baked in the
        // lineup + frequencies the round's config produced when it was filled; without this the
        // edit changes the round but not the heat it already made, and that heat races stale
        // forever (the fill dedups by heat id, so re-filling never revisits it). Every heat this
        // touches is still `Scheduled` — anything in progress was refused above, and a raced
        // round's channel config is frozen — so nothing under way is rewritten.
        self.rematerialize_round_heats(id, round_id, &meta, &timers);
        Ok(round)
    }

    /// Rewrite the round's still-`Scheduled` heats against its just-edited config (#387): append a
    /// fresh [`Event::HeatScheduled`](gridfpv_events::Event::HeatScheduled) for each heat whose
    /// lineup or channels the new config changes.
    ///
    /// Every by-id read of a heat (lineup, class, round, frequencies, label) takes its **most
    /// recent** schedule, so the re-emitted event updates the heat in place rather than creating a
    /// second one — and `heat_state` re-seeds it to `Scheduled`, which is where it already was.
    ///
    /// Best-effort by design: a round whose new config cannot be planned (an empty field, an
    /// unassignable lineup) simply leaves its heats alone. The round edit itself has already been
    /// validated and persisted; the RD's next fill surfaces any real problem.
    fn rematerialize_round_heats(
        &self,
        id: &EventId,
        round_id: &RoundId,
        meta: &EventMeta,
        timers: &TimerRegistry,
    ) {
        let Some(state) = self.resolve(id) else {
            return;
        };
        // Read-check-append under the command lock, like every other validated write: the heats
        // are re-planned off the log they are appended to, so nothing can stage a heat in between
        // and have its lineup rewritten underneath it.
        let _guard = state.command_guard();
        let Ok((events, _cursor)) = state.read() else {
            return;
        };
        let class = round_engine::round_class(meta, round_id);
        for heat in round_engine::rematerialize_round_heats(meta, timers, round_id, &events) {
            let _ = state.append(
                gridfpv_events::Event::HeatScheduled {
                    heat: heat.heat,
                    lineup: heat.lineup,
                    class: class.clone(),
                    round: Some(round_id.clone()),
                    frequencies: heat.frequencies,
                    // The heat keeps whatever custom name it carried — a re-materialization is not
                    // a rename.
                    label: heat.label,
                },
                None,
            );
        }
    }

    /// Remove a **round** from an event (race redesign Slice 2a), returning the event's updated
    /// [`EventMeta`].
    ///
    /// Unknown event → [`RoundError::EventNotFound`] (404); unknown round id →
    /// [`RoundError::RoundNotFound`] (404). Other rounds that seed from the removed round
    /// ([`SeedingRule::FromRanking`]) are **left as-is** (a dangling source is caught the next time
    /// that round is edited); pruning is a later-slice concern. Written through to disk (issue #115).
    pub fn remove_round(&self, id: &EventId, round_id: &RoundId) -> Result<EventMeta, RoundError> {
        // A round with heats in the log cannot be removed: its heats would strand (they resolve
        // their name, win condition, and scoring through the round), and a raced round's results
        // would lose their scoring config entirely. The log is append-only, so there is nothing
        // safe to "cascade" — the RD abandons a misconfigured round by just not filling it.
        if self.round_heat_facts(id, round_id).has_heats {
            return Err(RoundError::Invalid(
                "this round has scheduled heats — it can no longer be removed (leave it \
                 unfilled, or discard its heats and re-use it)"
                    .to_string(),
            ));
        }
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
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
            .ok_or_else(|| RegistryError::not_found(format!("no event with id {:?}", id.0)))?;
        event.meta.roster.retain(|p| p != pilot);
        // Same staleness hole as the roster PUT (#336): the removed pilot's membership slots go
        // with them, so no class can still seat a pilot who left the event.
        prune_membership_to_roster(&mut event.meta);
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
                    RegistryError::io(format!("could not open event log {}: {e}", path.display()))
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

    /// **Permanently delete** an event and all of its data (the headline papercut fix).
    ///
    /// Removes the registry entry, deletes the event's on-disk state (its `<id>.sqlite` log plus
    /// the WAL/SHM sidecars under the data dir), and — if it was the Director's active event —
    /// clears the active pointer (persisting the cleared pointer so the picker is shown after a
    /// restart). The deletion is complete: nothing of the event survives a restart (the boot scan
    /// finds no `<id>.sqlite` to restore).
    ///
    /// The built-in **Practice** event ([`PRACTICE_EVENT_ID`]) cannot be deleted — it is the
    /// always-present in-memory scratch event — so an attempt is a [`RegistryError`] the caller
    /// maps to a `BadRequest`. An unknown id is a [`RegistryError`] the caller maps to a typed 404.
    ///
    /// The on-disk file removal is best-effort *after* the in-memory drop: dropping the
    /// [`RegisteredEvent`] closes the live SQLite connection (its `AppState` is the only holder),
    /// so the files are then free to unlink. A missing file is not an error (idempotent cleanup);
    /// a genuine unlink failure is surfaced as a [`RegistryError`] so the caller can report it.
    pub fn delete(&self, id: &EventId) -> Result<(), RegistryError> {
        let mut reg = self.write();

        if id.0 == PRACTICE_EVENT_ID {
            return Err(RegistryError::invalid(
                "the built-in Practice event cannot be deleted".to_string(),
            ));
        }
        // Drop the in-memory entry first; this closes the event's own SQLite connection (its
        // `AppState` is the sole holder) so the on-disk files are unlocked for removal below.
        let removed = reg.events.remove(id);
        if removed.is_none() {
            return Err(RegistryError::not_found(format!(
                "no event with id {:?}",
                id.0
            )));
        }
        drop(removed);

        // If it was the active event, clear the pointer (and persist the cleared state) so a
        // reload/restart lands on the picker rather than dangling at a now-gone event.
        if reg.active_event.as_ref() == Some(id) {
            reg.active_event = None;
            if let Some(dir) = reg.data_dir.clone() {
                // Best-effort: removing the pointer file degrades a stale read to `None` anyway.
                let _ = std::fs::remove_file(active_event_path(&dir));
            }
        }

        // Permanently remove the event's persisted state: the `<id>.sqlite` log plus its WAL/SHM
        // sidecars. A missing file is fine (idempotent); a real unlink error is reported.
        if let Some(dir) = reg.data_dir.clone() {
            remove_event_files(&dir, id)?;
        }
        Ok(())
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

/// Permanently remove an event's on-disk state under `dir`: its `<id>.sqlite` log and the
/// WAL/SHM sidecars SQLite leaves alongside it in WAL journal mode (`<id>.sqlite-wal`,
/// `<id>.sqlite-shm`). Each removal is best-effort against a *missing* file (already gone is
/// success — the cleanup is idempotent), but a genuine unlink failure (e.g. a permission error)
/// on the main log file is surfaced as a [`RegistryError`] so a partial delete is not silent.
fn remove_event_files(dir: &Path, id: &EventId) -> Result<(), RegistryError> {
    let main = event_db_path(dir, id);
    // The WAL/SHM sidecars share the main path with a suffix appended to the full file name.
    let wal = dir.join(format!("{}{}-wal", id.0, EVENT_DB_SUFFIX));
    let shm = dir.join(format!("{}{}-shm", id.0, EVENT_DB_SUFFIX));
    // Sidecars are pure cache/journal — a removal failure there is not fatal (SQLite recreates
    // them), so they are unlinked best-effort. The main log file is the durable state; a real
    // failure to remove it (not just "already absent") is reported.
    let _ = std::fs::remove_file(&wal);
    let _ = std::fs::remove_file(&shm);
    match std::fs::remove_file(&main) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(RegistryError::io(format!(
            "could not delete event log {}: {e}",
            main.display()
        ))),
    }
}

/// Persist an event's [`EventMeta`] into its own SQLite file's sidecar `meta` table (issue
/// #111), so a Director restart can restore it. Opens the event's `<dir>/<id>.sqlite` (WAL
/// allows this alongside the live `AppState` connection), serialises the meta to JSON, and
/// upserts it under [`EVENT_META_KEY`].
fn persist_event_meta(dir: &Path, meta: &EventMeta) -> Result<(), RegistryError> {
    let path = event_db_path(dir, &meta.id);
    let log = SqliteLog::open(&path).map_err(|e| {
        RegistryError::io(format!(
            "could not open event log {} to persist meta: {e}",
            path.display()
        ))
    })?;
    let json = serde_json::to_string(meta)
        .map_err(|e| RegistryError::io(format!("could not serialise event meta: {e}")))?;
    log.set_meta(EVENT_META_KEY, &json)
        .map_err(|e| RegistryError::io(format!("could not persist event meta: {e}")))?;
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
            Err(e) => {
                // An unreadable file — skip, don't fail boot, but make it LOUD: a silent skip here
                // would vanish the event with no trace (release-hardening P1-6).
                eprintln!(
                    "WARNING: skipping event log {} on boot: could not open it: {e}",
                    path.display()
                );
                continue;
            }
        };
        let meta = match log.get_meta(EVENT_META_KEY) {
            Ok(Some(json)) => match serde_json::from_str::<EventMeta>(&json) {
                Ok(meta) => meta,
                Err(e) => {
                    // Unparseable meta — skip, but LOUDLY. A non-additive `EventMeta` change would
                    // make EVERY prior event fail to parse and silently disappear on upgrade; this
                    // log line is the only signal that happened (release-hardening P1-6).
                    eprintln!(
                        "ERROR: skipping event {:?} (log {}): its persisted meta could not be \
                         parsed — a non-additive EventMeta change can do this: {e}",
                        stem,
                        path.display()
                    );
                    continue;
                }
            },
            // No persisted meta (a pre-#111 file, or a half-written create) — skip rather
            // than fabricate a name; without meta the event isn't safely reconstructable.
            Ok(None) => continue,
            Err(e) => {
                eprintln!(
                    "WARNING: skipping event {:?} (log {}): could not read its persisted meta: {e}",
                    stem,
                    path.display()
                );
                continue;
            }
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

/// An error mutating the event registry (a missing event, an invalid request, or a storage
/// failure). Carries a [`RegistryErrorKind`] so the HTTP layer can map an *unknown id* to `404`, a
/// *bad request* to `400`, and an *I/O / persistence* failure to `500` — mirroring [`PilotError`].
///
/// This matters because the in-memory state is mutated **before** the write-through: a persistence
/// failure must surface as a `500`, not a `404`/`400`, so the caller knows the change did not durably
/// land (issue: release-hardening P1-7).
#[derive(Debug, Clone)]
pub struct RegistryError {
    /// What kind of failure this is (drives the HTTP status the handler picks).
    pub kind: RegistryErrorKind,
    /// A human-readable message.
    pub message: String,
}

/// The class of a [`RegistryError`], so a handler can pick the right status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistryErrorKind {
    /// The addressed event/entity does not exist → 404.
    NotFound,
    /// A bad request value (e.g. a primary timer not in the selection) → 400.
    Invalid,
    /// A server-side storage / persistence failure → 500.
    Io,
}

impl RegistryError {
    /// An unknown-id error (HTTP 404).
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::NotFound,
            message: message.into(),
        }
    }

    /// A validation / bad-request error (HTTP 400).
    pub fn invalid(message: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::Invalid,
            message: message.into(),
        }
    }

    /// An I/O / persistence error (HTTP 500).
    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: RegistryErrorKind::Io,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "event registry error: {}", self.message)
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
        RoundError::Invalid(e.message)
    }
}

/// Validate a round's class selection, format, and seeding against the event and the directories
/// (race redesign Slice 2a) — the shared check the add/update paths run.
///
/// Returns [`RoundError::Invalid`] when: a `classes` entry is unknown to the directory or is not one
/// of the event's selected [`classes`](EventMeta::classes); `format` is not a
/// [`FormatRegistry::standard`] name; or a [`SeedingRule::FromRanking`]'s `source_rounds` is empty
/// or names a round that does not exist in this event (excluding `editing` — a round may not seed
/// from itself).
/// The maximum nesting depth a [`SeedingRule`] may reach — the `Combine`-within-`Combine` cap
/// checked at add/update (and mirrored at fill time by the round engine's depth guard, which also
/// bounds cross-round seeding cycles). A real multi-main composes only a couple of levels deep; the
/// cap rejects pathological / malicious nesting before it can be stored.
pub(crate) const MAX_SEEDING_DEPTH: usize = 8;

/// Walk a [`SeedingRule`] collecting every **source round** it names into `acc`, recursing into
/// [`Combine`](SeedingRule::Combine) sub-sources, while enforcing the per-rule structural
/// invariants and the [`MAX_SEEDING_DEPTH`] nesting cap (race redesign multi-main).
///
/// Pushes the sources of [`FromRanking`](SeedingRule::FromRanking),
/// [`FromRankingRange`](SeedingRule::FromRankingRange) and
/// [`FromHeatWinners`](SeedingRule::FromHeatWinners); ignores [`FromRoster`](SeedingRule::FromRoster)
/// / [`AllChannels`](SeedingRule::AllChannels) (no source rounds). Rejects: a `FromRanking` /
/// `FromRankingRange` with an empty source list, a `FromRankingRange` whose `take` is `0`, an empty
/// `Combine.sources`, and nesting deeper than [`MAX_SEEDING_DEPTH`]. The caller validates the
/// collected source rounds (existence + no self-seed) afterwards.
fn collect_source_rounds<'a>(
    seeding: &'a SeedingRule,
    acc: &mut Vec<&'a RoundId>,
    depth: usize,
) -> Result<(), RoundError> {
    if depth > MAX_SEEDING_DEPTH {
        return Err(RoundError::Invalid("seeding nesting too deep".to_string()));
    }
    match seeding {
        SeedingRule::FromRanking {
            source_rounds,
            top_n,
        } => {
            if source_rounds.is_empty() {
                return Err(RoundError::Invalid(
                    "FromRanking seeding must name at least one source round".to_string(),
                ));
            }
            if *top_n == 0 {
                return Err(RoundError::Invalid(
                    "FromRanking top_n must be > 0".to_string(),
                ));
            }
            acc.extend(source_rounds.iter());
        }
        SeedingRule::FromRankingRange {
            source_rounds,
            take,
            ..
        } => {
            if source_rounds.is_empty() {
                return Err(RoundError::Invalid(
                    "FromRankingRange seeding must name at least one source round".to_string(),
                ));
            }
            if *take == 0 {
                return Err(RoundError::Invalid(
                    "FromRankingRange take must be > 0".to_string(),
                ));
            }
            acc.extend(source_rounds.iter());
        }
        SeedingRule::FromHeatWinners { source_round } => acc.push(source_round),
        SeedingRule::Combine { sources } => {
            if sources.is_empty() {
                return Err(RoundError::Invalid(
                    "Combine sources must be non-empty".to_string(),
                ));
            }
            for sub in sources {
                collect_source_rounds(sub, acc, depth + 1)?;
            }
        }
        SeedingRule::FromRoster | SeedingRule::AllChannels { .. } => {}
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_round_fields(
    meta: &EventMeta,
    directory: &ClassDirectory,
    classes: &[ClassId],
    format: &str,
    seeding: &SeedingRule,
    channel_mode: ChannelMode,
    win_condition: &WinCondition,
    time_limit_secs: Option<u32>,
    editing: Option<&RoundId>,
) -> Result<(), RoundError> {
    // A `Static` round (time-trial / qualifying, GQ-style) forms its raced field straight from class
    // membership via the channel-balanced builder, but `round_ranking`/standings rank the
    // *seeding-resolved* field. Those only agree when seeding is the identity `FromRoster`; any other
    // seeding (creatable only via the raw API — the rounds form pairs Static with FromRoster) would
    // race a different field than it ranks. Reject it (release-hardening P1-2).
    if channel_mode == ChannelMode::Static && !matches!(seeding, SeedingRule::FromRoster) {
        return Err(RoundError::Invalid(
            "a Static (time-trial / qualifying) round must use FromRoster seeding; use a \
             PerHeat round for ranking- or bracket-seeded fields"
                .to_string(),
        ));
    }
    // A **scored** round's heats must be able to END. `Timed` / `FirstToLaps` self-terminate; `BestLap`
    // / `BestConsecutive` only *rank* (they never end a heat), so they need a race time
    // (`time_limit_secs`) — without one the heat would run forever. Open practice is exempt: it
    // intentionally has no end condition and runs until the RD's `ForceEnd` (so no win condition + no
    // time limit is valid there). The rounds form always supplies one; this guards the raw API.
    if format != OpenPractice::NAME {
        let self_terminates = matches!(
            win_condition,
            WinCondition::Timed { .. } | WinCondition::FirstToLaps { .. }
        );
        if !self_terminates && time_limit_secs.is_none() {
            return Err(RoundError::Invalid(
                "this win condition only ranks — it does not end a heat; set a race time \
                 (time_limit_secs), or use a Timed / First-to-N win condition"
                    .to_string(),
            ));
        }
    }
    // Degenerate end-condition values (raw-API guards; the form clamps these). A zero/negative
    // Timed window or a First-to-0 would end every heat the instant it starts (the completion
    // clock fires before any pass); a zero time limit likewise.
    if let WinCondition::Timed { window_micros } = win_condition {
        if *window_micros <= 0 {
            return Err(RoundError::Invalid(
                "a Timed round's race window must be positive".to_string(),
            ));
        }
    }
    if let WinCondition::FirstToLaps { n: 0 } = win_condition {
        return Err(RoundError::Invalid(
            "a First-to-N round must require at least 1 lap".to_string(),
        ));
    }
    if time_limit_secs == Some(0) {
        return Err(RoundError::Invalid(
            "the race time (time_limit_secs) must be at least 1 second".to_string(),
        ));
    }
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

    // The seeding rules that name **source rounds** (the bracket/cut carries) must name rounds
    // that exist in this event and may never name the round being edited (a round can't seed from
    // itself). `FromRanking` / `FromRankingRange` may name several (issue #51 multi-select) and
    // require at least one; `FromHeatWinners` (bracket advancement, #217) names exactly one prior
    // level; `Combine` recurses into its sub-sources. The recursive collector also enforces the
    // per-rule invariants (`take > 0`, non-empty `Combine`) and the nesting-depth cap as it walks.
    let mut source_rounds: Vec<&RoundId> = Vec::new();
    collect_source_rounds(seeding, &mut source_rounds, 0)?;
    for source_round in source_rounds {
        if Some(source_round) == editing {
            return Err(RoundError::Invalid(
                "a round cannot seed from itself".to_string(),
            ));
        }
        if !meta.rounds.iter().any(|r| &r.id == source_round) {
            return Err(RoundError::Invalid(format!(
                "seeding source round {:?} does not exist in this event",
                source_round.0
            )));
        }
    }

    Ok(())
}

/// Validate a round's `params` against `format`'s DECLARED schema (release-hardening): params
/// are stored verbatim, so garbage used to surface only at FILL time — mid-event, at the worst
/// moment. A declared number must parse as a positive whole number, an enum must be one of its
/// options, a bool must be true/false. Undeclared keys pass through untouched (e.g. the points
/// table, which has its own editor). Called from add_round/update_round alongside
/// [`validate_round_fields`].
/// Validate the min-lap floor (D26): 0/omitted is OFF; anything above 10 minutes is a typo
/// (no track has a 10-minute minimum lap), rejected before it can silently eat every lap.
fn validate_min_lap(min_lap_secs: Option<u32>) -> Result<(), RoundError> {
    if let Some(secs) = min_lap_secs {
        if secs > 600 {
            return Err(RoundError::Invalid(format!(
                "min lap time {secs}s is out of range (0 = off, up to 600s)"
            )));
        }
    }
    Ok(())
}

fn validate_round_params(
    format: &str,
    params: &BTreeMap<String, String>,
) -> Result<(), RoundError> {
    use gridfpv_engine::format::{FormatRegistry, ParamKind};
    let Some(schema) = FormatRegistry::standard_schemas()
        .into_iter()
        .find(|s| s.name == format)
    else {
        return Ok(()); // an unoffered/legacy format validates nothing new
    };
    for declared in &schema.params {
        let Some(value) = params.get(&declared.key) else {
            continue; // absent falls back to the default
        };
        match declared.kind {
            ParamKind::Number => {
                // Zero is meaningful for some knobs (an open-ended `rounds: 0`), so the guard
                // is "a whole number", not "positive" — the generators clamp semantics.
                if value.trim().parse::<u64>().is_err() {
                    return Err(RoundError::Invalid(format!(
                        "{} ({}) must be a whole number, got {value:?}",
                        declared.label, declared.key
                    )));
                }
            }
            ParamKind::Enum => {
                if !declared.options.iter().any(|o| o == value) {
                    return Err(RoundError::Invalid(format!(
                        "{} ({}) must be one of {:?}, got {value:?}",
                        declared.label, declared.key, declared.options
                    )));
                }
            }
            ParamKind::Bool => {
                if value != "true" && value != "false" {
                    return Err(RoundError::Invalid(format!(
                        "{} ({}) must be true or false, got {value:?}",
                        declared.label, declared.key
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Deduplicate `items` **preserving first-seen order** — a wholesale per-event selection (roster /
/// classes / timers) records each id at most once, so a duplicate in the request never double-counts
/// (a duplicate timer, for instance, would otherwise double-feed the source bridge).
fn dedup_preserving_order<T: PartialEq>(items: Vec<T>) -> Vec<T> {
    let mut out: Vec<T> = Vec::with_capacity(items.len());
    for item in items {
        if !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Drop every [`classes_membership`](EventMeta::classes_membership) slot whose pilot is **not on
/// the roster** (#336) — the shared prune the roster-shrinking mutations (the roster PUT and the
/// per-pilot DELETE) apply so membership never outlives the roster it joins. A membership entry
/// left with no pilots is removed entirely (no empty entries are persisted — the same invariant
/// [`EventRegistry::set_class_membership`] keeps). Surviving slots are untouched, so a remaining
/// member keeps their assigned channel.
fn prune_membership_to_roster(meta: &mut EventMeta) {
    let roster = &meta.roster;
    for membership in &mut meta.classes_membership {
        membership
            .pilots
            .retain(|slot| roster.contains(&slot.pilot));
    }
    meta.classes_membership.retain(|m| !m.pilots.is_empty());
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
    use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};

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
                    label: None,
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
                        label: None,
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
    fn delete_removes_the_event_and_all_its_data_and_survives_restart() {
        // The headline papercut: deleting an event must remove the registry entry, its persisted
        // SQLite log (+ wal/shm), clear it as the active event, and stay gone after a restart.
        let dir = std::env::temp_dir().join(format!("gridfpv-delete-test-{}", short_suffix()));
        let created_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Doomed Event")).unwrap();
            created_id = created.id.clone();
            // Give it a log fact and make it the active event so deletion must clear that too.
            reg.set_active(&created.id).unwrap();
            reg.resolve(&created.id)
                .unwrap()
                .append(
                    Event::HeatScheduled {
                        heat: HeatId("q-1".into()),
                        lineup: vec![CompetitorRef("A".into())],
                        class: None,
                        round: None,
                        frequencies: vec![],
                        label: None,
                    },
                    None,
                )
                .unwrap();

            // The SQLite file exists on disk before the delete.
            let db = event_db_path(&dir, &created_id);
            assert!(
                db.exists(),
                "the event's SQLite file should exist pre-delete"
            );

            // Delete it.
            reg.delete(&created_id).unwrap();

            // Gone from the registry, from the list, and as the active event.
            assert!(reg.resolve(&created_id).is_none());
            assert!(!reg.list().iter().any(|m| m.id == created_id));
            assert!(
                reg.active().is_none(),
                "deleting the active event clears it"
            );

            // And the on-disk state is gone (no orphan log / wal / shm files).
            assert!(!db.exists(), "the event's SQLite file is removed");
            assert!(!dir.join(format!("{}.sqlite-wal", created_id.0)).exists());
            assert!(!dir.join(format!("{}.sqlite-shm", created_id.0)).exists());
        }

        // Survives a restart: a fresh registry over the same data dir does not re-list it.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        assert!(
            reopened.resolve(&created_id).is_none(),
            "a deleted event must not reappear after a restart"
        );
        assert!(reopened.active().is_none());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn delete_rejects_practice_and_an_unknown_event() {
        let reg = EventRegistry::new(None).unwrap();
        // Practice (the built-in in-memory event) cannot be deleted.
        let practice = EventId(PRACTICE_EVENT_ID.into());
        assert!(reg.delete(&practice).is_err());
        assert!(
            reg.resolve(&practice).is_some(),
            "Practice survives a delete attempt"
        );
        // An unknown id is an error and removes nothing.
        assert!(reg.delete(&EventId("no-such-event".into())).is_err());
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
            win_condition: Some(WinCondition::BestLap),
            seeding: SeedingRule::FromRoster,
            // Best-lap only ranks, so a scored round needs a race time to end (validation).
            time_limit_secs: Some(60),
            channel_mode: None,
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        }
    }

    #[test]
    fn min_lap_is_normalized_validated_and_frozen_once_raced() {
        use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Floor Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        // 0 normalizes to OFF (None) — omitted and zero mean the same on the wire.
        let mut zero = round_req("Zeroed", vec![open.clone()]);
        zero.min_lap_secs = Some(0);
        let round = reg.add_round(&event.id, zero).unwrap();
        assert_eq!(round.min_lap_secs, None);

        // Out-of-range is rejected (a >10-minute floor eats every lap — a typo).
        let mut typo = round_req("Typo", vec![open.clone()]);
        typo.min_lap_secs = Some(601);
        assert!(reg.add_round(&event.id, typo).is_err());

        // A real floor sticks…
        let mut real = round_req("Floored", vec![open.clone()]);
        real.min_lap_secs = Some(5);
        let round = reg.add_round(&event.id, real).unwrap();
        assert_eq!(round.min_lap_secs, Some(5));

        // …and FREEZES once the round has raced (editing it re-scores official heats).
        let state = reg.resolve(&event.id).unwrap();
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: Some(open.clone()),
                    round: Some(round.id.clone()),
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        // Drive it all the way to Final: a heat that is merely *in progress* refuses the round
        // edit outright (#387), so the raced-freeze this asserts is only reachable once finalized.
        for transition in [
            HeatTransition::Running,
            HeatTransition::Finished,
            HeatTransition::Finalized,
        ] {
            state
                .append(
                    Event::HeatStateChanged {
                        heat: HeatId("q-1".into()),
                        transition,
                    },
                    None,
                )
                .unwrap();
        }
        let frozen_req = UpdateRoundReq {
            label: round.label.clone(),
            classes: round.classes.clone(),
            format: round.format.clone(),
            params: round.params.clone(),
            win_condition: Some(round.win_condition),
            seeding: round.seeding.clone(),
            time_limit_secs: round.time_limit_secs,
            channel_mode: Some(round.channel_mode),
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: Some(10), // was 5 — a scoring change on a raced round
        };
        let err = reg
            .update_round(&event.id, &round.id, frozen_req)
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("min lap time"),
            "expected the min-lap freeze, got {err:?}"
        );
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
    fn open_practice_round_saves_with_no_win_condition_and_a_time_limit() {
        // Open-practice refinement: an open-practice round needs **no win condition** (the request
        // omits it — `win_condition: None`) and carries an optional `time_limit_secs`. The round
        // saves: the inert default win condition is stored (never consulted), the seeding is
        // AllChannels, and the time limit round-trips.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Practice Event")).unwrap();

        let round = reg
            .add_round(
                &event.id,
                NewRoundReq {
                    label: "Open Practice".into(),
                    classes: vec![],
                    format: "open_practice".into(),
                    params: BTreeMap::new(),
                    // No win condition — the form is not forced to supply one for open practice.
                    win_condition: None,
                    seeding: SeedingRule::AllChannels {
                        channels: vec![0, 1, 2],
                    },
                    time_limit_secs: Some(3600),
                    channel_mode: None,
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .expect("an open-practice round with no win condition saves");

        // The inert default win condition is stored (BestLap), never consulted for open practice.
        assert_eq!(round.win_condition, default_win_condition());
        assert_eq!(round.time_limit_secs, Some(3600));
        assert!(matches!(round.seeding, SeedingRule::AllChannels { .. }));

        // It round-trips through the event meta with the time limit intact.
        let rounds = reg.rounds_of(&event.id).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].time_limit_secs, Some(3600));
    }

    #[test]
    fn round_channel_mode_defaults_by_format_and_is_overridable() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Modes Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        // timed_qual → Static; every other registered format → PerHeat (RD-overridable). Only
        // registered formats are exercised here (add_round validates the format), so the carved-out
        // bracket formats are gone — zippyq stands in for the `_ => PerHeat` default.
        let cases = [
            ("timed_qual", ChannelMode::Static),
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
    fn a_raced_round_freezes_its_scoring_config_but_not_the_race_day_knobs() {
        use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Freeze Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let round = reg
            .add_round(&event.id, round_req("Qual", vec![open.clone()]))
            .unwrap();

        // Race a heat under the round (Scheduled -> Final in the event's log).
        let state = reg.resolve(&event.id).unwrap();
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("q-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: Some(open.clone()),
                    round: Some(round.id.clone()),
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
        // Drive it all the way to Final: a heat that is merely *in progress* refuses the round
        // edit outright (#387), so the raced-freeze this asserts is only reachable once finalized.
        for transition in [
            HeatTransition::Running,
            HeatTransition::Finished,
            HeatTransition::Finalized,
        ] {
            state
                .append(
                    Event::HeatStateChanged {
                        heat: HeatId("q-1".into()),
                        transition,
                    },
                    None,
                )
                .unwrap();
        }

        let base = |label: &str| UpdateRoundReq {
            label: label.to_string(),
            classes: round.classes.clone(),
            format: round.format.clone(),
            params: round.params.clone(),
            win_condition: Some(round.win_condition),
            seeding: round.seeding.clone(),
            time_limit_secs: round.time_limit_secs,
            channel_mode: Some(round.channel_mode),
            staging_timer_secs: Some(45),
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        };

        // Race-day knobs (label / staging / etc.) and the `rounds` param stay editable.
        let mut ok_req = base("Qualifying (renamed)");
        ok_req.params.insert("rounds".to_string(), "4".to_string());
        let updated = reg.update_round(&event.id, &round.id, ok_req).unwrap();
        assert_eq!(updated.label, "Qualifying (renamed)");
        assert_eq!(updated.params.get("rounds"), Some(&"4".to_string()));

        // The scoring-defining fields are FROZEN: a changed win condition is rejected.
        let mut frozen_req = base("Qual");
        frozen_req.win_condition = Some(WinCondition::Timed {
            window_micros: 5_000_000,
        });
        let err = reg
            .update_round(&event.id, &round.id, frozen_req)
            .unwrap_err();
        assert!(
            format!("{err:?}").contains("raced"),
            "expected the raced-round freeze, got {err:?}"
        );

        // ...and a raced round can no longer be removed.
        let err = reg.remove_round(&event.id, &round.id).unwrap_err();
        assert!(format!("{err:?}").contains("heats"), "got {err:?}");
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
                    format: "head_to_head".to_string(),
                    params: BTreeMap::from([("advance".to_string(), "2".to_string())]),
                    win_condition: Some(WinCondition::FirstToLaps { n: 5 }),
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: None,
                    channel_mode: None,
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap();
        assert_eq!(updated.id, round.id, "the id is not editable");
        assert_eq!(updated.label, "Open Practice");
        assert_eq!(updated.classes, vec![open, spec]);
        assert_eq!(updated.format, "head_to_head");
        assert_eq!(updated.params.get("advance").map(String::as_str), Some("2"));

        // The list reflects the update (still one round).
        let rounds = reg.rounds_of(&event.id).unwrap();
        assert_eq!(rounds.len(), 1);
        assert_eq!(rounds[0].format, "head_to_head");

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

        // FromRanking with a dangling source round → Invalid. (Ranking-seeded fields are PerHeat;
        // a Static round must use FromRoster — P1-2.)
        let mut dangling = round_req("Bracket", vec![open.clone()]);
        dangling.channel_mode = Some(ChannelMode::PerHeat);
        dangling.seeding = SeedingRule::FromRanking {
            source_rounds: vec![RoundId("does-not-exist".into())],
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
        bracket.channel_mode = Some(ChannelMode::PerHeat);
        bracket.seeding = SeedingRule::FromRanking {
            source_rounds: vec![q.id.clone()],
            top_n: 4,
        };
        let bracket = reg.add_round(&event.id, bracket).unwrap();
        assert_eq!(
            bracket.seeding,
            SeedingRule::FromRanking {
                source_rounds: vec![q.id],
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
                win_condition: Some(bracket.win_condition),
                seeding: SeedingRule::FromRanking {
                    source_rounds: vec![bracket.id.clone()],
                    top_n: 2,
                },
                time_limit_secs: None,
                channel_mode: Some(ChannelMode::PerHeat),
                staging_timer_secs: None,
                start_procedure: None,
                grace_window: None,
                protest_window: None,
                min_lap_secs: None,
            },
        );
        assert!(matches!(self_ref, Err(RoundError::Invalid(_))));

        // FromHeatWinners (bracket advancement, #217) validates its single source the same way:
        // a dangling source is rejected, an existing one is accepted, and self-seeding is caught.
        let mut dangling_winners = round_req("Next level", vec![bracket.classes[0].clone()]);
        dangling_winners.format = "head_to_head".to_string();
        dangling_winners.seeding = SeedingRule::FromHeatWinners {
            source_round: RoundId("does-not-exist".into()),
        };
        assert!(matches!(
            reg.add_round(&event.id, dangling_winners),
            Err(RoundError::Invalid(_))
        ));

        let mut next_level = round_req("Next level", vec![bracket.classes[0].clone()]);
        next_level.format = "head_to_head".to_string();
        next_level.seeding = SeedingRule::FromHeatWinners {
            source_round: bracket.id.clone(),
        };
        let next_level = reg.add_round(&event.id, next_level).unwrap();
        assert_eq!(
            next_level.seeding,
            SeedingRule::FromHeatWinners {
                source_round: bracket.id.clone(),
            }
        );

        let self_winners = reg.update_round(
            &event.id,
            &next_level.id,
            UpdateRoundReq {
                label: next_level.label.clone(),
                classes: next_level.classes.clone(),
                format: next_level.format.clone(),
                params: BTreeMap::new(),
                win_condition: Some(next_level.win_condition),
                seeding: SeedingRule::FromHeatWinners {
                    source_round: next_level.id.clone(),
                },
                time_limit_secs: None,
                channel_mode: None,
                staging_timer_secs: None,
                start_procedure: None,
                grace_window: None,
                protest_window: None,
                min_lap_secs: None,
            },
        );
        assert!(matches!(self_winners, Err(RoundError::Invalid(_))));
    }

    #[test]
    fn round_validation_rejects_bad_multi_main_seeding() {
        // The multi-main carries (FromRankingRange / Combine) validate through the shared recursive
        // `collect_source_rounds`: a zero-width range, an empty Combine, an over-deep nesting, and a
        // dangling source nested inside a Combine are all rejected; a well-formed Combine is accepted.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Multi-main Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let q = reg
            .add_round(&event.id, round_req("Qualifying", vec![open.clone()]))
            .unwrap();

        // FromRankingRange with take == 0 → Invalid.
        let mut zero_take = round_req("C-main", vec![open.clone()]);
        zero_take.format = "head_to_head".to_string();
        zero_take.win_condition = Some(WinCondition::FirstToLaps { n: 3 });
        zero_take.seeding = SeedingRule::FromRankingRange {
            source_rounds: vec![q.id.clone()],
            skip: 12,
            take: 0,
        };
        assert!(matches!(
            reg.add_round(&event.id, zero_take),
            Err(RoundError::Invalid(_))
        ));

        // An empty Combine → Invalid.
        let mut empty_combine = round_req("Empty", vec![open.clone()]);
        empty_combine.format = "head_to_head".to_string();
        empty_combine.win_condition = Some(WinCondition::FirstToLaps { n: 3 });
        empty_combine.seeding = SeedingRule::Combine { sources: vec![] };
        assert!(matches!(
            reg.add_round(&event.id, empty_combine),
            Err(RoundError::Invalid(_))
        ));

        // A Combine nested past MAX_SEEDING_DEPTH → Invalid (rejected at add, before it can be stored).
        let mut seeding = SeedingRule::FromRoster;
        for _ in 0..(MAX_SEEDING_DEPTH + 2) {
            seeding = SeedingRule::Combine {
                sources: vec![seeding],
            };
        }
        let mut too_deep = round_req("Deep", vec![open.clone()]);
        too_deep.format = "head_to_head".to_string();
        too_deep.win_condition = Some(WinCondition::FirstToLaps { n: 3 });
        too_deep.seeding = seeding;
        assert!(matches!(
            reg.add_round(&event.id, too_deep),
            Err(RoundError::Invalid(_))
        ));

        // A dangling source nested inside a Combine is still caught (the collector recurses).
        let mut nested_dangling = round_req("Nested dangling", vec![open.clone()]);
        nested_dangling.format = "head_to_head".to_string();
        nested_dangling.win_condition = Some(WinCondition::FirstToLaps { n: 3 });
        nested_dangling.seeding = SeedingRule::Combine {
            sources: vec![SeedingRule::FromRanking {
                source_rounds: vec![RoundId("does-not-exist".into())],
                top_n: 2,
            }],
        };
        assert!(matches!(
            reg.add_round(&event.id, nested_dangling),
            Err(RoundError::Invalid(_))
        ));

        // A well-formed Combine of two real-source sub-rules is accepted (the B-main shape).
        let mut b_main = round_req("B-main", vec![open]);
        b_main.format = "head_to_head".to_string();
        b_main.win_condition = Some(WinCondition::FirstToLaps { n: 3 });
        b_main.seeding = SeedingRule::Combine {
            sources: vec![
                SeedingRule::FromRankingRange {
                    source_rounds: vec![q.id.clone()],
                    skip: 6,
                    take: 6,
                },
                SeedingRule::FromRanking {
                    source_rounds: vec![q.id.clone()],
                    top_n: 2,
                },
            ],
        };
        let b_main = reg.add_round(&event.id, b_main).unwrap();
        assert!(matches!(b_main.seeding, SeedingRule::Combine { .. }));
    }

    #[test]
    fn static_round_rejects_non_from_roster_seeding() {
        // P1-2: a Static round forms its field from class membership but ranks the seeding-resolved
        // field — they only agree under FromRoster. Any other seeding on a Static round is rejected.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Static Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let q = reg
            .add_round(&event.id, round_req("Qualifying", vec![open.clone()]))
            .unwrap();

        // Static + FromRanking → Invalid.
        let mut bad = round_req("Bad", vec![open.clone()]);
        bad.channel_mode = Some(ChannelMode::Static);
        bad.seeding = SeedingRule::FromRanking {
            source_rounds: vec![q.id.clone()],
            top_n: 4,
        };
        assert!(matches!(
            reg.add_round(&event.id, bad),
            Err(RoundError::Invalid(_))
        ));

        // Static + FromRoster → ok (the time-trial / qualifying default).
        let mut ok = round_req("Good", vec![open.clone()]);
        ok.channel_mode = Some(ChannelMode::Static);
        assert!(reg.add_round(&event.id, ok).is_ok());

        // The SAME ranking seeding is fine on a PerHeat round (the bracket path).
        let mut per_heat = round_req("PerHeat", vec![open]);
        per_heat.channel_mode = Some(ChannelMode::PerHeat);
        per_heat.seeding = SeedingRule::FromRanking {
            source_rounds: vec![q.id],
            top_n: 4,
        };
        assert!(reg.add_round(&event.id, per_heat).is_ok());
    }

    #[test]
    fn from_ranking_rejects_zero_top_n() {
        // P2: a `FromRanking { top_n: 0 }` advances nobody — reject it, like FromRankingRange.take.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Zero Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let q = reg
            .add_round(&event.id, round_req("Qualifying", vec![open.clone()]))
            .unwrap();

        let mut zero = round_req("Bracket", vec![open]);
        zero.channel_mode = Some(ChannelMode::PerHeat);
        zero.seeding = SeedingRule::FromRanking {
            source_rounds: vec![q.id],
            top_n: 0,
        };
        assert!(matches!(
            reg.add_round(&event.id, zero),
            Err(RoundError::Invalid(_))
        ));
    }

    #[test]
    fn wholesale_selections_dedup_preserving_order() {
        // P2: roster / classes / timers store each id once, preserving first-seen order — a
        // duplicate timer would otherwise double-feed the source bridge.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Dedup Event")).unwrap();

        let a = seed_class(&reg, "A");
        let b = seed_class(&reg, "B");
        let meta = reg
            .set_classes(&event.id, vec![a.clone(), b.clone(), a.clone()])
            .unwrap();
        assert_eq!(meta.classes, vec![a, b]);

        let mock = TimerId(MOCK_TIMER_ID.to_string());
        let meta = reg
            .set_timers(&event.id, vec![mock.clone(), mock.clone()])
            .unwrap();
        assert_eq!(meta.timers, vec![mock]);

        let p = reg
            .pilots()
            .create(&crate::pilots::CreatePilotRequest {
                callsign: "P".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let meta = reg
            .set_roster(&event.id, vec![p.clone(), p.clone()])
            .unwrap();
        assert_eq!(meta.roster, vec![p]);
    }

    /// Seed a directory pilot by callsign — the membership-prune tests' shorthand.
    fn seed_pilot(reg: &EventRegistry, callsign: &str) -> PilotId {
        reg.pilots()
            .create(&crate::pilots::CreatePilotRequest {
                callsign: callsign.to_string(),
                ..Default::default()
            })
            .unwrap()
            .id
    }

    #[test]
    fn roster_shrink_prunes_the_departed_pilots_membership() {
        // #336: a pilot dropped from the roster must not linger in classes_membership —
        // a stale slot would still be seated by FillRound, bypassing the membership PUT's
        // roster guard. Surviving members keep their slots AND their assigned channels.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Shrink Event")).unwrap();
        let open = seed_class(&reg, "Open");
        let (a, b) = (seed_pilot(&reg, "alpha"), seed_pilot(&reg, "bravo"));
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        reg.set_roster(&event.id, vec![a.clone(), b.clone()])
            .unwrap();
        reg.set_class_membership(
            &event.id,
            open.clone(),
            vec![
                MemberSlot {
                    pilot: a.clone(),
                    channel: Some(5658),
                },
                MemberSlot {
                    pilot: b.clone(),
                    channel: Some(5917),
                },
            ],
        )
        .unwrap();

        // Shrink the roster to just A: B's membership slot goes with them.
        let meta = reg.set_roster(&event.id, vec![a.clone()]).unwrap();
        assert_eq!(meta.classes_membership.len(), 1);
        let membership = &meta.classes_membership[0];
        assert_eq!(membership.class, open);
        assert_eq!(membership.pilots.len(), 1, "B's slot is pruned");
        assert_eq!(membership.pilots[0].pilot, a);
        assert_eq!(
            membership.pilots[0].channel,
            Some(5658),
            "the surviving member keeps their channel"
        );

        // Emptying the roster removes the now-empty membership entry entirely (no empty
        // entries are persisted — the set_class_membership invariant).
        let meta = reg.set_roster(&event.id, vec![]).unwrap();
        assert!(meta.classes_membership.is_empty());
    }

    #[test]
    fn removing_a_roster_pilot_prunes_their_membership() {
        // The per-pilot DELETE has the same staleness hole as the roster PUT (#336).
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Remove Event")).unwrap();
        let open = seed_class(&reg, "Open");
        let (a, b) = (seed_pilot(&reg, "alpha"), seed_pilot(&reg, "bravo"));
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        reg.set_roster(&event.id, vec![a.clone(), b.clone()])
            .unwrap();
        reg.set_class_membership(&event.id, open, slots(vec![a.clone(), b.clone()]))
            .unwrap();

        let meta = reg.remove_from_roster(&event.id, &b).unwrap();
        assert_eq!(meta.roster, vec![a.clone()]);
        assert_eq!(meta.classes_membership.len(), 1);
        assert_eq!(meta.classes_membership[0].pilots.len(), 1);
        assert_eq!(meta.classes_membership[0].pilots[0].pilot, a);
    }

    #[test]
    fn class_deselect_prunes_its_membership() {
        // #336: deselecting a class drops its membership entry — a stale entry would still
        // field pilots through any round that names the class. The surviving class's
        // membership (channels included) is untouched.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Deselect Event")).unwrap();
        let open = seed_class(&reg, "Open");
        let spec = seed_class(&reg, "Spec");
        let (a, b) = (seed_pilot(&reg, "alpha"), seed_pilot(&reg, "bravo"));
        reg.set_classes(&event.id, vec![open.clone(), spec.clone()])
            .unwrap();
        reg.set_roster(&event.id, vec![a.clone(), b.clone()])
            .unwrap();
        reg.set_class_membership(
            &event.id,
            open.clone(),
            vec![MemberSlot {
                pilot: a.clone(),
                channel: Some(5658),
            }],
        )
        .unwrap();
        reg.set_class_membership(&event.id, spec.clone(), slots(vec![b.clone()]))
            .unwrap();

        // Deselect Spec: its membership entry goes; Open's survives channel-intact.
        let meta = reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        assert_eq!(meta.classes, vec![open.clone()]);
        assert_eq!(meta.classes_membership.len(), 1);
        assert_eq!(meta.classes_membership[0].class, open);
        assert_eq!(meta.classes_membership[0].pilots[0].pilot, a);
        assert_eq!(meta.classes_membership[0].pilots[0].channel, Some(5658));
    }

    #[test]
    fn scored_round_requires_an_end_condition() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Ends Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        // Best-lap only ranks; a scored round with no race time would never end → rejected.
        let mut no_end = round_req("R", vec![open.clone()]);
        no_end.win_condition = Some(WinCondition::BestLap);
        no_end.time_limit_secs = None;
        assert!(matches!(
            reg.add_round(&event.id, no_end),
            Err(RoundError::Invalid(_))
        ));

        // Omitting the win condition (defaults to Best Lap) with no race time is also rejected.
        let mut omitted = round_req("R", vec![open.clone()]);
        omitted.win_condition = None;
        omitted.time_limit_secs = None;
        assert!(matches!(
            reg.add_round(&event.id, omitted),
            Err(RoundError::Invalid(_))
        ));

        // Best-lap WITH a race time is accepted.
        let mut with_time = round_req("R", vec![open.clone()]);
        with_time.time_limit_secs = Some(120);
        assert!(reg.add_round(&event.id, with_time).is_ok());

        // A self-terminating win condition (First-to-N) needs no time limit.
        let mut first_to = round_req("R", vec![open.clone()]);
        first_to.win_condition = Some(WinCondition::FirstToLaps { n: 3 });
        first_to.time_limit_secs = None;
        assert!(reg.add_round(&event.id, first_to).is_ok());

        // Open practice is EXEMPT — no win condition + no time limit is valid (runs until ForceEnd).
        let mut practice = round_req("Practice", vec![]);
        practice.format = "open_practice".to_string();
        practice.win_condition = None;
        practice.time_limit_secs = None;
        practice.seeding = SeedingRule::AllChannels {
            channels: vec![0, 1],
        };
        assert!(reg.add_round(&event.id, practice).is_ok());
    }

    #[test]
    fn from_heat_winners_seeding_accepts_either_source_key() {
        use serde_json::from_str;
        let canonical = SeedingRule::FromHeatWinners {
            source_round: RoundId("qf".into()),
        };
        // Singular `source_round` (canonical) and a one-element `source_rounds` both deserialize.
        assert_eq!(
            from_str::<SeedingRule>(r#"{"FromHeatWinners":{"source_round":"qf"}}"#).unwrap(),
            canonical
        );
        assert_eq!(
            from_str::<SeedingRule>(r#"{"FromHeatWinners":{"source_rounds":["qf"]}}"#).unwrap(),
            canonical
        );
        // A multi-element `source_rounds` is rejected (FromHeatWinners is single-source), as is none.
        assert!(
            from_str::<SeedingRule>(r#"{"FromHeatWinners":{"source_rounds":["a","b"]}}"#).is_err()
        );
        assert!(from_str::<SeedingRule>(r#"{"FromHeatWinners":{}}"#).is_err());
    }

    #[test]
    fn shelved_zippyq_round_still_validates_and_round_trips() {
        // #218: ZippyQ is **shelved** — removed from the offered format set (`standard_schemas`) so a
        // new round can't pick it, but the generator stays **registered** in
        // `FormatRegistry::standard()`. This is the persistence-stability guarantee: an event that
        // already stored a `zippyq` round (or the renamed-display `timed_qual` round) must still load
        // and validate. Adding a `zippyq` round through the same validation path the loader uses must
        // therefore succeed, and the persisted on-disk shape must round-trip unchanged.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Legacy Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();

        // `timed_qual` (the format whose *display* name became "Time Trials" — wire key unchanged).
        let tq = reg
            .add_round(&event.id, round_req("Time Trials R1", vec![open.clone()]))
            .unwrap();
        assert_eq!(tq.format, "timed_qual");

        // A `zippyq` round still passes validation (the generator is registered, just not offered).
        let mut zippy = round_req("Legacy ZippyQ", vec![open]);
        zippy.format = "zippyq".to_string();
        let zippy = reg
            .add_round(&event.id, zippy)
            .expect("a persisted zippyq round must still validate (shelved, not removed)");
        assert_eq!(zippy.format, "zippyq");

        // The event with both formats serializes and deserializes unchanged (the on-disk shape an
        // older Director wrote still loads bit-for-bit on the renamed/shelved build).
        let meta = reg
            .list()
            .into_iter()
            .find(|m| m.id == event.id)
            .expect("event present");
        let json = serde_json::to_string(&meta).unwrap();
        let restored: EventMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, meta);
        let formats: Vec<&str> = restored.rounds.iter().map(|r| r.format.as_str()).collect();
        assert!(formats.contains(&"timed_qual") && formats.contains(&"zippyq"));
    }

    #[test]
    fn from_ranking_deserializes_legacy_single_source_round() {
        // A round stored before issue #51 wrote a single `source_round` string. It must still
        // deserialize, lifting the legacy key into a one-element `source_rounds` list.
        let legacy = r#"{ "FromRanking": { "source_round": "qual-r1", "top_n": 4 } }"#;
        let rule: SeedingRule = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            rule,
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("qual-r1".into())],
                top_n: 4,
            }
        );

        // The current multi-round shape deserializes directly.
        let current =
            r#"{ "FromRanking": { "source_rounds": ["qual-r1", "qual-r2"], "top_n": 8 } }"#;
        let rule: SeedingRule = serde_json::from_str(current).unwrap();
        assert_eq!(
            rule,
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("qual-r1".into()), RoundId("qual-r2".into())],
                top_n: 8,
            }
        );

        // Round-trips through serialize → deserialize on the current shape.
        let json = serde_json::to_string(&rule).unwrap();
        let back: SeedingRule = serde_json::from_str(&json).unwrap();
        assert_eq!(rule, back);

        // The other variants are unaffected.
        let roster: SeedingRule = serde_json::from_str(r#""FromRoster""#).unwrap();
        assert_eq!(roster, SeedingRule::FromRoster);
        let channels: SeedingRule =
            serde_json::from_str(r#"{ "AllChannels": { "channels": [0, 1, 2] } }"#).unwrap();
        assert_eq!(
            channels,
            SeedingRule::AllChannels {
                channels: vec![0, 1, 2]
            }
        );

        // FromHeatWinners (bracket advancement, #217) deserializes from its single source round
        // and round-trips through serialize.
        let winners: SeedingRule =
            serde_json::from_str(r#"{ "FromHeatWinners": { "source_round": "quarters" } }"#)
                .unwrap();
        assert_eq!(
            winners,
            SeedingRule::FromHeatWinners {
                source_round: RoundId("quarters".into()),
            }
        );
        let json = serde_json::to_string(&winners).unwrap();
        assert_eq!(
            serde_json::from_str::<SeedingRule>(&json).unwrap(),
            winners,
            "FromHeatWinners round-trips through serialize → deserialize"
        );
    }

    #[test]
    fn multi_main_seeding_round_trips_and_is_back_compat() {
        use serde_json::from_str;

        // FromRankingRange: current `source_rounds` shape + skip/take.
        let range = SeedingRule::FromRankingRange {
            source_rounds: vec![RoundId("qual".into())],
            skip: 12,
            take: 8,
        };
        let json = serde_json::to_string(&range).unwrap();
        assert_eq!(from_str::<SeedingRule>(&json).unwrap(), range);

        // FromRankingRange accepts the legacy single `source_round` key (lifted to a one-element list).
        let legacy_range = r#"{"FromRankingRange":{"source_round":"qual","skip":12,"take":8}}"#;
        assert_eq!(from_str::<SeedingRule>(legacy_range).unwrap(), range);

        // Combine — including a NESTED Combine — round-trips through serialize → deserialize (the
        // recursive self-reference dispatches back through the hand-written Deserialize).
        let combine = SeedingRule::Combine {
            sources: vec![
                SeedingRule::FromRankingRange {
                    source_rounds: vec![RoundId("qual".into())],
                    skip: 6,
                    take: 6,
                },
                SeedingRule::Combine {
                    sources: vec![SeedingRule::FromRanking {
                        source_rounds: vec![RoundId("c-final".into())],
                        top_n: 2,
                    }],
                },
            ],
        };
        let json = serde_json::to_string(&combine).unwrap();
        assert_eq!(
            from_str::<SeedingRule>(&json).unwrap(),
            combine,
            "a nested Combine round-trips"
        );

        // The legacy variants are unaffected by the additive variants.
        assert_eq!(
            from_str::<SeedingRule>(r#""FromRoster""#).unwrap(),
            SeedingRule::FromRoster
        );
        assert_eq!(
            from_str::<SeedingRule>(r#"{"FromRanking":{"source_round":"q","top_n":4}}"#).unwrap(),
            SeedingRule::FromRanking {
                source_rounds: vec![RoundId("q".into())],
                top_n: 4,
            }
        );
        assert_eq!(
            from_str::<SeedingRule>(r#"{"FromHeatWinners":{"source_round":"qf"}}"#).unwrap(),
            SeedingRule::FromHeatWinners {
                source_round: RoundId("qf".into()),
            }
        );
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

    #[test]
    fn restores_an_older_additive_event_meta_json() {
        // P1-6 back-compat: a known-good *older* EventMeta JSON (only the pre-#73 core fields,
        // before timers/roster/classes/rounds existed) must still load on a newer Director — the
        // additive fields take their serde defaults. Mirrors the pilots/classes/timers back-compat
        // load tests, and guards the loud-skip logging added for unparseable meta.
        let dir = std::env::temp_dir().join(format!("gridfpv-meta-backcompat-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let id = EventId("legacy-event".into());
        let legacy_json = r#"{
            "id": "legacy-event",
            "name": "Legacy Spring Cup",
            "created_at": 1700000000000,
            "persistent": true
        }"#;
        {
            // Hand-write the older shape directly into the event's sqlite meta sidecar.
            let path = event_db_path(&dir, &id);
            let log = SqliteLog::open(&path).unwrap();
            log.set_meta(EVENT_META_KEY, legacy_json).unwrap();
        }
        // Reopen the registry over the dir → the older event restores with defaulted additive fields.
        let reg = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reg.meta_of(&id).expect("older event restored");
        assert_eq!(restored.name, "Legacy Spring Cup");
        assert_eq!(restored.created_at, 1_700_000_000_000);
        assert!(restored.persistent);
        assert!(restored.timers.is_empty());
        assert!(restored.roster.is_empty());
        assert!(restored.classes.is_empty());
        assert!(restored.rounds.is_empty());
        assert!(restored.classes_membership.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // --- Editing a round re-materializes its scheduled heats (#387) --------------------------

    /// An open-practice round over `channels` — the round the #387 report is written against (its
    /// heat's lineup *is* its channel set, so a channel edit must reach the already-filled heat).
    fn practice_round(channels: &[usize]) -> NewRoundReq {
        NewRoundReq {
            label: "Practice".to_string(),
            classes: vec![],
            format: OpenPractice::NAME.to_string(),
            params: BTreeMap::new(),
            win_condition: None,
            seeding: SeedingRule::AllChannels {
                channels: channels.to_vec(),
            },
            time_limit_secs: None,
            channel_mode: None,
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        }
    }

    /// The same round as an **edit**, over a (possibly different) channel set.
    fn practice_edit(label: &str, channels: &[usize]) -> UpdateRoundReq {
        UpdateRoundReq {
            label: label.to_string(),
            classes: vec![],
            format: OpenPractice::NAME.to_string(),
            params: BTreeMap::new(),
            win_condition: None,
            seeding: SeedingRule::AllChannels {
                channels: channels.to_vec(),
            },
            time_limit_secs: None,
            channel_mode: None,
            staging_timer_secs: None,
            start_procedure: None,
            grace_window: None,
            protest_window: None,
            min_lap_secs: None,
        }
    }

    /// Fill the round's next heat exactly as the control handler does, appending the tagged
    /// `HeatScheduled`, and return its id.
    fn fill_next_heat(reg: &EventRegistry, id: &EventId, round: &RoundId) -> HeatId {
        let meta = reg.meta_of(id).unwrap();
        let timers = reg.timers();
        let state = reg.resolve(id).unwrap();
        let (events, _) = state.read().unwrap();
        match round_engine::fill_round(&meta, &timers, round, &events).unwrap() {
            round_engine::FillOutcome::Scheduled {
                heat,
                lineup,
                frequencies,
                ..
            } => {
                let frequencies = match frequencies {
                    Some(freqs) => freqs,
                    None => round_engine::assign_for_event(&meta, &timers, &lineup).unwrap(),
                };
                state
                    .append(
                        Event::HeatScheduled {
                            heat: heat.clone(),
                            lineup,
                            class: round_engine::round_class(&meta, round),
                            round: Some(round.clone()),
                            frequencies,
                            label: None,
                        },
                        None,
                    )
                    .unwrap();
                heat
            }
            other => panic!("expected a scheduled heat, got {other:?}"),
        }
    }

    /// A heat's currently-effective `(lineup, frequencies)` — its most recent `HeatScheduled`.
    fn heat_now(
        reg: &EventRegistry,
        id: &EventId,
        heat: &HeatId,
    ) -> (Vec<CompetitorRef>, Vec<(CompetitorRef, u16)>) {
        let (events, _) = reg.resolve(id).unwrap().read().unwrap();
        let mut out = (Vec::new(), Vec::new());
        for event in &events {
            if let Event::HeatScheduled {
                heat: h,
                lineup,
                frequencies,
                ..
            } = event
            {
                if h == heat {
                    out = (lineup.clone(), frequencies.clone());
                }
            }
        }
        out
    }

    fn refs(names: &[&str]) -> Vec<CompetitorRef> {
        names.iter().map(|n| CompetitorRef((*n).into())).collect()
    }

    #[test]
    fn editing_a_round_rebuilds_its_scheduled_heat() {
        // #387: a filled practice heat baked in the round's channels. Editing the round used to
        // leave that heat untouched, so it raced the old channel set forever.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let round = reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
        let heat = fill_next_heat(&reg, &created.id, &round.id);
        assert_eq!(
            heat_now(&reg, &created.id, &heat).0,
            refs(&["node-0", "node-1"])
        );

        reg.update_round(
            &created.id,
            &round.id,
            practice_edit("Practice", &[2, 3, 4]),
        )
        .unwrap();

        assert_eq!(
            heat_now(&reg, &created.id, &heat).0,
            refs(&["node-2", "node-3", "node-4"]),
            "the scheduled heat now runs the round's new channels"
        );
    }

    #[test]
    fn editing_a_round_rebuilds_a_per_heat_heat_lineup_and_frequencies() {
        // Not practice-specific: a normal (per-heat) round's scheduled heat is rebuilt the same
        // way, channels re-assigned from the timer's pool.
        let reg = EventRegistry::new(None).unwrap();
        let class = reg
            .classes()
            .create(&crate::classes::CreateClassRequest {
                name: "Open".into(),
                ..Default::default()
            })
            .unwrap()
            .id;
        let created = reg.create(&req("Race Night")).unwrap();
        reg.set_timers(&created.id, vec![TimerId(MOCK_TIMER_ID.into())])
            .unwrap();
        reg.set_classes(&created.id, vec![class.clone()]).unwrap();
        let mut pilots: Vec<PilotId> = Vec::new();
        for callsign in ["A", "B", "C"] {
            pilots.push(
                reg.pilots()
                    .create(&crate::pilots::CreatePilotRequest {
                        callsign: callsign.into(),
                        ..Default::default()
                    })
                    .unwrap()
                    .id,
            );
        }
        reg.set_class_membership(&created.id, class.clone(), slots(pilots[..2].to_vec()))
            .unwrap();

        let round = reg
            .add_round(
                &created.id,
                NewRoundReq {
                    label: "Qualifying".to_string(),
                    classes: vec![class.clone()],
                    format: "timed_qual".to_string(),
                    params: BTreeMap::from([("rounds".to_string(), "1".to_string())]),
                    win_condition: None,
                    seeding: SeedingRule::FromRoster,
                    time_limit_secs: Some(120),
                    channel_mode: Some(ChannelMode::PerHeat),
                    staging_timer_secs: None,
                    start_procedure: None,
                    grace_window: None,
                    protest_window: None,
                    min_lap_secs: None,
                },
            )
            .unwrap();
        let heat = fill_next_heat(&reg, &created.id, &round.id);
        let (before_lineup, before_freqs) = heat_now(&reg, &created.id, &heat);
        assert_eq!(before_lineup.len(), 2);
        assert_eq!(before_freqs.len(), 2, "the mock timer assigns channels");

        // A third pilot joins the class, then the round is re-saved (label-only change on the
        // round itself — the field moved underneath it).
        reg.set_class_membership(&created.id, class.clone(), slots(pilots.clone()))
            .unwrap();
        reg.update_round(
            &created.id,
            &round.id,
            UpdateRoundReq {
                label: "Qualifying".to_string(),
                classes: vec![class],
                format: "timed_qual".to_string(),
                params: BTreeMap::from([("rounds".to_string(), "1".to_string())]),
                win_condition: None,
                seeding: SeedingRule::FromRoster,
                time_limit_secs: Some(120),
                channel_mode: Some(ChannelMode::PerHeat),
                staging_timer_secs: None,
                start_procedure: None,
                grace_window: None,
                protest_window: None,
                min_lap_secs: None,
            },
        )
        .unwrap();

        let (after_lineup, after_freqs) = heat_now(&reg, &created.id, &heat);
        assert_eq!(after_lineup.len(), 3, "the lineup was rebuilt");
        assert_ne!(before_lineup, after_lineup);
        assert_eq!(after_freqs.len(), 3, "channels were re-assigned for it");
        assert_ne!(before_freqs, after_freqs);
    }

    #[test]
    fn editing_a_round_leaves_a_raced_heat_untouched() {
        // A raced round's channel config is frozen, and its heats keep exactly what they raced.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let round = reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
        let heat = fill_next_heat(&reg, &created.id, &round.id);
        let state = reg.resolve(&created.id).unwrap();
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished,
            HeatTransition::Finalized,
        ] {
            state
                .append(
                    Event::HeatStateChanged {
                        heat: heat.clone(),
                        transition,
                    },
                    None,
                )
                .unwrap();
        }
        let before = heat_now(&reg, &created.id, &heat);

        // The channels are frozen once raced — the edit is refused …
        let err = reg
            .update_round(&created.id, &round.id, practice_edit("Practice", &[5, 6]))
            .unwrap_err();
        assert!(
            matches!(&err, RoundError::Invalid(msg) if msg.contains("raced heats")),
            "expected the raced freeze, got {err:?}"
        );
        // … and an edit that IS allowed on a raced round (the label) leaves the heat alone.
        reg.update_round(&created.id, &round.id, practice_edit("Renamed", &[0, 1]))
            .unwrap();
        assert_eq!(
            heat_now(&reg, &created.id, &heat),
            before,
            "a raced heat is never re-materialized"
        );
    }

    #[test]
    fn editing_a_round_is_refused_while_one_of_its_heats_is_in_progress() {
        // The binding rule (#387): staged / armed / running / unofficial all refuse the edit, and
        // the refusal names the heat by its FRIENDLY name — never the raw id (repo display rule).
        for transition in [
            HeatTransition::Staged,
            HeatTransition::Armed,
            HeatTransition::Running,
            HeatTransition::Finished, // → Unofficial
        ] {
            let reg = EventRegistry::new(None).unwrap();
            let created = reg.create(&req("Race Night")).unwrap();
            let round = reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
            let heat = fill_next_heat(&reg, &created.id, &round.id);
            let state = reg.resolve(&created.id).unwrap();
            for step in [
                HeatTransition::Staged,
                HeatTransition::Armed,
                HeatTransition::Running,
                HeatTransition::Finished,
            ] {
                state
                    .append(
                        Event::HeatStateChanged {
                            heat: heat.clone(),
                            transition: step,
                        },
                        None,
                    )
                    .unwrap();
                if step == transition {
                    break;
                }
            }

            let before = heat_now(&reg, &created.id, &heat);
            let err = reg
                .update_round(&created.id, &round.id, practice_edit("Practice", &[4, 5]))
                .unwrap_err();
            let RoundError::Invalid(msg) = &err else {
                panic!("expected a refusal for {transition:?}, got {err:?}");
            };
            assert!(
                msg.contains("heat in progress") && msg.contains("Practice Heat"),
                "the refusal must name the heat: {msg}"
            );
            assert!(
                !msg.contains(&heat.0),
                "the refusal must not leak the raw heat id: {msg}"
            );
            assert_eq!(
                heat_now(&reg, &created.id, &heat),
                before,
                "a refused edit changes nothing"
            );
        }
    }

    #[test]
    fn editing_a_round_is_refused_while_a_scheduled_heat_is_loaded_on_the_timer() {
        // Still `Scheduled`, but the RD has it up in Live control — it may be on deck with its
        // channels already read off, so rewriting its lineup underneath is the silent swap the
        // rule prevents.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let round = reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
        let heat = fill_next_heat(&reg, &created.id, &round.id);
        reg.resolve(&created.id)
            .unwrap()
            .append(Event::CurrentHeatSelected { heat: heat.clone() }, None)
            .unwrap();

        let err = reg
            .update_round(&created.id, &round.id, practice_edit("Practice", &[4, 5]))
            .unwrap_err();
        assert!(
            matches!(&err, RoundError::Invalid(msg) if msg.contains("Practice Heat")),
            "expected the on-timer refusal, got {err:?}"
        );
    }

    #[test]
    fn editing_a_round_appends_nothing_when_the_heats_do_not_change() {
        // A label-only edit must not churn the log with an identical re-schedule.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let round = reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
        fill_next_heat(&reg, &created.id, &round.id);
        let state = reg.resolve(&created.id).unwrap();
        let before = state.read().unwrap().0.len();

        reg.update_round(&created.id, &round.id, practice_edit("Renamed", &[0, 1]))
            .unwrap();
        assert_eq!(state.read().unwrap().0.len(), before);
    }
}
