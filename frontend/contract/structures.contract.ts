/**
 * Tournament-structure contract (format model, D17): the multi-wave generators — multi-main with
 * bump-ups, double-elimination, and round-robin — driven end to end against the real Director.
 *
 * These are the structures the new format model composes from racing. Unlike a single-elim level
 * (one wave of heats), each of these emits heats in **multiple waves**: round-robin round-by-round,
 * multi-main's bump ladder one main at a time bottom-up, double-elim's winners bracket then losers
 * bracket then grand final. The Director schedules each wave only once the prior wave's heats are
 * finalized (FillRound re-reads the log and the generator emits the next wave). This proves that
 * whole loop, plus each structure's defining behaviour: a lower main feeds the next main up, the
 * winners/losers brackets resolve to one champion, and a round-robin rotates and ranks its field.
 *
 * The sim makes earlier seeds finish first, so qualifying — and therefore every structure's seeding
 * and outcomes — is deterministic.
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

const TOKEN = 'rd-structures-contract';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 25 });
});

afterAll(async () => {
  await director?.stop();
});

/** Every heat tagged with `round`, in schedule order, as `{ id, lineup }`. */
async function heatsOfRound(roundId: string): Promise<Array<{ id: string; lineup: string[] }>> {
  const resp = await fetch(`${eventRoot(director.baseUrl)}/heats`);
  const heats = (await resp.json()) as Array<{ heat: string; round?: string; lineup: string[] }>;
  return heats
    .filter((h) => h.round === roundId)
    .map((h) => ({ id: h.heat, lineup: h.lineup.map(String) }));
}

/** FillRound (All) a round, surfacing the ack error if it is rejected. */
async function fillAll(roundId: string): Promise<void> {
  const ack = await rdControl(director.baseUrl, TOKEN, {
    FillRound: { round: roundId, mode: 'All' }
  });
  if (!ack.ok) throw new Error(`FillRound ${roundId} rejected: ${JSON.stringify(ack.error)}`);
}

/** Drive a heat to Running, wait until each competitor banks a lap, then ForceEnd + Finalize. */
async function runAndFinalize(heat: string, competitors: number): Promise<void> {
  await driveToRunning(director.baseUrl, TOKEN, heat);
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    const resp = await fetch(
      `${eventRoot(director.baseUrl)}/snapshot/heat/${heat}?projection=laps`
    );
    if (resp.ok) {
      const snap = (await resp.json()) as {
        body: { LapList?: { competitors: Array<{ laps: unknown[] }> } };
      };
      const cs = snap.body.LapList?.competitors ?? [];
      if (cs.length >= competitors && cs.every((c) => c.laps.length >= 1)) break;
    }
    await new Promise((r) => setTimeout(r, 25));
  }
  expect((await rdControl(director.baseUrl, TOKEN, { ForceEnd: { heat } })).ok).toBe(true);
  expect((await rdControl(director.baseUrl, TOKEN, { Finalize: { heat } })).ok).toBe(true);
}

/**
 * Drive a multi-wave round to completion: fill, run every freshly-scheduled heat, repeat until a
 * fill schedules nothing new (the generator returned Complete). Returns the heats in run order.
 */
async function drainRound(roundId: string): Promise<Array<{ id: string; lineup: string[] }>> {
  const run = new Set<string>();
  const order: Array<{ id: string; lineup: string[] }> = [];
  for (let guard = 0; guard < 64; guard++) {
    await fillAll(roundId);
    const fresh = (await heatsOfRound(roundId)).filter((h) => !run.has(h.id));
    if (fresh.length === 0) return order;
    for (const h of fresh) {
      await runAndFinalize(h.id, h.lineup.length);
      run.add(h.id);
      order.push(h);
    }
  }
  throw new Error(`drainRound(${roundId}) did not converge — generator never returned Complete`);
}

/** A class + `n` pilots rostered on Practice, plus a finished 1-round timed qual; returns its ranking. */
async function setupQual(
  prefix: string,
  n: number
): Promise<{ klassId: string; qualId: string; ranking: string[] }> {
  const klass = await createClass(director.baseUrl, { name: `${prefix}-class` }, TOKEN);
  const ids: string[] = [];
  for (let i = 1; i <= n; i++) {
    const p = await createPilot(director.baseUrl, { callsign: `${prefix}${i}` }, TOKEN);
    ids.push(p.id);
  }
  await setEventClasses(director.baseUrl, PRACTICE_EVENT_ID, [klass.id], TOKEN);
  await setEventRoster(director.baseUrl, PRACTICE_EVENT_ID, ids, TOKEN);
  await setClassMembership(director.baseUrl, PRACTICE_EVENT_ID, klass.id, ids, TOKEN);

  const qual = await createRound(
    director.baseUrl,
    PRACTICE_EVENT_ID,
    {
      label: `${prefix} Qualifying`,
      classes: [klass.id],
      format: 'timed_qual',
      params: { rounds: '1' },
      win_condition: 'BestLap',
      time_limit_secs: 60,
      seeding: 'FromRoster',
      channel_mode: 'PerHeat'
    },
    TOKEN
  );
  await fillAll(qual.id);
  for (const h of await heatsOfRound(qual.id)) await runAndFinalize(h.id, h.lineup.length);
  const ranking = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, qual.id);
  return { klassId: klass.id, qualId: qual.id, ranking: ranking.map((e) => String(e.competitor)) };
}

describe('tournament structures: multi-wave generators end to end', () => {
  it('multi-main bump-up: lower mains feed the next main up, bottom-up, to one standing', async () => {
    // 6 seeds, main_size 2, bump_n 1 → A[s1,s2] B[s3,s4] C[s5,s6]; the lowest main runs first and
    // its winner bumps up, repeating to the A main.
    const { klassId, qualId, ranking } = await setupQual('mm', 6);
    const mm = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Mains',
        classes: [klassId],
        format: 'multi_main',
        params: { main_size: '2', bump_n: '1' },
        win_condition: { FirstToLaps: { n: 1 } },
        seeding: { FromRanking: { source_rounds: [qualId], top_n: 6 } },
        channel_mode: 'PerHeat'
      },
      TOKEN
    );

    const order = (await drainRound(mm.id)).map((h) => h.id);
    // Bottom-up, one main at a time: C, then B, then A.
    expect(order).toEqual(['main-C', 'main-B', 'main-A']);

    const byId = Object.fromEntries((await heatsOfRound(mm.id)).map((h) => [h.id, h.lineup]));
    // C's winner (seed 5 = ranking[4]) bumped up into the B main (its 2 seeds + 1 bumped).
    expect(byId['main-B']).toContain(ranking[4]);
    expect(byId['main-B'].length).toBe(3);
    // B's winner bumped up into the A main.
    expect(byId['main-A'].length).toBe(3);
    expect(byId['main-A']).toContain(ranking[2]);

    const finalRank = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, mm.id);
    expect(finalRank.length).toBe(6);
    // Every pilot ranked exactly once — no one duplicated across the mains they bumped through.
    expect(new Set(finalRank.map((e) => String(e.competitor))).size).toBe(6);
  });

  it('double-elim: winners + losers brackets resolve to one champion', async () => {
    const { klassId, qualId, ranking } = await setupQual('de', 4);
    const de = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Double Elim',
        classes: [klassId],
        format: 'double_elim',
        params: {},
        win_condition: { FirstToLaps: { n: 1 } },
        seeding: { FromRanking: { source_rounds: [qualId], top_n: 4 } },
        channel_mode: 'PerHeat'
      },
      TOKEN
    );

    const ids = (await drainRound(de.id)).map((h) => h.id);
    // Both brackets and a grand final are run.
    expect(ids.some((id) => id.startsWith('de-w'))).toBe(true);
    expect(ids.some((id) => id.startsWith('de-l'))).toBe(true);
    expect(ids.some((id) => id.startsWith('de-gf'))).toBe(true);

    const finalRank = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, de.id);
    expect(finalRank.length).toBe(4);
    // The top seed wins out (the sim favours earlier seeds), so it's champion.
    expect(String(finalRank[0].competitor)).toBe(ranking[0]);
  });

  it('round-robin: rotates rounds and aggregates into a points ranking', async () => {
    const { klassId, qualId } = await setupQual('rr', 4);
    const rr = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Round Robin',
        classes: [klassId],
        format: 'round_robin',
        params: { rounds: '2', heat_size: '2', points: '10, 3' },
        win_condition: { FirstToLaps: { n: 1 } },
        seeding: { FromRanking: { source_rounds: [qualId], top_n: 4 } },
        channel_mode: 'PerHeat'
      },
      TOKEN
    );

    const ids = (await drainRound(rr.id)).map((h) => h.id);
    // Two rotated rounds of heats ran.
    expect(ids.some((id) => id.startsWith('rr-r1'))).toBe(true);
    expect(ids.some((id) => id.startsWith('rr-r2'))).toBe(true);

    const finalRank = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, rr.id);
    expect(
      finalRank.length,
      `heats=${JSON.stringify(ids)} rank=${JSON.stringify(finalRank.map((e) => String(e.competitor)))}`
    ).toBe(4);
    expect(new Set(finalRank.map((e) => String(e.competitor))).size).toBe(4);
  });
});
