/**
 * @gridfpv/types — the single import seam for GridFPV protocol types.
 *
 * Every app and package imports protocol/wire types from here:
 *
 *     import type { RaceSnapshot, PilotId } from '@gridfpv/types';
 *
 * The actual definitions come from the ts-rs–generated bindings (see
 * ./generated.ts for how regenerated bindings flow in, and the standalone
 * fallback used when `bindings/` is absent).
 */
export * from './generated.js';
