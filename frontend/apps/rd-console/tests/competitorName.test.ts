/**
 * The shared competitor → display-name resolver (`competitorName.ts`).
 *
 * Both Live control and Marshaling resolve a competitor ref to a friendly name through this one
 * helper. The four cases it must cover (and which the Marshaling raw-id bug exercised):
 *   1. an explicit `Register` binding → the bound pilot's callsign;
 *   2. the roster-seeded binding (ref IS the pilot id) → the directory callsign, no progress needed;
 *   3. an unbound open-practice `node-{i}` seat → its channel label (never "node-0");
 *   4. a bare human handle (a sim heat) → as-is.
 */
import { describe, expect, it } from 'vitest';
import type { Pilot } from '@gridfpv/types';
import { createCompetitorNameResolver } from '../src/lib/competitorName.js';

const pilot = (id: string, callsign: string): Pilot =>
  ({ id, callsign, vtx_types: [] }) as unknown as Pilot;

describe('createCompetitorNameResolver', () => {
  it('resolves an EXPLICIT registration binding to the bound pilot callsign', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map([['pilot-1', pilot('pilot-1', 'Maverick')]]),
      explicitPilotByRef: new Map([['node-0', 'pilot-1']])
    });
    expect(resolve('node-0')).toBe('Maverick');
  });

  it('resolves the ROSTER-SEEDED case where the ref IS the pilot id (no progress binding)', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map([['goose-yla6dp', pilot('goose-yla6dp', 'Goose')]]),
      explicitPilotByRef: new Map()
    });
    // The ref equals the pilot id (the FromRoster seeding) — resolves to the callsign directly.
    expect(resolve('goose-yla6dp')).toBe('Goose');
  });

  it('falls back to the CHANNEL LABEL for an unbound node-{i} seat (never "node-0")', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map(),
      explicitPilotByRef: new Map(),
      channelByRef: new Map([['node-0', 'Raceband R1 · 5658']])
    });
    expect(resolve('node-0')).toBe('Raceband R1 · 5658');
  });

  it('returns the bare ref for a human handle with no binding (a sim heat)', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map(),
      explicitPilotByRef: new Map()
    });
    expect(resolve('ALICE')).toBe('ALICE');
  });

  it('an unbound node seat with no channel map still falls back to the bare ref', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map(),
      explicitPilotByRef: new Map()
    });
    expect(resolve('node-2')).toBe('node-2');
  });

  it('the explicit binding wins over a same-named directory id', () => {
    const resolve = createCompetitorNameResolver({
      pilotById: new Map([
        ['pilot-1', pilot('pilot-1', 'Maverick')],
        ['node-0', pilot('node-0', 'WRONG')]
      ]),
      explicitPilotByRef: new Map([['node-0', 'pilot-1']])
    });
    expect(resolve('node-0')).toBe('Maverick');
  });
});
