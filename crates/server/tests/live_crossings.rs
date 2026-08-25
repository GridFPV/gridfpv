//! The live **crossing feed** with dispositions (#397, wire slice).
//!
//! `LiveRaceState::progress` reports *laps*, and laps are derived (`passes.windows(2)`), so a
//! lap-derived consumer is structurally blind to most of what the gate actually saw:
//!
//! - the **holeshot** opens the first lap and closes none — it derives no `Lap` at all;
//! - a crossing **rejected under the round's min-lap floor** is auto-voided in the projection
//!   (`VoidReason::UnderMinLap`) and reaches no live consumer whatsoever;
//! - a **marshal-voided** crossing leaves the lap chain entirely.
//!
//! `LiveRaceState::crossings` carries the crossings themselves, each labelled with what became of
//! it, so "the gate saw nothing" and "the gate saw something that did not count" stop being the
//! same silence. These tests pin the three things that make it usable: **every disposition
//! surfaces**, **identity is stable under re-folding** (the tone must fire once per crossing, never
//! once per delivered frame — #396 is what happens when a console assumes frames imply novelty),
//! and **the feed is bounded** without breaking either.
//!
//! Pure fold tests — no Docker, no server, no timer.

use gridfpv_events::{
    AdapterId, CompetitorRef, Event, GateIndex, HeatId, HeatTransition, LogRef, Pass, PilotId,
    SourceTime,
};
use gridfpv_projection::{CrossingDisposition, lap_list_marshaled_with_floor};
use gridfpv_server::live_state::{
    LiveRaceState, MAX_LIVE_CROSSINGS, live_state, live_state_over_with_floor,
};

const SECOND: i64 = 1_000_000;

fn scheduled(id: &str, lineup: &[&str]) -> Event {
    Event::HeatScheduled {
        heat: HeatId(id.into()),
        lineup: lineup.iter().map(|c| CompetitorRef((*c).into())).collect(),
        class: None,
        round: None,
        frequencies: vec![],
        label: None,
    }
}

fn changed(id: &str, transition: HeatTransition) -> Event {
    Event::HeatStateChanged {
        heat: HeatId(id.into()),
        transition,
    }
}

fn pass(competitor: &str, at: i64) -> Event {
    Event::Pass(Pass {
        adapter: AdapterId("rh".into()),
        competitor: CompetitorRef(competitor.into()),
        at: SourceTime::from_micros(at),
        sequence: None,
        gate: GateIndex::LAP,
        signal: None,
        heat: None,
    })
}

/// Pair a log with its append offsets, the shape the windowed fold consumes. Offsets are the
/// positional ones a whole-log fold uses, so both entry points agree on identity.
fn with_offsets(events: &[Event]) -> Vec<(u64, Event)> {
    events
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, e)| (i as u64, e))
        .collect()
}

/// Fold under a min-lap floor (the D26 path the heat-scoped snapshot uses).
fn folded_with_floor(events: &[Event], floor: Option<i64>) -> LiveRaceState {
    live_state_over_with_floor(&with_offsets(events), floor)
}

/// The feed's `(pass_ref, disposition, lap_number)` triples, for compact assertions.
fn shape(state: &LiveRaceState) -> Vec<(u64, CrossingDisposition, Option<u32>)> {
    state
        .crossings
        .iter()
        .map(|c| (c.pass_ref.0, c.disposition, c.lap_number))
        .collect()
}

/// A heat running with `passes` already on the log, plus the offset the first pass sits at.
fn running_heat(passes: Vec<Event>) -> (Vec<Event>, u64) {
    let mut events = vec![
        scheduled("q-1", &["A", "B"]),
        changed("q-1", HeatTransition::Staged),
        changed("q-1", HeatTransition::Armed),
        changed("q-1", HeatTransition::Running),
    ];
    let first_pass_offset = events.len() as u64;
    events.extend(passes);
    (events, first_pass_offset)
}

// --- Each disposition surfaces ------------------------------------------------------------

/// **The holeshot is the chain's first pass — derived, not flagged.**
///
/// Nothing in the log says "holeshot": the first crossing is an ordinary `GateIndex::LAP` pass.
/// It closes no lap, so `progress` still reads zero laps and a lap-derived consumer sees nothing
/// at all — which is exactly why it has to reach the console some other way.
#[test]
fn a_lone_crossing_is_the_holeshot_and_derives_no_lap() {
    let (events, base) = running_heat(vec![pass("A", 0)]);
    let state = live_state(&events);

    assert_eq!(
        shape(&state),
        vec![(base, CrossingDisposition::Holeshot, None)],
        "the single crossing is the holeshot, closing no lap"
    );
    // The gap this closes: laps alone report NOTHING for a pilot who has crossed once.
    assert!(
        state.progress.iter().all(|p| p.laps_completed == 0),
        "a holeshot completes no lap, so lap-derived progress is silent about it"
    );
}

/// A counted crossing carries **the lap it closed**, and that number is the lap list's own
/// numbering — the feed and the lap projection are two readings of one fold, so they cannot drift.
#[test]
fn counted_crossings_carry_the_lap_number_the_lap_list_gives_them() {
    let (events, base) = running_heat(vec![
        pass("A", 0),
        pass("A", 30 * SECOND),
        pass("A", 61 * SECOND),
    ]);
    let state = live_state(&events);

    assert_eq!(
        shape(&state),
        vec![
            (base, CrossingDisposition::Holeshot, None),
            (base + 1, CrossingDisposition::Counted, Some(1)),
            (base + 2, CrossingDisposition::Counted, Some(2)),
        ]
    );

    // Cross-check against the lap projection: a counted crossing's `pass_ref` is the `end_ref` of
    // the lap it is labelled with.
    let laps =
        lap_list_marshaled_with_floor(with_offsets(&events).iter().map(|(o, e)| (*o, e)), None);
    let a = laps
        .competitors
        .iter()
        .find(|c| c.competitor.competitor == CompetitorRef("A".into()))
        .expect("A has laps");
    for crossing in &state.crossings {
        let Some(number) = crossing.lap_number else {
            continue;
        };
        let lap = a
            .laps
            .iter()
            .find(|l| l.number == number)
            .expect("the labelled lap exists");
        assert_eq!(
            lap.end_ref, crossing.pass_ref,
            "lap {number} must be closed by exactly this crossing"
        );
    }
}

/// **A crossing rejected under the min-lap floor surfaces — today it vanishes.**
///
/// The corrected fold auto-voids it (`VoidReason::UnderMinLap`), so it derives no lap, does not
/// move `progress`, and reaches no live consumer at all. It is the single most valuable thing in
/// the feed: a too-sensitive gate is as broken as an insensitive one, and nothing else surfaces it
/// live.
#[test]
fn a_crossing_rejected_under_min_lap_surfaces_where_it_used_to_vanish() {
    // A@0 opens the lap; A@1s is a gate echo 1s later (under a 10s floor); A@20s closes lap 1.
    let (events, base) = running_heat(vec![
        pass("A", 0),
        pass("A", SECOND),
        pass("A", 20 * SECOND),
    ]);
    let floor = Some(10 * SECOND);
    let state = folded_with_floor(&events, floor);

    assert_eq!(
        shape(&state),
        vec![
            (base, CrossingDisposition::Holeshot, None),
            (base + 1, CrossingDisposition::RejectedTooShort, None),
            (base + 2, CrossingDisposition::Counted, Some(1)),
        ],
        "the echo is reported as a real crossing that did not count"
    );

    // It is genuinely invisible everywhere else: the floor suppressed it, so the lap count is 1
    // (the echo did NOT open a second lap) and nothing in `progress` hints a third crossing
    // happened.
    let a = state
        .progress
        .iter()
        .find(|p| p.competitor == CompetitorRef("A".into()))
        .expect("A is in the lineup");
    assert_eq!(a.laps_completed, 1);

    // Without the floor the same log labels it `Counted` — the disposition is not intrinsic to the
    // pass, it is the fold's verdict on it, and the fold needs the round's floor to reach it.
    let floorless = folded_with_floor(&events, None);
    assert_eq!(
        shape(&floorless)[1],
        (base + 1, CrossingDisposition::Counted, Some(1)),
        "with no floor supplied there is nothing to reject against"
    );
}

/// A **marshal-voided** crossing stays in the feed, relabelled — it was a real observation when it
/// happened, and the removal is a later ruling over it.
#[test]
fn a_marshal_voided_crossing_is_relabelled_not_dropped() {
    let (mut events, base) = running_heat(vec![
        pass("A", 0),
        pass("A", 30 * SECOND),
        pass("A", 61 * SECOND),
    ]);
    // Void the middle crossing.
    events.push(Event::DetectionVoided {
        target: LogRef(base + 1),
    });
    let state = live_state(&events);

    assert_eq!(
        shape(&state),
        vec![
            (base, CrossingDisposition::Holeshot, None),
            (base + 1, CrossingDisposition::VoidedByMarshal, None),
            // The chain closed up: the surviving third crossing now closes lap 1, not lap 2.
            (base + 2, CrossingDisposition::Counted, Some(1)),
        ]
    );
}

/// Voiding the **holeshot** promotes the next crossing to holeshot — under an unchanged
/// `pass_ref`. A disposition may change; an identity never does, which is why deduplication keys
/// on `pass_ref` alone and a re-labelled crossing does not re-fire.
#[test]
fn voiding_the_holeshot_promotes_the_next_crossing_without_changing_identities() {
    let (mut events, base) = running_heat(vec![
        pass("A", 0),
        pass("A", 30 * SECOND),
        pass("A", 61 * SECOND),
    ]);
    let before = live_state(&events);
    events.push(Event::DetectionVoided {
        target: LogRef(base),
    });
    let after = live_state(&events);

    assert_eq!(
        shape(&after),
        vec![
            (base, CrossingDisposition::VoidedByMarshal, None),
            (base + 1, CrossingDisposition::Holeshot, None),
            (base + 2, CrossingDisposition::Counted, Some(1)),
        ]
    );
    let ids = |s: &LiveRaceState| s.crossings.iter().map(|c| c.pass_ref.0).collect::<Vec<_>>();
    assert_eq!(
        ids(&before),
        ids(&after),
        "a ruling re-labels crossings; it never renumbers them"
    );
}

// --- What the feed must NOT filter out ----------------------------------------------------

/// A crossing on a competitor who is **not in the lineup** is still reported. A phantom detection
/// on an empty seat is precisely the thing an RD needs to notice, so the feed must never filter
/// toward "only meaningful laps".
#[test]
fn a_crossing_by_an_unseated_competitor_is_still_reported() {
    let (events, base) = running_heat(vec![pass("A", 0), pass("ghost", 5 * SECOND)]);
    let state = live_state(&events);

    assert!(
        !state.active_pilots.contains(&CompetitorRef("ghost".into())),
        "the ghost is not in the lineup"
    );
    assert_eq!(
        shape(&state),
        vec![
            (base, CrossingDisposition::Holeshot, None),
            // Its own chain, so its own holeshot.
            (base + 1, CrossingDisposition::Holeshot, None),
        ]
    );
    assert_eq!(state.crossings[1].competitor, CompetitorRef("ghost".into()));
}

/// The feed resolves the pilot binding the same way `progress` does, so the console's shared
/// resolver has a pilot id to work from rather than a bare node handle.
#[test]
fn a_registered_competitor_carries_its_pilot_binding() {
    let mut events = vec![Event::CompetitorRegistered {
        adapter: AdapterId("rh".into()),
        competitor: CompetitorRef("A".into()),
        pilot: PilotId("nova".into()),
    }];
    let (rest, _) = running_heat(vec![pass("A", 0), pass("B", SECOND)]);
    events.extend(rest);
    let state = live_state(&events);

    assert_eq!(state.crossings[0].pilot, Some(PilotId("nova".into())));
    assert_eq!(state.crossings[1].pilot, None, "B is unregistered");
}

/// The feed is scoped to the current **run**, exactly like `progress`: an aborted run's crossings
/// leave the feed with its laps, so a re-run does not re-announce them.
#[test]
fn a_reset_clears_the_feed_with_the_laps() {
    let (mut events, _) = running_heat(vec![pass("A", 0), pass("A", 30 * SECOND)]);
    assert_eq!(live_state(&events).crossings.len(), 2);

    events.push(changed("q-1", HeatTransition::Aborted));
    let after = live_state(&events);
    assert!(
        after.crossings.is_empty(),
        "the abandoned run's crossings are out of the feed, like its laps"
    );
    assert!(after.progress.iter().all(|p| p.laps_completed == 0));
}

// --- Idempotency --------------------------------------------------------------------------

/// **Re-folding the same log yields identical crossing identities.** The whole feature rests on
/// this: the consumer fires once per crossing *identity*, never on receipt of a frame, so a
/// re-pushed or re-snapshotted `LiveRaceState` must be indistinguishable from the previous one.
#[test]
fn refolding_the_same_log_yields_identical_crossings() {
    let (events, _) = running_heat(vec![
        pass("A", 0),
        pass("B", SECOND),
        pass("A", 30 * SECOND),
        pass("B", 31 * SECOND),
        pass("A", 61 * SECOND),
    ]);

    let once = live_state(&events);
    let twice = live_state(&events);
    assert_eq!(
        once, twice,
        "the fold is pure — a re-push carries no novelty"
    );

    // And the windowed entry point agrees with the whole-log one about identity, so switching
    // scope (event → heat) cannot look like a burst of new crossings either.
    let windowed = folded_with_floor(&events, None);
    assert_eq!(shape(&once), shape(&windowed));
}

/// Folding the log **prefix by prefix** — the way the change-stream engine advances — only ever
/// *appends* to the feed. A consumer holding one high-water mark therefore sees each crossing
/// exactly once across the whole heat, however many frames it is handed.
#[test]
fn folding_prefix_by_prefix_only_ever_appends_new_identities() {
    let (events, _) = running_heat(vec![
        pass("A", 0),
        pass("B", SECOND),
        pass("A", 30 * SECOND),
        pass("B", 31 * SECOND),
        pass("A", 61 * SECOND),
        pass("B", 62 * SECOND),
    ]);

    let mut watermark: Option<u64> = None;
    let mut fired: Vec<u64> = Vec::new();
    for n in 0..=events.len() {
        let state = live_state(&events[..n]);
        // Ascending by `pass_ref` — the ordering the high-water-mark rule depends on.
        let refs: Vec<u64> = state.crossings.iter().map(|c| c.pass_ref.0).collect();
        let mut sorted = refs.clone();
        sorted.sort_unstable();
        assert_eq!(refs, sorted, "the feed is ordered by append offset");

        for r in refs {
            if watermark.is_none_or(|w| r > w) {
                watermark = Some(r);
                fired.push(r);
            }
        }
    }

    let mut unique = fired.clone();
    unique.dedup();
    assert_eq!(fired, unique, "no crossing is announced twice");
    assert_eq!(
        fired.len(),
        6,
        "every crossing is announced exactly once across the whole replay"
    );
}

// --- The bound ----------------------------------------------------------------------------

/// The feed is **bounded**, and the bound drops the OLDEST entries.
///
/// A 20-lap heat on 8 seats is ~168 crossings and open practice runs unbounded, so the feed cannot
/// be "every crossing of the run" — it rides a latency-sensitive frame that is re-folded and
/// re-sent once per appended offset. Keeping the *tail* is the one truncation that leaves the
/// high-water-mark rule sound: anything trimmed is already below any consumer's watermark and can
/// never be mistaken for new.
#[test]
fn the_feed_is_bounded_to_its_most_recent_crossings() {
    let seats = ["A", "B", "C", "D", "E", "F", "G", "H"];
    let mut passes = Vec::new();
    for lap in 0..21 {
        for (seat, name) in seats.iter().enumerate() {
            passes.push(pass(name, lap * 30 * SECOND + seat as i64 * SECOND));
        }
    }
    let total = passes.len();
    assert!(
        total > MAX_LIVE_CROSSINGS,
        "the scenario must overflow the bound"
    );

    let mut events = vec![scheduled("q-1", &seats)];
    events.push(changed("q-1", HeatTransition::Running));
    let base = events.len() as u64;
    events.extend(passes);

    let state = live_state(&events);
    assert_eq!(state.crossings.len(), MAX_LIVE_CROSSINGS);
    assert_eq!(
        state.crossings[0].pass_ref.0,
        base + (total - MAX_LIVE_CROSSINGS) as u64,
        "the window keeps the newest crossings and drops the oldest"
    );
    assert_eq!(
        state.crossings.last().expect("non-empty").pass_ref.0,
        base + total as u64 - 1,
        "the newest crossing is always present"
    );
}

/// The bound does not break idempotency: replaying the overflowing heat prefix by prefix still
/// announces every crossing exactly once, because the entries that fall out of the window are
/// always ones the watermark has already passed.
#[test]
fn the_bound_never_re_announces_a_crossing_that_fell_out_of_the_window() {
    let seats = ["A", "B", "C", "D"];
    let mut passes = Vec::new();
    for lap in 0..25 {
        for (seat, name) in seats.iter().enumerate() {
            passes.push(pass(name, lap * 30 * SECOND + seat as i64 * SECOND));
        }
    }
    let total = passes.len();
    let mut events = vec![scheduled("q-1", &seats)];
    events.push(changed("q-1", HeatTransition::Running));
    events.extend(passes);

    let mut watermark: Option<u64> = None;
    let mut fired = 0usize;
    for n in 0..=events.len() {
        for c in live_state(&events[..n]).crossings {
            if watermark.is_none_or(|w| c.pass_ref.0 > w) {
                watermark = Some(c.pass_ref.0);
                fired += 1;
            }
        }
    }
    assert_eq!(
        fired, total,
        "every crossing announced once, none re-announced after the window slid past it"
    );
}
