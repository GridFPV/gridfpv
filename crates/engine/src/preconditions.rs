//! What each format **requires of the field** it is handed — and the typed refusal when the
//! field does not meet it (#394).
//!
//! # Why this module exists
//!
//! [`Generator::next`](crate::format::Generator::next) has exactly two answers:
//! [`Run`](crate::format::GeneratorStep::Run) some heats, or
//! [`Complete`](crate::format::GeneratorStep::Complete). A generator that **cannot race the
//! field it was given** — Head-to-Head handed a single pilot: head-to-head means racing
//! *someone* — has no third answer to give, so it returns `Complete` and its refusal becomes
//! indistinguishable from a finished round at every layer above it.
//!
//! That is not hypothetical. A one-pilot Head-to-Head round reported *"no new heat — the round
//! is complete or awaiting a score"* on a round where nothing had raced; the project's own
//! author read it as a regression in the generator and started debugging the wrong thing. The
//! system knew exactly what was wrong and did not say.
//!
//! # The contract
//!
//! This module is the **one place** a format's field precondition is declared, so the fill path
//! can name the shortfall instead of rendering a generic "complete". A precondition is a
//! property of the format itself (Head-to-Head needs an opponent), not of any one round, so it
//! is answered from the registry name + the field size alone — no log, no clock, no RNG.
//!
//! **Adding a format?** If its generator ever returns `Complete` because the field is *unfit*
//! rather than because the racing is *finished*, declare that here. A refusal that is not
//! declared here is a refusal the RD is told nothing about.
//!
//! An **empty** field is deliberately not a shortfall: the fill path rejects it earlier and more
//! specifically (there is no round to run at all, for any format), so declaring it here too
//! would only produce a second, vaguer message for the same condition.
//!
//! # Why `Complete` is still one word (#401)
//!
//! The #394/#395 audit noted that `Complete` now carries three meanings: genuine completion
//! (`open_practice`, the demo formats), the precondition failure this module answers out-of-band,
//! and **"done *for now*, awaiting an RD request"** (`zippyq`). Splitting
//! [`GeneratorStep`](crate::format::GeneratorStep) was weighed again while making `Advance` say
//! what it did (#401) and deliberately **not** done, because the third meaning is not reachable:
//!
//! - `zippyq` is shelved (#218) — registered so persisted rounds still load, never offered for a
//!   new round;
//! - and the server has no `request_round` plumbing at all. Its fill path builds a **fresh**
//!   generator from the log on every draw, so a `zippyq` round's pending queue is always empty and
//!   its `Complete` is, from the server's side, indistinguishable from — and as durable as —
//!   genuine completion. "The round is complete" is the true statement there today;
//! - `RollingDemo`/`KnockoutDemo` are test fixtures, not in
//!   [`FormatRegistry::standard`](crate::format::FormatRegistry::standard).
//!
//! So the split would widen the `Generator` contract — every implementor, `schedule.rs`,
//! `event.rs` and their tests — to distinguish a state no caller can produce, and would buy
//! `Advance`'s message nothing. The variant belongs with the command that makes it reachable: when
//! ZippyQ's "queue another round" request is built, that is when `Complete` must stop meaning two
//! things, and this note is the marker for it.

use std::fmt;

use crate::head_to_head::HeadToHead;
use crate::timed_qual::TimedQualifying;

/// A format a shortfall message can point the RD at as the way forward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FormatHint {
    /// The format's human-readable name, e.g. `"Timed Qualifying"`.
    pub label: &'static str,
    /// Its [`FormatRegistry`](crate::format::FormatRegistry) name, e.g. `"timed_qual"` — the
    /// value an RD picks in the round form / an API caller puts in a round's `format`.
    pub name: &'static str,
}

/// Fly laps against the clock — the format that *does* fit a solo pilot.
const TIMED_QUAL: FormatHint = FormatHint {
    label: "Timed Qualifying",
    name: TimedQualifying::NAME,
};

/// A format **refusing** the field it was given: it needs more competitors than the round has.
///
/// The refusal is correct — this is not the generator failing, it is the generator declining an
/// impossible race. What it carries is the *reason*, so the layer that talks to the RD can say
/// it instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldShortfall {
    /// The refusing format's human-readable name, e.g. `"Head-to-Head"`.
    pub format: &'static str,
    /// The smallest field this format can race.
    pub required: usize,
    /// The field the round actually has.
    pub have: usize,
    /// A format that *would* fit this field, when one exists — the way forward to offer the RD.
    pub alternative: Option<FormatHint>,
}

impl fmt::Display for FieldShortfall {
    /// The RD-facing sentence: what the format needs, what the round has, and what to do about
    /// it. Names formats only — never a round/heat/pilot id (the caller supplies the round's
    /// friendly name around this, per the repo display rule).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} needs at least {} pilots in the field — this round has {}",
            self.format, self.required, self.have
        )?;
        if let Some(hint) = self.alternative {
            write!(
                f,
                ". A field this size can fly {} ({}) instead",
                hint.label, hint.name
            )?;
        }
        write!(f, ".")
    }
}

/// Whether `format` refuses a field of `field` competitors, and why.
///
/// `format` is a [`FormatRegistry`](crate::format::FormatRegistry) name. An unknown format has no
/// declared precondition (the fill path rejects it separately as an unknown format), and neither
/// does an empty field — see the module docs.
pub fn field_shortfall(format: &str, field: usize) -> Option<FieldShortfall> {
    if field == 0 {
        return None;
    }
    match format {
        // Head-to-Head races competitors AGAINST each other; one pilot has no opponent, so its
        // generator returns `Complete` for a field of one (see `head_to_head.rs`).
        HeadToHead::NAME => (field < 2).then_some(FieldShortfall {
            format: "Head-to-Head",
            required: 2,
            have: field,
            alternative: Some(TIMED_QUAL),
        }),
        // Timed Qualifying (solo laps against the clock), ZippyQ (whatever lineup the RD queues)
        // and Open Practice (the active channels are the field) all race any non-empty field.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_to_head_refuses_a_field_of_one_and_names_the_shortfall() {
        let shortfall = field_shortfall(HeadToHead::NAME, 1).expect("one pilot is a shortfall");
        assert_eq!(shortfall.required, 2);
        assert_eq!(shortfall.have, 1);
        let message = shortfall.to_string();
        // The three things the RD needs: the format, the requirement vs. what they have, and a
        // way forward.
        assert!(message.contains("Head-to-Head"), "{message}");
        assert!(message.contains("at least 2"), "{message}");
        assert!(message.contains("has 1"), "{message}");
        assert!(message.contains("timed_qual"), "{message}");
    }

    #[test]
    fn head_to_head_accepts_a_field_that_can_actually_race() {
        assert_eq!(field_shortfall(HeadToHead::NAME, 2), None);
        assert_eq!(field_shortfall(HeadToHead::NAME, 8), None);
    }

    #[test]
    fn a_solo_friendly_format_has_no_shortfall() {
        // The whole point of the `timed_qual` hint: it must not itself refuse the field it is
        // being recommended for.
        assert_eq!(field_shortfall(TimedQualifying::NAME, 1), None);
        assert_eq!(field_shortfall("open_practice", 1), None);
        assert_eq!(field_shortfall("zippyq", 1), None);
    }

    #[test]
    fn an_empty_field_is_not_a_shortfall() {
        // An empty field is rejected upstream as "no field at all", for every format — a second
        // message here would only be vaguer.
        assert_eq!(field_shortfall(HeadToHead::NAME, 0), None);
        assert_eq!(field_shortfall(TimedQualifying::NAME, 0), None);
    }

    #[test]
    fn an_unknown_format_has_no_declared_precondition() {
        assert_eq!(field_shortfall("no-such-format", 1), None);
    }
}
