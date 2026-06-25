//! Full-event capstone — deterministic fixture replay (#37, the v0.3 milestone).
//!
//! This is the core (non-live) proof that a **whole event runs end to end**: a
//! qualifying phase produces a ranking, that ranking seeds a single-elimination
//! bracket, and the bracket runs to a single winner — with correct results, the
//! marshaling folded in, and **byte-identical on replay** (testing-strategy.html §8).
//!
//! Everything is hand-authored canonical-log heats: each heat is a `Vec<Event>` of
//! lap-gate [`Pass`]es (plus, in one heat, a marshaling adjudication). The heats are
//! scored with [`gridfpv_engine::event::score_marshaled`] — the same scorer the
//! un-marshaled path uses, but folding [`Event::DetectionVoided`] / `LapInserted` /
//! `LapAdjusted` into a corrected view first — and the scored results drive the
//! qualifying → bracket flow via [`gridfpv_engine::event::run_event`].
//!
//! # What the marshaling case proves
//!
//! The qualifying field is A, B, C, D, scored by **best single lap** (smaller is
//! better). In round 1, pilot **D logs a phantom blazing-fast lap** that, taken at face
//! value, makes D the top qualifier. A marshal rules that crossing a false detection
//! and appends a [`Event::DetectionVoided`] for it. With the void folded in, D's real
//! best lap is ordinary and D drops to the bottom of qualifying — which **reseeds the
//! bracket and changes who wins the event**. The test asserts both the un-marshaled and
//! marshaled outcomes so the adjudication's effect is explicit (it is not a no-op).
#![forbid(unsafe_code)]

use gridfpv_engine::event::{EventOutcome, RunHeat, run_event, score_marshaled};
use gridfpv_engine::format::{HeatPlan, RankEntry};
use gridfpv_engine::scoring::{HeatResult, WinCondition};
use gridfpv_engine::single_elim::SingleElim;
use gridfpv_engine::timed_qual::{QualMetric, TimedQualifying};
use gridfpv_events::{AdapterId, CompetitorRef, Event, GateIndex, LogRef, Pass, SourceTime};

const ADAPTER: &str = "vd";

fn cref(name: &str) -> CompetitorRef {
    CompetitorRef(name.into())
}

fn field(names: &[&str]) -> Vec<CompetitorRef> {
    names.iter().map(|n| cref(n)).collect()
}

/// A lap-gate pass for `competitor` at `at` µs.
fn pass(competitor: &str, at: i64) -> Event {
    Event::Pass(Pass {
        adapter: AdapterId(ADAPTER.into()),
        competitor: cref(competitor),
        at: SourceTime::from_micros(at),
        sequence: None,
        gate: GateIndex::LAP,
        signal: None,
    })
}

fn names(entries: &[RankEntry]) -> Vec<String> {
    entries.iter().map(|e| e.competitor.0.clone()).collect()
}

/// Race start for the timed/qualifying clocks (RH-style: passes are µs since start).
fn race_start() -> SourceTime {
    SourceTime::from_micros(0)
}

// --- The hand-authored event fixture ---------------------------------------

/// Lap-gate timestamps (µs) for each pilot, so a pilot's best single lap is a clean,
/// reviewable number. Gaps are the per-lap durations.
///
/// Round-1 best laps (the gap between consecutive passes):
/// - A: passes 0, 1.8, 3.5  → laps 1.8s, 1.7s → best **1.7s**
/// - B: passes 0, 2.0, 3.6  → laps 2.0s, 1.6s → best **1.6s**
/// - C: passes 0, 1.9, 4.0  → laps 1.9s, 2.1s → best **1.9s**
/// - D: passes 0, 0.5, 2.3  → laps **0.5s**, 1.8s → best **0.5s** (the phantom!)
///
/// D's 0.5s lap is impossibly fast — a phantom double-detection. Taken raw it makes D
/// the top qualifier; voided, D's best becomes 1.8s and D seeds last.
fn round_1_raw() -> Vec<Event> {
    vec![
        // A — offsets 0,1,2
        pass("A", 0),
        pass("A", 1_800_000),
        pass("A", 3_500_000),
        // B — offsets 3,4,5
        pass("B", 0),
        pass("B", 2_000_000),
        pass("B", 3_600_000),
        // C — offsets 6,7,8
        pass("C", 0),
        pass("C", 1_900_000),
        pass("C", 4_000_000),
        // D — offsets 9,10,11; offset 10 is the phantom 0.5s crossing
        pass("D", 0),
        pass("D", 500_000),
        pass("D", 2_300_000),
    ]
}

/// The qualifying round-1 log **with the marshal's ruling**: void D's phantom crossing
/// (offset 10). Folded in, D's best lap reverts to its real 1.8s pace.
fn round_1_marshaled() -> Vec<Event> {
    let mut log = round_1_raw();
    log.push(Event::DetectionVoided { target: LogRef(10) });
    log
}

/// Bracket heat logs, keyed by the head-to-head winner: the winner banks more, faster
/// laps than the loser. Scored by most-laps-in-window, the busier pilot wins.
fn bracket_h2h(winner: &str, loser: &str) -> Vec<Event> {
    vec![
        // Winner: 4 crossings ⇒ 3 laps, all inside the window.
        pass(winner, 0),
        pass(winner, 1_500_000),
        pass(winner, 3_000_000),
        pass(winner, 4_500_000),
        // Loser: 3 crossings ⇒ 2 laps.
        pass(loser, 0),
        pass(loser, 1_800_000),
        pass(loser, 3_700_000),
    ]
}

/// The per-heat fixture log lookup. Qualifying round-1 carries the marshaling ruling
/// when `marshaled` is set; bracket heats are derived from the heat id's seed pairing.
///
/// The bracket heat ids are `single_elim`'s `se-r{round}-h{index}`; for each we know
/// which two seeds it pairs from the seeding, so we author the busier pilot as the
/// higher seed (smaller qualifying position) and let it win.
struct Fixture {
    marshaled: bool,
}

impl Fixture {
    /// Score the qualifying round-1 heat (best-lap) or a bracket heat (most-laps),
    /// folding marshaling adjudications via [`score_marshaled`].
    fn score_heat(&self, plan: &HeatPlan) -> HeatResult {
        if plan.heat.0 == "round-1" {
            let log = if self.marshaled {
                round_1_marshaled()
            } else {
                round_1_raw()
            };
            score_marshaled(&log, WinCondition::BestLap, race_start())
        } else {
            // A bracket heat: the higher-seeded (alphabetically/numerically lower
            // qualifying rank) of the two lineup competitors is the intended winner.
            // We pick the winner as the lineup's first entry (single_elim lays the
            // stronger seed first in each pair via bracket_pairs) so the bracket
            // resolves deterministically toward the top seed.
            let winner = plan.lineup.first().expect("non-empty lineup");
            let loser = plan
                .lineup
                .get(1)
                .cloned()
                .unwrap_or_else(|| winner.clone());
            let log = bracket_h2h(&winner.0, &loser.0);
            score_marshaled(
                &log,
                WinCondition::Timed {
                    window_micros: 60_000_000,
                },
                race_start(),
            )
        }
    }
}

impl RunHeat for Fixture {
    fn run(&mut self, plan: &HeatPlan) -> HeatResult {
        self.score_heat(plan)
    }
}

/// Run the whole fixtured event with or without the marshaling ruling folded in.
fn run_fixture_event(marshaled: bool) -> EventOutcome {
    let mut qual = TimedQualifying::new(field(&["A", "B", "C", "D"]), 1, QualMetric::BestLap);
    let mut fixture = Fixture { marshaled };
    run_event(
        &mut qual,
        |seeds| Box::new(SingleElim::new(seeds, 2)),
        4,
        &mut fixture,
        64,
    )
}

// --- Tests ------------------------------------------------------------------

#[test]
fn full_event_runs_qualifying_through_bracket_to_a_single_winner() {
    // The headline capstone path: with the marshal's ruling folded in, the event runs
    // qualifying → bracket → one winner with the EXACT expected results.
    let outcome = run_fixture_event(true);

    // Qualifying by best lap, D's phantom voided:
    //   B 1.6s, A 1.7s, C 1.9s, D 1.8s → B, A, C, D.
    assert_eq!(
        names(&outcome.qualifying),
        vec!["B", "A", "C", "D"],
        "qualifying ranking by best lap with D's phantom voided"
    );
    // The bracket is seeded by that ranking.
    assert_eq!(outcome.bracket_seeds, field(&["B", "A", "C", "D"]));

    // bracket_pairs([B,A,C,D]) = [B,D,A,C] → heats (B v D), (A v C); the higher seed
    // (lineup-first) wins each, so the final is B v A and B takes the event.
    assert_eq!(
        outcome.winner(),
        Some(&cref("B")),
        "the top qualifier wins the bracket"
    );
    assert_eq!(outcome.bracket[0].position, 1);
    assert_eq!(
        outcome.bracket.iter().filter(|e| e.position == 1).count(),
        1,
        "exactly one event winner"
    );
    // Level-per-round (#217): `bracket` is the FINAL level's standings — the two finalists,
    // winner first (the earlier levels' standings are each their own round). The two finalists
    // are the bracket's top seeds B and A; B wins.
    assert_eq!(names(&outcome.bracket), vec!["B", "A"]);
    assert_eq!(outcome.bracket.len(), 2);
}

#[test]
fn marshaling_adjudication_changes_the_full_event_result() {
    // The load-bearing marshaling proof: voiding D's phantom lap reshapes qualifying,
    // which reseeds the bracket, which changes the event winner.
    let raw = run_fixture_event(false);
    let marshaled = run_fixture_event(true);

    // RAW: D's 0.5s phantom is the fastest lap, so D tops qualifying:
    //   D 0.5s, B 1.6s, A 1.7s, C 1.9s → D, B, A, C.
    assert_eq!(
        names(&raw.qualifying),
        vec!["D", "B", "A", "C"],
        "raw qualifying lets D's phantom lap seed it first"
    );
    // MARSHALED: D's phantom voided → D's best is its real 1.8s, so D seeds last.
    assert_eq!(
        names(&marshaled.qualifying),
        vec!["B", "A", "C", "D"],
        "the void demotes D to last in qualifying"
    );

    // The reseed changes the bracket winner: raw seeds [D,B,A,C] → pairs [D,C,B,A] →
    // heats (D v C),(B v A) → final D v B → D wins. Marshaled → B wins.
    assert_eq!(
        raw.winner(),
        Some(&cref("D")),
        "raw: the phantom carries D to the title"
    );
    assert_eq!(
        marshaled.winner(),
        Some(&cref("B")),
        "marshaled: B wins instead"
    );
    assert_ne!(
        raw.winner(),
        marshaled.winner(),
        "the marshaling adjudication changed the event winner"
    );
}

#[test]
fn full_event_replays_byte_identically() {
    // Determinism backbone (TS §8): run the entire fixtured event twice and assert the
    // outcomes are equal AND serialize byte-for-byte identically. Nothing in the flow
    // reads a clock or an RNG, so replay is exact.
    let first = run_fixture_event(true);
    let second = run_fixture_event(true);

    assert_eq!(
        first, second,
        "two runs of the full event are structurally equal"
    );

    // Byte-identical: serialize the rankings and the completed heats and compare bytes.
    let bytes = |o: &EventOutcome| -> (String, String, String, String) {
        (
            serde_json_names(&o.qualifying),
            serde_json_names(&o.bracket),
            heat_ids(&o.qualifying_heats),
            heat_ids(&o.bracket_heats),
        )
    };
    assert_eq!(
        bytes(&first),
        bytes(&second),
        "the full event replays byte-identically"
    );
}

/// A stable string rendering of a ranking (competitor + position), used as the
/// byte-identity surface — `RankEntry` is not `Serialize`, so we render it ourselves.
fn serde_json_names(ranking: &[RankEntry]) -> String {
    ranking
        .iter()
        .map(|e| format!("{}:{}", e.competitor.0, e.position))
        .collect::<Vec<_>>()
        .join(",")
}

/// The completed heat ids joined, as a second byte-identity surface.
fn heat_ids(heats: &[gridfpv_engine::format::CompletedHeat]) -> String {
    heats
        .iter()
        .map(|h| h.heat.0.clone())
        .collect::<Vec<_>>()
        .join(",")
}
