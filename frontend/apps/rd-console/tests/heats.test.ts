/**
 * `lib/heats.ts` after #456: the console **consumes** the heat name the server resolved onto
 * {@link HeatSummary.name} rather than re-deriving it.
 *
 * So these no longer assert the naming convention — that lives once, in the server's
 * `round_engine::heat_name`, and is pinned by its own tests (including the practice-heat numbering
 * the two used to disagree about). What is left to assert here is the console's actual job: the
 * id → summary lookup, the removed-heat answer for an id the event no longer serves, and the
 * raw-handle last resort.
 */
import { describe, expect, it } from 'vitest';
import type { HeatSummary, RoundDef } from '@gridfpv/types';
import {
  heatDisplayName,
  heatNameById,
  REMOVED_HEAT_NAME,
  isDeterministicRound,
  isOpenPracticeRound
} from '../src/lib/heats.js';

const round = (over: Partial<RoundDef> = {}): RoundDef =>
  ({
    id: 'r1',
    label: 'Qualifying R1',
    classes: ['c1'],
    format: 'timed_qual',
    params: {},
    win_condition: 'BestLap',
    seeding: 'FromRoster',
    channel_mode: 'Static',
    staging_timer_secs: 300,
    start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
    grace_window: { Duration: { micros: 3_000_000 } },
    ...over
  }) as RoundDef;

/** A heat as the server serves it — `name` already resolved. */
const heat = (id: string, name = `Qualifying R1 ${id}`): HeatSummary => ({
  heat: id,
  name,
  lineup: [],
  round: 'r1',
  class: 'c1',
  frequencies: [],
  phase: 'Scheduled',
  is_current: false
});

describe('heats — heatDisplayName renders the name the server resolved', () => {
  it('returns the wire name verbatim', () => {
    expect(heatDisplayName(heat('h-b', 'Qualifying R1 Heat 2'))).toBe('Qualifying R1 Heat 2');
    expect(heatDisplayName(heat('main-B', 'B-Main'))).toBe('B-Main');
    expect(heatDisplayName(heat('p-2', 'Practice Heat 2'))).toBe('Practice Heat 2');
    // A custom label is resolved server-side too — it arrives as the name, not as a second field
    // the console has to prefer for itself.
    expect(heatDisplayName(heat('h-a', 'Featured Heat'))).toBe('Featured Heat');
  });

  it('falls back to the raw handle when the server had no name to give', () => {
    // A sim / free-text heat with no round to derive from: the handle IS the RD's own identifier,
    // and this is the resolver's documented last resort rather than an id leak.
    expect(heatDisplayName(heat('q-1', 'q-1'))).toBe('q-1');
    expect(heatDisplayName(heat('q-1', '   '))).toBe('q-1');
    expect(heatDisplayName({ ...heat('q-1'), name: undefined as unknown as string })).toBe('q-1');
  });
});

describe('heats — heatNameById (by-id resolution for Live control)', () => {
  it('resolves a heat id through the heats list to that heat’s name', () => {
    const list = [heat('h-a', 'Qualifying R1 Heat 1'), heat('h-b', 'Qualifying R1 Heat 2')];
    expect(heatNameById('h-b', list)).toBe('Qualifying R1 Heat 2');
  });

  it('#418: a heat the event no longer serves is NAMED as removed, never rendered as its id', () => {
    // Removing a round takes its unstarted heats with it. The live `current_heat` can still be
    // pointing at one (the RD had it loaded in Live control), and the raw id must not surface.
    expect(heatNameById('unknown', [heat('h-a')])).toBe(REMOVED_HEAT_NAME);
    expect(heatNameById('unknown', [heat('h-a')])).not.toContain('unknown');
  });

  it('falls back to the bare handle for a heat the server could not name', () => {
    expect(heatNameById('q-1', [{ ...heat('q-1', 'q-1'), round: undefined }])).toBe('q-1');
  });
});

describe('heats — round predicates', () => {
  it('reports an open-practice round as such, and a competition round as not', () => {
    expect(isOpenPracticeRound(round({ format: 'open_practice' }))).toBe(true);
    expect(isOpenPracticeRound(round())).toBe(false);
  });

  it('reports every format but open practice as deterministic (one-action generate)', () => {
    expect(isDeterministicRound(round())).toBe(true);
    expect(isDeterministicRound(round({ format: 'open_practice' }))).toBe(false);
  });
});
