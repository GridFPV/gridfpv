//! Multi-class scheduling + engine-side frequency allocation (#36).
//!
//! Several classes can be **live at once**, each running its own format, while
//! sharing two scarce resources: the **timer** (only one heat flies at a time) and,
//! for IRL sources, the **frequency pool** (each pilot in a heat needs a distinct
//! video channel) — race-engine.html §5, "Multi-class scheduling". This module is
//! the engine-side machinery that interleaves heats across those classes and hands
//! out the shared resources:
//!
//! - [`FrequencyPool`] / [`allocate`] — the engine **allocates** channels to a
//!   heat's pilots; the adapter later **applies** the tuning. This is the resolved
//!   ownership split of RE §7.3: *"The Race Engine allocates channels (it knows the
//!   heat / field); the adapter advertises its capabilities and applies the
//!   tuning."* Allocation here is the pure, deterministic "who flies on what
//!   channel" decision — no hardware, no clock, no RNG.
//! - [`Scheduler`] / [`Scheduler::next_heat`] — holds several classes and the shared
//!   pool, returns the **one** heat that flies next (the timer is serialized),
//!   allocating that heat's frequencies from the shared pool as it goes.
//!
//! # Policy (RE §7, R7 — picked and documented here)
//!
//! RE §7 leaves the *interleaving* and *frequency-allocation* policies open. This
//! module commits to the simplest fair, fully-deterministic pair:
//!
//! 1. **Interleaving — round-robin across classes.** [`Scheduler::next_heat`]
//!    advances classes in a fixed rotation so no class starves: each call serves the
//!    next class (in add order) that still has a heat to fly, then moves the cursor
//!    on. Over a run the heats of two equally-busy classes alternate `A, B, A, B, …`;
//!    a class that completes simply drops out of the rotation and the others carry
//!    on. A format whose generator emits several heats in one
//!    [`GeneratorStep::Run`] has them **buffered** and drained one-per-turn, so a
//!    multi-heat round still interleaves with the other classes rather than
//!    monopolising the timer.
//! 2. **Frequencies — deterministic first-fit by lineup order.** [`allocate`] walks
//!    the heat's `lineup` in order and assigns each pilot the **first** channel of
//!    the pool not yet taken in that heat (so the top seed gets the pool's first
//!    channel, etc.). Channels are distinct within a heat and freed when the heat
//!    ends (the pool is shared across classes but only one heat flies at a time, so
//!    the whole pool is available to each heat). Too few channels for the lineup is
//!    an [`AllocError`].
//!
//! Both choices are pure and replayable (RE §6): given the same classes, the same
//! generator outputs, and the same recorded results, [`Scheduler::next_heat`] yields
//! the same heats with the same channels every run.
//!
//! # Sim sources have no frequency step (RE §5)
//!
//! IRL staging "assigns frequencies/channels and avoids conflicts"; a simulator has
//! **no frequency step** (RE §5, sim note). A class is therefore added either as an
//! IRL class ([`Scheduler::add_class`], allocation runs) or a sim class
//! ([`Scheduler::add_sim_class`], allocation is skipped and the heat's `frequencies`
//! come back empty). The engine never asks "is this a sim?" by type — it carries a
//! per-class flag set from the source's frequency-management capability, mirroring
//! the adapters' capability model.
#![forbid(unsafe_code)]

use std::error::Error;
use std::fmt;

use gridfpv_events::{CompetitorRef, HeatId};

use crate::format::{CompletedHeat, Generator, GeneratorStep, HeatPlan, RankEntry};

// --- Frequencies ------------------------------------------------------------

/// A video channel a pilot flies on, identified by its frequency in **MHz**.
///
/// A thin wrapper over the band frequency (e.g. 5658 = Raceband R1). The engine only
/// needs distinct, comparable handles to hand out; the adapter maps the MHz back to
/// whatever band/channel label its hardware tunes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Frequency {
    /// The channel's centre frequency in megahertz.
    pub mhz: u16,
}

impl Frequency {
    /// A frequency from a raw MHz value.
    pub const fn new(mhz: u16) -> Self {
        Self { mhz }
    }
}

impl fmt::Display for Frequency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} MHz", self.mhz)
    }
}

/// The shared pool of channels a heat's pilots are allocated from.
///
/// Channels are tried **in order** by [`allocate`] (first-fit), so the pool's order
/// is the allocation preference. The pool is shared across all classes in a
/// [`Scheduler`]; because the timer is serialized (one heat at a time) the whole pool
/// is available to whichever heat flies next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrequencyPool {
    channels: Vec<Frequency>,
}

impl FrequencyPool {
    /// A pool from an explicit, ordered list of channels. Duplicate channels are
    /// de-duplicated (keeping first occurrence) so a heat never gets the same channel
    /// twice from one pool.
    pub fn new(channels: impl IntoIterator<Item = Frequency>) -> Self {
        let mut seen: Vec<Frequency> = Vec::new();
        for channel in channels {
            if !seen.contains(&channel) {
                seen.push(channel);
            }
        }
        Self { channels: seen }
    }

    /// The standard **5.8 GHz Raceband** R1–R8 pool — the common 8-pilot default.
    ///
    /// R1 5658, R2 5695, R3 5732, R4 5769, R5 5806, R6 5843, R7 5880, R8 5917 MHz, in
    /// channel order (R1 first), so first-fit hands the top seed R1.
    pub fn raceband() -> Self {
        Self::new([
            Frequency::new(5658),
            Frequency::new(5695),
            Frequency::new(5732),
            Frequency::new(5769),
            Frequency::new(5806),
            Frequency::new(5843),
            Frequency::new(5880),
            Frequency::new(5917),
        ])
    }

    /// The channels in the pool, in allocation-preference order.
    pub fn channels(&self) -> &[Frequency] {
        &self.channels
    }

    /// How many distinct channels the pool holds — the largest heat it can fill.
    pub fn capacity(&self) -> usize {
        self.channels.len()
    }
}

/// Allocating channels failed: the pool has fewer channels than the lineup needs.
///
/// Carries how many pilots needed a channel and how many the pool could offer, so the
/// RD sees exactly how short the pool was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllocError {
    /// Pilots in the lineup needing a distinct channel.
    pub needed: usize,
    /// Distinct channels the pool could supply.
    pub available: usize,
}

impl fmt::Display for AllocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "frequency pool too small: {} pilot(s) need a channel but only {} available",
            self.needed, self.available
        )
    }
}

impl Error for AllocError {}

/// Allocate a **distinct** channel to each pilot in a heat — the engine's half of the
/// RE §7.3 split (the engine allocates; the adapter applies).
///
/// Deterministic **first-fit by lineup order**: pilots are walked in `lineup` order
/// and each is given the first pool channel not already taken by an earlier pilot in
/// this heat. So the lineup's top seed gets [`FrequencyPool::channels`]`[0]`, the next
/// gets `[1]`, and so on. The result preserves lineup order, pairing each
/// [`CompetitorRef`] with its [`Frequency`].
///
/// Returns [`AllocError`] when the pool has fewer distinct channels than there are
/// pilots. A duplicated competitor in the lineup still consumes a channel per
/// occurrence (the caller is responsible for a sane lineup; allocation is purely the
/// "lay channels onto seats" step).
pub fn allocate(
    lineup: &[CompetitorRef],
    pool: &FrequencyPool,
) -> Result<Vec<(CompetitorRef, Frequency)>, AllocError> {
    if lineup.len() > pool.capacity() {
        return Err(AllocError {
            needed: lineup.len(),
            available: pool.capacity(),
        });
    }
    let assignment = lineup
        .iter()
        .cloned()
        .zip(pool.channels().iter().copied())
        .collect();
    Ok(assignment)
}

// --- Multi-class scheduling -------------------------------------------------

/// One heat the scheduler hands out to fly next: which class it belongs to, the heat
/// id and lineup the generator chose, and the channels the engine allocated for it.
///
/// `frequencies` pairs each pilot with its allocated [`Frequency`] in lineup order for
/// an IRL class; for a **sim** class it is **empty** (no frequency step — RE §5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduledHeat {
    /// The class (by name) this heat belongs to.
    pub class: String,
    /// The heat id the generator assigned.
    pub heat: HeatId,
    /// The competitors in the heat, in the generator's seed order.
    pub lineup: Vec<CompetitorRef>,
    /// The allocated channels, one per pilot in lineup order — empty for a sim class.
    pub frequencies: Vec<(CompetitorRef, Frequency)>,
}

/// One class in the scheduler: its name, its live generator, the history of heats it
/// has completed, whether it needs frequencies, and any heats buffered from a
/// multi-heat [`GeneratorStep::Run`] not yet handed out.
struct ClassState {
    /// The class name (a [`ScheduledHeat::class`] and the key [`Scheduler::record`]
    /// addresses).
    name: String,
    /// The class's format generator.
    generator: Box<dyn Generator>,
    /// Every scored heat fed back via [`Scheduler::record`] — the generator's input.
    completed: Vec<CompletedHeat>,
    /// Whether this class's source manages frequencies (IRL true, sim false).
    needs_frequencies: bool,
    /// Heats the generator emitted but the scheduler has not handed out yet (a
    /// multi-heat `Run` is buffered and drained one per turn so it still interleaves).
    pending: Vec<HeatPlan>,
    /// `true` once the generator has returned [`GeneratorStep::Complete`] with nothing
    /// buffered — the class is done and drops out of the rotation.
    complete: bool,
}

impl ClassState {
    /// Refill the heat buffer from the generator if it is empty and the format is not
    /// already complete: pull one [`Generator::next`] step over the **current**
    /// completed history — a `Run` buffers its heats, a `Complete` marks the class
    /// done. Pulling only when the buffer is empty (never looking ahead past a heat the
    /// caller has not yet recorded a result for) keeps a history-dependent generator
    /// correct: it always sees every recorded result before it decides the next heat.
    fn refill(&mut self) {
        if self.pending.is_empty() && !self.complete {
            match self.generator.next(&self.completed) {
                // A generator never emits an empty Run (format.rs contract); an empty
                // buffer still then reads as "nothing to fly right now".
                GeneratorStep::Run(plans) => self.pending = plans,
                GeneratorStep::Complete => self.complete = true,
            }
        }
    }

    /// The next heat this class wants to fly, or `None` when the class is complete.
    /// Refills the buffer (pulling the generator's first/next step) then removes one
    /// buffered heat.
    fn next_plan(&mut self) -> Option<HeatPlan> {
        self.refill();
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }

    /// Whether this class can still produce a heat *right now*, refilling its buffer to
    /// find out. Probing here (rather than reading a possibly-stale `complete` flag)
    /// makes [`Scheduler::is_complete`] exact — a class whose generator has emitted its
    /// last heat is recognised as done the moment its buffer drains, without waiting
    /// for the next [`Scheduler::next_heat`]. The probe only pulls `next` when the
    /// buffer is empty, so it never looks past an unrecorded heat.
    fn active(&mut self) -> bool {
        self.refill();
        !self.complete || !self.pending.is_empty()
    }
}

/// Interleaves several classes over the shared timer and frequency pool (RE §5).
///
/// Build with [`Scheduler::new`] (the shared pool), add classes with
/// [`Scheduler::add_class`] (IRL — allocates frequencies) or
/// [`Scheduler::add_sim_class`] (sim — no frequency step), then drive the loop:
/// [`Scheduler::next_heat`] returns the one heat that flies next (round-robin across
/// classes, frequencies allocated), and [`Scheduler::record`] feeds the scored result
/// back to its class.
pub struct Scheduler {
    /// The classes, in add order — the round-robin rotation order.
    classes: Vec<ClassState>,
    /// The shared frequency pool every IRL heat allocates from.
    pool: FrequencyPool,
    /// The rotation cursor: the index of the class to *try first* on the next
    /// [`Scheduler::next_heat`] call. Advances by one each time a heat is handed out so
    /// classes take turns and none starves.
    cursor: usize,
}

impl Scheduler {
    /// A scheduler with no classes yet, sharing `pool` across every IRL class.
    pub fn new(pool: FrequencyPool) -> Self {
        Self {
            classes: Vec::new(),
            pool,
            cursor: 0,
        }
    }

    /// Add an **IRL** class: its heats get frequencies allocated from the shared pool.
    ///
    /// `name` keys [`Scheduler::record`] and labels the class's [`ScheduledHeat`]s.
    /// Classes run in add order in the round-robin.
    pub fn add_class(&mut self, name: impl Into<String>, generator: Box<dyn Generator>) {
        self.push_class(name.into(), generator, true);
    }

    /// Add a **sim** class: it has no frequency step (RE §5), so its scheduled heats
    /// come back with empty `frequencies`. Otherwise identical to
    /// [`Scheduler::add_class`].
    pub fn add_sim_class(&mut self, name: impl Into<String>, generator: Box<dyn Generator>) {
        self.push_class(name.into(), generator, false);
    }

    fn push_class(&mut self, name: String, generator: Box<dyn Generator>, needs_frequencies: bool) {
        self.classes.push(ClassState {
            name,
            generator,
            completed: Vec::new(),
            needs_frequencies,
            pending: Vec::new(),
            complete: false,
        });
    }

    /// The next heat to fly across all classes, or `None` when **every** class is
    /// complete.
    ///
    /// Round-robin: starting from the rotation cursor, find the next class (in add
    /// order, wrapping) that still has a heat to fly, hand out that heat, and advance
    /// the cursor past it so the following call prefers the next class. The heat's
    /// frequencies are allocated from the shared pool for an IRL class (empty for a
    /// sim class). Returns `None` only once no class can produce another heat.
    ///
    /// # Panics
    ///
    /// Panics if an IRL heat's lineup is larger than the shared pool ([`allocate`]
    /// returns [`AllocError`]). A class whose heats can exceed the pool is a
    /// configuration error the RD must fix (a bigger pool or smaller heats); the
    /// scheduler surfaces it loudly rather than silently dropping pilots. Callers
    /// needing to handle it gracefully can pre-check with [`allocate`].
    pub fn next_heat(&mut self) -> Option<ScheduledHeat> {
        let count = self.classes.len();
        for offset in 0..count {
            let index = (self.cursor + offset) % count;
            if let Some(plan) = self.classes[index].next_plan() {
                // Advance the rotation past the class we just served so the next call
                // prefers the following class (fair round-robin, no starvation).
                self.cursor = (index + 1) % count;

                let class = &self.classes[index];
                let frequencies = if class.needs_frequencies {
                    allocate(&plan.lineup, &self.pool).unwrap_or_else(|e| {
                        panic!("class {:?} heat {:?}: {e}", class.name, plan.heat.0)
                    })
                } else {
                    Vec::new()
                };
                return Some(ScheduledHeat {
                    class: class.name.clone(),
                    heat: plan.heat,
                    lineup: plan.lineup,
                    frequencies,
                });
            }
        }
        None
    }

    /// Feed a scored heat back to its class's generator.
    ///
    /// `class` is the [`ScheduledHeat::class`] the heat came from; the completed heat
    /// is appended to that class's history so its generator sees it on the next
    /// [`Scheduler::next_heat`]. Unknown class names are ignored (no panic) — a result
    /// for a class the scheduler does not hold has nowhere to go.
    pub fn record(&mut self, class: &str, completed: CompletedHeat) {
        if let Some(state) = self.classes.iter_mut().find(|c| c.name == class) {
            state.completed.push(completed);
        }
    }

    /// `true` once every class has completed and [`Scheduler::next_heat`] will only
    /// ever return `None`.
    ///
    /// Takes `&mut self` because confirming a class is finished may **probe** its
    /// generator (ask it for the next step over the recorded history); the probe only
    /// buffers a heat the very next `next_heat` would return anyway, so it changes no
    /// observable behaviour. For a history-dependent class, record all results before
    /// calling this (the same ordering the `next_heat`/`record` loop already follows).
    pub fn is_complete(&mut self) -> bool {
        self.classes.iter_mut().all(|c| !c.active())
    }

    /// The current ranking for the class named `name`, or `None` if there is no such
    /// class. Provisional while the class runs, final once it has completed.
    pub fn rankings(&self, name: &str) -> Option<Vec<RankEntry>> {
        self.classes
            .iter()
            .find(|c| c.name == name)
            .map(|c| c.generator.ranking(&c.completed))
    }

    /// The names of the classes the scheduler holds, in add (rotation) order.
    pub fn class_names(&self) -> Vec<&str> {
        self.classes.iter().map(|c| c.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scoring::HeatResult;

    fn cref(name: &str) -> CompetitorRef {
        CompetitorRef(name.into())
    }

    fn field(names: &[&str]) -> Vec<CompetitorRef> {
        names.iter().map(|n| cref(n)).collect()
    }

    /// A trivial empty [`HeatResult`] for the scheduler tests, which only need *a*
    /// result to feed back — the scheduler never inspects its contents.
    fn empty_result() -> HeatResult {
        HeatResult {
            places: Vec::new(),
            ..Default::default()
        }
    }

    // --- Frequency allocation -----------------------------------------------

    #[test]
    fn raceband_pool_is_r1_through_r8_in_order() {
        let pool = FrequencyPool::raceband();
        assert_eq!(pool.capacity(), 8);
        assert_eq!(pool.channels()[0], Frequency::new(5658)); // R1
        assert_eq!(pool.channels()[7], Frequency::new(5917)); // R8
    }

    #[test]
    fn allocate_assigns_distinct_channels_first_fit_in_lineup_order() {
        let pool = FrequencyPool::raceband();
        let lineup = field(&["A", "B", "C"]);
        let alloc = allocate(&lineup, &pool).expect("pool has room");

        // One channel per pilot, in lineup order, top seed gets R1.
        assert_eq!(alloc.len(), 3);
        assert_eq!(alloc[0], (cref("A"), Frequency::new(5658)));
        assert_eq!(alloc[1], (cref("B"), Frequency::new(5695)));
        assert_eq!(alloc[2], (cref("C"), Frequency::new(5732)));

        // The channels are distinct.
        let channels: Vec<Frequency> = alloc.iter().map(|(_, f)| *f).collect();
        let mut deduped = channels.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(channels.len(), deduped.len(), "channels must be distinct");
    }

    #[test]
    fn allocate_is_deterministic() {
        let pool = FrequencyPool::raceband();
        let lineup = field(&["X", "Y", "Z", "W"]);
        assert_eq!(
            allocate(&lineup, &pool).unwrap(),
            allocate(&lineup, &pool).unwrap(),
            "same lineup + pool always allocate the same channels"
        );
    }

    #[test]
    fn allocate_errors_when_the_pool_is_too_small() {
        // A 2-channel pool cannot seat a 3-pilot heat.
        let pool = FrequencyPool::new([Frequency::new(5658), Frequency::new(5695)]);
        let err = allocate(&field(&["A", "B", "C"]), &pool).unwrap_err();
        assert_eq!(
            err,
            AllocError {
                needed: 3,
                available: 2
            }
        );
        // It is a real error with a helpful message.
        assert!(format!("{err}").contains("too small"));
    }

    #[test]
    fn allocate_exactly_fills_the_pool() {
        let pool = FrequencyPool::new([Frequency::new(5658), Frequency::new(5695)]);
        let alloc = allocate(&field(&["A", "B"]), &pool).unwrap();
        assert_eq!(alloc.len(), 2);
    }

    #[test]
    fn pool_dedupes_repeated_channels() {
        let pool = FrequencyPool::new([
            Frequency::new(5658),
            Frequency::new(5658),
            Frequency::new(5695),
        ]);
        assert_eq!(pool.capacity(), 2);
    }

    #[test]
    fn sim_class_allocation_is_a_no_op() {
        // A sim class's scheduled heats carry no frequencies (RE §5: no freq step).
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_sim_class("sim", oneshot("h", field(&["A", "B"])));
        let heat = sched.next_heat().expect("one heat");
        assert!(
            heat.frequencies.is_empty(),
            "sim heats skip frequency allocation"
        );
        assert_eq!(heat.lineup, field(&["A", "B"]));
    }

    // --- A tiny generator for scheduler tests -------------------------------

    /// A generator that emits a fixed list of heats, one per `next` call, then
    /// completes. Lets the scheduler tests control exactly how many heats each class
    /// offers and in what order, without depending on a concrete format.
    struct Scripted {
        plans: std::collections::VecDeque<HeatPlan>,
    }

    impl Scripted {
        fn boxed(plans: Vec<HeatPlan>) -> Box<dyn Generator> {
            Box::new(Self {
                plans: plans.into(),
            })
        }
    }

    impl Generator for Scripted {
        fn next(&mut self, _completed: &[CompletedHeat]) -> GeneratorStep {
            match self.plans.pop_front() {
                Some(plan) => GeneratorStep::Run(vec![plan]),
                None => GeneratorStep::Complete,
            }
        }

        fn ranking(&self, _completed: &[CompletedHeat]) -> Vec<RankEntry> {
            Vec::new()
        }
    }

    /// A generator that emits a single one-heat `Run` then completes.
    fn oneshot(heat: &str, lineup: Vec<CompetitorRef>) -> Box<dyn Generator> {
        Scripted::boxed(vec![HeatPlan::new(heat, lineup)])
    }

    /// A generator that emits several heats in a SINGLE `Run` step, then completes —
    /// to prove the scheduler buffers and interleaves a multi-heat round.
    struct Burst {
        plans: Option<Vec<HeatPlan>>,
    }

    impl Burst {
        fn boxed(plans: Vec<HeatPlan>) -> Box<dyn Generator> {
            Box::new(Self { plans: Some(plans) })
        }
    }

    impl Generator for Burst {
        fn next(&mut self, _completed: &[CompletedHeat]) -> GeneratorStep {
            match self.plans.take() {
                Some(plans) => GeneratorStep::Run(plans),
                None => GeneratorStep::Complete,
            }
        }
        fn ranking(&self, _completed: &[CompletedHeat]) -> Vec<RankEntry> {
            Vec::new()
        }
    }

    // --- Interleaving --------------------------------------------------------

    #[test]
    fn two_classes_interleave_in_round_robin_order() {
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class(
            "alpha",
            Scripted::boxed(vec![
                HeatPlan::new("a1", field(&["A"])),
                HeatPlan::new("a2", field(&["A"])),
            ]),
        );
        sched.add_class(
            "beta",
            Scripted::boxed(vec![
                HeatPlan::new("b1", field(&["B"])),
                HeatPlan::new("b2", field(&["B"])),
            ]),
        );

        // The classes alternate: alpha, beta, alpha, beta — neither starves.
        let order: Vec<(String, String)> = std::iter::from_fn(|| {
            sched
                .next_heat()
                .map(|h| (h.class.clone(), h.heat.0.clone()))
        })
        .collect();
        assert_eq!(
            order,
            vec![
                ("alpha".into(), "a1".into()),
                ("beta".into(), "b1".into()),
                ("alpha".into(), "a2".into()),
                ("beta".into(), "b2".into()),
            ]
        );
    }

    #[test]
    fn frequencies_are_allocated_for_each_scheduled_heat() {
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class("c", oneshot("h", field(&["A", "B", "C"])));
        let heat = sched.next_heat().unwrap();
        assert_eq!(heat.frequencies.len(), 3);
        assert_eq!(heat.frequencies[0], (cref("A"), Frequency::new(5658)));
    }

    #[test]
    fn a_completed_class_drops_out_of_the_rotation() {
        // alpha has ONE heat; beta has THREE. Once alpha completes, beta keeps going
        // alone — alpha is dropped from the rotation, beta never starves waiting on it.
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class("alpha", oneshot("a1", field(&["A"])));
        sched.add_class(
            "beta",
            Scripted::boxed(vec![
                HeatPlan::new("b1", field(&["B"])),
                HeatPlan::new("b2", field(&["B"])),
                HeatPlan::new("b3", field(&["B"])),
            ]),
        );

        let order: Vec<String> =
            std::iter::from_fn(|| sched.next_heat().map(|h| h.heat.0.clone())).collect();
        // alpha's single heat goes first (cursor starts at alpha), then beta's three.
        assert_eq!(order, vec!["a1", "b1", "b2", "b3"]);
    }

    #[test]
    fn next_heat_is_none_only_when_all_classes_complete() {
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class("alpha", oneshot("a1", field(&["A"])));
        sched.add_class("beta", oneshot("b1", field(&["B"])));

        assert!(!sched.is_complete());
        assert!(sched.next_heat().is_some()); // a1
        // alpha is now done but beta still has a heat: not complete, not None.
        assert!(!sched.is_complete());
        assert!(sched.next_heat().is_some()); // b1
        // Both done now.
        assert!(sched.is_complete());
        assert!(sched.next_heat().is_none());
        // Idempotent terminal state.
        assert!(sched.next_heat().is_none());
    }

    #[test]
    fn a_multi_heat_run_is_buffered_and_interleaves() {
        // alpha bursts two heats in one Run; beta has one heat. The burst must NOT
        // monopolise the timer — beta's heat interleaves between alpha's two.
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class(
            "alpha",
            Burst::boxed(vec![
                HeatPlan::new("a1", field(&["A"])),
                HeatPlan::new("a2", field(&["A"])),
            ]),
        );
        sched.add_class("beta", oneshot("b1", field(&["B"])));

        let order: Vec<String> =
            std::iter::from_fn(|| sched.next_heat().map(|h| h.heat.0.clone())).collect();
        // alpha a1, then beta b1 (interleaved), then alpha's buffered a2.
        assert_eq!(order, vec!["a1", "b1", "a2"]);
    }

    // --- record advances the right class ------------------------------------

    #[test]
    fn record_feeds_the_named_class_only() {
        // A generator that echoes how many completed heats it has seen as the next
        // heat id, so we can observe that `record` reached the right class's history.
        struct Echo;
        impl Generator for Echo {
            fn next(&mut self, completed: &[CompletedHeat]) -> GeneratorStep {
                if completed.len() >= 2 {
                    GeneratorStep::Complete
                } else {
                    GeneratorStep::Run(vec![HeatPlan::new(
                        format!("seen-{}", completed.len()),
                        vec![CompetitorRef("A".into())],
                    )])
                }
            }
            fn ranking(&self, _c: &[CompletedHeat]) -> Vec<RankEntry> {
                Vec::new()
            }
        }

        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class("alpha", Box::new(Echo));
        sched.add_class("beta", Box::new(Echo));

        // First heat from each class: both have seen zero completed heats.
        let a = sched.next_heat().unwrap();
        assert_eq!((a.class.as_str(), a.heat.0.as_str()), ("alpha", "seen-0"));
        let b = sched.next_heat().unwrap();
        assert_eq!((b.class.as_str(), b.heat.0.as_str()), ("beta", "seen-0"));

        // Record a result for alpha ONLY.
        sched.record("alpha", CompletedHeat::new("seen-0", empty_result()));

        // alpha now reports one completed heat; beta is untouched (still seen-0).
        let a2 = sched.next_heat().unwrap();
        assert_eq!((a2.class.as_str(), a2.heat.0.as_str()), ("alpha", "seen-1"));
        let b2 = sched.next_heat().unwrap();
        assert_eq!((b2.class.as_str(), b2.heat.0.as_str()), ("beta", "seen-0"));
    }

    #[test]
    fn record_for_unknown_class_is_ignored() {
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class("alpha", oneshot("a1", field(&["A"])));
        // No panic, no effect.
        sched.record("nope", CompletedHeat::new("x", empty_result()));
        assert_eq!(sched.next_heat().unwrap().heat.0, "a1");
    }

    // --- rankings -----------------------------------------------------------

    #[test]
    fn rankings_reports_per_class_and_none_for_unknown() {
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class("alpha", oneshot("a1", field(&["A"])));
        assert!(sched.rankings("alpha").is_some());
        assert!(sched.rankings("missing").is_none());
        assert_eq!(sched.class_names(), vec!["alpha"]);
    }

    // --- Full multi-class run to completion ---------------------------------

    #[test]
    fn full_multiclass_run_drains_every_class() {
        let mut sched = Scheduler::new(FrequencyPool::raceband());
        sched.add_class(
            "alpha",
            Scripted::boxed(vec![
                HeatPlan::new("a1", field(&["A", "B"])),
                HeatPlan::new("a2", field(&["A", "B"])),
            ]),
        );
        sched.add_sim_class(
            "sim",
            Scripted::boxed(vec![HeatPlan::new("s1", field(&["P", "Q"]))]),
        );

        let mut irl_heats = 0;
        let mut sim_heats = 0;
        let mut total = 0;
        while let Some(heat) = sched.next_heat() {
            total += 1;
            if heat.class == "sim" {
                sim_heats += 1;
                assert!(heat.frequencies.is_empty(), "sim heats carry no freqs");
            } else {
                irl_heats += 1;
                assert_eq!(
                    heat.frequencies.len(),
                    heat.lineup.len(),
                    "every IRL pilot gets a channel"
                );
            }
            // Feed a trivial result back so a real generator would advance.
            sched.record(&heat.class, CompletedHeat::new(heat.heat.0, empty_result()));
        }

        assert_eq!(total, 3, "two alpha heats + one sim heat");
        assert_eq!(irl_heats, 2);
        assert_eq!(sim_heats, 1);
        assert!(sched.is_complete());
        assert!(sched.next_heat().is_none());
    }
}
