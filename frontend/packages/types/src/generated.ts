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
 * This list is kept in lockstep with `bindings/` (one line per `bindings/*.ts`).
 */

export type * from '@bindings/AdapterId';
export type * from '@bindings/Change';
export type * from '@bindings/ChangeEnvelope';
export type * from '@bindings/ClassId';
export type * from '@bindings/Command';
export type * from '@bindings/CommandAck';
export type * from '@bindings/CompetitorKey';
export type * from '@bindings/CompetitorLaps';
export type * from '@bindings/CompetitorRef';
export type * from '@bindings/CompletedHeat';
export type * from '@bindings/ContractVersion';
export type * from '@bindings/Cursor';
export type * from '@bindings/ErrorCode';
export type * from '@bindings/Event';
export type * from '@bindings/EventId';
export type * from '@bindings/EventOutcome';
export type * from '@bindings/GateIndex';
export type * from '@bindings/HeatId';
export type * from '@bindings/HeatPhase';
export type * from '@bindings/HeatResult';
export type * from '@bindings/HeatTransition';
export type * from '@bindings/Hello';
export type * from '@bindings/JoinTokenResponse';
export type * from '@bindings/Lap';
export type * from '@bindings/LapList';
export type * from '@bindings/LiveRaceState';
export type * from '@bindings/LogRef';
export type * from '@bindings/Metric';
export type * from '@bindings/Pass';
export type * from '@bindings/Penalty';
export type * from '@bindings/PilotId';
export type * from '@bindings/PilotProgress';
export type * from '@bindings/Placement';
export type * from '@bindings/ProjectionBody';
export type * from '@bindings/ProjectionKind';
export type * from '@bindings/ProtocolError';
export type * from '@bindings/RankEntry';
export type * from '@bindings/Scope';
export type * from '@bindings/ServerHello';
export type * from '@bindings/SessionId';
export type * from '@bindings/SignalContext';
export type * from '@bindings/Snapshot';
export type * from '@bindings/SourceTime';
export type * from '@bindings/SubscribeRequest';
export type * from '@bindings/WinCondition';
