/**
 * Adapter to the ts-rs–generated protocol bindings — the single seam that knows
 * where the generated wire types physically live.
 *
 * The wire types are generated from the Rust server crate into the repo-root
 * `bindings/` directory (one file per type), per docs/clients.html §3 and
 * architecture.html §6. The frontend NEVER hand-writes a wire type.
 *
 * ── How this barrel works ───────────────────────────────────────────────────
 * ts-rs emits one file per type (`bindings/EventMeta.ts`, `bindings/Pilot.ts`, …)
 * with extensionless cross-imports and no `index.ts` barrel of its own. This file
 * is that barrel: it re-exports every generated type module, resolved through the
 * `@bindings/*` tsconfig path alias (`@bindings/* → ../../../bindings/*`, set in
 * packages/types/tsconfig.json — the one place that knows their physical location).
 *
 * Because everything downstream imports from `@gridfpv/types`, a contract change in
 * Rust (a renamed field, a removed variant) surfaces here — and in every consumer —
 * as a TypeScript compile error rather than silent drift.
 *
 * ── Regenerating ────────────────────────────────────────────────────────────
 * The Rust side regenerates `bindings/*.ts` (`cargo xtask gen`). When a *new* type
 * is added, add a matching `export type * from '@bindings/<Name>';` line below;
 * when one is removed, drop its line. Nothing else in the frontend changes.
 *
 * This list is kept in lockstep with `bindings/` (one line per `bindings/*.ts`), and
 * `cargo xtask ci` enforces that (`cargo xtask barrel`): a generated binding with no line
 * here, or a line here naming a binding that no longer exists, fails the build.
 *
 * That check exists because the gap is invisible otherwise. A type that is generated but
 * never re-exported cannot be imported from `@gridfpv/types` at all, so an author who wants
 * the real wire type is pushed into hand-declaring one — and a hand-declared wire type passes
 * `tsc`, the unit tests and the lint while being wrong in every field name (#410).
 *
 * Ordering is presentation only; the check is about presence.
 */
export type * from '@bindings/ActiveEvent';
export type * from '@bindings/AdapterId';
export type * from '@bindings/BracketRequest';
export type * from '@bindings/Class';
export type * from '@bindings/ClassId';
export type * from '@bindings/ClassPlaceView';
export type * from '@bindings/ClassSource';
export type * from '@bindings/CombineRule';
export type * from '@bindings/Command';
export type * from '@bindings/CommandOutcome';
export type * from '@bindings/CompletionRule';
export type * from '@bindings/ContractVersion';
export type * from '@bindings/CreateClassRequest';
export type * from '@bindings/CreateEventRequest';
export type * from '@bindings/CreatePilotRequest';
export type * from '@bindings/CreateTimerRequest';
export type * from '@bindings/CrossingView';
export type * from '@bindings/DiscardReason';
export type * from '@bindings/DiscoveredTimer';
export type * from '@bindings/EliminationKind';
export type * from '@bindings/EndCondition';
export type * from '@bindings/ErrorCode';
export type * from '@bindings/Event';
export type * from '@bindings/EventId';
export type * from '@bindings/EventMeta';
export type * from '@bindings/EventView';
export type * from '@bindings/FieldSlice';
export type * from '@bindings/FirstLap';
export type * from '@bindings/Format';
export type * from '@bindings/GateIndex';
export type * from '@bindings/HeatSeat';
export type * from '@bindings/Hello';
export type * from '@bindings/JoinTokenResponse';
export type * from '@bindings/LapView';
export type * from '@bindings/LineupEntry';
export type * from '@bindings/LogRef';
export type * from '@bindings/MainsRequest';
export type * from '@bindings/Measure';
export type * from '@bindings/OptionalEdit';
export type * from '@bindings/Pass';
export type * from '@bindings/PassRequest';
export type * from '@bindings/PhaseDef';
export type * from '@bindings/PhaseId';
export type * from '@bindings/PhaseKind';
export type * from '@bindings/PhaseView';
export type * from '@bindings/Pilot';
export type * from '@bindings/PilotId';
export type * from '@bindings/PlacedView';
export type * from '@bindings/PresetSeat';
export type * from '@bindings/ProposedDisqualificationView';
export type * from '@bindings/ProtestId';
export type * from '@bindings/ProtestOutcome';
export type * from '@bindings/ProtestView';
export type * from '@bindings/ProtocolError';
export type * from '@bindings/RaceId';
export type * from '@bindings/RaceRules';
export type * from '@bindings/RaceSeatView';
export type * from '@bindings/RaceSetup';
export type * from '@bindings/RaceSignal';
export type * from '@bindings/RaceStateView';
export type * from '@bindings/RaceView';
export type * from '@bindings/RankingRule';
export type * from '@bindings/RemovalReason';
export type * from '@bindings/RosterView';
export type * from '@bindings/SeatLevels';
export type * from '@bindings/SeatRef';
export type * from '@bindings/SeatSignal';
export type * from '@bindings/SeatSource';
export type * from '@bindings/SeatView';
export type * from '@bindings/SeatWhoView';
export type * from '@bindings/ServerHello';
export type * from '@bindings/SessionId';
export type * from '@bindings/SetActiveEventRequest';
export type * from '@bindings/SetClassHiddenRequest';
export type * from '@bindings/SignalChunk';
export type * from '@bindings/SignalContext';
export type * from '@bindings/SignalHistory';
export type * from '@bindings/SignalThresholds';
export type * from '@bindings/SignalUnit';
export type * from '@bindings/SourceTime';
export type * from '@bindings/StartOwner';
export type * from '@bindings/Timer';
export type * from '@bindings/TimerConfig';
export type * from '@bindings/TimerKind';
export type * from '@bindings/TimerLive';
export type * from '@bindings/TimerSeatConfig';
export type * from '@bindings/TimerStatus';
export type * from '@bindings/TimerThreshold';
export type * from '@bindings/TimerView';
export type * from '@bindings/UncountedView';
export type * from '@bindings/UncountedWhy';
export type * from '@bindings/UpdateClassRequest';
export type * from '@bindings/UpdatePilotRequest';
export type * from '@bindings/VtxType';
