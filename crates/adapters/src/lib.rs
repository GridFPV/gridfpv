//! Timer adapters — the one interface every live timing source sits behind.
//!
//! Velocidrone, RotorHazard, LapRF and manual entry all reduce to the same small
//! set of canonical events ([`gridfpv_events`]). An adapter does three things:
//! connect to its source, **translate** source input into canonical events, and
//! **declare what it can do** ([`Capabilities`]).
//!
//! Two principles the type design enforces (see `docs/timer-adapters.html`):
//! - **Adapters are pure translators.** Raw source input in, canonical events out.
//!   They hold no race logic, own no truth, and never block the engine. That purity
//!   is what makes them testable by replaying recorded sessions.
//! - **Capabilities, not types.** The engine never asks "is this a sim?"; it asks
//!   the adapter "do you support signal context / calibration / frequency mgmt?"
//!   ([`Capabilities::has`]). IRL/sim divergence stays out of the engine.
#![forbid(unsafe_code)]

use gridfpv_events::{AdapterId, Event};
use serde::{Deserialize, Serialize};

pub mod clock;
pub mod dedup;
pub mod rotorhazard;
pub mod velocidrone;

/// One thing a timing source may or may not be able to do. The engine queries
/// these to adapt its UI and marshaling instead of special-casing source kinds.
/// Mirrors the capability matrix in `docs/timer-adapters.html` §5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Emits live gate-crossing passes. (Every real source has this.)
    LivePasses,
    /// Reports multiple gates / lap splits (Velocidrone), not just the lap gate.
    GatesSplits,
    /// Attaches signal context (RSSI / thresholds) beneath a crossing (hardware).
    SignalContext,
    /// Supports enter/exit calibration (RotorHazard).
    Calibration,
    /// Can re-derive a missed lap from captured signal history (hardware only).
    SignalRecovery,
    /// Manages frequencies / channels for its nodes.
    FrequencyMgmt,
    /// Exposes its own race/heat lifecycle (start/stop).
    SourceLifecycle,
}

/// What a source can do. A plain set of flags; construct with [`Capabilities::none`]
/// and the `with_*` builders, then query with [`Capabilities::has`].
///
/// Example — Velocidrone (live passes, splits, lifecycle; no signal/calibration/
/// frequency):
/// ```
/// use gridfpv_adapters::{Capabilities, Capability};
/// let caps = Capabilities::none()
///     .with_live_passes()
///     .with_gates_splits()
///     .with_source_lifecycle();
/// assert!(caps.has(Capability::GatesSplits));
/// assert!(!caps.has(Capability::SignalContext));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// Emits live gate-crossing passes.
    pub live_passes: bool,
    /// Reports gates / splits beyond the lap gate.
    pub gates_splits: bool,
    /// Attaches signal context (RSSI / thresholds).
    pub signal_context: bool,
    /// Supports enter/exit calibration.
    pub calibration: bool,
    /// Can re-derive a missed lap from captured signal.
    pub signal_recovery: bool,
    /// Manages frequencies / channels.
    pub frequency_mgmt: bool,
    /// Exposes its own race/heat lifecycle.
    pub source_lifecycle: bool,
}

impl Capabilities {
    /// No capabilities — the starting point for the builders.
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether this source supports the given capability.
    pub fn has(&self, capability: Capability) -> bool {
        match capability {
            Capability::LivePasses => self.live_passes,
            Capability::GatesSplits => self.gates_splits,
            Capability::SignalContext => self.signal_context,
            Capability::Calibration => self.calibration,
            Capability::SignalRecovery => self.signal_recovery,
            Capability::FrequencyMgmt => self.frequency_mgmt,
            Capability::SourceLifecycle => self.source_lifecycle,
        }
    }

    /// Every capability this source declares, for display / introspection.
    pub fn list(&self) -> Vec<Capability> {
        use Capability::*;
        [
            LivePasses,
            GatesSplits,
            SignalContext,
            Calibration,
            SignalRecovery,
            FrequencyMgmt,
            SourceLifecycle,
        ]
        .into_iter()
        .filter(|&c| self.has(c))
        .collect()
    }

    /// Declares live passes.
    pub fn with_live_passes(mut self) -> Self {
        self.live_passes = true;
        self
    }
    /// Declares gates / splits.
    pub fn with_gates_splits(mut self) -> Self {
        self.gates_splits = true;
        self
    }
    /// Declares signal context.
    pub fn with_signal_context(mut self) -> Self {
        self.signal_context = true;
        self
    }
    /// Declares calibration.
    pub fn with_calibration(mut self) -> Self {
        self.calibration = true;
        self
    }
    /// Declares signal-based lap recovery.
    pub fn with_signal_recovery(mut self) -> Self {
        self.signal_recovery = true;
        self
    }
    /// Declares frequency / channel management.
    pub fn with_frequency_mgmt(mut self) -> Self {
        self.frequency_mgmt = true;
        self
    }
    /// Declares a source race lifecycle.
    pub fn with_source_lifecycle(mut self) -> Self {
        self.source_lifecycle = true;
        self
    }
}

/// A live timing source behind the one interface.
///
/// Implementors are **pure translators**: [`translate`](Adapter::translate) takes a
/// raw, source-specific message and returns the canonical events it produces, with
/// no side effects on the outside world. It may update internal translation state —
/// the clock offset ([`clock`], #20), the dedup window ([`dedup`], #21), running
/// sequence — which is exactly what makes a recorded session replay deterministically.
pub trait Adapter {
    /// The raw, source-specific message this adapter consumes (a decoded
    /// Socket.IO event, a Velocidrone WebSocket frame, a manual entry, …).
    type Raw;

    /// Which source/adapter this is — stamped onto every event it emits.
    fn id(&self) -> &AdapterId;

    /// What this source can do. The engine queries this rather than the source kind.
    fn capabilities(&self) -> Capabilities;

    /// Translate one raw source message into zero or more canonical events.
    fn translate(&mut self, raw: Self::Raw) -> Vec<Event>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use gridfpv_events::{CompetitorRef, GateIndex, Pass, SourceTime};

    /// A trivial manual adapter: raw input is `(competitor, micros)` and it emits a
    /// single lap-gate `Pass`. Proves the interface + capability model compile and
    /// translate end to end (the #19 done-when).
    struct ManualAdapter {
        id: AdapterId,
    }

    impl Adapter for ManualAdapter {
        type Raw = (&'static str, i64);

        fn id(&self) -> &AdapterId {
            &self.id
        }

        fn capabilities(&self) -> Capabilities {
            // Manual entry: live passes only.
            Capabilities::none().with_live_passes()
        }

        fn translate(&mut self, (competitor, micros): Self::Raw) -> Vec<Event> {
            vec![Event::Pass(Pass {
                adapter: self.id.clone(),
                competitor: CompetitorRef(competitor.to_string()),
                at: SourceTime::from_micros(micros),
                sequence: None,
                gate: GateIndex::LAP,
                signal: None,
                heat: None,
            })]
        }
    }

    #[test]
    fn capabilities_are_queryable() {
        let caps = Capabilities::none()
            .with_live_passes()
            .with_gates_splits()
            .with_source_lifecycle();
        assert!(caps.has(Capability::LivePasses));
        assert!(caps.has(Capability::GatesSplits));
        assert!(caps.has(Capability::SourceLifecycle));
        assert!(!caps.has(Capability::SignalContext));
        assert!(!caps.has(Capability::Calibration));
        assert_eq!(caps.list().len(), 3);
    }

    #[test]
    fn capabilities_round_trip_through_json() {
        let caps = Capabilities::none()
            .with_live_passes()
            .with_signal_context();
        let json = serde_json::to_string(&caps).unwrap();
        let back: Capabilities = serde_json::from_str(&json).unwrap();
        assert_eq!(caps, back);
    }

    #[test]
    fn fake_adapter_translates_raw_into_a_pass() {
        let mut adapter = ManualAdapter {
            id: AdapterId("manual".into()),
        };
        assert_eq!(adapter.id().0, "manual");
        assert!(adapter.capabilities().has(Capability::LivePasses));

        let events = adapter.translate(("Ace", 12_000_000));
        assert_eq!(events.len(), 1);
        match &events[0] {
            Event::Pass(pass) => {
                assert_eq!(pass.competitor.0, "Ace");
                assert_eq!(pass.at, SourceTime::from_micros(12_000_000));
                assert!(pass.gate.is_lap_gate());
            }
            other => panic!("expected a Pass, got {other:?}"),
        }
    }
}
