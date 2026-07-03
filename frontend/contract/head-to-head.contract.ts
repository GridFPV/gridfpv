/**
 * Head-to-Head multi-rotation contract (rotations param): a `head_to_head` round with
 * `rotations: 2` drives the REAL Director end-to-end — `FillRound(All)` lays out the same
 * groups twice (one grouping decision per round; rotation-scoped `h2h-r{n}-h{m}` ids), every
 * rotation's heat runs and finalizes, and the round ranking accumulates points across the
 * heats each pilot flew.
 *
 * Mirrors the standings contract's drive pattern: real class + pilots + membership on the
 * Practice event, deterministic mock-sim laps (top seed fastest), ForceEnd → Finalize per heat.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createClass,
  createPilot,
  createRound,
  PRACTICE_EVENT_ID,
  roundRanking,
  setClassMembership,
  setEventClasses,
  setEventRoster
} from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { driveToRunning, eventRoot, rdControl, startContractDirector } from './harness.ts';

const TOKEN = 'rd-h2h-contract';

let director: Director;

beforeAll(async () => {
  // A brisk sim (one quick lap per pilot, earlier seeds faster) so each heat scores fast and
  // finishing order inside a heat is deterministic (lineup order).
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 30 });
});

afterAll(async () => {
  await director?.stop();
});

/** The wire shape of a scheduled heat as `GET /events/{id}/heats` serves it. */
interface WireHeat {
  heat: string;
  lineup: string[];
  round?: string;
}

/** Read the round's heats off the real heats route, in served order. */
async function roundHeats(roundId: string): Promise<WireHeat[]> {
  const res = await fetch(`${eventRoot(director.baseUrl)}/heats`);
  expect(res.status).toBe(200);
  const all = (await res.json()) as WireHeat[];
  return all.filter((h) => h.round === roundId);
}

/** Poll the heat's `laps` projection until every competitor present has banked ≥ 1 lap. */
async function waitForLaps(heat: string, pilots: number, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await fetch(
      `${eventRoot(director.baseUrl)}/snapshot/heat/${heat}?projection=laps`
    );
    if (resp.ok) {
      const snap = (await resp.json()) as {
        body: { LapList?: { competitors: Array<{ laps: unknown[] }> } };
      };
      const competitors = snap.body.LapList?.competitors ?? [];
      if (competitors.length >= pilots && competitors.every((c) => c.laps.length >= 1)) return;
    }
    await new Promise((r) => setTimeout(r, 30));
  }
  throw new Error(`heat ${heat}: not all ${pilots} pilots banked a lap within ${timeoutMs}ms`);
}

describe('head_to_head rotations: the same groups race N times and points accumulate', () => {
  it('fills 2 rotations of identical groups, runs them all, and ranks by summed points', async () => {
    // Real directory setup: one class, four pilots, selected + rostered + membered on Practice.
    const klass = await createClass(director.baseUrl, { name: 'ProSpec' }, TOKEN);
    const pilots = [];
    for (const callsign of ['alpha', 'bravo', 'charlie', 'delta']) {
      pilots.push(await createPilot(director.baseUrl, { callsign }, TOKEN));
    }
    const ids = pilots.map((p) => p.id);
    await setEventClasses(director.baseUrl, PRACTICE_EVENT_ID, [klass.id], TOKEN);
    await setEventRoster(director.baseUrl, PRACTICE_EVENT_ID, ids, TOKEN);
    await setClassMembership(director.baseUrl, PRACTICE_EVENT_ID, klass.id, ids, TOKEN);

    // The multi-rotation round: 2-up groups, the SAME groups twice, points scoring.
    const round = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'ProSpec R1',
        classes: [klass.id],
        format: 'head_to_head',
        params: { group_size: '2', rotations: '2', scoring: 'points' },
        // First-to-N self-terminates, so no round time limit is required; the sim emits one lap
        // and the heat closes via the ForceEnd completion override below.
        win_condition: { FirstToLaps: { n: 3 } },
        channel_mode: 'PerHeat',
        seeding: 'FromRoster'
      },
      TOKEN
    );

    // FillRound(All): the generator lays out BOTH rotations up front — 2 groups × 2 rotations,
    // rotation-scoped ids, and rotation 2 repeats rotation 1's lineups verbatim (the grouping
    // is drawn once).
    expect(
      (await rdControl(director.baseUrl, TOKEN, { FillRound: { round: round.id, mode: 'All' } })).ok
    ).toBe(true);
    const heats = await roundHeats(round.id);
    expect(heats.map((h) => h.heat).sort()).toEqual([
      'h2h-r1-h0',
      'h2h-r1-h1',
      'h2h-r2-h0',
      'h2h-r2-h1'
    ]);
    const byId = new Map(heats.map((h) => [h.heat, h.lineup]));
    expect(byId.get('h2h-r2-h0')).toEqual(byId.get('h2h-r1-h0'));
    expect(byId.get('h2h-r2-h1')).toEqual(byId.get('h2h-r1-h1'));
    // Groups follow the roster draw: (alpha, bravo) and (charlie, delta).
    expect(byId.get('h2h-r1-h0')).toEqual([ids[0], ids[1]]);
    expect(byId.get('h2h-r1-h1')).toEqual([ids[2], ids[3]]);

    // Run every rotation's heats sequentially (finalize each before the next, so no dangling
    // Running heat absorbs the next heat's passes).
    for (const heat of ['h2h-r1-h0', 'h2h-r1-h1', 'h2h-r2-h0', 'h2h-r2-h1']) {
      await driveToRunning(director.baseUrl, TOKEN, heat);
      await waitForLaps(heat, 2);
      expect((await rdControl(director.baseUrl, TOKEN, { ForceEnd: { heat } })).ok).toBe(true);
      expect((await rdControl(director.baseUrl, TOKEN, { Finalize: { heat } })).ok).toBe(true);
    }

    // The round ranking sums linear 2-up points across both rotations: alpha and charlie each
    // win both of their heats (2+2 = 4 points, tied at position 1, field order breaking the
    // listing); bravo and delta each lose both (1+1 = 2, tied at position 3).
    const ranking = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, round.id);
    expect(ranking.map((e) => e.competitor)).toEqual([ids[0], ids[2], ids[1], ids[3]]);
    expect(ranking.map((e) => e.position)).toEqual([1, 1, 3, 3]);

    // With every rotation finalized the round is spent: another fill schedules nothing new.
    expect(
      (await rdControl(director.baseUrl, TOKEN, { FillRound: { round: round.id, mode: 'Next' } }))
        .ok
    ).toBe(true);
    expect((await roundHeats(round.id)).length).toBe(4);
  });
});
