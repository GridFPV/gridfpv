/**
 * Adapter to the ts-rs–generated protocol bindings — the single seam that knows
 * where the generated wire types physically live.
 *
 * The wire types are generated from the Rust server crate into the repo-root
 * `bindings/` directory (one file per type), per docs/clients.html §3 and
 * architecture.html §6. The frontend NEVER hand-writes a wire type.
 *
 * ── How this barrel works ───────────────────────────────────────────────────
 * ts-rs emits one file per type (`bindings/Snapshot.ts`, `bindings/Cursor.ts`, …)
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
 * Ordering is presentation only; the check is about presence. `VoidReason` deliberately sits
 * beside `LapList` rather than in alphabetical order.
 */

export type * from '@bindings/ActiveEvent';
export type * from '@bindings/AdapterId';
export type * from '@bindings/AdvanceOutcome';
export type * from '@bindings/AdvanceStop';
export type * from '@bindings/AuditEntry';
export type * from '@bindings/AuditKind';
export type * from '@bindings/CalibrationDispatch';
export type * from '@bindings/CalibrationRequest';
export type * from '@bindings/Change';
export type * from '@bindings/ChangeEnvelope';
export type * from '@bindings/ChannelCapability';
export type * from '@bindings/ChannelCatalogEntry';
export type * from '@bindings/ChannelDispatch';
export type * from '@bindings/ChannelLayout';
export type * from '@bindings/ChannelLayouts';
export type * from '@bindings/ChannelRequest';
export type * from '@bindings/ChannelMode';
export type * from '@bindings/Class';
export type * from '@bindings/ClassId';
export type * from '@bindings/ClassMembership';
export type * from '@bindings/ClassSource';
export type * from '@bindings/ClassStanding';
export type * from '@bindings/ClassStandings';
export type * from '@bindings/Command';
export type * from '@bindings/CommandAck';
export type * from '@bindings/CommandOutcome';
export type * from '@bindings/CompetitorKey';
export type * from '@bindings/CompetitorLaps';
export type * from '@bindings/CompetitorRef';
export type * from '@bindings/CompetitorTrace';
export type * from '@bindings/CompletedHeat';
export type * from '@bindings/ContractVersion';
export type * from '@bindings/CreateClassRequest';
export type * from '@bindings/CreateEventRequest';
export type * from '@bindings/CreatePilotRequest';
export type * from '@bindings/CreateTimerRequest';
export type * from '@bindings/CrossingDisposition';
export type * from '@bindings/Cursor';
export type * from '@bindings/ErrorCode';
export type * from '@bindings/Event';
export type * from '@bindings/EventAuditEntry';
export type * from '@bindings/EventId';
export type * from '@bindings/EventMeta';
export type * from '@bindings/EventOutcome';
export type * from '@bindings/FillMode';
export type * from '@bindings/FillRoundOutcome';
export type * from '@bindings/FillStop';
export type * from '@bindings/FormatParam';
export type * from '@bindings/FormatSchema';
export type * from '@bindings/GateIndex';
export type * from '@bindings/GraceWindow';
export type * from '@bindings/HeatId';
export type * from '@bindings/HeatPhase';
export type * from '@bindings/HeatResult';
export type * from '@bindings/HeatSummary';
export type * from '@bindings/HeatTransition';
export type * from '@bindings/Hello';
export type * from '@bindings/ImdProduct';
export type * from '@bindings/ImdReading';
export type * from '@bindings/JoinTokenResponse';
export type * from '@bindings/Lap';
export type * from '@bindings/LapList';
export type * from '@bindings/NodeCalibration';
export type * from '@bindings/NodeChannel';
export type * from '@bindings/VoidReason';
export type * from '@bindings/LayoutId';
export type * from '@bindings/LayoutNode';
export type * from '@bindings/LayoutOverlap';
export type * from '@bindings/LayoutRating';
export type * from '@bindings/LifecycleState';
export type * from '@bindings/LiveCrossing';
export type * from '@bindings/LiveRaceState';
export type * from '@bindings/LogRef';
export type * from '@bindings/MemberSlot';
export type * from '@bindings/Metric';
export type * from '@bindings/NewChannelLayoutRequest';
export type * from '@bindings/NewRoundReq';
export type * from '@bindings/NodeDrift';
export type * from '@bindings/NodeSignal';
export type * from '@bindings/OptionalEdit';
export type * from '@bindings/ParamKind';
export type * from '@bindings/Pass';
export type * from '@bindings/Penalty';
export type * from '@bindings/Pilot';
export type * from '@bindings/PilotId';
export type * from '@bindings/PilotProgress';
export type * from '@bindings/Placement';
export type * from '@bindings/PluginPresence';
export type * from '@bindings/ProjectionBody';
export type * from '@bindings/ProjectionKind';
export type * from '@bindings/ProtestOutcome';
export type * from '@bindings/ProtestWindow';
export type * from '@bindings/ProtocolError';
export type * from '@bindings/RankEntry';
export type * from '@bindings/RoundDef';
export type * from '@bindings/RoundId';
export type * from '@bindings/RoundIssue';
export type * from '@bindings/RoundMetric';
export type * from '@bindings/RoundStanding';
export type * from '@bindings/ScheduledHeat';
export type * from '@bindings/Scope';
export type * from '@bindings/SeatProblem';
export type * from '@bindings/SeedingRule';
export type * from '@bindings/ServerHello';
export type * from '@bindings/SessionId';
export type * from '@bindings/SetActiveEventRequest';
export type * from '@bindings/SetClassHiddenRequest';
export type * from '@bindings/SetChannelLayoutRequest';
export type * from '@bindings/SetClassMembershipRequest';
export type * from '@bindings/SetEventClassesRequest';
export type * from '@bindings/SetEventRosterRequest';
export type * from '@bindings/SetEventTimersRequest';
export type * from '@bindings/SetPrimaryTimerRequest';
export type * from '@bindings/SetTimerNodesRequest';
export type * from '@bindings/SignalChunk';
export type * from '@bindings/SignalContext';
export type * from '@bindings/SignalHistory';
export type * from '@bindings/SignalThresholds';
export type * from '@bindings/SignalTraceView';
export type * from '@bindings/Snapshot';
export type * from '@bindings/SourceTime';
export type * from '@bindings/StartProcedure';
export type * from '@bindings/StartTone';
export type * from '@bindings/StreamMessage';
export type * from '@bindings/SubscribeRequest';
export type * from '@bindings/Timer';
export type * from '@bindings/TimerId';
export type * from '@bindings/TimerKind';
export type * from '@bindings/TimerNode';
export type * from '@bindings/TimerNodes';
export type * from '@bindings/TimerSignal';
export type * from '@bindings/TimerStatus';
export type * from '@bindings/UpdateClassRequest';
export type * from '@bindings/UpdatePilotRequest';
export type * from '@bindings/UpdateRoundReq';
export type * from '@bindings/UpdateTimerRequest';
export type * from '@bindings/VoidedPass';
export type * from '@bindings/VtxType';
export type * from '@bindings/WinCondition';
