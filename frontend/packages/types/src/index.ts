/**
 * @gridfpv/types — the single import seam for GridFPV protocol types.
 *
 * Every app and package imports protocol/wire types from here:
 *
 *     import type { Snapshot, ChangeEnvelope, Command, LapList } from '@gridfpv/types';
 *
 * The actual definitions come from the ts-rs–generated bindings; see
 * ./generated.ts for how the barrel re-exports each `bindings/*.ts` and how
 * regenerated bindings flow in.
 */
export * from './generated.js';
