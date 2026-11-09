//! The competition engine: Grid's primitives as pure folds over the event log.
//!
//! `docs/primitives.md` is the design. Everything here is **pure**: a fold takes logged
//! events and gives a state; a check takes that state and a command and gives either the
//! facts to append or the reason it is refused. Nothing in this crate reads a clock, talks
//! to a timer or touches storage, so every rule can be tested by replaying events.
//!
//! Built so far:
//! - [`race`] — one start-to-stop flight, the atom (§5.1).
//! - [`laps`] — a race's laps per seat, with the lap rulings (§5.7).
//! - [`heat`] — a race's finishing order, a heat's standings under a ranking rule, and
//!   whether its completion rule says it is done (§5.4, §5.5).
//! - [`phase`] — the phase tree: fields, seat references, combined rankings, completion and
//!   inherited formats (§5.2 – §5.6).
//! - [`presets`] — generators that write plain phases: a double elimination bracket, and
//!   mains stacked with bump-ups (§6).
//! - [`rulings`] — false starts counted against a phase's limit, disqualification, locking
//!   and protests (§5.7, §5.8).
//!
//! Still to come: time and points penalties, and the director's position override.
#![forbid(unsafe_code)]

pub mod heat;
pub mod laps;
pub mod phase;
pub mod presets;
pub mod race;
pub mod rulings;
