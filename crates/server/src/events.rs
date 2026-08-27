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
//!   schema). A registry with no configured data dir falls back to an **in-memory** log
//!   ([`InMemoryLog`](gridfpv_storage::InMemoryLog)) per created event, so an unconfigured
//!   Director (and the tests) still work — non-durably.
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
use gridfpv_engine::imd::{ImdReading, imd_reading};
use gridfpv_engine::scoring::WinCondition;
use gridfpv_events::{HeatId, RoundId};
use gridfpv_storage::{InMemoryLog, SqliteLog};

use crate::app::AppState;
use crate::auth::TokenStore;
use crate::classes::ClassDirectory;
use crate::pilots::PilotDirectory;
use crate::round_engine;
use crate::scope::{ClassId, EventId, PilotId};
use crate::timers::{MOCK_TIMER_ID, Timer, TimerId, TimerRegistry};

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
/// creation time, and whether the event is **persistent** (file-backed) or ephemeral (an
/// in-memory log, when the Director has no data dir configured). Derives serde (its JSON *is*
/// the wire form) and `ts_rs::TS`
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
    /// on the wire).
    #[ts(type = "number")]
    pub created_at: i64,
    /// Whether the event's log is durable (a SQLite file) or ephemeral (an in-memory log —
    /// `false`, which happens only when no data dir is configured).
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
    /// events default to `["mock"]` (the built-in Mock) so they work out of the
    /// box. The per-event source bridge runs the selected Sim timers; a selected RotorHazard timer is
    /// dialled by the RH connection reconciler instead (#65/#73), not by this bridge.
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
    /// roster; a new event defaults to an **empty** roster. Channels (which frequency a
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
    /// selection; a new event defaults to an **empty** selection. This is the registry
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
    /// before Slice 1a reads back with no membership; a new event defaults to an
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
    /// Slice 2a reads back with no rounds; a new event defaults to an **empty** list. The
    /// whole field round-trips through the event's persisted meta (issue #115), so it is restart-safe
    /// for free.
    ///
    /// Read through [`lenient_rounds`]: a stored round the current code can no longer parse — a
    /// [`SeedingRule`] variant that has been renamed, say — is **dropped**, and the event still
    /// opens. Losing one round is a thing the RD can recreate; an event that will not open is not
    /// (`CLAUDE.md`, "a stored record in an old shape must still LOAD").
    #[serde(
        default,
        deserialize_with = "lenient_rounds",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub rounds: Vec<RoundDef>,
    /// The event's **channel layouts** (#117 S2) — the event-scoped answer to *what goes on which
    /// node?*
    ///
    /// Each [`ChannelLayout`] is one complete tuning of the event's timer (one channel per enabled
    /// node), drawn from the timer's **allowed** set ([`Timer::available_channels`], S1). A bracket
    /// runs off one layout all tournament; a GQ-style qualifier defines many so each pilot keeps
    /// their own channel — the RD picks the strategy, the model does not.
    ///
    /// **This is the field that stops "editing channels in the event" from mutating the global timer
    /// record.** It sits beside [`timers`](Self::timers) / [`roster`](Self::roster) /
    /// [`classes`](Self::classes) because it is the same kind of thing: an event-scoped decision,
    /// next to the log rather than in it. Global is the seed, the event owns what it runs — the same
    /// layering as #411's base profile → event tune.
    ///
    /// # Who reads it (#117 S3)
    ///
    /// A **round** names the layouts its heats may fly ([`RoundDef::layouts`]); a **heat** binds one
    /// ([`Event::HeatLayoutSet`](gridfpv_events::Event::HeatLayoutSet)), and that binding is what
    /// its channels are assigned from
    /// ([`assign_from_layout`](crate::round_engine::assign_from_layout)). The console resolves a
    /// seat's channel through the same mapping.
    ///
    /// Additive (`#[serde(default)]`, omitted from the wire when empty) so an event persisted
    /// before #117 S2 reads back with no layouts; a new event defaults to an **empty** list (the RD
    /// defines the first one). The whole field round-trips through the event's persisted meta
    /// (issue #115), so it is restart-safe for free. An event stored under the pre-rename
    /// `channel_layers` key simply **loads with no layouts** — pre-release, a stale record may lose
    /// a field, but it must never fail to open.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channel_layouts: Vec<ChannelLayout>,
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

// ── Event channel layouts (#117 S2) ──────────────────────────────────────────────────────────────
//
// Three scopes answer three different questions about channels, and conflating any two of them has
// been this repo's most repeated bug (#402, #412, #413, #416):
//
// | scope             | question                          | state                                |
// |-------------------|-----------------------------------|--------------------------------------|
// | Global (a timer)  | what may this timer *ever* use?   | [`Timer::available_channels`] (S1)   |
// | **Event**         | **what goes on which node?**      | **[`ChannelLayout`] — this slice**    |
// | Heat              | which layout does this heat fly?   | S3, not built                        |
//
// A layout is **event-scoped**, and that is the point. Today the Timers-page checkboxes edit one
// per-timer field, and the event workspace embeds the *same* `TimerManager` — so editing channels
// "in the event" mutates the **global** timer record. Layouts live on [`EventMeta`] beside
// `timers` / `roster` / `classes`, the same place every other event-scoped decision already lives,
// and they are written through to the event's SQLite `meta` table (issue #115) so they are
// restart-safe for free. The global allowed set is the **seed**; the event owns what it runs —
// deliberately the same layering as #411's base-profile → event-tune, so there is one mental model
// for both.

/// Identifies one **channel layout** within an event — re-exported from the event model.
///
/// **Auto-generated** (a slug of the layout's name plus a short random suffix — the same id-gen as
/// events / pilots / rounds), never user-entered, and a **wire handle only**: what an RD reads is
/// [`ChannelLayout::name`] (CLAUDE.md's display rule).
///
/// It lives in `gridfpv_events` beside [`ClassId`] / [`RoundId`] because #117 S3 tags a scheduled
/// heat with the layout it flies, and a fact about a heat belongs in the log. Re-exported here so
/// the config side (this module) and the log side name the *same* type — the discipline that keeps
/// `ClassId` from meaning two things.
pub use gridfpv_events::LayoutId;

/// One node's tuning within a [`ChannelLayout`] (#117 S2): the node index and the raw-MHz channel it
/// is tuned to.
///
/// **0-based on the wire, 1-based on screen** — index `2` is the node the RD calls "Node 3"
/// ([`Timer::node_label`]), and `channel` is a raw frequency that renders through
/// [`crate::timers::channel_label`] as `"Raceband R7"`. Neither raw value may reach a person; both
/// are wire handles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LayoutNode {
    /// The node's index on the timer, **0-based** — the same index [`TimerNode`] carries, and the
    /// one `NodeSignal::node` and the `node-{i}` seat ref mean.
    pub node: u32,
    /// The raw centre frequency in **MHz** this node is tuned to in this layout. Must be one of the
    /// timer's [`available_channels`](Timer::available_channels) — the *allowed* set S1 clarified.
    pub channel: u16,
}

/// One event **channel layout** (#117 S2): a complete tuning of the event's timer — one channel per
/// enabled node.
///
/// ```text
/// Layout A:  Node 1→R1  Node 2→R2  Node 3→R3  Node 4→R4
/// Layout B:  Node 1→F1  Node 2→F2  Node 3→F4  Node 4→F8
/// ```
///
/// # Why a layout, and why the system does not choose one for you
///
/// The RD picks the strategy, per format, and both strategies fall out of this one mechanism with
/// no special case:
///
/// - a **bracket** is *one layout for the whole tournament* — n channels for n pilots per heat, and
///   they never move;
/// - a **GQ-style qualifier** defines *many* layouts so each pilot can stay on their own channel.
///
/// So nothing here encodes a policy that forces either. What the model does enforce is that a layout
/// is a **complete, conflict-free tuning**: every enabled node has exactly one channel, and no two
/// nodes share one (a node cannot share a frequency with its neighbour). Reusing a channel *between*
/// layouts is a [`LayoutOverlap`] **warning**, never a refusal — it only matters for the
/// keep-pilots-on-one-channel strategy, and an RD running a bracket off a single layout does not
/// care.
///
/// Carried in [`EventMeta::channel_layouts`]. **Not yet wired into heat filling or
/// `assign_frequencies`** — which heat flies which layout is S3.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ChannelLayout {
    /// The stable, **auto-generated** handle (a slug of [`name`](Self::name) + a short random
    /// suffix). Never user-entered; the name is display-only but is what a person reads.
    pub id: LayoutId,
    /// The RD-typed display name (`"Bracket A"`, `"Qual pack 2"`). Non-empty after trimming, and
    /// unique within the event — two layouts called "Bracket A" is a mis-click, not a choice.
    pub name: String,
    /// The node → channel mapping, **ascending by node**, one entry per enabled node of the event's
    /// timer. Complete and duplicate-free (see the type doc).
    pub nodes: Vec<LayoutNode>,
}

impl ChannelLayout {
    /// The channel this layout tunes `node` to, or `None` when the layout says nothing about it (a
    /// layout stored before the RD enabled that node).
    ///
    /// **The per-node mapping the allowed set never had.** `competitorName.ts`'s resolver source (3)
    /// currently reads a seat's channel as `available_channels[node]`, which S1 documented as a
    /// plausible-looking fabrication; this is the value that replaces it once a heat names a layout
    /// (S3).
    pub fn channel_for(&self, node: u32) -> Option<u16> {
        self.nodes
            .iter()
            .find(|n| n.node == node)
            .map(|n| n.channel)
    }

    /// Every channel this layout uses, ascending and de-duplicated — the join key for
    /// [`layout_overlaps`].
    pub fn channels(&self) -> Vec<u16> {
        let mut out: Vec<u16> = self.nodes.iter().map(|n| n.channel).collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Two layouts that share at least one channel (#117 S2) — a **warning**, never a refusal.
///
/// The RD settled this explicitly: *"if I have R1-4 in one layout, I cannot use R1-4 in the next"*
/// only matters for the **keep-pilots-on-one-channel** strategy. If layouts are just timer tunings,
/// reusing a channel across them is harmless — so it is flagged so an RD pursuing that strategy
/// sees it, and it never blocks an RD who does not care. Nothing in the write path consults this;
/// it is computed on top of a layout set that has *already been accepted*.
///
/// Carries the two [`LayoutId`]s as **wire handles** and the shared channels as raw MHz. Both resolve
/// to names in the console (layout id → `ChannelLayout.name`, MHz → `channelLabel`), which is the
/// repo's rule: ids travel, names display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LayoutOverlap {
    /// The **earlier** of the two layouts, in the event's layout order.
    pub layout: LayoutId,
    /// The **later** of the two layouts, in the event's layout order.
    pub other: LayoutId,
    /// The channels both layouts use, ascending. Never empty (an empty intersection is not reported).
    pub channels: Vec<u16>,
}

/// One layout's **IMD reading** (#117 S4) — how cleanly its channels fly together, and what the
/// worst offending mixing product is.
///
/// Keyed by [`LayoutId`] rather than positional, for the same reason [`LayoutOverlap`] carries ids:
/// a parallel array silently mis-labels every layout the day someone filters the list.
///
/// **Advisory, exactly like [`LayoutOverlap`].** Nothing in the write path consults it — a layout
/// with a poor rating saves like any other, because the RD may have no better option and a
/// Raceband-only timer genuinely cannot do better than 0 at five pilots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct LayoutRating {
    /// The layout this reading is for.
    pub layout: LayoutId,
    /// The reading itself — IMDTabler's rating plus the worst two-tone product, or no offender at
    /// all when the set is clean.
    pub imd: ImdReading,
}

/// An event's layouts **and what is worth telling the RD about them** (#117 S2, #117 S4) — the body
/// of `GET /events/{id}/layouts`, and of every layout write.
///
/// One view type for the read and all three writes so the console never has to re-derive the
/// warnings from a write's response: a write returns the resulting whole picture, exactly like the
/// read. Errors do not appear here — they are refusals, returned as a typed 400 with a sentence the
/// RD can act on. This carries only the things that are *allowed* and still worth flagging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct ChannelLayouts {
    /// The event's layouts, in definition order.
    pub layouts: Vec<ChannelLayout>,
    /// Cross-layout channel reuse ([`LayoutOverlap`]) — **advisory**. Empty when no two layouts share
    /// a channel, and empty is *not* a goal: a bracket run off one layout trivially has none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub overlaps: Vec<LayoutOverlap>,
    /// Each layout's IMD reading (#117 S4), one entry per layout, in the same order — **advisory**,
    /// and never a reason to refuse anything.
    ///
    /// Computed here, from [`gridfpv_engine::imd`], so the console never carries a second
    /// implementation of IMDTabler. That is the whole point of #430: an RD must read the *same*
    /// number off GridFPV that they read off RotorHazard for the same channels, and two ports of
    /// the same algorithm is exactly how that stops being true.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ratings: Vec<LayoutRating>,
}

impl ChannelLayouts {
    /// The whole view for a layout set: the layouts, their cross-layout overlaps, and each one's
    /// IMD reading.
    ///
    /// The single constructor for all four routes (the read and the three writes) — the advisories
    /// are properties of the accepted set, so building them in one place is what keeps a write's
    /// answer identical to the read's.
    fn of(layouts: Vec<ChannelLayout>) -> Self {
        Self {
            overlaps: layout_overlaps(&layouts),
            ratings: layouts
                .iter()
                .map(|l| LayoutRating {
                    layout: l.id.clone(),
                    imd: imd_reading(&l.channels()),
                })
                .collect(),
            layouts,
        }
    }
}

/// The body of `POST /events/{id}/layouts` — define a new channel layout (#117 S2).
///
/// The id is generated server-side (never user-entered). `nodes` is **optional and that is the
/// global→event seam**: omit it and the layout is *seeded* from the event timer's allowed set —
/// enabled node *i* takes the *i*-th allowed channel, in the RD's own preference order. That is the
/// whole of "global is the default subset an event starts from"; from the moment the layout exists
/// it is event state, and editing it never touches [`Timer::available_channels`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct NewChannelLayoutRequest {
    /// The layout's display name. Trimmed; must be non-empty and unique within the event.
    pub name: String,
    /// The explicit node → channel mapping, or omitted to **seed** it from the timer's allowed set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub nodes: Option<Vec<LayoutNode>>,
}

/// The body of `PUT /events/{id}/layouts/{layout_id}` — replace a layout's editable fields (#117 S2).
///
/// The [`LayoutId`] is fixed (it is the path segment); the name and the whole mapping are replaced
/// wholesale, and re-validated exactly as on create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct SetChannelLayoutRequest {
    /// The layout's display name. Trimmed; must be non-empty and unique within the event.
    pub name: String,
    /// The complete node → channel mapping (one entry per enabled node, no duplicate channels).
    pub nodes: Vec<LayoutNode>,
}

/// Why a channel-layout write was refused (#117 S2) — the twin of [`RoundError`].
///
/// Every [`Invalid`](LayoutError::Invalid) message is written to be **read by an RD at a venue**: it
/// names the layout, the node (`"Node 3"`) and the channel (`"Raceband R7"`) by their friendly names
/// and says what to do next — never a raw index, a bare MHz, or a timer id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutError {
    /// No event with the given id — a typed 404.
    EventNotFound(String),
    /// No layout with the given id in this event — a typed 404.
    LayoutNotFound(String),
    /// The layout is not a valid tuning (duplicate channel, a channel outside the allowed set, a
    /// disabled/out-of-range node, an incomplete mapping, a blank/duplicate name) — a 400.
    Invalid(String),
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LayoutError::EventNotFound(id) => write!(f, "no event with id {id:?}"),
            LayoutError::LayoutNotFound(id) => write!(f, "no channel layout with id {id:?}"),
            LayoutError::Invalid(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for LayoutError {}

impl From<RegistryError> for LayoutError {
    fn from(e: RegistryError) -> Self {
        LayoutError::Invalid(e.message)
    }
}

/// Every pair of layouts that shares a channel (#117 S2), in event order — the **warning** half of
/// the layout model.
///
/// Pure and total: it takes the layout list and answers, with no notion of whether the set is
/// "good". Cross-layout reuse is legal by decision (see [`LayoutOverlap`]), so this never gates a
/// write — [`EventRegistry::add_channel_layout`] and friends compute it *after* the layout has been
/// accepted and persisted, purely so the console has something to show.
pub fn layout_overlaps(layouts: &[ChannelLayout]) -> Vec<LayoutOverlap> {
    let mut out = Vec::new();
    for (i, layout) in layouts.iter().enumerate() {
        let mine = layout.channels();
        for other in &layouts[i + 1..] {
            let shared: Vec<u16> = other
                .channels()
                .into_iter()
                .filter(|c| mine.contains(c))
                .collect();
            if !shared.is_empty() {
                out.push(LayoutOverlap {
                    layout: layout.id.clone(),
                    other: other.id.clone(),
                    channels: shared,
                });
            }
        }
    }
    out
}

/// The event's timer for the purpose of layouts (#117 S2) — its **effective primary**.
///
/// A layout is one tuning of *the* timer, and #112's redundant timers are two boxes at **one gate**:
/// an alternate that takes over mid-event has to be listening on the same channels, so one layout
/// per event (validated against the primary) is the honest model, not one layout per timer. This is
/// also the timer `set_class_membership` already validates a pilot's fixed channel against, so the
/// two channel surfaces cannot disagree about which timer they mean.
fn layout_timer(meta: &EventMeta, timers: &TimerRegistry) -> Result<Timer, LayoutError> {
    meta.effective_primary()
        .and_then(|id| timers.get(&id))
        .ok_or_else(|| {
            LayoutError::Invalid(
                "this event has no timer selected, so there is no node set to tune — \
                 pick a timer for this event before defining a channel layout."
                    .to_string(),
            )
        })
}

/// **Seed** a layout from the timer's allowed set (#117 S2) — the global→event seam.
///
/// Enabled node *i* takes the *i*-th allowed channel, in the RD's own preference order: the global
/// set is a *default subset an event starts from*, and from here on the layout is event state that
/// no edit to the timer record can reach.
///
/// Two refusals, both S1's semantics applied one level up. An **empty** allowed set is "the RD has
/// not configured this timer" and never "this timer has no channels" — seeding from the catalog
/// would scatter a layout across the band with no intent behind it. And **fewer allowed channels
/// than enabled nodes** cannot produce a complete tuning at all; saying so, with both numbers and
/// both repairs, is more use than a half-filled layout.
fn seed_layout_nodes(timer: &Timer) -> Result<Vec<LayoutNode>, LayoutError> {
    let enabled = timer.enabled_nodes();
    if timer.available_channels.is_empty() {
        return Err(LayoutError::Invalid(format!(
            "{:?} has no channels configured — choose the channels it may use on the Timers page \
             before defining a channel layout.",
            timer.name
        )));
    }
    if timer.available_channels.len() < enabled.len() {
        return Err(LayoutError::Invalid(format!(
            "{:?} allows {} channels but has {} enabled nodes, and a layout tunes every node. Allow \
             more channels on the Timers page, or disable the nodes this event will not fly.",
            timer.name,
            timer.available_channels.len(),
            enabled.len()
        )));
    }
    Ok(enabled
        .into_iter()
        .zip(timer.available_channels.iter().copied())
        .map(|(node, channel)| LayoutNode { node, channel })
        .collect())
}

/// Validate one layout against the event's timer and the event's other layouts (#117 S2).
///
/// The rules, in the order an RD hits them:
///
/// 1. the **name** is non-blank and not already used by another layout of this event;
/// 2. every node is **enabled and on the timer** (#412 — a disabled node seats nobody, so tuning it
///    is at best pointless and at worst hides a dead gate);
/// 3. every channel is in the timer's **allowed set** (S1's "allowed", not "capable");
/// 4. **no two nodes share a channel** — the one hard rule inside a layout: a node cannot share a
///    frequency with its neighbour;
/// 5. the mapping is **complete** — one channel for every enabled node, because a layout is a
///    complete tuning of the timer.
///
/// Cross-layout channel reuse is deliberately **absent** from this list: it is a [`LayoutOverlap`]
/// warning, computed after the fact, and never a refusal.
fn validate_layout(
    timer: &Timer,
    layouts: &[ChannelLayout],
    editing: Option<&LayoutId>,
    name: &str,
    nodes: &[LayoutNode],
) -> Result<(), LayoutError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(LayoutError::Invalid(
            "a channel layout needs a name — it is what you pick when a heat flies it.".to_string(),
        ));
    }
    if let Some(clash) = layouts
        .iter()
        .find(|l| Some(&l.id) != editing && l.name.trim().eq_ignore_ascii_case(name))
    {
        return Err(LayoutError::Invalid(format!(
            "this event already has a channel layout called {:?} — give this one a different name.",
            clash.name
        )));
    }
    if timer.available_channels.is_empty() {
        return Err(LayoutError::Invalid(format!(
            "{:?} has no channels configured — choose the channels it may use on the Timers page \
             before defining a channel layout.",
            timer.name
        )));
    }
    let mut seen_nodes: Vec<u32> = Vec::with_capacity(nodes.len());
    for entry in nodes {
        // #412: the node must exist on the timer AND be one the RD has left enabled. `node_view()` /
        // `GET /timers/{id}/nodes` is the console's half of this same rule.
        if !timer.node_enabled(entry.node) {
            return Err(LayoutError::Invalid(format!(
                "{} is not available on {:?} — it is disabled or does not exist, so a layout cannot \
                 tune it.",
                Timer::node_label(entry.node),
                timer.name
            )));
        }
        if seen_nodes.contains(&entry.node) {
            return Err(LayoutError::Invalid(format!(
                "{} is listed twice in this layout — a node has exactly one channel.",
                Timer::node_label(entry.node)
            )));
        }
        seen_nodes.push(entry.node);
        // S1's clarified semantics: `available_channels` is what this timer MAY use. A layout draws
        // from it and nothing else — never from the catalog, which would invent a channel the RD
        // never allowed.
        if !timer.available_channels.contains(&entry.channel) {
            return Err(LayoutError::Invalid(format!(
                "{} is not one of the channels {:?} is allowed to use — tick it on the Timers page \
                 first, or pick another channel for {}.",
                crate::timers::channel_label(entry.channel),
                timer.name,
                Timer::node_label(entry.node)
            )));
        }
        // The one hard rule inside a layout.
        if let Some(clash) = nodes
            .iter()
            .find(|n| n.node != entry.node && n.channel == entry.channel)
        {
            let (first, second) = if clash.node < entry.node {
                (clash.node, entry.node)
            } else {
                (entry.node, clash.node)
            };
            return Err(LayoutError::Invalid(format!(
                "{} and {} are both on {} in this layout — two nodes cannot share a frequency.",
                Timer::node_label(first),
                Timer::node_label(second),
                crate::timers::channel_label(entry.channel)
            )));
        }
    }
    // A layout is a COMPLETE tuning: every enabled node flies something. An incomplete layout would
    // leave a gate on whatever it happened to be tuned to last — the D27 hole this model closes.
    let missing: Vec<u32> = timer
        .enabled_nodes()
        .into_iter()
        .filter(|node| !seen_nodes.contains(node))
        .collect();
    if let Some(&first) = missing.first() {
        let labels: Vec<String> = missing.iter().map(|n| Timer::node_label(*n)).collect();
        return Err(LayoutError::Invalid(format!(
            "this layout does not tune {} — a layout sets a channel for every enabled node on {:?}. \
             Still to set: {}.",
            Timer::node_label(first),
            timer.name,
            labels.join(", ")
        )));
    }
    Ok(())
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
    /// The **channel layouts this round's heats may fly** (#117 S3) — the round scope of the
    /// three-scope channel model.
    ///
    /// A layout is a complete `node → channel` tuning of the event's timer
    /// ([`EventMeta::channel_layouts`]); a round *names* the ones its heats may choose from, and
    /// the RD's strategy falls out of how many it names:
    ///
    /// - **one** — the bracket case. *"n channels where n is the number of pilots per heat, and
    ///   those channels stay for the whole tournament."* Every heat the round draws flies that
    ///   layout automatically; there is nothing per-heat to do.
    /// - **several** — the round's heats **alternate** across them, round-robin by each heat's
    ///   position in the round: heat 1 flies the first, heat 2 the second, and back round again
    ///   (#117 S3). The point is what that buys with no pilot awareness at all — adjacent heats stop
    ///   sharing channels, so a group landing does not sit on the frequencies of the group staging
    ///   behind it. The RD still re-picks any individual heat, and that pick wins. Keeping pilots
    ///   on their own channel (the GQ strategy) is a *different* problem and is #419's, deferred.
    /// - **none** (the default, and every round persisted before S3) — the round names no layout,
    ///   so its heats fall back to the auto-pick from the timer's allowed set. Unchanged behaviour.
    ///
    /// Order is meaningful: it is the order the round's heats cycle through. Each id must name a
    /// layout the event actually has (checked on add *and* update), and no layout may be named
    /// twice — a repeat would only skew the cycle. A layout a round names cannot be deleted out
    /// from under it. Additive (`#[serde(default)]`) so pre-S3 meta reads back empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<LayoutId>,
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
/// **timer nodes** ([`ActiveNodes`](Self::ActiveNodes)) rather than pilots. Derives serde +
/// `ts_rs::TS`.
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
    /// Seed from a set of active **timer nodes** rather than pilots — the **open-practice** seeding
    /// (open-practice format). The field builder lays each node index out as a `node-{i}`
    /// [`CompetitorRef`](gridfpv_events::CompetitorRef) (the timer-seat handle the timer emits
    /// passes for), and the one open heat runs over those seats. Those seats are **unbound** (no
    /// pilot registration) — which is fine: their laps are appended to the durable log like every
    /// other format's (D5, reversed — see [`crate::open_practice`]); practice's only difference is
    /// that it is never scored. An open-practice round is `format: "open_practice"` +
    /// `seeding: ActiveNodes { nodes }`; its [`classes`](RoundDef::classes) may be empty (it is
    /// not a class round).
    ///
    /// # Nodes, not channels
    ///
    /// This variant was called `AllChannels { channels }` until #117 S3's follow-up, and the name
    /// lied twice: the entries were never channels (they are **node indices**), and the set is a
    /// *subset* the RD picks, not "all" of anything. That name is a large part of why #416's
    /// `[6]` — node 6 of a **four-node** timer, a heat that could never record a lap — read as
    /// fine. What each node is *tuned to* is a [`ChannelLayout`] (#117 S2/S3), a different
    /// vocabulary entirely; keeping the two apart is the point of the rename.
    ///
    /// The old tag is **not** accepted on read (no `serde(alias)`, no migration — see the
    /// pre-release rule in `CLAUDE.md`). A stored round written under it is dropped when the event
    /// loads and the RD recreates it; the **event still opens** (see `lenient_rounds`).
    ActiveNodes {
        /// The active **node indices** (the timer-seat indices the RD made live), laid out as
        /// `node-{i}` competitor refs by the field builder, in this order. These are seats on the
        /// timer, never frequencies — what a seat is *tuned to* comes from the round's
        /// [`ChannelLayout`].
        nodes: Vec<usize>,
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
            ActiveNodes { nodes: Vec<usize> },
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
            Shadow::ActiveNodes { nodes } => Ok(SeedingRule::ActiveNodes { nodes }),
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

/// Read [`EventMeta::rounds`] **leniently**: a stored round that the current code can no longer
/// parse is **dropped**, and the rest of the event loads normally.
///
/// # Why the whole event must not fall over
///
/// `EventMeta` is read back from the event's sidecar `meta` table on boot, and
/// [`restore_persisted_events`] skips an event whose meta will not parse — so *one* unreadable
/// round would vanish the entire event, its heats, its results and all. `CLAUDE.md` is explicit
/// that this is the line: a pre-release rename may lose a stored record, but "failing to open the
/// event, or 500ing, is not" excused.
///
/// This is what makes a breaking [`SeedingRule`] change affordable without a `serde(alias)` or a
/// migration. The rename of `AllChannels { channels }` → [`SeedingRule::ActiveNodes`] is exactly
/// that case: a round stored under the old tag no longer matches any variant, is dropped **here**
/// with a loud line on stderr, and the RD recreates it — while the event opens.
///
/// Not a compatibility shim: nothing here understands any old shape. It is the drop-one-record
/// boundary, and it is deliberately as narrow as it can be (one round, not the list, not the meta).
fn lenient_rounds<'de, D>(deserializer: D) -> Result<Vec<RoundDef>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    // `EventMeta` is only ever read from JSON (the sidecar `meta` table and the HTTP wire), so
    // buffering each round as a `Value` and re-parsing it is exact — the round that fails is the
    // only one lost.
    let raw = Vec::<serde_json::Value>::deserialize(deserializer)?;
    let mut out = Vec::with_capacity(raw.len());
    for value in raw {
        match serde_json::from_value::<RoundDef>(value) {
            Ok(round) => out.push(round),
            // LOUD, never silent: a dropped round is a thing the RD has to recreate, and finding
            // that out in the field is exactly the outcome the rule forbids.
            Err(e) => eprintln!(
                "WARNING: dropping a stored round that no longer parses — recreate it on the \
                 event's Rounds page: {e}"
            ),
        }
    }
    Ok(out)
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
    /// The **channel layouts** this round's heats may fly (#117 S3). Optional — omit for none (the
    /// auto-pick, the pre-S3 behaviour). Each must name a layout this event has, and none twice.
    /// Stored on [`RoundDef::layouts`]; naming several makes the round's heats **alternate** across
    /// them in this order (#117 S3), which the RD may still override per heat.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<LayoutId>,
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
    /// The new **channel layouts** this round's heats may fly (#117 S3), replaced wholesale.
    /// Optional — omit for none (the auto-pick). Each must name a layout this event has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layouts: Vec<LayoutId>,
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
    /// One of the event's [`channel_layouts`](Self::channel_layouts) by id (#117 S3), or `None`
    /// when the event has no such layout.
    ///
    /// The one lookup every layout consumer goes through — a round resolving the layouts it names,
    /// a heat resolving the layout it flies — so a bind naming a layout that has since been deleted
    /// resolves the same way everywhere: to nothing, reported by
    /// [`round_issues`](EventRegistry::round_issues), never to a guess.
    pub fn layout(&self, id: &LayoutId) -> Option<&ChannelLayout> {
        self.channel_layouts.iter().find(|l| &l.id == id)
    }

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
/// the wire means two events can share a name without colliding and a client cannot pick its
/// own id.
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

impl CreateEventRequest {
    /// A **name-only** create request — the one-click path the console's "create your first
    /// event" affordance and the setup wizard both take, and the shape every test uses to build
    /// its event now that there is no built-in one (#414).
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            date: None,
            location: None,
            description: None,
            organizer: None,
        }
    }
}

/// One registered event: its metadata plus the [`AppState`] (its own log + append-notify,
/// the shared token store) every per-event surface serves against.
struct RegisteredEvent {
    meta: EventMeta,
    state: AppState,
}

/// The registry of all events on this Director (issue #72) — the backend-agnostic
/// `EventRegistry` the routing layout resolves an [`EventId`] through.
///
/// Maps each [`EventId`] to its [`AppState`] (and so its own [`EventLog`]). A fresh registry
/// holds **no events at all** (#414 removed the built-in in-memory Practice event) — the RD
/// creates the first one. Created events get a file-backed [`SqliteLog`](gridfpv_storage::SqliteLog) under the
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
    /// `EventId → RegisteredEvent`. A `BTreeMap` so listing is deterministic (id order).
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
/// read off) layout that on top rather than widening this.
fn is_racing_phase(state: gridfpv_engine::heat::HeatState) -> bool {
    use gridfpv_engine::heat::HeatState;
    matches!(
        state,
        HeatState::Staged | HeatState::Armed | HeatState::Running | HeatState::Unofficial
    )
}

/// What a round's heats say about how far its config may still move — and whether the round may
/// still be **removed** (#418) — the answer
/// [`round_heat_facts`](EventRegistry::round_heat_facts) folds off the event's log in ONE pass.
///
/// # One vocabulary for "not active"
///
/// Both fields carry a **friendly heat name** rather than a bare flag, and both are populated by
/// the same phase test the timer-side refusals use ([`is_racing_phase`], the set behind
/// [`EventRegistry::heat_in_progress_on_timer`] / [`scored_heat_in_progress_on_timer`]). Round
/// deletion used to reason about mere *existence* while the timer actions reasoned about phase —
/// two vocabularies for one question (#418). There is now one: a heat is either **in progress**,
/// or it **carries results**, or it is unstarted and holds nothing worth protecting.
///
/// [`scored_heat_in_progress_on_timer`]: EventRegistry::scored_heat_in_progress_on_timer
#[derive(Debug, Default)]
struct RoundHeatFacts {
    /// The **friendly name** of the first heat of this round that has left `Scheduled` (staged /
    /// raced / scored), or `None` when every heat is still unstarted.
    ///
    /// Scoring re-derives from the round's CURRENT config on every read, so editing a raced
    /// round's scoring fields would silently rewrite already-official results
    /// ([`EventRegistry::update_round`] rejects that) — and *removing* the round would strand
    /// those results without the scoring config that produced them
    /// ([`EventRegistry::remove_round`] rejects that).
    ///
    /// It carries the **name**, not the id: it goes straight into an RD-facing refusal, and a raw
    /// id must never reach a user (repo display rule).
    raced: Option<String>,
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
    /// Build a registry over `data_dir`, restoring every event previously created there.
    ///
    /// A registry over a fresh data dir holds **no events** (#414): there is no built-in event,
    /// so the RD's first act on a new Director is to create one. When `data_dir` is `Some`,
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

        if let Some(dir) = &data_dir {
            std::fs::create_dir_all(dir).map_err(|e| {
                RegistryError::io(format!("could not create data dir {}: {e}", dir.display()))
            })?;
            // Reload every previously-created event (issue #111): scan the data dir for the
            // per-event `<id>.sqlite` files and restore each event's `EventMeta` + its log into
            // the registry. Without this the registry booted empty every time, so created events
            // vanished on a Director restart (and the persisted active-event id degraded to the
            // picker because its event wasn't loaded).
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

    // ── Event channel layouts (#117 S2) ──────────────────────────────────────────────────────────
    //
    // The event-scoped answer to *what goes on which node?*. Mirrors the rounds API exactly — a
    // generated id, individual add / update / remove, every write persisted through to the event's
    // SQLite `meta` table (issue #115) — because a layout is the same kind of thing as a round: a
    // named, RD-authored piece of event configuration.
    //
    // Every one of these returns the whole resulting [`ChannelLayouts`] view rather than just the
    // layout that changed. The overlap warnings are a property of the *set*, so a write that returns
    // only its own layout would leave the console to re-derive them — a second implementation of a
    // rule, which is how rules drift.

    /// An event's [`ChannelLayouts`] — the layouts plus their cross-layout overlap warnings (#117 S2).
    ///
    /// The body of `GET /events/{id}/layouts`, and `None` for an unknown event (→ a typed 404).
    pub fn channel_layouts(&self, id: &EventId) -> Option<ChannelLayouts> {
        let layouts = self.read().events.get(id)?.meta.channel_layouts.clone();
        Some(ChannelLayouts::of(layouts))
    }

    /// Define a **channel layout** on an event (#117 S2), returning the event's whole updated
    /// [`ChannelLayouts`] view.
    ///
    /// The id is auto-generated — a slug of the request's `name` plus a short random suffix
    /// (mirroring the round/event/pilot id-gen) — retried on the (astronomically unlikely) collision
    /// with an existing layout id.
    ///
    /// [`nodes`](NewChannelLayoutRequest::nodes) omitted means **seed from the global allowed set**
    /// ([`seed_layout_nodes`]): enabled node *i* takes the *i*-th channel the RD ticked for this
    /// timer on the Timers page. That is the whole of "global is the default an event starts from" —
    /// the moment the layout exists it is event state, and no later edit to it touches
    /// [`Timer::available_channels`].
    ///
    /// Validation is [`validate_layout`]'s (all [`LayoutError::Invalid`] → a 400): a named,
    /// duplicate-free, complete tuning drawn from the allowed set, over nodes that exist and are
    /// enabled. **Cross-layout channel reuse is not validated** — it comes back as a
    /// [`LayoutOverlap`] in the response and never blocks the write. An unknown event is a
    /// [`LayoutError::EventNotFound`] (→ 404). On success the layout is appended to
    /// [`EventMeta::channel_layouts`] and written through to the event's SQLite `meta` table (issue
    /// #115) so it survives a Director restart — exactly the rounds path.
    pub fn add_channel_layout(
        &self,
        id: &EventId,
        req: NewChannelLayoutRequest,
    ) -> Result<ChannelLayouts, LayoutError> {
        let mut reg = self.write();
        // Cloned out before the mutable borrow below (the same reason `add_round` clones its
        // directories): resolving the event's timer would re-lock the registry we already hold.
        let timers = reg.timers.clone();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| LayoutError::EventNotFound(id.0.clone()))?;

        let timer = layout_timer(&event.meta, &timers)?;
        // The global→event seam: an omitted mapping is seeded from what the RD allowed globally.
        let mut nodes = match req.nodes {
            Some(nodes) => nodes,
            None => seed_layout_nodes(&timer)?,
        };
        nodes.sort_by_key(|n| n.node);
        validate_layout(&timer, &event.meta.channel_layouts, None, &req.name, &nodes)?;

        // Auto-generate a unique layout id within this event: slug(name) + short suffix, retried on
        // the (astronomically unlikely) collision so the id is always fresh.
        let layout_id = loop {
            let candidate = LayoutId(format!("{}-{}", slugify(&req.name), short_suffix()));
            if !event.meta.channel_layouts.iter().any(|l| l.id == candidate) {
                break candidate;
            }
        };
        event.meta.channel_layouts.push(ChannelLayout {
            id: layout_id,
            name: req.name.trim().to_string(),
            nodes,
        });
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(ChannelLayouts::of(meta.channel_layouts))
    }

    /// Replace an existing **channel layout**'s name and mapping (#117 S2), returning the event's
    /// whole updated [`ChannelLayouts`] view.
    ///
    /// The layout's [`id`](ChannelLayout::id) is fixed (the path segment); the name and the entire
    /// node → channel mapping are replaced wholesale and re-validated exactly as on create. Unknown
    /// event → [`LayoutError::EventNotFound`] (404); unknown layout id → [`LayoutError::LayoutNotFound`]
    /// (404); an invalid tuning → [`LayoutError::Invalid`] (400). Written through to disk (issue
    /// #115).
    pub fn update_channel_layout(
        &self,
        id: &EventId,
        layout_id: &LayoutId,
        req: SetChannelLayoutRequest,
    ) -> Result<ChannelLayouts, LayoutError> {
        let mut reg = self.write();
        let timers = reg.timers.clone();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| LayoutError::EventNotFound(id.0.clone()))?;
        let index = event
            .meta
            .channel_layouts
            .iter()
            .position(|l| &l.id == layout_id)
            .ok_or_else(|| LayoutError::LayoutNotFound(layout_id.0.clone()))?;

        let timer = layout_timer(&event.meta, &timers)?;
        let mut nodes = req.nodes;
        nodes.sort_by_key(|n| n.node);
        validate_layout(
            &timer,
            &event.meta.channel_layouts,
            Some(layout_id),
            &req.name,
            &nodes,
        )?;

        event.meta.channel_layouts[index] = ChannelLayout {
            id: layout_id.clone(),
            name: req.name.trim().to_string(),
            nodes,
        };
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        let timers = reg.timers.clone();
        // Release the registry write lock BEFORE touching the log — the same ordering
        // `update_round` observes (command lock ahead of the log mutex).
        drop(reg);

        // #117 S3: **editing a layout re-tunes the heats already flying it.** The RD must be able
        // to fix a layout without deleting and rebuilding every heat under it, and a `Scheduled`
        // heat that kept the OLD channels after its layout changed would be the #387 bug in a new
        // costume — a heat racing config that no longer exists anywhere the RD can see.
        //
        // Deliberately the same mechanism as a round edit, not a second one: re-materialize every
        // round that names this layout. Only still-`Scheduled` heats are rewritten (anything staged
        // or raced keeps the channels it raced on), and nothing is appended for a heat the new
        // mapping does not actually change.
        for round in meta.rounds.iter().filter(|r| r.layouts.contains(layout_id)) {
            self.rematerialize_round_heats(id, &round.id, &meta, &timers);
        }
        Ok(ChannelLayouts::of(meta.channel_layouts))
    }

    /// Remove a **channel layout** from an event (#117 S2), returning the event's whole updated
    /// [`ChannelLayouts`] view.
    ///
    /// Unknown event → [`LayoutError::EventNotFound`] (404); unknown layout id →
    /// [`LayoutError::LayoutNotFound`] (404) rather than a silent no-op, so a console deleting a layout
    /// someone else already deleted is told rather than left believing it removed something.
    /// Written through to disk (issue #115).
    pub fn remove_channel_layout(
        &self,
        id: &EventId,
        layout_id: &LayoutId,
    ) -> Result<ChannelLayouts, LayoutError> {
        let mut reg = self.write();
        let event = reg
            .events
            .get_mut(id)
            .ok_or_else(|| LayoutError::EventNotFound(id.0.clone()))?;
        let index = event
            .meta
            .channel_layouts
            .iter()
            .position(|l| &l.id == layout_id)
            .ok_or_else(|| LayoutError::LayoutNotFound(layout_id.0.clone()))?;
        // #117 S3: a layout a **round** names cannot be deleted out from under it. Allowing it
        // would leave the round pointing at nothing — its next fill drawing a heat with no channels
        // and no explanation — so the refusal names the round and the layout, both by their
        // friendly names, and tells the RD which end to undo first.
        if let Some(round) = event
            .meta
            .rounds
            .iter()
            .find(|r| r.layouts.contains(layout_id))
        {
            let layout = event.meta.channel_layouts[index].name.clone();
            return Err(LayoutError::Invalid(format!(
                "{:?} is the channel layout {:?} flies — remove it from that round before deleting \
                 it",
                layout, round.label
            )));
        }
        event.meta.channel_layouts.remove(index);
        let meta = event.meta.clone();
        let data_dir = reg.data_dir.clone();
        persist_meta_change(data_dir.as_deref(), &meta)?;
        Ok(ChannelLayouts::of(meta.channel_layouts))
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
        // Cloned out before the mutable borrow below (the same reason `directory` is): the round's
        // open-practice channels are validated against the event's primary timer's enabled node
        // set (#412), and `self.timers()` would re-lock the registry we already hold.
        let timers = reg.timers.clone();
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
            &timers,
            &req.classes,
            &req.format,
            &req.seeding,
            channel_mode,
            &req.layouts,
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
            // The channel layouts this round's heats may fly (#117 S3), validated above.
            layouts: req.layouts,
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
            let Some(heat_state) = heat_state(&events, &heat) else {
                continue;
            };
            // The heat's RD-facing name, resolved the way the console resolves it. Never the id:
            // every one of these strings ends up in a refusal a person reads (repo display rule).
            let name = || match &round {
                Some(round) => round_engine::heat_display_name(round, &events, &heat),
                None => "a heat".to_string(),
            };
            if heat_state != HeatState::Scheduled && facts.raced.is_none() {
                facts.raced = Some(name());
            }
            // Countdown begun / gate open / racing / passes recorded but not yet official — plus,
            // stricter than [`is_racing_phase`], a still-`Scheduled` heat the RD has loaded in Live
            // control: it may be on deck with its channels already read off, so it is off limits to
            // a round edit too.
            let in_progress = is_racing_phase(heat_state)
                || (heat_state == HeatState::Scheduled && on_timer.as_ref() == Some(&heat));
            if in_progress && facts.in_progress.is_none() {
                facts.in_progress = Some(name());
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
        self.in_progress_on_timer(timer, false)
    }

    /// The **friendly name** of a *scored* heat in progress on `timer` right now (#355), or `None`
    /// when nothing scored is under way on it — the refusal probe behind a **calibration write**.
    ///
    /// The same scan as [`heat_in_progress_on_timer`](Self::heat_in_progress_on_timer), with one
    /// heat kind stepped over: an **open-practice** heat does not block a threshold change.
    ///
    /// # Why calibration and restart gate differently
    ///
    /// They have different blast radii. Restarting RotorHazard takes the timing hardware down and
    /// destroys the session it is running, practice included — so [`heat_in_progress_on_timer`]
    /// refuses on *any* racing heat. Nudging a detection threshold does not: the only thing at risk
    /// is the adjudication of the laps being recorded, and an open-practice round is **excluded from
    /// scoring** (#398), so there is no result to protect.
    ///
    /// And refusing here would break the page's whole workflow. #355's requirement is *"I want to
    /// slide the slider and then test right away"* — which is a pilot in the air on a practice heat
    /// while the RD adjusts. Refuse during practice and an RD can only tune an idle gate, wave a
    /// quad through by hand, and walk back: exactly the unusable RotorHazard-UI loop this page was
    /// built to replace. A competition heat stays refused, absolutely.
    ///
    /// **Practice-ness is [`open_practice::excluded_from_scoring`] and nothing else** — the same
    /// predicate the scoring surfaces consult, deliberately reused rather than re-derived, so this
    /// gate and the scoring exclusion cannot drift apart. A heat with no round at all (an ad-hoc /
    /// free-text heat) is **not** excluded and so still refuses, matching
    /// [`open_practice::heat_excluded_from_scoring`]'s neutral fallback.
    ///
    /// [`open_practice::excluded_from_scoring`]: crate::open_practice::excluded_from_scoring
    /// [`open_practice::heat_excluded_from_scoring`]: crate::open_practice::heat_excluded_from_scoring
    pub fn scored_heat_in_progress_on_timer(&self, timer: &TimerId) -> Option<String> {
        self.in_progress_on_timer(timer, true)
    }

    /// The shared scan behind [`heat_in_progress_on_timer`](Self::heat_in_progress_on_timer) and
    /// [`scored_heat_in_progress_on_timer`](Self::scored_heat_in_progress_on_timer).
    ///
    /// One implementation on purpose: two copies of "which heat is racing on this timer" would
    /// drift, and the difference between the two callers is a single predicate. With `scored_only`,
    /// a heat whose round is excluded from scoring is stepped over as though it were not racing at
    /// all — so a practice heat running with nothing else yields `None`.
    fn in_progress_on_timer(&self, timer: &TimerId, scored_only: bool) -> Option<String> {
        use gridfpv_engine::heat::heat_state;
        use gridfpv_events::Event;

        // Snapshot the candidate events (id + their rounds) and release the registry lock BEFORE
        // resolving/reading a log — `resolve` takes the same lock.
        //
        // Scoped to the **active** event on purpose. Only the active event's selection opens a
        // connection (see the RH connection reconciler), so only its heats can be driven on this
        // timer — and scanning every event that merely *lists* the timer meant one abandoned event
        // holding a heat in `Unofficial` (raced, never finalized) refused every restart forever,
        // naming a heat in an event the RD is not running and cannot find.
        let candidates: Vec<(EventId, Vec<RoundDef>)> = {
            let reg = self.read();
            reg.active_event
                .as_ref()
                .and_then(|id| reg.events.get(id))
                .filter(|e| e.meta.timers.contains(timer))
                .map(|e| (e.meta.id.clone(), e.meta.rounds.clone()))
                .into_iter()
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
                // A calibration write is allowed to land under an OPEN PRACTICE heat (#355, #398):
                // practice is excluded from scoring, so there is no result for a moved threshold to
                // corrupt — and practice is the moment an RD actually wants to tune, pilots in the
                // air. Step over it and keep scanning; anything scored still refuses.
                if scored_only && crate::open_practice::heat_excluded_from_scoring(round) {
                    continue;
                }
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
        // Cloned out before the mutable borrow, as in `add_round` (#412).
        let timers = reg.timers.clone();
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
            &timers,
            &req.classes,
            &req.format,
            &req.seeding,
            channel_mode,
            &req.layouts,
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
        if facts.raced.is_some() {
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
            // #117 S3: a raced round's layouts are frozen with its channel mode. They do not affect
            // scoring, but they decide what a re-materialized heat is tuned to — and a raced heat
            // must keep the channels it raced on. Freezing here means the question never arises.
            if req.layouts != existing.layouts {
                frozen.push("channel layouts");
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
            // The channel layouts this round's heats may fly (#117 S3), replaced wholesale.
            layouts: req.layouts,
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
            // #117 S3: re-assert which layout the re-formed heat flies, so the recorded answer
            // keeps up with a round whose named layouts have just changed. Appended BEFORE the
            // schedule, so a reader folding the log in order never sees a heat carrying channels
            // from one layout while still recorded against another.
            if heat.layout.is_some() {
                let _ = state.append(
                    gridfpv_events::Event::HeatLayoutSet {
                        heat: heat.heat.clone(),
                        layout: heat.layout.clone(),
                    },
                    None,
                );
            }
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
    ///
    /// # The gate is on state, not on existence (#418)
    ///
    /// This used to refuse on *any* heat being tagged to the round, which made a round permanently
    /// undeletable the moment its heats were generated — even a practice round whose single heat
    /// was never armed, never run and holds no laps. A practice round is precisely the one an RD
    /// creates, misconfigures and throws away, so "you filled it once, it is yours forever" is the
    /// wrong rule; and the refusal recommended *"discard its heats and re-use it"* through a route
    /// that has never existed (`/events/{id}/heats` is `GET` only), sending the RD looking for a
    /// control that is not there at the moment they are already stuck.
    ///
    /// The rule now is the one the timer-side refusals already use
    /// ([`heat_in_progress_on_timer`](Self::heat_in_progress_on_timer) /
    /// [`scored_heat_in_progress_on_timer`](Self::scored_heat_in_progress_on_timer)), read off the
    /// same [`RoundHeatFacts`] fold, and it **names** what is blocking:
    ///
    /// * a heat **in progress** (staged / armed / running / unofficial, or loaded on the timer) →
    ///   refused, naming that heat — removing the round would pull its config out from under a
    ///   race that is happening;
    /// * a heat that **carries results** (anything past `Scheduled`) → refused, naming that heat —
    ///   scoring re-derives from the round, so the results would lose the config that produced them;
    /// * otherwise every heat is still unstarted and holds nothing worth protecting, so the round
    ///   **deletes, and its heats go with it**.
    ///
    /// "Go with it" is a read-side discard, because the log is append-only: the `HeatScheduled`
    /// entries stay in the log as the historical fact that they were once planned, and every read
    /// that could put one in front of the RD drops it. There is nothing left to advise the RD to
    /// do, so the message no longer advises anything.
    ///
    /// **Every read means every read** (#439). The discard used to live only at `GET /heats`
    /// ([`heats_of_defined_rounds`](crate::live_state::heats_of_defined_rounds)), and the RD does
    /// not reach the next heat through a list: `on_deck` and `Advance` walked the raw
    /// `HeatScheduled` entries and happily loaded a heat of a round that no longer existed —
    /// unnameable (its ack printed the raw heat id), unconfigurable (its layouts, staging timer
    /// and min-lap went with the round) and unfindable (it is on no screen). So the live fold
    /// ([`current_heat`](crate::live_state), [`on_deck`](crate::live_state)) and the Advance
    /// control take the same defined-round list, built once by
    /// [`defined_round_ids`](crate::live_state::defined_round_ids).
    pub fn remove_round(&self, id: &EventId, round_id: &RoundId) -> Result<EventMeta, RoundError> {
        // Probe the log BEFORE taking the registry write lock (the log has its own mutex) — the
        // same order `update_round` uses.
        let facts = self.round_heat_facts(id, round_id);
        if let Some(heat) = &facts.in_progress {
            return Err(RoundError::Invalid(format!(
                "this round has a heat in progress ({heat}) — finalize or reset it before \
                 removing the round"
            )));
        }
        if let Some(heat) = &facts.raced {
            return Err(RoundError::Invalid(format!(
                "this round has raced heats ({heat}) — removing it would strand their results, \
                 which are scored through this round's config"
            )));
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

    /// Everything wrong with this event's **stored** round configuration (#416 / #117 S3), or
    /// `None` if no such event.
    ///
    /// The read-side twin of the write-side refusal in [`validate_round_fields`]. Three walks, one
    /// list, because #412, #416 and #117 S3 all landed on *validate stored config on read* and a
    /// second mechanism is how two answers to one question start disagreeing:
    ///
    /// 1. every `node-{i}` **seat that cannot record a lap**, checked against the event's
    ///    **effective primary** timer through the shared [`seat_problems`] rule;
    /// 2. every **stale channel layout** a round names, through [`layout_problems`];
    /// 3. every **scheduled heat still bound to a layout its round no longer names** — which needs
    ///    no timer at all, only the round and the log.
    ///
    /// An event with no resolvable timer has no node set to check (1) and (2) against and reports
    /// neither; (3) is still reported. An empty list is "nothing wrong", never "not checked".
    ///
    /// This is what makes a round the RD *already has* repairable: #412 stopped new rounds from
    /// being authored onto a dead gate, but the one on the bench predates it, and a round that
    /// silently seats a pilot on a node that does not exist is the worst available behaviour.
    pub fn round_issues(&self, id: &EventId) -> Option<Vec<RoundIssue>> {
        let (meta, timers) = {
            let reg = self.read();
            let event = reg.events.get(id)?;
            (event.meta.clone(), reg.timers.clone())
        };
        // The event's own log, for the heat→layout binds (walk 3). Read AFTER the registry lock is
        // released — the log has its own mutex, and this is the order `round_heat_facts` uses. An
        // unreadable log costs walk 3 only; walks 1 and 2 are pure config.
        let events = self
            .resolve(id)
            .and_then(|state| state.read().ok())
            .map(|(events, _cursor)| events)
            .unwrap_or_default();
        let timer = meta.effective_primary().and_then(|id| timers.get(&id));
        let mut out = Vec::new();
        for round in &meta.rounds {
            out.extend(orphaned_bind_issues(&meta, round, &events));
            let Some(timer) = &timer else {
                continue;
            };
            if let SeedingRule::ActiveNodes { nodes } = &round.seeding {
                for (node, problem) in seat_problems(nodes, timer) {
                    out.push(RoundIssue {
                        round: round.id.clone(),
                        round_label: round.label.clone(),
                        timer: Some(timer.id.clone()),
                        timer_name: Some(timer.name.clone()),
                        node: Some(node),
                        node_label: Some(Timer::node_label(node)),
                        problem,
                        layout: None,
                        layout_name: None,
                        heat: None,
                        heat_name: None,
                        detail: seat_problem_detail(round, timer, node, problem),
                    });
                }
            }
            // #117 S3: the **stored channel layouts** this round's heats fly, re-checked against
            // the timer as it is now. A layout is validated as a complete, allowed tuning when it
            // is written — and then the RD enables a node, or unticks a channel, and it is not one
            // any more. #412 and #416 both landed on *validate stored config on read*, and this is
            // that same read, so a stale layout surfaces exactly where the impossible seat does
            // rather than at arm time when nothing can be changed.
            for id in &round.layouts {
                let Some(layout) = meta.layout(id) else {
                    continue;
                };
                for (node, problem) in layout_problems(layout, timer) {
                    out.push(RoundIssue {
                        round: round.id.clone(),
                        round_label: round.label.clone(),
                        timer: Some(timer.id.clone()),
                        timer_name: Some(timer.name.clone()),
                        node: Some(node),
                        node_label: Some(Timer::node_label(node)),
                        problem,
                        layout: Some(layout.id.clone()),
                        layout_name: Some(layout.name.clone()),
                        heat: None,
                        heat_name: None,
                        detail: layout_problem_detail(round, layout, timer, node, problem),
                    });
                }
            }
        }
        Some(out)
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

    /// The metadata for every event, in id order.
    ///
    /// The order is stable (the map is a `BTreeMap`) so `GET /events` is deterministic. The list
    /// is **empty on a fresh Director** — that is the first-run state the picker must handle
    /// (#414), not an error.
    pub fn list(&self) -> Vec<EventMeta> {
        self.read()
            .events
            .values()
            .map(|e| e.meta.clone())
            .collect()
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
        // unlikely) collision so the id is always fresh.
        let id = loop {
            let candidate = EventId(format!("{}-{}", slugify(name), short_suffix()));
            if !reg.events.contains_key(&candidate) {
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
            // No channel layouts until the RD defines one (#117 S2). Deliberately not seeded at
            // create time: the timer selection is not settled yet, and a layout seeded from the
            // wrong timer's allowed set is worse than no layout.
            channel_layouts: Vec::new(),
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
    /// Every event is deletable — there is no reserved built-in event any more (#414). An unknown
    /// id is a [`RegistryError`] the caller maps to a typed 404.
    ///
    /// The on-disk file removal is best-effort *after* the in-memory drop: dropping the
    /// [`RegisteredEvent`] closes the live SQLite connection (its `AppState` is the only holder),
    /// so the files are then free to unlink. A missing file is not an error (idempotent cleanup);
    /// a genuine unlink failure is surfaced as a [`RegistryError`] so the caller can report it.
    pub fn delete(&self, id: &EventId) -> Result<(), RegistryError> {
        let mut reg = self.write();

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
/// no-op: it has nothing to persist to and is gone on restart by design.
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
/// boot.
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
        if stem.is_empty() {
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
/// failure). Carries a [`RegistryErrorKind`] so the HTTP layout can map an *unknown id* to `404`, a
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

/// What is wrong with a **stored** round's channel configuration, found on read (#412 / #416 /
/// #117 S3).
///
/// The original three cases are about a `node-{i}` **seat that cannot record a lap**: an
/// open-practice round's field *is* its active nodes, laid out as `node-{i}` seats
/// ([`SeedingRule::ActiveNodes`], whose entries are **node indices**). A seat naming a node the
/// timer does not have, or one the RD switched off, is a pilot on a dead gate: the heat runs, the
/// clock counts, and nothing is ever detected. Silently rendering that seat is the worst available
/// behaviour, so it is surfaced on **read** as well as refused on write.
///
/// The `Layout*` cases extend the same idea to a stored [`ChannelLayout`] that has gone stale, and
/// the `HeatLayout*` cases to a **heat still bound to a layout its round no longer names**. One
/// enum and one read (`GET /events/{id}/round-issues`) on purpose: #412, #416 and #117 S3 all
/// landed on *validate stored config on read*, and a second mechanism is how two answers to one
/// question start disagreeing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub enum SeatProblem {
    /// The seat names a node index at or beyond the timer's **effective width**
    /// ([`Timer::node_width`]) — there is no such node at all. The RD's bench case: a round seeded
    /// `ActiveNodes { nodes: [6] }` on a four-node timer.
    NoSuchNode,
    /// The node exists, but the RD has **disabled** it (#412) — a dead receiver, a gate that will
    /// not tune. A disabled node seats no pilot and is offered no channel.
    Disabled,
    /// The node is within the width GridFPV is **configured** for but beyond what the timer
    /// **reported** ([`NodeDrift`](crate::timers::NodeDrift)) — GridFPV is set wider than the
    /// hardware. A notice, never a refusal: D27's rule is that an observation about a timer is
    /// evidence, not an input to a decision, so this is shown and the round is left alone.
    NotOnTimer,
    /// A **channel layout** the round names says nothing about a node the timer has **enabled**
    /// (#117 S3) — the layout went stale when the RD switched that node on.
    ///
    /// A layout is validated as a *complete* tuning when it is written, so this can only appear
    /// afterwards. A heat flying it would seat a pilot on a gate with no channel, which is why the
    /// fill refuses it (`AssignError::LayoutNodeUntuned`) rather than seating them anyway.
    LayoutNodeUntuned,
    /// A **channel layout** the round names tunes a node the timer no longer has, or that the RD
    /// has since switched **off** (#117 S3). The layout entry is dead weight: no heat will ever fly
    /// that gate.
    LayoutNodeGone,
    /// A **channel layout** the round names tunes a node to a channel the timer is no longer
    /// **allowed** to use (#117 S3) — the RD unticked it on the Timers page after defining the
    /// layout.
    ///
    /// Grid would still push the frequency (D27: the layout is Grid-owned config *applied* to the
    /// timer), so this is not a silent failure — but it flies a channel the RD has said this timer
    /// may not use, and one of the two statements needs to change.
    LayoutChannelNotAllowed,
    /// A **scheduled heat** is still bound to a channel layout its round **no longer names**
    /// (#117 S3 follow-up) — the RD dropped the layout from the round *after* the heat was drawn.
    ///
    /// The bind is an [`Event::HeatLayoutSet`](gridfpv_events::Event::HeatLayoutSet) in the log,
    /// and it outlives a round edit by design (that is what stops a re-fill from silently losing
    /// it). So the heat keeps the layout's channels and flies frequencies its round no longer
    /// sanctions, with nothing saying so.
    ///
    /// **Reported, never refused.** The round edit stands: there are two valid repairs — bind the
    /// heat to a layout the round *does* name (`Command::SetHeatLayout`), or set its channels by
    /// hand (`Command::OverrideHeatSeating`) — and blocking the edit would prevent both.
    ///
    /// A **raced** heat is never reported: it flew what it flew, and its channels are the durable
    /// record of that. Only a still-`Scheduled` heat has anything left to decide.
    HeatLayoutNotInRound,
    /// A **scheduled heat** is still bound to a channel layout the event **no longer has** (#117
    /// S3 follow-up) — the same orphaned bind as [`HeatLayoutNotInRound`](Self::HeatLayoutNotInRound),
    /// one step further along.
    ///
    /// Reachable precisely because the delete refusal is scoped to *rounds*: `remove_channel_layout`
    /// refuses while a round names the layout, so the RD drops it from the round first (creating
    /// the orphan) and only then deletes it. The heat keeps the channels it was last scheduled
    /// with, and [`layout_for_heat`](crate::round_engine::layout_for_heat) resolves to `None`.
    ///
    /// There is no layout name to show, which is the whole difference from the case above; the
    /// repairs are the same two.
    HeatLayoutGone,
}

/// One problem found in a **stored** round's configuration, on read (#416).
///
/// # Why read and not only write
///
/// #412 refuses an impossible seat at add *and* update, so every round authored since is safe. But
/// nothing re-checked the rounds already on disk — and the rounds already on disk are exactly where
/// the bug lives, because they predate the fix. A stored round is also not static: the RD can
/// disable a node, narrow a timer's width, or swap the event's primary timer, and any of those can
/// make a round that validated cleanly at write time impossible at race time. So the check runs
/// where the RD looks (`GET /events/{id}/round-issues`), against the same
/// [`Timer::node_view`](crate::timers::Timer::node_view) answer `GET /timers/{id}/nodes` serves.
///
/// Every field a person reads is a **friendly name** — the round's label, the timer's name, the
/// heat's name, the layout's name, the 1-based node label — never a raw id or a bare index (repo
/// display rule). `round` / `node` / `layout` / `heat` are the wire handles the console repairs
/// *through* (they address the round-edit form, the layouts page, the heat), not labels.
///
/// # Only `round` and `problem` are always there
///
/// The set has grown past "an impossible seat". A stale-layout problem is about a node *and* a
/// layout; an orphaned heat bind ([`SeatProblem::HeatLayoutNotInRound`]) is about a **heat** and
/// has no node at all, and is not checked against a timer. Rather than fabricate a node index or a
/// timer to fill the struct — the display rule's exact failure mode — the fields that do not apply
/// are simply absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "bindings/")]
pub struct RoundIssue {
    /// The round the problem is in — the handle the console's repair action edits.
    pub round: RoundId,
    /// The round's **label**, for display.
    pub round_label: String,
    /// The timer the seat was checked against (the event's effective primary). Absent for a
    /// problem that is not about the timer at all (an orphaned heat bind).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub timer: Option<TimerId>,
    /// That timer's **name**, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub timer_name: Option<String>,
    /// The offending node index, **0-based** — a wire handle (it is what the round's seeding
    /// stores), never shown as-is. Absent for a problem that names no single node.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node: Option<u32>,
    /// The node's **display name**, 1-based: index `6` is `"Node 7"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub node_label: Option<String>,
    /// Which case this is — an impossible **seat** (#412/#416), a stale **channel layout**
    /// (#117 S3), or a heat still bound to a layout its round dropped.
    pub problem: SeatProblem,
    /// The **channel layout** the problem is in, for the `Layout*` cases — the handle the console's
    /// repair action edits. Absent for a seat problem, which is about the round's own seeding, and
    /// for [`SeatProblem::HeatLayoutGone`], where the layout no longer exists to name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub layout: Option<LayoutId>,
    /// That layout's **name**, for display — never its [`LayoutId`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub layout_name: Option<String>,
    /// The **heat** the problem is in, for the `HeatLayout*` cases — the handle the console's
    /// repair actions (`SetHeatLayout` / `OverrideHeatSeating`) address. Absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub heat: Option<HeatId>,
    /// That heat's **name**, for display — resolved through
    /// [`round_engine::heat_display_name`](crate::round_engine::heat_display_name), the server-side
    /// twin of the console's `heatNameById`, so both surfaces call the heat the same thing. Never
    /// its [`HeatId`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub heat_name: Option<String>,
    /// The RD-facing sentence: what is wrong, and what to do about it. Written server-side so the
    /// console does not re-derive (and drift from) the explanation.
    pub detail: String,
}

/// The **seat problems** in an `ActiveNodes` seeding, checked against `timer` — the one place that
/// decides whether a `node-{i}` seat can record a lap (#412 / #416).
///
/// Shared by the write-side refusal ([`validate_round_fields`]) and the read-side notice
/// ([`EventRegistry::round_issues`]) precisely so the two cannot disagree about what "this seat is
/// impossible" means. Reads [`Timer::node_view`] — the same answer `GET /timers/{id}/nodes` serves
/// the console — rather than re-deriving width/enabled locally.
///
/// Returns `(node, problem)` in seeding order, one entry per offending seat.
fn seat_problems(seeded_nodes: &[usize], timer: &Timer) -> Vec<(u32, SeatProblem)> {
    let view = timer.node_view();
    let mut out = Vec::new();
    for seeded in seeded_nodes {
        let node = u32::try_from(*seeded).unwrap_or(u32::MAX);
        let problem = if node >= view.width {
            SeatProblem::NoSuchNode
        } else if !view.enabled.contains(&node) {
            SeatProblem::Disabled
        } else if view.reported.is_some_and(|reported| node >= reported) {
            SeatProblem::NotOnTimer
        } else {
            continue;
        };
        out.push((node, problem));
    }
    out
}

/// The **stale-layout problems** in a stored [`ChannelLayout`], checked against `timer` (#117 S3)
/// — the layout twin of [`seat_problems`], and deliberately the same shape.
///
/// A layout is written as a *complete, conflict-free, allowed* tuning of the timer. It stops being
/// one when the world moves underneath it, and this is the read-side check for exactly that:
///
/// - an **enabled** node the layout says nothing about ([`SeatProblem::LayoutNodeUntuned`]) — the
///   RD switched a node on afterwards, and a heat flying this layout would seat somebody on a gate
///   with no channel;
/// - a layout entry for a node the timer no longer has, or that is switched **off**
///   ([`SeatProblem::LayoutNodeGone`]);
/// - an entry whose channel is no longer in the timer's **allowed** set
///   ([`SeatProblem::LayoutChannelNotAllowed`]).
///
/// A reported-vs-configured node **drift** is deliberately not checked here: it is an observation
/// about a timer, and D27 says an observation is evidence, never an input to a decision. The seat
/// walk already reports that case once, against the round's own seeding.
///
/// Returns `(node, problem)` in ascending node order, one entry per offending node.
fn layout_problems(layout: &ChannelLayout, timer: &Timer) -> Vec<(u32, SeatProblem)> {
    let view = timer.node_view();
    let mut out: Vec<(u32, SeatProblem)> = Vec::new();
    for node in &view.enabled {
        if layout.channel_for(*node).is_none() {
            out.push((*node, SeatProblem::LayoutNodeUntuned));
        }
    }
    for entry in &layout.nodes {
        if !view.enabled.contains(&entry.node) {
            out.push((entry.node, SeatProblem::LayoutNodeGone));
        } else if !timer.available_channels.contains(&entry.channel) {
            out.push((entry.node, SeatProblem::LayoutChannelNotAllowed));
        }
    }
    out.sort_by_key(|(node, _)| *node);
    out
}

/// The **orphaned layout binds** in one round: every still-`Scheduled` heat bound to a channel
/// layout `round` no longer names (#117 S3 follow-up).
///
/// # The hole this closes
///
/// A heat's layout is an [`Event::HeatLayoutSet`](gridfpv_events::Event::HeatLayoutSet) in the log,
/// deliberately **not** a field on `HeatScheduled` — that is what stops a round re-fill from
/// silently dropping the RD's choice. The same durability means the bind outlives a *round* edit:
/// drop the layout from the round and the heat keeps flying the layout's channels, sanctioned by
/// nothing. Until now, nothing said so.
///
/// # Reported, not refused
///
/// The round edit is legitimate and stands. There are two valid repairs — rebind the heat
/// ([`Command::SetHeatLayout`](crate::control::Command::SetHeatLayout)) or set its channels by hand
/// ([`Command::OverrideHeatSeating`](crate::control::Command::OverrideHeatSeating)) — and refusing
/// the edit would prevent both. This is the RD's own call.
///
/// # Scope
///
/// - Only an **explicit** bind can be orphaned. A heat with no `HeatLayoutSet` (or one the RD
///   cleared) already falls back to its round's first layout, so it is by definition current.
/// - Only a **`Scheduled`** heat. A raced heat flew what it flew, and its channels are the durable
///   record of that: reporting it would invite the RD to "repair" history. A heat that is staged or
///   running is equally past repair.
/// - Deletion is a **different** case and is not touched here: `remove_channel_layout` refuses
///   while a round still names the layout. That refusal is what forces this orphan to be created
///   in two steps (drop from the round, then delete), which is why
///   [`SeatProblem::HeatLayoutGone`] exists.
fn orphaned_bind_issues(
    meta: &EventMeta,
    round: &RoundDef,
    events: &[gridfpv_events::Event],
) -> Vec<RoundIssue> {
    use gridfpv_engine::heat::{HeatState, heat_state};

    let mut out = Vec::new();
    for heat in round_engine::scheduled_round_heats(events, &round.id) {
        if heat_state(events, &heat) != Some(HeatState::Scheduled) {
            continue;
        }
        // `Some(Some(_))` is an explicit bind; `Some(None)` is the RD clearing it and `None` is
        // never having touched it — both of which mean "the round's default", which cannot be
        // stale by construction.
        let Some(Some(bound)) = round_engine::heat_layout_bind(events, &heat) else {
            continue;
        };
        if round.layouts.contains(&bound) {
            continue;
        }
        let layout = meta.layout(&bound);
        let heat_name = round_engine::heat_display_name(round, events, &heat);
        let detail = orphaned_bind_detail(round, &heat_name, layout.map(|l| l.name.trim()));
        out.push(RoundIssue {
            round: round.id.clone(),
            round_label: round.label.clone(),
            // Not a timer question: the heat's channels are wrong relative to its ROUND, whatever
            // hardware it lands on. Naming a timer here would be decoration.
            timer: None,
            timer_name: None,
            node: None,
            node_label: None,
            problem: match layout {
                Some(_) => SeatProblem::HeatLayoutNotInRound,
                None => SeatProblem::HeatLayoutGone,
            },
            layout: layout.map(|l| l.id.clone()),
            layout_name: layout.map(|l| l.name.clone()),
            heat: Some(heat),
            heat_name: Some(heat_name),
            detail,
        });
    }
    out
}

/// The RD-facing sentence for one **stale-layout** problem (#117 S3): what is wrong, and the way
/// out of it.
///
/// Every noun is a friendly name — the round's label, the layout's name, the timer's name, the
/// 1-based node label, and the channel as its band+channel label rather than a bare MHz number
/// (CLAUDE.md). Written server-side, like [`seat_problem_detail`], so the console renders one
/// explanation rather than re-deriving (and drifting from) it.
fn layout_problem_detail(
    round: &RoundDef,
    layout: &ChannelLayout,
    timer: &Timer,
    node: u32,
    problem: SeatProblem,
) -> String {
    let node_label = Timer::node_label(node);
    let round_label = round.label.trim();
    let timer_name = timer.name.trim();
    let layout_name = layout.name.trim();
    match problem {
        SeatProblem::LayoutNodeUntuned => format!(
            "{round_label} flies the {layout_name} channel layout, which does not say what \
             {node_label} is tuned to — but {node_label} is switched on for {timer_name}, so a heat \
             would seat a pilot there with no channel. Add {node_label} to {layout_name}."
        ),
        SeatProblem::LayoutNodeGone => format!(
            "{round_label} flies the {layout_name} channel layout, which tunes {node_label} — but \
             {node_label} is switched off on {timer_name} (or no longer exists), so nothing will \
             fly it. Re-enable the node, or drop it from {layout_name}."
        ),
        SeatProblem::LayoutChannelNotAllowed => {
            let channel = layout
                .channel_for(node)
                .map(crate::timers::channel_label)
                .unwrap_or_else(|| node_label.clone());
            format!(
                "{round_label} flies the {layout_name} channel layout, which puts {node_label} on \
                 {channel} — a channel {timer_name} is no longer allowed to use. Re-tick it on the \
                 Timers page, or pick another channel for {node_label} in {layout_name}."
            )
        }
        // The seat problems are explained by `seat_problem_detail`, and the orphaned heat binds by
        // `orphaned_bind_detail`; neither reaches here.
        SeatProblem::NoSuchNode
        | SeatProblem::Disabled
        | SeatProblem::NotOnTimer
        | SeatProblem::HeatLayoutNotInRound
        | SeatProblem::HeatLayoutGone => seat_problem_detail(round, timer, node, problem),
    }
}

/// The RD-facing sentence for a heat still bound to a channel layout its round **no longer names**
/// (#117 S3 follow-up): what is wrong, and the two ways out of it.
///
/// `layout_name` is the layout's friendly name when the event still has it, and `None` once it has
/// been deleted ([`SeatProblem::HeatLayoutGone`]) — there is then nothing to name, and the sentence
/// says so rather than printing a [`LayoutId`].
///
/// Both remedies are named because the RD chose to be **told, not blocked**: the round edit that
/// created this is legitimate, and either repair may be the right one.
///
/// - [`Command::SetHeatLayout`](crate::control::Command::SetHeatLayout) rebinds the heat to a
///   layout the round does name;
/// - [`Command::OverrideHeatSeating`](crate::control::Command::OverrideHeatSeating) sets the heat's
///   channels by hand.
fn orphaned_bind_detail(round: &RoundDef, heat_name: &str, layout_name: Option<&str>) -> String {
    let round_label = round.label.trim();
    match layout_name {
        Some(layout_name) => format!(
            "{heat_name} still flies the {layout_name} channel layout, but {round_label} no longer              names it — the heat keeps {layout_name}'s channels even though its round no longer              says it may. Pick a layout {round_label} names for {heat_name}, or set its channels              by hand."
        ),
        None => format!(
            "{heat_name} is still bound to a channel layout {round_label} no longer names, and              that layout has since been deleted — the heat keeps the channels it was last drawn              with. Pick a layout {round_label} names for {heat_name}, or set its channels by hand."
        ),
    }
}

/// The RD-facing sentence for one seat problem: what is wrong, and the way out of it.
///
/// The `Layout*` cases belong to [`layout_problem_detail`] — they are about a stored channel
/// layout, not about the round's own seeding — and are delegated back to it rather than given a
/// second, drifting explanation here.
fn seat_problem_detail(round: &RoundDef, timer: &Timer, node: u32, problem: SeatProblem) -> String {
    let node_label = Timer::node_label(node);
    let round_label = round.label.trim();
    let timer_name = timer.name.trim();
    match problem {
        SeatProblem::LayoutNodeUntuned
        | SeatProblem::LayoutNodeGone
        | SeatProblem::LayoutChannelNotAllowed => format!(
            "{round_label} has a stale channel layout on {node_label} of {timer_name} — open the \
             event's Channel layouts page to repair it."
        ),
        // Written by `orphaned_bind_detail`: those are about a heat, not about a node on a timer,
        // so there is nothing useful this function could say about them. Delegating back the way
        // `layout_problem_detail` does would recurse, so this is the one honest fallback.
        SeatProblem::HeatLayoutNotInRound | SeatProblem::HeatLayoutGone => format!(
            "{round_label} has a heat bound to a channel layout it no longer names — open the \
             event's Rounds page to repair it."
        ),
        SeatProblem::NoSuchNode => format!(
            "{round_label} seats a pilot on {node_label}, but {timer_name} has only {} nodes — \
             that seat can never record a lap. Edit the round and pick a node the timer has.",
            timer.node_width()
        ),
        SeatProblem::Disabled => format!(
            "{round_label} seats a pilot on {node_label}, which is switched off on {timer_name} — \
             that seat can never record a lap. Re-enable the node on the timer, or edit the round \
             and pick another."
        ),
        SeatProblem::NotOnTimer => format!(
            "{round_label} seats a pilot on {node_label}, but {timer_name} reported only {} nodes \
             — that seat records nothing. Fix the timer's node width, or edit the round and pick \
             another.",
            timer.reported_nodes.unwrap_or(0)
        ),
    }
}

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
/// / [`ActiveNodes`](SeedingRule::ActiveNodes) (no source rounds). Rejects: a `FromRanking` /
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
        SeedingRule::FromRoster | SeedingRule::ActiveNodes { .. } => {}
    }
    Ok(())
}

/// Validate a round's class selection, format, and seeding against the event and the directories
/// (race redesign Slice 2a) — the shared check the add/update paths run.
///
/// Returns [`RoundError::Invalid`] when: a `classes` entry is unknown to the directory or is not one
/// of the event's selected [`classes`](EventMeta::classes); `format` is not a
/// [`FormatRegistry::standard`] name; or a [`SeedingRule::FromRanking`]'s `source_rounds` is empty
/// or names a round that does not exist in this event (excluding `editing` — a round may not seed
/// from itself).
#[allow(clippy::too_many_arguments)]
fn validate_round_fields(
    meta: &EventMeta,
    directory: &ClassDirectory,
    timers: &TimerRegistry,
    classes: &[ClassId],
    format: &str,
    seeding: &SeedingRule,
    channel_mode: ChannelMode,
    layouts: &[LayoutId],
    win_condition: &WinCondition,
    time_limit_secs: Option<u32>,
    editing: Option<&RoundId>,
) -> Result<(), RoundError> {
    // #117 S3: a round may only name **channel layouts this event has**. A round pointing at a
    // layout that does not exist would draw heats with no channels and no explanation, so the
    // refusal names the layout the RD typed — and `remove_channel_layout` refuses to delete a
    // layout a round names, which closes the same hole from the other end. Duplicates are a
    // mis-click, not a choice (the first entry is the heats' default, so a repeat says nothing).
    let mut seen: Vec<&LayoutId> = Vec::new();
    for layout in layouts {
        let Some(found) = meta.layout(layout) else {
            return Err(RoundError::Invalid(
                "this round names a channel layout the event does not have — pick one from the \
                 event's Channel layouts page"
                    .to_string(),
            ));
        };
        if seen.contains(&layout) {
            return Err(RoundError::Invalid(format!(
                "this round names the {:?} channel layout twice",
                found.name
            )));
        }
        seen.push(layout);
    }
    // A `Static` round (time-trial / qualifying, GQ-style) forms its raced field straight from class
    // membership via the channel-balanced builder, but `round_ranking`/standings rank the
    // *seeding-resolved* field. Those only agree when seeding is the identity `FromRoster`; any other
    // seeding (creatable only via the raw API — the rounds form pairs Static with FromRoster) would
    // race a different field than it ranks. Reject it (release-hardening P1-2).
    // **A disabled node is not offered a channel** (#412). An open-practice round's field IS its
    // active nodes, laid out as `node-{i}` seats — so a round naming a node the RD has switched
    // off (or one the timer does not have) would show a lineup slot that can never record a lap:
    // the silent zero-lap heat this issue exists to stop, in its practice costume. Checked against
    // the event's **primary** timer; an event with no resolvable timer is not checked (a pure-sim
    // event has no node set to check against).
    if let SeedingRule::ActiveNodes { nodes } = seeding {
        if let Some(timer) = meta.effective_primary().and_then(|id| timers.get(&id)) {
            for (node, problem) in seat_problems(nodes, &timer) {
                // A reported-vs-configured DRIFT is a notice, never a refusal (#412, D27): an
                // observation about a timer is evidence, not an input to a decision. It is
                // surfaced by `round_issues` where the RD can see and repair it, not here.
                if problem == SeatProblem::NotOnTimer {
                    continue;
                }
                return Err(RoundError::Invalid(format!(
                    "{} is not available on the timer {:?} — it is disabled or does not exist",
                    Timer::node_label(node),
                    timer.name
                )));
            }
        }
    }
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
/// ([`MOCK_TIMER_ID`]). A new event selects it so it runs a sim race out of the box.
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
        CreateEventRequest::named(name)
    }

    #[test]
    fn a_fresh_registry_has_no_events() {
        // #414: there is no built-in event any more. A brand-new Director lists nothing, and
        // nothing resolves — that empty state is the console's "create your first event" screen,
        // not an error.
        let reg = EventRegistry::new(None).unwrap();
        assert!(reg.list().is_empty());
        assert!(reg.active().is_none());
        assert!(reg.resolve(&EventId("practice".into())).is_none());
    }

    #[test]
    fn creating_the_first_event_makes_it_resolvable_and_listed() {
        let reg = EventRegistry::new(None).unwrap();
        let meta = reg.create(&req("Practice")).unwrap();
        // The RD's own "Practice" event is an ordinary created event: it carries the default
        // timer selection and resolves to a usable AppState.
        assert_eq!(meta.name, "Practice");
        assert_eq!(meta.timers, default_timer_selection());
        assert!(reg.resolve(&meta.id).is_some());
        assert_eq!(reg.list().len(), 1);
        assert_eq!(reg.list()[0].id, meta.id);
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
        // Both resolve, and both are listed.
        assert!(reg.resolve(&a.id).is_some());
        let ids: Vec<_> = reg.list().into_iter().map(|m| m.id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&a.id) && ids.contains(&b.id));
    }

    #[test]
    fn each_created_event_gets_its_own_log() {
        let reg = EventRegistry::new(None).unwrap();
        let other = reg.create(&req("Practice")).unwrap();
        let other_state = reg.resolve(&other.id).unwrap();
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

        // That event's log has the heat; the other event's log is untouched (per-event dense).
        let (created_events, _) = created_state.read().unwrap();
        assert_eq!(created_events.len(), 1);
        let (other_events, _) = other_state.read().unwrap();
        assert_eq!(other_events.len(), 0);
    }

    #[test]
    fn one_rd_token_controls_every_event() {
        let reg = EventRegistry::new(None).unwrap();
        let rd = reg.tokens().issue_rd_token();
        let created = reg.create(&req("Race Night")).unwrap();
        let other = reg.create(&req("Club Night")).unwrap();
        // The shared token store is the same instance behind every event's AppState.
        let other_state = reg.resolve(&other.id).unwrap();
        let created_state = reg.resolve(&created.id).unwrap();
        assert!(other_state.tokens().authenticate_control(Some(&rd)).is_ok());
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
        let created = reg.create(&req("Race Night")).unwrap().id;
        let meta = reg.set_active(&created).unwrap();
        assert_eq!(meta.id, created);
        assert_eq!(reg.active().map(|m| m.id), Some(created));
    }

    #[test]
    fn a_stale_active_event_pointer_degrades_to_the_picker() {
        // #414: a Director that ran before this change has `<data_dir>/active-event` holding
        // "practice" — an id that no longer names anything. Booting over that data dir must
        // land on the picker, not fail to boot and not dangle at a missing event.
        let dir = std::env::temp_dir().join(format!("gridfpv-stale-active-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(ACTIVE_EVENT_FILE), "practice").unwrap();

        let reg =
            EventRegistry::new(Some(dir.clone())).expect("a stale pointer must not fail boot");
        assert!(
            reg.active().is_none(),
            "the stale id degrades to the picker"
        );
        assert!(reg.resolve(&EventId("practice".into())).is_none());
        assert!(reg.list().is_empty());

        // And the RD can still create an event and make it active over the same data dir.
        let created = reg.create(&req("Race Night")).unwrap();
        reg.set_active(&created.id).unwrap();
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        assert_eq!(reopened.active().map(|m| m.id), Some(created.id));

        std::fs::remove_dir_all(&dir).ok();
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

            // A second created event takes over the pointer, and that survives too.
            let second = reg.create(&req("Second")).unwrap();
            reg.set_active(&second.id).unwrap();
            let reopened2 = EventRegistry::new(Some(dir.clone())).unwrap();
            assert_eq!(reopened2.active().map(|m| m.id), Some(second.id));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_event_selecting_a_plugin_less_rh_timer_still_opens_after_a_restart() {
        // #405, "do not break existing events": events persisted before the GridFPV-plugin gate
        // may already select a RotorHazard timer with no plugin. Loading is deliberately NOT
        // validated — a hard load-time rejection would make such an event **unopenable**, which is
        // strictly worse than the problem it guards against. The event opens with its selection
        // intact; the refusals live at *selection* (`PUT /events/{id}/timers`, for newly added
        // ids) and at the *arm* (`control_handler`), where they can be acted on.
        use crate::timers::{CreateTimerRequest, TimerKind};

        let dir = std::env::temp_dir().join(format!("gridfpv-legacy-rh-{}", short_suffix()));
        let event_id;
        let rh_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let rh = reg
                .timers()
                .create(&CreateTimerRequest {
                    name: "Field RH".into(),
                    kind: TimerKind::Rotorhazard {
                        url: "http://rh.local:5000".into(),
                    },
                    channel_capability: None,
                    node_count: None,
                    available_channels: None,
                })
                .unwrap();
            rh_id = rh.id.clone();
            let created = reg
                .create(&CreateEventRequest {
                    name: "Legacy Cup".to_string(),
                    date: None,
                    location: None,
                    description: None,
                    organizer: None,
                })
                .unwrap();
            event_id = created.id.clone();
            reg.set_timers(&created.id, vec![rh.id.clone()]).unwrap();
            reg.set_primary_timer(&created.id, Some(rh.id)).unwrap();
        }

        // Restart. The RH timer restores with `plugin: None` (presence is never persisted), so on
        // a fresh boot every selected RH timer looks exactly like the pre-gate case.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened
            .meta_of(&event_id)
            .expect("an event selecting a plugin-less RH timer must still load");
        assert_eq!(restored.timers, vec![rh_id.clone()]);
        assert_eq!(restored.primary_timer, Some(rh_id.clone()));
        assert!(reopened.timers().get(&rh_id).unwrap().plugin.is_none());
        // …and it is fully openable: activatable, resolvable, appendable.
        reopened.set_active(&event_id).unwrap();
        let state = reopened
            .resolve(&event_id)
            .expect("the event's log must still open");
        assert!(state.read().is_ok());

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

        // The event is listed again with its metadata.
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

        // It is in the public list.
        let ids: Vec<_> = reopened.list().into_iter().map(|m| m.id).collect();
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
    fn delete_rejects_an_unknown_event_and_no_event_is_reserved() {
        let reg = EventRegistry::new(None).unwrap();
        // An unknown id is an error and removes nothing.
        assert!(reg.delete(&EventId("no-such-event".into())).is_err());
        // Nothing is reserved any more (#414): the old built-in `practice` id is just an
        // unknown event, and an event an RD names "Practice" deletes like any other.
        assert!(reg.delete(&EventId("practice".into())).is_err());
        let mine = reg.create(&req("Practice")).unwrap();
        assert!(reg.delete(&mine.id).is_ok());
        assert!(reg.resolve(&mine.id).is_none());
        assert!(reg.list().is_empty());
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
            layouts: Vec::new(),
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
            layouts: Vec::new(),
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
        // ActiveNodes, and the time limit round-trips.
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Practice Event")).unwrap();

        let round = reg
            .add_round(
                &event.id,
                NewRoundReq {
                    layouts: Vec::new(),
                    label: "Open Practice".into(),
                    classes: vec![],
                    format: "open_practice".into(),
                    params: BTreeMap::new(),
                    // No win condition — the form is not forced to supply one for open practice.
                    win_condition: None,
                    seeding: SeedingRule::ActiveNodes {
                        nodes: vec![0, 1, 2],
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
        assert!(matches!(round.seeding, SeedingRule::ActiveNodes { .. }));

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
            layouts: Vec::new(),
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

    /// #418 — a round whose heats are ALL still `Scheduled` deletes cleanly, heats included.
    ///
    /// The old gate refused on `has_heats`, so filling a round once made it undeletable forever.
    /// A practice round is exactly the one an RD misconfigures and throws away, and an unstarted
    /// heat holds nothing worth protecting.
    #[test]
    fn a_round_whose_heats_are_all_unstarted_deletes_with_its_heats() {
        use crate::live_state::{heat_summaries, heats_of_defined_rounds};
        use gridfpv_events::{CompetitorRef, Event, HeatId};

        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Practice Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let round = reg
            .add_round(&event.id, round_req("Practice", vec![open.clone()]))
            .unwrap();

        // Fill it: one heat, scheduled and never touched again.
        let state = reg.resolve(&event.id).unwrap();
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("p-1".into()),
                    lineup: vec![CompetitorRef("node-0".into())],
                    class: Some(open.clone()),
                    round: Some(round.id.clone()),
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();

        // It deletes.
        let meta = reg.remove_round(&event.id, &round.id).unwrap();
        assert!(meta.rounds.is_empty(), "the round is gone");

        // ...and its heat goes with it: the read side no longer lists a heat whose round the event
        // does not define. The log still carries the `HeatScheduled` (it is append-only); what
        // changed is that nothing renders it.
        let (events, _cursor) = state.read().unwrap();
        assert_eq!(
            heat_summaries(&events, None).len(),
            1,
            "the log is untouched"
        );
        let defined: Vec<RoundId> = meta.rounds.iter().map(|r| r.id.clone()).collect();
        assert!(
            heats_of_defined_rounds(heat_summaries(&events, Some(&defined)), &defined).is_empty(),
            "the removed round's heats are discarded on read"
        );
    }

    /// #418 — an UNTAGGED heat (the free-text / sim path) is never discarded: it belongs to no
    /// round and resolves its own name.
    #[test]
    fn discarding_a_removed_rounds_heats_leaves_untagged_heats_alone() {
        use crate::live_state::{heat_summaries, heats_of_defined_rounds};
        use gridfpv_events::{CompetitorRef, Event, HeatId};

        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Mixed Event")).unwrap();
        let state = reg.resolve(&event.id).unwrap();
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("free-1".into()),
                    lineup: vec![CompetitorRef("A".into())],
                    class: None,
                    round: None,
                    frequencies: vec![],
                    label: Some("Grudge match".into()),
                },
                None,
            )
            .unwrap();
        let (events, _cursor) = state.read().unwrap();
        assert_eq!(
            heats_of_defined_rounds(heat_summaries(&events, Some(&[])), &[]).len(),
            1,
            "an untagged heat survives a round list with nothing in it"
        );
    }

    /// #418 — a round with a heat IN PROGRESS is still refused, and the refusal NAMES the heat
    /// (never its raw id — repo display rule).
    #[test]
    fn a_round_with_a_heat_in_progress_refuses_removal_and_names_it() {
        use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};

        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Live Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let round = reg
            .add_round(&event.id, round_req("Qual", vec![open.clone()]))
            .unwrap();

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
        state
            .append(
                Event::HeatStateChanged {
                    heat: HeatId("q-1".into()),
                    transition: HeatTransition::Staged,
                },
                None,
            )
            .unwrap();

        let err = reg.remove_round(&event.id, &round.id).unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("in progress"),
            "the refusal says WHICH rule bit, got {message}"
        );
        assert!(
            message.contains("Qual Heat 1"),
            "the refusal names the heat, got {message}"
        );
        assert!(
            !message.contains("q-1"),
            "a raw heat id must never reach a user, got {message}"
        );
        assert!(
            !message.contains("discard"),
            "the refusal must not recommend a route that does not exist, got {message}"
        );
    }

    /// #418 — a round with a RACED heat is still refused, named, and for the other reason.
    #[test]
    fn a_round_with_raced_heats_refuses_removal_and_says_why() {
        use gridfpv_events::{CompetitorRef, Event, HeatId, HeatTransition};

        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Raced Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        let round = reg
            .add_round(&event.id, round_req("Qual", vec![open.clone()]))
            .unwrap();

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

        let err = reg.remove_round(&event.id, &round.id).unwrap_err();
        let message = format!("{err:?}");
        assert!(
            message.contains("raced heats"),
            "the refusal says WHICH rule bit, got {message}"
        );
        assert!(
            message.contains("Qual Heat 1"),
            "the refusal names the heat, got {message}"
        );
        assert!(
            !message.contains("discard"),
            "the refusal must not recommend a route that does not exist, got {message}"
        );
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
                    layouts: Vec::new(),
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
                layouts: Vec::new(),
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
                layouts: Vec::new(),
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
        practice.seeding = SeedingRule::ActiveNodes { nodes: vec![0, 1] };
        assert!(reg.add_round(&event.id, practice).is_ok());
    }

    /// A four-node RotorHazard timer, selected (and therefore primary) on `event`.
    fn seed_four_node_timer(reg: &EventRegistry, event: &EventId) -> TimerId {
        let timer = reg
            .timers()
            .create(&crate::timers::CreateTimerRequest {
                name: "Field RH".into(),
                kind: crate::timers::TimerKind::Rotorhazard {
                    url: "http://rh.local:5000".into(),
                },
                channel_capability: None,
                node_count: Some(4),
                available_channels: None,
            })
            .unwrap();
        reg.set_timers(event, vec![timer.id.clone()]).unwrap();
        timer.id
    }

    /// #416 — a STORED round seating onto a node the timer does not have is flagged on **read**,
    /// by friendly name, with the round to repair.
    ///
    /// This is the RD's live bench case: `ActiveNodes { nodes: [6] }` — node index 6, the 7th
    /// node — on a four-node timer. #412 refuses that at add and update, but the round on the
    /// bench predates the fix and nothing surfaced it, so the practice heat ran and recorded
    /// nothing at all.
    #[test]
    fn a_stored_round_seating_a_node_the_timer_does_not_have_is_flagged_on_read() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Bench Event")).unwrap();

        // Author the round BEFORE the timer is selected — with no resolvable timer there is no node
        // set to check against, which is exactly how a pre-#412 round came to be stored.
        let mut practice = round_req("Open Practice", vec![]);
        practice.format = "open_practice".to_string();
        practice.win_condition = None;
        practice.time_limit_secs = None;
        practice.seeding = SeedingRule::ActiveNodes { nodes: vec![6] };
        let round = reg.add_round(&event.id, practice).unwrap();
        assert!(
            reg.round_issues(&event.id).unwrap().is_empty(),
            "an event with no timer has nothing to check against"
        );

        seed_four_node_timer(&reg, &event.id);

        let issues = reg.round_issues(&event.id).unwrap();
        assert_eq!(issues.len(), 1, "one impossible seat, got {issues:?}");
        let issue = &issues[0];
        assert_eq!(issue.round, round.id);
        assert_eq!(issue.problem, SeatProblem::NoSuchNode);
        assert_eq!(issue.node, Some(6));
        // 1-based on screen, 0-based on the wire (repo display rule).
        assert_eq!(issue.node_label.as_deref(), Some("Node 7"));
        assert_eq!(issue.round_label, "Open Practice");
        assert_eq!(issue.timer_name.as_deref(), Some("Field RH"));
        assert!(
            issue.detail.contains("Node 7") && issue.detail.contains("Field RH"),
            "the sentence names the node and the timer: {}",
            issue.detail
        );
        assert!(
            !issue.detail.contains("node-6"),
            "a raw seat ref must never reach a user: {}",
            issue.detail
        );
    }

    /// #416 / #412 — a stored round seating a node the RD has **disabled** is flagged too, and
    /// says so distinctly from a node that does not exist.
    #[test]
    fn a_stored_round_seating_a_disabled_node_is_flagged_on_read() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Disabled Event")).unwrap();
        let timer = seed_four_node_timer(&reg, &event.id);

        let mut practice = round_req("Open Practice", vec![]);
        practice.format = "open_practice".to_string();
        practice.win_condition = None;
        practice.time_limit_secs = None;
        practice.seeding = SeedingRule::ActiveNodes { nodes: vec![0, 2] };
        reg.add_round(&event.id, practice).unwrap();
        assert!(
            reg.round_issues(&event.id).unwrap().is_empty(),
            "both seats are live to begin with"
        );

        // The RD switches node index 2 ("Node 3") off — a dead receiver. The round is now seating
        // a pilot on a gate nothing is listening to.
        reg.timers()
            .set_nodes(
                &timer,
                &crate::timers::SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some(vec![0, 1, 3]),
                },
            )
            .unwrap();

        let issues = reg.round_issues(&event.id).unwrap();
        assert_eq!(issues.len(), 1, "got {issues:?}");
        assert_eq!(issues[0].problem, SeatProblem::Disabled);
        assert_eq!(issues[0].node_label.as_deref(), Some("Node 3"));
        assert!(
            issues[0].detail.contains("switched off"),
            "the sentence says WHY: {}",
            issues[0].detail
        );
    }

    /// #416 / #412 / D27 — GridFPV configured wider than the hardware reported is a **notice**,
    /// not a refusal: the round still saves, and the impossible seat is surfaced on read.
    #[test]
    fn a_seat_beyond_what_the_timer_reported_is_a_notice_not_a_refusal() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Drift Event")).unwrap();
        let timer = seed_four_node_timer(&reg, &event.id);
        // The hardware says two nodes; GridFPV is configured for four.
        reg.timers().set_reported_nodes(&timer, 2);

        let mut practice = round_req("Open Practice", vec![]);
        practice.format = "open_practice".to_string();
        practice.win_condition = None;
        practice.time_limit_secs = None;
        practice.seeding = SeedingRule::ActiveNodes { nodes: vec![0, 3] };
        // The write is NOT refused — an observation about a timer is evidence, not a decision.
        reg.add_round(&event.id, practice)
            .expect("drift must not refuse the write (#412, D27)");

        let issues = reg.round_issues(&event.id).unwrap();
        assert_eq!(issues.len(), 1, "got {issues:?}");
        assert_eq!(issues[0].problem, SeatProblem::NotOnTimer);
        assert_eq!(issues[0].node_label.as_deref(), Some("Node 4"));
    }

    /// #416 — an event with no impossible seat reports nothing, and a non-practice round (whose
    /// field is pilots, not node seats) is never checked.
    #[test]
    fn round_issues_is_empty_for_a_healthy_event() {
        let reg = EventRegistry::new(None).unwrap();
        let event = reg.create(&req("Healthy Event")).unwrap();
        let open = seed_class(&reg, "Open");
        reg.set_classes(&event.id, vec![open.clone()]).unwrap();
        seed_four_node_timer(&reg, &event.id);
        reg.add_round(&event.id, round_req("Qual", vec![open]))
            .unwrap();

        assert!(reg.round_issues(&event.id).unwrap().is_empty());
        assert!(
            reg.round_issues(&EventId("nope".into())).is_none(),
            "an unknown event is a 404, not an empty list"
        );
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
        let active: SeedingRule =
            serde_json::from_str(r#"{ "ActiveNodes": { "nodes": [0, 1, 2] } }"#).unwrap();
        assert_eq!(
            active,
            SeedingRule::ActiveNodes {
                nodes: vec![0, 1, 2]
            }
        );
        // …and back out under the same tag and key — the wire form the console writes.
        assert_eq!(
            serde_json::to_string(&active).unwrap(),
            r#"{"ActiveNodes":{"nodes":[0,1,2]}}"#
        );
        // The PRE-rename tag is gone, deliberately: no `serde(alias)`, no migration (CLAUDE.md's
        // pre-release rule). A round stored under it is lost — and `lenient_rounds` is what keeps
        // that from taking the whole event with it.
        assert!(
            serde_json::from_str::<SeedingRule>(r#"{ "AllChannels": { "channels": [0, 1, 2] } }"#)
                .is_err(),
            "the old AllChannels tag must not be accepted"
        );
        // Nor is the old FIELD name under the new tag — half a rename is not a shape.
        assert!(
            serde_json::from_str::<SeedingRule>(r#"{ "ActiveNodes": { "channels": [0] } }"#)
                .is_err(),
            "the old `channels` key must not be accepted"
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
    fn an_event_whose_round_uses_the_pre_rename_seeding_tag_still_opens() {
        // `AllChannels { channels }` → `ActiveNodes { nodes }` with no alias and no migration
        // (CLAUDE.md's pre-release rule). The stored round IS lost — that is the deal — but the
        // event, its heats and its results are not: `EventMeta` is what `restore_persisted_events`
        // parses, so one unreadable round would otherwise vanish the whole event on boot.
        let dir = std::env::temp_dir().join(format!("gridfpv-old-seeding-{}", short_suffix()));
        let event_id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Bench Night")).unwrap();
            event_id = created.id.clone();
            let open = seed_class(&reg, "Open");
            reg.set_classes(&created.id, vec![open.clone()]).unwrap();
            // The round that will carry the old tag, and one that will not.
            reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
            reg.add_round(&created.id, round_req("Qualifying R1", vec![open]))
                .unwrap();
        }
        // Rewrite the practice round's seeding to the pre-rename shape, exactly as it sits in the
        // RD's sidecar `meta` table today.
        {
            let log = SqliteLog::open(event_db_path(&dir, &event_id)).unwrap();
            let json = log.get_meta(EVENT_META_KEY).unwrap().unwrap();
            let legacy = json.replace(
                r#""ActiveNodes":{"nodes":[0,1]}"#,
                r#""AllChannels":{"channels":[0,1]}"#,
            );
            assert_ne!(legacy, json, "the fixture must actually carry the old tag");
            log.set_meta(EVENT_META_KEY, &legacy).unwrap();
        }

        // Restart over the same data dir: the event OPENS.
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let restored = reopened.meta_of(&event_id).expect("the event still opens");
        assert_eq!(restored.name, "Bench Night");
        // …and the unreadable round — and only it — is gone. The RD recreates it.
        assert_eq!(
            restored.rounds.len(),
            1,
            "the old-tag round is dropped; every other round survives: {:?}",
            restored.rounds
        );
        assert_eq!(restored.rounds[0].label, "Qualifying R1");
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

    /// An open-practice round over `nodes` — the round the #387 report is written against (its
    /// heat's lineup *is* its active-node set, so editing it must reach the already-filled heat).
    fn practice_round(nodes: &[usize]) -> NewRoundReq {
        NewRoundReq {
            layouts: Vec::new(),
            label: "Practice".to_string(),
            classes: vec![],
            format: OpenPractice::NAME.to_string(),
            params: BTreeMap::new(),
            win_condition: None,
            seeding: SeedingRule::ActiveNodes {
                nodes: nodes.to_vec(),
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

    /// The same round as an **edit**, over a (possibly different) active-node set.
    fn practice_edit(label: &str, nodes: &[usize]) -> UpdateRoundReq {
        UpdateRoundReq {
            layouts: Vec::new(),
            label: label.to_string(),
            classes: vec![],
            format: OpenPractice::NAME.to_string(),
            params: BTreeMap::new(),
            win_condition: None,
            seeding: SeedingRule::ActiveNodes {
                nodes: nodes.to_vec(),
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
                layout,
                ..
            } => {
                let frequencies = match frequencies {
                    Some(freqs) => freqs,
                    None => round_engine::assign_for_event(&meta, &timers, None, &lineup).unwrap(),
                };
                // #117 S3: the handler records which layout the heat flies, before the schedule
                // carrying the channels it produced. Mirrored here or the fixture would drift from
                // the thing it is standing in for.
                if layout.is_some() {
                    state
                        .append(
                            Event::HeatLayoutSet {
                                heat: heat.clone(),
                                layout,
                            },
                            None,
                        )
                        .unwrap();
                }
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
    fn a_practice_round_is_not_offered_a_disabled_node() {
        // #412: an open-practice round's field IS its active channels (`node-{i}` seats), so a
        // round naming a node the RD switched off would show a lineup slot that can never record a
        // lap — the silent zero-lap heat, in its practice costume.
        use crate::timers::SetTimerNodesRequest;
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        // The event's primary timer is the built-in Mock (8 nodes). Switch off "Node 3" — index 2.
        let mock = TimerId(MOCK_TIMER_ID.to_string());
        reg.timers()
            .set_nodes(
                &mock,
                &SetTimerNodesRequest {
                    node_count: None,
                    enabled: Some(vec![0, 1, 3, 4, 5, 6, 7]),
                },
            )
            .unwrap();

        let err = reg
            .add_round(&created.id, practice_round(&[0, 1, 2]))
            .unwrap_err();
        assert!(
            // 1-based on screen (the repo display rule), 0-based on the wire.
            err.to_string().contains("Node 3"),
            "the refusal must name the node the way the RD does: {err}"
        );
        // …and a node the timer does not have at all.
        assert!(
            reg.add_round(&created.id, practice_round(&[0, 99]))
                .is_err()
        );

        // The enabled ones — including the one *past* the hole — are fine.
        let round = reg
            .add_round(&created.id, practice_round(&[0, 1, 3]))
            .unwrap();
        // An edit is guarded identically.
        assert!(
            reg.update_round(&created.id, &round.id, practice_edit("Practice", &[0, 2]))
                .is_err()
        );
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
                    layouts: Vec::new(),
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
                layouts: Vec::new(),
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

    // ── Event channel layouts (#117 S2) ──────────────────────────────────────────────────────────
    //
    // The registry's Mock is the fixture: 8 nodes, `available_channels` = Raceband R1–R8, and a
    // fresh event selects it. So the default event is exactly the "allowed set is the same size as
    // the node set" case a seed has to handle, and narrowing the allowed set / disabling a node is
    // how each refusal is provoked.

    /// The event's timer (the Mock), reconfigured for one test.
    fn tune_mock(reg: &EventRegistry, channels: Vec<u16>, disabled: Vec<u32>) {
        let timers = reg.timers();
        let mock = TimerId(MOCK_TIMER_ID.to_string());
        timers
            .update(
                &mock,
                &crate::timers::UpdateTimerRequest {
                    available_channels: Some(channels),
                    ..Default::default()
                },
            )
            .unwrap();
        timers
            .set_nodes(
                &mock,
                &crate::timers::SetTimerNodesRequest {
                    enabled: Some(
                        (0..8u32)
                            .filter(|n| !disabled.contains(n))
                            .collect::<Vec<_>>(),
                    ),
                    ..Default::default()
                },
            )
            .unwrap();
    }

    /// A `node → channel` mapping, for building a layout request by hand.
    fn nodes(pairs: &[(u32, u16)]) -> Vec<LayoutNode> {
        pairs
            .iter()
            .map(|(node, channel)| LayoutNode {
                node: *node,
                channel: *channel,
            })
            .collect()
    }

    /// The Raceband tuning of every one of the Mock's eight nodes — the layout a seed produces.
    fn raceband_layout() -> Vec<LayoutNode> {
        (0..8u32)
            .map(|node| LayoutNode {
                node,
                channel: crate::channels::RACEBAND_MHZ[node as usize],
            })
            .collect()
    }

    #[test]
    fn a_seeded_layout_takes_the_timers_allowed_set_in_order() {
        // The global→event seam: omitting `nodes` seeds enabled node `i` from allowed channel `i`,
        // in the RD's own preference order. The global record is the DEFAULT an event starts from.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        assert_eq!(view.layouts.len(), 1);
        assert_eq!(view.layouts[0].name, "Bracket A");
        assert!(view.layouts[0].id.0.starts_with("bracket-a-"));
        assert_eq!(view.layouts[0].nodes, raceband_layout());
        // Nothing about the global timer record changed — seeding reads it, it never writes it.
        let mock = reg.timers().get(&TimerId(MOCK_TIMER_ID.into())).unwrap();
        assert_eq!(mock.available_channels, crate::channels::RACEBAND_MHZ);
    }

    #[test]
    fn editing_a_layout_never_touches_the_global_allowed_set() {
        // The bug underneath this slice: the event workspace embeds the same TimerManager, so
        // "editing channels in the event" mutated the GLOBAL timer record. A layout is event state.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        let id = view.layouts[0].id.clone();
        // Re-tune every node onto a different allowed channel (reverse the Raceband order).
        let reversed: Vec<LayoutNode> = (0..8u32)
            .map(|node| LayoutNode {
                node,
                channel: crate::channels::RACEBAND_MHZ[7 - node as usize],
            })
            .collect();
        let after = reg
            .update_channel_layout(
                &created.id,
                &id,
                SetChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: reversed.clone(),
                },
            )
            .unwrap();
        assert_eq!(after.layouts[0].nodes, reversed);
        let mock = reg.timers().get(&TimerId(MOCK_TIMER_ID.into())).unwrap();
        assert_eq!(
            mock.available_channels,
            crate::channels::RACEBAND_MHZ,
            "the global allowed set is the seed, not the storage"
        );
    }

    #[test]
    fn a_valid_layout_round_trips_and_survives_a_restart() {
        // Layouts ride the event's persisted meta (issue #115), exactly like rounds/membership.
        let dir = std::env::temp_dir().join(format!("gridfpv-layouts-{}", short_suffix()));
        std::fs::create_dir_all(&dir).unwrap();
        let id;
        {
            let reg = EventRegistry::new(Some(dir.clone())).unwrap();
            let created = reg.create(&req("Race Night")).unwrap();
            id = created.id.clone();
            reg.add_channel_layout(
                &id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: Some(raceband_layout()),
                },
            )
            .unwrap();
        }
        let reopened = EventRegistry::new(Some(dir.clone())).unwrap();
        let view = reopened.channel_layouts(&id).unwrap();
        assert_eq!(view.layouts.len(), 1);
        assert_eq!(view.layouts[0].name, "Bracket A");
        assert_eq!(view.layouts[0].nodes, raceband_layout());
        // And the same layout is on the event's meta — one storage, not two.
        let meta = reopened.meta_of(&id).unwrap();
        assert_eq!(meta.channel_layouts, view.layouts);
    }

    #[test]
    fn two_nodes_on_the_same_channel_is_refused() {
        // The one hard rule INSIDE a layout: a node cannot share a frequency with its neighbour.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let mut clashing = raceband_layout();
        clashing[2].channel = clashing[1].channel;
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: Some(clashing),
                },
            )
            .unwrap_err();
        let LayoutError::Invalid(msg) = &err else {
            panic!("expected an Invalid refusal, got {err:?}");
        };
        // CLAUDE.md: the RD reads node LABELS and a band+channel name, never an index or a bare MHz.
        assert!(msg.contains("Node 2") && msg.contains("Node 3"), "{msg}");
        assert!(msg.contains("Raceband R2"), "{msg}");
        assert!(!msg.contains("5695"), "a bare MHz reached the RD: {msg}");
        // Nothing was stored.
        assert!(reg.channel_layouts(&created.id).unwrap().layouts.is_empty());
    }

    #[test]
    fn a_channel_outside_the_allowed_set_is_refused() {
        // S1's semantics one level up: `available_channels` is what this timer MAY use, and a layout
        // draws from it and nothing else — never from the catalog.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        tune_mock(&reg, vec![5658, 5695, 5732, 5769], vec![4, 5, 6, 7]);
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    // 5800 is Fatshark F4 — a real catalog channel the RD did not tick.
                    nodes: Some(nodes(&[(0, 5658), (1, 5695), (2, 5732), (3, 5800)])),
                },
            )
            .unwrap_err();
        let LayoutError::Invalid(msg) = &err else {
            panic!("expected an Invalid refusal, got {err:?}");
        };
        assert!(msg.contains("Fatshark F4"), "{msg}");
        assert!(msg.contains("Mock"), "the timer is named, not id'd: {msg}");
        assert!(msg.contains("Node 4"), "{msg}");
    }

    #[test]
    fn a_disabled_or_out_of_range_node_is_refused() {
        // #412: a disabled node seats nobody, so a layout must not pretend to tune it — and a node
        // beyond the timer's width does not exist to tune at all.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        tune_mock(&reg, crate::channels::RACEBAND_MHZ.to_vec(), vec![2]);

        let disabled = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: Some(nodes(&[
                        (0, 5658),
                        (1, 5695),
                        (2, 5732),
                        (3, 5769),
                        (4, 5806),
                        (5, 5843),
                        (6, 5880),
                        (7, 5917),
                    ])),
                },
            )
            .unwrap_err();
        let LayoutError::Invalid(msg) = &disabled else {
            panic!("expected an Invalid refusal, got {disabled:?}");
        };
        assert!(msg.contains("Node 3"), "{msg}");
        assert!(msg.contains("disabled or does not exist"), "{msg}");

        let beyond = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: Some(nodes(&[(99, 5658)])),
                },
            )
            .unwrap_err();
        assert!(
            matches!(&beyond, LayoutError::Invalid(msg) if msg.contains("Node 100")),
            "expected the out-of-range refusal, got {beyond:?}"
        );
    }

    #[test]
    fn a_layout_must_tune_every_enabled_node() {
        // A layout is a COMPLETE tuning: leaving a gate on whatever it was last set to is exactly
        // the D27 hole this model closes.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: Some(nodes(&[(0, 5658), (1, 5695)])),
                },
            )
            .unwrap_err();
        let LayoutError::Invalid(msg) = &err else {
            panic!("expected an Invalid refusal, got {err:?}");
        };
        assert!(msg.contains("Node 3"), "the first untuned node: {msg}");
        assert!(msg.contains("Node 8"), "and every other one: {msg}");
    }

    #[test]
    fn seeding_refuses_an_unconfigured_timer_rather_than_inventing_channels() {
        // The fifth-and-sixth instance of the empty-`available_channels` trap, headed off: empty
        // means "the RD has not configured this timer", never "this timer has no channels" — and
        // seeding from the catalog would scatter a layout across the band with no intent behind it.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        tune_mock(&reg, vec![], vec![]);
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap_err();
        assert!(
            matches!(&err, LayoutError::Invalid(msg)
                if msg.contains("Mock") && msg.contains("Timers page")),
            "expected the unconfigured-timer refusal, got {err:?}"
        );
    }

    #[test]
    fn seeding_refuses_when_the_allowed_set_cannot_cover_every_node() {
        // Four channels ticked, eight nodes enabled: there is no complete tuning to seed, and the
        // refusal names both numbers and both repairs rather than half-filling the layout.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        tune_mock(&reg, vec![5658, 5695, 5732, 5769], vec![]);
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap_err();
        assert!(
            matches!(&err, LayoutError::Invalid(msg)
                if msg.contains("4 channels") && msg.contains("8 enabled nodes")),
            "expected the too-few-channels refusal, got {err:?}"
        );
    }

    #[test]
    fn cross_layout_channel_overlap_warns_without_blocking() {
        // The RD's own call: reuse only matters for the keep-pilots-on-one-channel strategy, so it
        // is FLAGGED and never refused. A bracket run off one layout does not care.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let a = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        assert!(a.overlaps.is_empty(), "one layout overlaps nothing");

        // The identical tuning again, under a different name — the maximal overlap.
        let both = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket B".into(),
                    nodes: None,
                },
            )
            .expect("overlap is a warning, not a refusal");
        assert_eq!(both.layouts.len(), 2, "the layout was accepted and stored");
        assert_eq!(both.overlaps.len(), 1);
        assert_eq!(both.overlaps[0].layout, both.layouts[0].id);
        assert_eq!(both.overlaps[0].other, both.layouts[1].id);
        assert_eq!(
            both.overlaps[0].channels,
            crate::channels::RACEBAND_MHZ.to_vec()
        );
        // And the read agrees with the write — one computation, not two.
        assert_eq!(reg.channel_layouts(&created.id).unwrap(), both);
    }

    #[test]
    fn layouts_that_share_nothing_raise_no_warning() {
        // The GQ strategy done right: two disjoint packs, so nothing to flag.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        // Raceband R1-R4 and Fatshark F1/F2/F4/F8, on a four-node timer.
        tune_mock(
            &reg,
            vec![5658, 5695, 5732, 5769, 5740, 5760, 5800, 5880],
            vec![4, 5, 6, 7],
        );
        reg.add_channel_layout(
            &created.id,
            NewChannelLayoutRequest {
                name: "Pack A".into(),
                nodes: Some(nodes(&[(0, 5658), (1, 5695), (2, 5732), (3, 5769)])),
            },
        )
        .unwrap();
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Pack B".into(),
                    nodes: Some(nodes(&[(0, 5740), (1, 5760), (2, 5800), (3, 5880)])),
                },
            )
            .unwrap();
        assert_eq!(view.layouts.len(), 2);
        assert!(view.overlaps.is_empty(), "{:?}", view.overlaps);
    }

    #[test]
    fn a_layout_is_renamed_and_removed_by_id() {
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        let id = view.layouts[0].id.clone();
        let renamed = reg
            .update_channel_layout(
                &created.id,
                &id,
                SetChannelLayoutRequest {
                    name: "Mains".into(),
                    nodes: raceband_layout(),
                },
            )
            .unwrap();
        assert_eq!(renamed.layouts[0].name, "Mains");
        assert_eq!(renamed.layouts[0].id, id, "the id is fixed across an edit");

        let after = reg.remove_channel_layout(&created.id, &id).unwrap();
        assert!(after.layouts.is_empty());
        // Removing it twice is a 404, not a silent success.
        assert!(matches!(
            reg.remove_channel_layout(&created.id, &id),
            Err(LayoutError::LayoutNotFound(_))
        ));
    }

    #[test]
    fn two_layouts_cannot_share_a_name() {
        // The name is what an RD picks a layout BY (S3), so a duplicate is a mis-click, not a choice.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        reg.add_channel_layout(
            &created.id,
            NewChannelLayoutRequest {
                name: "Bracket A".into(),
                nodes: None,
            },
        )
        .unwrap();
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "  bracket a ".into(),
                    nodes: None,
                },
            )
            .unwrap_err();
        assert!(
            matches!(&err, LayoutError::Invalid(msg) if msg.contains("Bracket A")),
            "expected the duplicate-name refusal, got {err:?}"
        );
        // A blank name is refused for the same reason.
        assert!(matches!(
            reg.add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "   ".into(),
                    nodes: None,
                },
            ),
            Err(LayoutError::Invalid(_))
        ));
    }

    #[test]
    fn an_event_with_no_timer_cannot_define_a_layout() {
        // A layout is a tuning OF a timer. With no timer selected there is no node set to tune, and
        // the refusal says so rather than producing an empty layout.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        reg.set_timers(&created.id, vec![]).unwrap();
        let err = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap_err();
        assert!(
            matches!(&err, LayoutError::Invalid(msg) if msg.contains("no timer selected")),
            "expected the no-timer refusal, got {err:?}"
        );
    }

    #[test]
    fn layouts_are_addressed_within_their_own_event() {
        let reg = EventRegistry::new(None).unwrap();
        let a = reg.create(&req("Friday")).unwrap();
        let b = reg.create(&req("Saturday")).unwrap();
        let view = reg
            .add_channel_layout(
                &a.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        assert!(reg.channel_layouts(&b.id).unwrap().layouts.is_empty());
        assert!(matches!(
            reg.remove_channel_layout(&b.id, &view.layouts[0].id),
            Err(LayoutError::LayoutNotFound(_))
        ));
        assert!(reg.channel_layouts(&EventId("nope".into())).is_none());
    }

    // ── #117 S3: rounds name layouts, heats fly them ─────────────────────────────────────────

    /// An event with one channel layout over the Mock's eight Raceband nodes, and its id.
    fn event_with_layout(reg: &EventRegistry, name: &str) -> (EventId, LayoutId) {
        let created = reg.create(&req(name)).unwrap();
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        let id = view.layouts[0].id.clone();
        (created.id, id)
    }

    #[test]
    fn a_round_may_only_name_a_layout_the_event_has() {
        // A round pointing at a layout that does not exist would draw heats with no channels and no
        // explanation. Refused at write time, on add and on update alike.
        let reg = EventRegistry::new(None).unwrap();
        let (event, layout) = event_with_layout(&reg, "Race Night");

        let mut good = round_req("Bracket", vec![]);
        good.layouts = vec![layout.clone()];
        let round = reg.add_round(&event, good).unwrap();
        assert_eq!(round.layouts, vec![layout.clone()]);

        let mut bogus = round_req("Nope", vec![]);
        bogus.layouts = vec![LayoutId("never-existed".into())];
        let err = reg.add_round(&event, bogus).unwrap_err();
        assert!(
            matches!(&err, RoundError::Invalid(m) if m.contains("does not have")),
            "expected the unknown-layout refusal, got {err:?}"
        );

        // Naming the same layout twice says nothing useful — it only skews the cycle (#117 S3).
        let mut twice = round_req("Twice", vec![]);
        twice.layouts = vec![layout.clone(), layout];
        let err = reg.add_round(&event, twice).unwrap_err();
        assert!(
            matches!(&err, RoundError::Invalid(m) if m.contains("Bracket A") && m.contains("twice")),
            "the refusal names the layout, got {err:?}"
        );
    }

    #[test]
    fn a_layout_a_round_flies_cannot_be_deleted_out_from_under_it() {
        // The other end of the same hole: validation stops a round naming a layout that is gone,
        // and this stops a layout going while a round still names it. Both nouns by friendly name.
        let reg = EventRegistry::new(None).unwrap();
        let (event, layout) = event_with_layout(&reg, "Race Night");
        let mut req_round = round_req("Bracket", vec![]);
        req_round.layouts = vec![layout.clone()];
        reg.add_round(&event, req_round).unwrap();

        let err = reg.remove_channel_layout(&event, &layout).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Bracket A"), "names the layout: {msg}");
        assert!(msg.contains("Bracket"), "names the round: {msg}");
        assert!(!msg.contains(&layout.0), "leaks the raw layout id: {msg}");

        // Drop it from the round and the delete goes through.
        let rounds = reg.rounds_of(&event).unwrap();
        let mut edit = round_edit_of(&rounds[0]);
        edit.layouts = Vec::new();
        reg.update_round(&event, &rounds[0].id, edit).unwrap();
        assert!(reg.remove_channel_layout(&event, &layout).is_ok());
    }

    /// The `UpdateRoundReq` that reproduces `round` unchanged — for a test that edits one field.
    fn round_edit_of(round: &RoundDef) -> UpdateRoundReq {
        UpdateRoundReq {
            layouts: round.layouts.clone(),
            label: round.label.clone(),
            classes: round.classes.clone(),
            format: round.format.clone(),
            params: round.params.clone(),
            win_condition: Some(round.win_condition),
            seeding: round.seeding.clone(),
            time_limit_secs: round.time_limit_secs,
            channel_mode: Some(round.channel_mode),
            staging_timer_secs: Some(round.staging_timer_secs),
            start_procedure: Some(round.start_procedure.clone()),
            grace_window: Some(round.grace_window),
            protest_window: Some(round.protest_window),
            min_lap_secs: round.min_lap_secs,
        }
    }

    #[test]
    fn a_stale_layout_surfaces_on_round_issues() {
        // Layouts are validated at WRITE time only, and a stored one goes stale when the world moves
        // under it. #412 and #416 both landed on "validate stored config on read", so this is that
        // same read — `GET /events/{id}/round-issues` — rather than a second mechanism.
        let reg = EventRegistry::new(None).unwrap();
        let (event, layout) = event_with_layout(&reg, "Race Night");
        let mut req_round = round_req("Bracket", vec![]);
        req_round.layouts = vec![layout.clone()];
        reg.add_round(&event, req_round).unwrap();
        assert!(
            reg.round_issues(&event).unwrap().is_empty(),
            "a freshly-seeded layout is clean"
        );

        // Untick R3 on the timer AFTER the layout was written: node 3 is now on a channel this
        // timer is no longer allowed to use.
        let mut allowed = crate::channels::RACEBAND_MHZ.to_vec();
        let dropped = allowed.remove(2);
        tune_mock(&reg, allowed, vec![]);
        let issues = reg.round_issues(&event).unwrap();
        let stale: Vec<_> = issues
            .iter()
            .filter(|i| i.problem == SeatProblem::LayoutChannelNotAllowed)
            .collect();
        assert_eq!(stale.len(), 1, "one node lost its channel: {issues:?}");
        assert_eq!(stale[0].layout_name.as_deref(), Some("Bracket A"));
        assert_eq!(stale[0].node_label.as_deref(), Some("Node 3"));
        // Friendly names only — the round, the layout, the node and the CHANNEL, never a bare MHz.
        let detail = &stale[0].detail;
        assert!(detail.contains("Bracket"), "names the round: {detail}");
        assert!(detail.contains("Bracket A"), "names the layout: {detail}");
        assert!(detail.contains("Node 3"), "names the node: {detail}");
        assert!(
            detail.contains(&crate::timers::channel_label(dropped)),
            "names the channel by band+channel: {detail}"
        );
        assert!(
            !detail.contains(&dropped.to_string()),
            "leaks a bare MHz: {detail}"
        );
    }

    #[test]
    fn enabling_a_node_after_a_layout_was_written_leaves_it_untuned() {
        // The other staleness: the layout was a complete tuning when it was written, and then the
        // RD switched a node ON. A heat flying it would seat a pilot on a gate with no channel, so
        // the fill refuses — and this is where the RD is told, before they get there.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        // Seven enabled nodes when the layout is defined; node 7 comes back afterwards.
        tune_mock(&reg, crate::channels::RACEBAND_MHZ.to_vec(), vec![7]);
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Bracket A".into(),
                    nodes: None,
                },
            )
            .unwrap();
        let layout = view.layouts[0].id.clone();
        let mut req_round = round_req("Bracket", vec![]);
        req_round.layouts = vec![layout];
        reg.add_round(&created.id, req_round).unwrap();
        assert!(reg.round_issues(&created.id).unwrap().is_empty());

        tune_mock(&reg, crate::channels::RACEBAND_MHZ.to_vec(), vec![]);
        let issues = reg.round_issues(&created.id).unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].problem, SeatProblem::LayoutNodeUntuned);
        assert_eq!(issues[0].node_label.as_deref(), Some("Node 8"));
        assert!(issues[0].detail.contains("Node 8"));
    }

    /// A round with one layout, one still-`Scheduled` heat bound to it, and distinct friendly
    /// names for all three — the fixture the orphaned-bind tests share.
    ///
    /// Returns `(event, round, layout, heat)`. The round is labelled **Warmup**, the layout is
    /// **Bracket A** and the heat is **Practice Heat**: three different strings on purpose, so an
    /// assertion that the sentence names the heat cannot pass by naming the round.
    fn heat_bound_to_a_layout(reg: &EventRegistry) -> (EventId, RoundId, LayoutId, HeatId) {
        let (event, layout) = event_with_layout(reg, "Race Night");
        let mut req_round = practice_round(&[0, 1, 2]);
        req_round.label = "Warmup".to_string();
        req_round.layouts = vec![layout.clone()];
        let round = reg.add_round(&event, req_round).unwrap();
        let heat = fill_next_heat(reg, &event, &round.id);
        (event, round.id, layout, heat)
    }

    /// Drop every layout from `round` — the RD's edit that orphans a heat already bound to one.
    fn drop_layouts(reg: &EventRegistry, event: &EventId, round: &RoundId) {
        let stored = reg
            .rounds_of(event)
            .unwrap()
            .into_iter()
            .find(|r| &r.id == round)
            .expect("the round is stored");
        let mut edit = round_edit_of(&stored);
        edit.layouts = Vec::new();
        reg.update_round(event, round, edit)
            .expect("removing a layout from a round is allowed — the RD is told, not blocked");
    }

    #[test]
    fn a_heat_bound_to_a_layout_its_round_no_longer_names_is_reported() {
        // The RD's scenario, verbatim: "we create a round, it has a layout, create a heat from that
        // round, remove the layout from the round, but the heat stays on the layout channels?" Yes
        // — the bind is a logged event so a re-fill cannot lose it, and that same durability makes
        // it outlive a round edit. Nothing said so before this.
        let reg = EventRegistry::new(None).unwrap();
        let (event, round, layout, heat) = heat_bound_to_a_layout(&reg);
        assert!(
            reg.round_issues(&event).unwrap().is_empty(),
            "the bind is current while the round still names the layout"
        );

        drop_layouts(&reg, &event, &round);

        let issues = reg.round_issues(&event).unwrap();
        assert_eq!(issues.len(), 1, "one orphaned bind: {issues:?}");
        let issue = &issues[0];
        assert_eq!(issue.problem, SeatProblem::HeatLayoutNotInRound);
        assert_eq!(issue.heat.as_ref(), Some(&heat));
        // Every noun a person reads is a friendly name (CLAUDE.md).
        assert_eq!(issue.heat_name.as_deref(), Some("Practice Heat"));
        assert_eq!(issue.layout_name.as_deref(), Some("Bracket A"));
        assert_eq!(issue.round_label, "Warmup");
        // Not a timer question, and not about any one node — so neither is fabricated to fill the
        // struct, which is the display rule's exact failure mode.
        assert_eq!(issue.node, None);
        assert_eq!(issue.timer, None);

        let detail = &issue.detail;
        assert!(detail.contains("Practice Heat"), "names the heat: {detail}");
        assert!(detail.contains("Bracket A"), "names the layout: {detail}");
        assert!(detail.contains("Warmup"), "names the round: {detail}");
        for raw in [&heat.0, &layout.0, &round.0] {
            assert!(!detail.contains(raw.as_str()), "leaks a raw id: {detail}");
        }
        // Both repairs the RD has, because they chose to be told rather than blocked: rebind the
        // heat (`SetHeatLayout`), or set its channels by hand (`OverrideHeatSeating`).
        assert!(
            detail.contains("Pick a layout") && detail.contains("by hand"),
            "offers both remedies: {detail}"
        );
    }

    #[test]
    fn a_raced_heat_on_a_layout_its_round_dropped_is_left_alone() {
        // A raced heat flew what it flew, and its channels are the durable record of that. There is
        // nothing to repair and nothing to warn about — inviting the RD to "fix" it would be worse
        // than saying nothing.
        use gridfpv_events::HeatTransition;

        let reg = EventRegistry::new(None).unwrap();
        let (event, round, _layout, heat) = heat_bound_to_a_layout(&reg);
        // Orphan it FIRST: a raced round's layouts are frozen, so this is the only order in which
        // a raced heat can end up on a layout its round no longer names.
        drop_layouts(&reg, &event, &round);
        assert_eq!(
            reg.round_issues(&event).unwrap().len(),
            1,
            "reported while the heat is still Scheduled"
        );

        let state = reg.resolve(&event).unwrap();
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

        assert!(
            reg.round_issues(&event).unwrap().is_empty(),
            "a raced heat is never reported — it flew what it flew"
        );
    }

    /// #441: a round's layout decision must still reach the heats it generated.
    ///
    /// The fill records an **explicit** `HeatLayoutSet` bind for every heat it draws, even though
    /// the RD chose nothing about that heat — it only resolved the round's default. An explicit
    /// bind then wins in `layout_for_heat`, so swapping the round from Bracket A to Bracket B
    /// re-materializes each heat straight back onto A and `round_issues` flags every one of them
    /// as bound to a layout its round no longer names. A whole round's worth of manual repair for
    /// a decision the RD made once, at the round.
    ///
    /// A heat the RD has *personally* re-picked (`SetHeatLayout`) is a different matter and must
    /// keep its pick — that is what a bind is for. This heat was never touched.
    #[test]
    #[ignore = "known bug #441: the fill freezes a layout bind on every generated heat — un-ignore with the fix"]
    fn editing_a_rounds_layouts_re_tunes_the_heats_it_generated() {
        let reg = EventRegistry::new(None).unwrap();
        let (event, a) = event_with_layout(&reg, "Race Night");
        // A second complete tuning — the seeded Raceband order reversed — so which layout a heat
        // flies is legible from its channels alone.
        let reversed: Vec<LayoutNode> = crate::channels::RACEBAND_MHZ
            .iter()
            .rev()
            .enumerate()
            .map(|(node, channel)| LayoutNode {
                node: node as u32,
                channel: *channel,
            })
            .collect();
        let view = reg
            .add_channel_layout(
                &event,
                NewChannelLayoutRequest {
                    name: "Bracket B".into(),
                    nodes: Some(reversed),
                },
            )
            .unwrap();
        let b = view
            .layouts
            .iter()
            .find(|l| l.name == "Bracket B")
            .unwrap()
            .id
            .clone();

        let mut req_round = practice_round(&[0, 1, 2]);
        req_round.label = "Warmup".to_string();
        req_round.layouts = vec![a.clone()];
        let round = reg.add_round(&event, req_round).unwrap();
        let heat = fill_next_heat(&reg, &event, &round.id);
        let channels = |reg: &EventRegistry| -> Vec<u16> {
            heat_now(reg, &event, &heat)
                .1
                .iter()
                .map(|(_, f)| *f)
                .collect()
        };
        assert_eq!(
            channels(&reg),
            vec![5658, 5695, 5732],
            "the generated heat flies the round's only layout, Bracket A"
        );

        // The RD swaps the round onto Bracket B. Nobody has touched this heat.
        let stored = reg
            .rounds_of(&event)
            .unwrap()
            .into_iter()
            .find(|r| r.id == round.id)
            .expect("the round is stored");
        let mut edit = round_edit_of(&stored);
        edit.layouts = vec![b];
        reg.update_round(&event, &round.id, edit)
            .expect("swapping a round's layouts is allowed");

        assert_eq!(
            channels(&reg),
            vec![5917, 5880, 5843],
            "re-materializing the round re-tunes its untouched heat onto Bracket B"
        );
        assert!(
            reg.round_issues(&event).unwrap().is_empty(),
            "nothing to repair: the RD edited the round, and the round's heats followed — {:?}",
            reg.round_issues(&event).unwrap()
        );
    }

    #[test]
    fn an_orphaned_bind_to_a_deleted_layout_still_reports_without_naming_it() {
        // One step further along, and reachable precisely because the delete refusal is scoped to
        // ROUNDS: drop the layout from the round (creating the orphan), and the delete then goes
        // through. There is no layout left to name, which is the whole difference.
        let reg = EventRegistry::new(None).unwrap();
        let (event, round, layout, heat) = heat_bound_to_a_layout(&reg);
        drop_layouts(&reg, &event, &round);
        reg.remove_channel_layout(&event, &layout).unwrap();

        let issues = reg.round_issues(&event).unwrap();
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].problem, SeatProblem::HeatLayoutGone);
        assert_eq!(issues[0].heat_name.as_deref(), Some("Practice Heat"));
        assert_eq!(issues[0].layout, None, "there is no layout to point at");
        assert_eq!(issues[0].layout_name, None);
        let detail = &issues[0].detail;
        assert!(
            detail.contains("Practice Heat") && detail.contains("Warmup"),
            "{detail}"
        );
        assert!(
            detail.contains("deleted"),
            "says the layout is gone: {detail}"
        );
        assert!(
            !detail.contains(layout.0.as_str()),
            "leaks a raw id: {detail}"
        );
        assert!(
            !detail.contains(heat.0.as_str()),
            "leaks a raw id: {detail}"
        );
    }

    #[test]
    fn a_raced_rounds_layouts_are_frozen() {
        // A raced heat keeps the channels it raced on. Which layouts the round may fly decides what
        // a re-materialized heat is tuned to, so freezing them with the channel mode means the
        // question never arises.
        use gridfpv_events::{CompetitorRef, HeatTransition};

        let reg = EventRegistry::new(None).unwrap();
        let (event, layout) = event_with_layout(&reg, "Race Night");
        let round = reg.add_round(&event, round_req("Bracket", vec![])).unwrap();

        let state = reg.resolve(&event).unwrap();
        state
            .append(
                Event::HeatScheduled {
                    heat: HeatId("h1".into()),
                    lineup: vec![CompetitorRef("a".into())],
                    class: None,
                    round: Some(round.id.clone()),
                    frequencies: vec![],
                    label: None,
                },
                None,
            )
            .unwrap();
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
                        heat: HeatId("h1".into()),
                        transition,
                    },
                    None,
                )
                .unwrap();
        }

        let mut edit = round_edit_of(&round);
        edit.layouts = vec![layout];
        let err = reg.update_round(&event, &round.id, edit).unwrap_err();
        assert!(
            matches!(&err, RoundError::Invalid(m) if m.contains("channel layouts")),
            "expected the raced freeze to name the layouts, got {err:?}"
        );
    }

    /// A practice event flying `layout_nodes`, with an already-filled heat. Returns
    /// `(event, round, layout, heat)` — the fixture the heat→layout tests share.
    ///
    /// Open practice on purpose: its seats *name* their own nodes, so what each seat is tuned to is
    /// exactly the `heat → channel` mapping #402 said did not exist.
    fn practice_on_a_layout(
        reg: &EventRegistry,
        layout_nodes: &[(u32, u16)],
    ) -> (EventId, RoundId, LayoutId, HeatId) {
        let created = reg.create(&req("Race Night")).unwrap();
        let view = reg
            .add_channel_layout(
                &created.id,
                NewChannelLayoutRequest {
                    name: "Practice A".into(),
                    nodes: Some(nodes(layout_nodes)),
                },
            )
            .unwrap();
        let layout = view.layouts[0].id.clone();
        let mut round_req = practice_round(&[0, 1, 2]);
        round_req.layouts = vec![layout.clone()];
        let round = reg.add_round(&created.id, round_req).unwrap();
        let heat = fill_next_heat(reg, &created.id, &round.id);
        (created.id, round.id, layout, heat)
    }

    #[test]
    fn a_round_restricted_to_a_layout_fills_its_heat_on_that_layouts_channels() {
        // The whole of #117 S3 in one assertion, and the close of #402: a practice heat's seats now
        // carry real channels, drawn from the layout its round flies. Before this they were EMPTY
        // by construction — `frequencies: open_practice.then(Vec::new)` — on the false premise that
        // "its lineup is the active channels themselves".
        let reg = EventRegistry::new(None).unwrap();
        let (event, _round, layout, heat) = practice_on_a_layout(
            &reg,
            &[
                (0, 5658),
                (1, 5695),
                (2, 5732),
                (3, 5769),
                (4, 5806),
                (5, 5843),
                (6, 5880),
                (7, 5917),
            ],
        );

        let (lineup, freqs) = heat_now(&reg, &event, &heat);
        assert_eq!(lineup, refs(&["node-0", "node-1", "node-2"]));
        assert_eq!(
            freqs,
            vec![
                (CompetitorRef("node-0".into()), 5658),
                (CompetitorRef("node-1".into()), 5695),
                (CompetitorRef("node-2".into()), 5732),
            ],
            "each practice seat is on the channel its layout puts that node on"
        );

        // And the heat records WHICH layout it flew, so the answer survives the round's default
        // changing later.
        let (events, _) = reg.resolve(&event).unwrap().read().unwrap();
        assert_eq!(
            round_engine::heat_layout_bind(&events, &heat),
            Some(Some(layout)),
        );
    }

    #[test]
    fn editing_a_layout_re_tunes_the_scheduled_heat_flying_it() {
        // The RD must be able to fix a layout without deleting and rebuilding every heat under it.
        // Deliberately the #387 mechanism rather than a second one: re-materialize the rounds that
        // fly the edited layout, and only their still-`Scheduled` heats.
        let reg = EventRegistry::new(None).unwrap();
        let (event, _round, layout, heat) = practice_on_a_layout(
            &reg,
            &[
                (0, 5658),
                (1, 5695),
                (2, 5732),
                (3, 5769),
                (4, 5806),
                (5, 5843),
                (6, 5880),
                (7, 5917),
            ],
        );
        assert_eq!(heat_now(&reg, &event, &heat).1[0].1, 5658);

        // Swap node 0 onto R8 and node 2 onto R2 — a rename in the same breath, to prove the whole
        // layout is replaced wholesale.
        reg.update_channel_layout(
            &event,
            &layout,
            SetChannelLayoutRequest {
                name: "Practice B".into(),
                nodes: nodes(&[
                    (0, 5917),
                    (1, 5695),
                    (2, 5658),
                    (3, 5769),
                    (4, 5806),
                    (5, 5843),
                    (6, 5880),
                    (7, 5732),
                ]),
            },
        )
        .unwrap();

        assert_eq!(
            heat_now(&reg, &event, &heat).1,
            vec![
                (CompetitorRef("node-0".into()), 5917),
                (CompetitorRef("node-1".into()), 5695),
                (CompetitorRef("node-2".into()), 5658),
            ],
            "the scheduled heat is re-tuned in place — no delete-and-rebuild"
        );
    }

    #[test]
    fn editing_a_layout_leaves_a_heat_that_has_raced_on_the_channels_it_raced_on() {
        // The binding constraint. A result is never retroactively relabelled: only `Scheduled`
        // heats are re-materialized, and everything past that keeps what it flew.
        use gridfpv_events::HeatTransition;

        let reg = EventRegistry::new(None).unwrap();
        let (event, _round, layout, heat) = practice_on_a_layout(
            &reg,
            &[
                (0, 5658),
                (1, 5695),
                (2, 5732),
                (3, 5769),
                (4, 5806),
                (5, 5843),
                (6, 5880),
                (7, 5917),
            ],
        );
        let raced = heat_now(&reg, &event, &heat);
        let state = reg.resolve(&event).unwrap();
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

        reg.update_channel_layout(
            &event,
            &layout,
            SetChannelLayoutRequest {
                name: "Practice A".into(),
                nodes: nodes(&[
                    (0, 5917),
                    (1, 5880),
                    (2, 5843),
                    (3, 5806),
                    (4, 5769),
                    (5, 5732),
                    (6, 5695),
                    (7, 5658),
                ]),
            },
        )
        .unwrap();

        assert_eq!(
            heat_now(&reg, &event, &heat),
            raced,
            "a heat that has raced keeps the channels it raced on"
        );
    }

    #[test]
    fn a_manual_seating_override_survives_a_re_fill() {
        // "An override silently lost when a round is refilled is worse than none" (#419). The fill
        // and the #387 re-materialization both apply it through the SAME `apply_heat_decisions`,
        // which is what makes forgetting impossible rather than merely unlikely.
        let reg = EventRegistry::new(None).unwrap();
        let (event, round, layout, heat) = practice_on_a_layout(
            &reg,
            &[
                (0, 5658),
                (1, 5695),
                (2, 5732),
                (3, 5769),
                (4, 5806),
                (5, 5843),
                (6, 5880),
                (7, 5917),
            ],
        );
        let state = reg.resolve(&event).unwrap();

        // The RD re-seats the heat by hand: two seats, on nodes 3 and 4.
        state
            .append(
                Event::HeatSeatingOverridden {
                    heat: heat.clone(),
                    lineup: refs(&["node-3", "node-4"]),
                    frequencies: vec![],
                },
                None,
            )
            .unwrap();

        // Re-fill the round the way a round edit does — the real path, which re-materializes and
        // appends. The override wins over the plan the round would otherwise produce.
        let mut edit = practice_edit("Practice", &[0, 1, 2]);
        edit.layouts = vec![layout.clone()];
        reg.update_round(&event, &round, edit).unwrap();

        let (lineup, freqs) = heat_now(&reg, &event, &heat);
        assert_eq!(
            lineup,
            refs(&["node-3", "node-4"]),
            "the RD's seating survived the round being re-formed"
        );
        assert_eq!(
            freqs,
            vec![
                (CompetitorRef("node-3".into()), 5769),
                (CompetitorRef("node-4".into()), 5806),
            ],
            "the RD's pilots, the layout's channels"
        );
        let (events, _) = state.read().unwrap();
        assert_eq!(
            round_engine::heat_layout_bind(&events, &heat),
            Some(Some(layout.clone())),
            "and it is still recorded against the layout it flies"
        );

        // An empty lineup is the explicit clear — the heat goes back to its round's own plan.
        state
            .append(
                Event::HeatSeatingOverridden {
                    heat: heat.clone(),
                    lineup: vec![],
                    frequencies: vec![],
                },
                None,
            )
            .unwrap();
        let mut edit = practice_edit("Practice", &[0, 1, 2]);
        edit.layouts = vec![layout];
        reg.update_round(&event, &round, edit).unwrap();
        assert_eq!(
            heat_now(&reg, &event, &heat).0,
            refs(&["node-0", "node-1", "node-2"]),
            "clearing the override returns the heat to its round's plan"
        );
    }

    #[test]
    fn an_override_may_set_the_pilots_and_leave_the_channels_to_the_layout() {
        // An RD swapping two pilots should not have to retype four frequencies — and an RD who
        // *does* type them gets exactly what they typed.
        let reg = EventRegistry::new(None).unwrap();
        let (event, round, _layout, heat) = practice_on_a_layout(
            &reg,
            &[
                (0, 5658),
                (1, 5695),
                (2, 5732),
                (3, 5769),
                (4, 5806),
                (5, 5843),
                (6, 5880),
                (7, 5917),
            ],
        );
        let state = reg.resolve(&event).unwrap();
        state
            .append(
                Event::HeatSeatingOverridden {
                    heat: heat.clone(),
                    lineup: refs(&["node-0", "node-1"]),
                    frequencies: vec![
                        (CompetitorRef("node-0".into()), 5917),
                        (CompetitorRef("node-1".into()), 5880),
                    ],
                },
                None,
            )
            .unwrap();
        let mut edit = practice_edit("Practice", &[0, 1, 2]);
        edit.layouts = vec![_layout];
        reg.update_round(&event, &round, edit).unwrap();
        assert_eq!(
            heat_now(&reg, &event, &heat).1,
            vec![
                (CompetitorRef("node-0".into()), 5917),
                (CompetitorRef("node-1".into()), 5880),
            ],
            "typed channels win over the layout's"
        );
    }

    #[test]
    fn a_round_that_names_no_layout_is_unchanged_by_all_of_this() {
        // The pre-S3 behaviour, kept: no layout means no `heat → channel` mapping to draw on, and
        // a practice heat's frequencies stay empty rather than being invented from the allowed set.
        let reg = EventRegistry::new(None).unwrap();
        let created = reg.create(&req("Race Night")).unwrap();
        let round = reg.add_round(&created.id, practice_round(&[0, 1])).unwrap();
        let heat = fill_next_heat(&reg, &created.id, &round.id);
        let (lineup, freqs) = heat_now(&reg, &created.id, &heat);
        assert_eq!(lineup, refs(&["node-0", "node-1"]));
        assert!(freqs.is_empty(), "nothing is invented from the allowed set");
        let (events, _) = reg.resolve(&created.id).unwrap().read().unwrap();
        assert_eq!(round_engine::heat_layout_bind(&events, &heat), None);
    }

    #[test]
    fn an_event_stored_under_the_old_channel_layers_key_still_loads() {
        // Pre-release: no migration, no old-key fallback — the field is simply lost. What is NOT
        // acceptable is failing to open the event, so the unknown key must be ignored on read.
        let json = serde_json::json!({
            "id": "e1",
            "name": "Race Night",
            "created_at": 0,
            "persistent": true,
            "channel_layers": [
                { "id": "bracket-a-xyz", "name": "Bracket A", "nodes": [{ "node": 0, "channel": 5658 }] }
            ]
        });
        let meta: EventMeta = serde_json::from_value(json).expect("an old-shape meta still loads");
        assert_eq!(meta.name, "Race Night");
        assert!(
            meta.channel_layouts.is_empty(),
            "the old key is ignored, not adopted"
        );
    }
}
