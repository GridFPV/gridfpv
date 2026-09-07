//! The **anti-drift gate** between a timer's kind and its adapter's declared capabilities (#522).
//!
//! Two crates state the same facts about a timing source, and they cannot see each other:
//!
//! - [`TimerKind`]'s predicates live in `gridfpv-server`, which the Director's registry and every
//!   REST route sit on, *below* the adapters;
//! - [`Capabilities`] lives in `gridfpv-adapters`, declared by the adapter that actually talks to
//!   the thing.
//!
//! The dependency arrow only points one way, so the duplication is structural rather than sloppy.
//! What is avoidable is the two **disagreeing**, silently, and then two screens answering the same
//! question differently — which is exactly the failure class CLAUDE.md's shared-resolver rule
//! exists for, one crate boundary up.
//!
//! `gridfpv-app` depends on both, so this is the one place the pair can be checked at all. It is
//! deliberately a whole test file rather than a unit test inside either crate: neither crate can
//! host it.
//!
//! **When this fails**, the fix is to make the two agree — not to edit the expectation. Which side
//! is wrong depends on what changed, and working that out is the point of the gate.

use gridfpv_adapters::rotorhazard::RotorHazardAdapter;
use gridfpv_adapters::velocidrone::VelocidroneAdapter;
use gridfpv_adapters::{Adapter, Capabilities, Capability};
use gridfpv_server::timers::TimerKind;

/// The kinds that have a real adapter behind them, paired with what that adapter declares.
///
/// `Mock` is absent on purpose: it is the built-in synthetic source with no adapter of its own, so
/// there is nothing to agree *with*. Its predicates are asserted separately below.
fn real_sources() -> Vec<(TimerKind, Capabilities)> {
    vec![
        (
            TimerKind::Rotorhazard {
                url: "http://rh.local:5000".into(),
            },
            RotorHazardAdapter::new().capabilities(),
        ),
        (
            TimerKind::Velocidrone {
                host: "192.168.1.10".into(),
            },
            VelocidroneAdapter::with_default_id().capabilities(),
        ),
    ]
}

/// `TimerKind::source_owns_race()` must say exactly what the adapter declares (#522).
///
/// This is the predicate the whole Velocidrone event model is derived from — what the console
/// hides, whether the Director defers to the sim's race-end, and whether a timer selection is
/// refused as a mix. A disagreement here would not fail loudly; it would quietly make one surface
/// treat a sim as a driven timer while another treats it as a sim.
#[test]
fn race_ownership_agrees_across_the_crate_boundary() {
    for (kind, caps) in real_sources() {
        assert_eq!(
            kind.source_owns_race(),
            caps.has(Capability::SourceOwnsRace),
            "{} disagrees with its adapter about who owns the race",
            kind.label()
        );
    }
}

/// `TimerKind::manages_frequencies()` must likewise match the adapter's `FrequencyMgmt`.
///
/// This one gates channel assignment: a mismatch either refuses to schedule a heat that is fine
/// (the #484 field bug, where a sim's empty channel set read as an unconfigured timer), or assigns
/// channels to a source with no receivers.
#[test]
fn frequency_management_agrees_across_the_crate_boundary() {
    for (kind, caps) in real_sources() {
        assert_eq!(
            kind.manages_frequencies(),
            caps.has(Capability::FrequencyMgmt),
            "{} disagrees with its adapter about frequency management",
            kind.label()
        );
    }
}

/// The Mock has no adapter to agree with, so its predicates are pinned directly.
///
/// It stands in for a driven timer — GridFPV owns its race, and its heats carry channel plans the
/// console renders — so it answers like RotorHazard on both counts. Changing that is #498's
/// business, and this test is here so it cannot happen by accident.
#[test]
fn the_built_in_mock_answers_like_a_driven_timer() {
    let mock = TimerKind::Mock { laps: 3, lap_ms: 1 };
    assert!(!mock.source_owns_race(), "GridFPV owns the Mock's race");
    assert!(mock.manages_frequencies());
}

/// Exactly one source owns its race today, and it is the sim. Stated as its own assertion because
/// the interesting property is the *asymmetry* — if a change ever made RotorHazard race-owning, or
/// Velocidrone Director-owned, every derived surface would silently invert.
#[test]
fn velocidrone_owns_its_race_and_rotorhazard_does_not() {
    let vd = TimerKind::Velocidrone {
        host: "192.168.1.10".into(),
    };
    let rh = TimerKind::Rotorhazard {
        url: "http://rh.local:5000".into(),
    };
    assert!(vd.source_owns_race());
    assert!(!rh.source_owns_race());
    // …and the mirror image on frequencies: the sim has no receivers, the hardware does.
    assert!(!vd.manages_frequencies());
    assert!(rh.manages_frequencies());
}
