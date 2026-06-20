/**
 * Adapter to the ts-rs–generated protocol bindings.
 *
 * The wire types are generated from the Rust server crate into the repo-root
 * `bindings/` directory (one file per type), per docs/clients.html §3 and
 * architecture.html §6. The frontend NEVER hand-writes a wire type — this file
 * is the only seam that knows where the generated types physically live.
 *
 * ── When real bindings exist ────────────────────────────────────────────────
 * `bindings/` is expected to expose a barrel (e.g. `bindings/index.ts`). Replace
 * the placeholder block below with a single re-export, resolved through the
 * `@bindings/*` tsconfig path alias:
 *
 *     export * from '@bindings/index';
 *
 * (or re-export the specific generated modules you need). Nothing else in the
 * frontend changes, because everything imports from `@gridfpv/types`.
 *
 * ── Standalone fallback (bindings/ absent) ──────────────────────────────────
 * Until the Rust generation step has run — e.g. a frontend-only checkout or CI
 * that builds the frontend in isolation — `bindings/` may not exist. To keep the
 * monorepo buildable and type-checkable on its own, we define a minimal set of
 * placeholder types here. These are intentionally thin and exist only so the
 * scaffold compiles; they are replaced wholesale by the generated re-export.
 */

/** Opaque identifier for a pilot. Generated type will supersede this. */
export type PilotId = string;

/** Opaque identifier for a race/heat. Generated type will supersede this. */
export type RaceId = string;

/**
 * Placeholder snapshot shape. The real, ts-rs–generated projection snapshot
 * type will replace this once `bindings/` is populated.
 */
export interface RaceSnapshot {
  raceId: RaceId;
  /** Pilots in finishing/standings order. */
  pilots: PilotId[];
}
