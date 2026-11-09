//! The competition engine: Grid's primitives as pure folds over the event log.
//!
//! `docs/primitives.md` is the design. Everything here is **pure**: a fold takes logged
//! events and gives a state; a check takes that state and a command and gives either the
//! facts to append or the reason it is refused. Nothing in this crate reads a clock, talks
//! to a timer or touches storage, so every rule can be tested by replaying events.
//!
//! Built so far: the [`race`] — one start-to-stop flight, the atom (§5.1). Heats, phases,
//! ranking and completion rules, formats and rulings are added on top of it.
#![forbid(unsafe_code)]

pub mod race;
