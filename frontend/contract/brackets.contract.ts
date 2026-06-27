/**
 * Bracket-chain contract (#217, decisions D13): the **level-per-round** single-elimination flow
 * end to end against the real Director — qualify → "Advance to bracket" (level 1, seeded
 * `FromRanking`) → "Advance bracket" (level 2, seeded `FromHeatWinners`) → champion.
 *
 * This is the regression net the suite was missing: the UI's advance helpers
 * (`advanceRoundReq` / `advanceLevelReq`) build the round requests this drives, so the chain only
 * round-trips if (a) a scored bracket round carries an end condition the server accepts (the
 * `time_limit_secs` carry — see the scored-round end-condition rule), (b) `FromRanking` seeds the
 * first level from the qual ranking, and (c) `FromHeatWinners` carries each level's heat winners
 * into the next level's heats. If any of those broke, this fails.
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

const TOKEN = 'rd-brackets-contract';

let director: Director;

beforeAll(async () => {
  // A brisk sim (one quick lap) so each heat scores fast; the sim makes earlier seeds faster, so
  // the qual ranking — and thus the bracket seeding — is deterministic.
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 25 });
});

afterAll(async () => {
  await director?.stop();
});

/** Every heat tagged with `round`, in schedule order, as `{ id, lineup }`. */
async function heatsOfRound(roundId: string): Promise<Array<{ id: string; lineup: string[] }>> {
  const resp = await fetch(`${eventRoot(director.baseUrl)}/heats`);
  const heats = (await resp.json()) as Array<{
    heat: string;
    round?: string;
    lineup: string[];
  }>;
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

describe('single-elim bracket chain: qualify → advance to bracket → advance level → champion', () => {
  it('seeds level 1 FromRanking, carries winners FromHeatWinners, and crowns a champion', async () => {
    // 1) A class + four pilots, selected + rostered + membered on Practice.
    const klass = await createClass(director.baseUrl, { name: 'Open' }, TOKEN);
    const pilots = [];
    for (const cs of ['alpha', 'bravo', 'charlie', 'delta']) {
      pilots.push(await createPilot(director.baseUrl, { callsign: cs }, TOKEN));
    }
    const ids = pilots.map((p) => p.id);
    await setEventClasses(director.baseUrl, PRACTICE_EVENT_ID, [klass.id], TOKEN);
    await setEventRoster(director.baseUrl, PRACTICE_EVENT_ID, ids, TOKEN);
    await setClassMembership(director.baseUrl, PRACTICE_EVENT_ID, klass.id, ids, TOKEN);

    // 2) A single-round timed_qual (Best Lap + a race time, as the rule requires). Fill + run its
    //    heat(s), finalize, and read the ranking — the bracket seed.
    const qual = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Qualifying',
        classes: [klass.id],
        format: 'timed_qual',
        params: { rounds: '1' },
        win_condition: 'BestLap',
        time_limit_secs: 60,
        seeding: 'FromRoster',
        // Force per-heat channels so the field draws from the timer pool (the members carry no
        // static channel here) — this test is about the bracket carry, not static channel layout.
        channel_mode: 'PerHeat'
      },
      TOKEN
    );
    await fillAll(qual.id);
    for (const h of await heatsOfRound(qual.id)) await runAndFinalize(h.id, h.lineup.length);
    const qualRanking = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, qual.id);
    expect(qualRanking.length).toBe(4);

    // 3) "Advance to bracket" → level 1 (Semifinals), single_elim seeded FromRanking top-4, carrying
    //    the qual's win condition + race time. Fill = the seeded matchups (4 seeds → 2 heads).
    const semis = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Semifinals',
        classes: [klass.id],
        format: 'single_elim',
        params: {},
        win_condition: 'BestLap',
        time_limit_secs: 60,
        seeding: { FromRanking: { source_rounds: [qual.id], top_n: 4 } }
      },
      TOKEN
    );
    await fillAll(semis.id);
    const semisHeats = await heatsOfRound(semis.id);
    expect(semisHeats.length).toBe(2);
    expect(semisHeats.every((h) => h.lineup.length === 2)).toBe(true);
    // Every semis pilot is one of the qual's top-4.
    const seeds = new Set(qualRanking.map((e) => e.competitor as string));
    for (const h of semisHeats) for (const c of h.lineup) expect(seeds.has(c)).toBe(true);
    for (const h of semisHeats) await runAndFinalize(h.id, 2);

    // 4) "Advance bracket" → level 2 (Final), seeded FromHeatWinners of the semis. Fill = one heat
    //    pairing the two semis winners.
    const final = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Final',
        classes: [klass.id],
        format: 'single_elim',
        params: {},
        win_condition: 'BestLap',
        time_limit_secs: 60,
        seeding: { FromHeatWinners: { source_round: semis.id } },
        channel_mode: 'PerHeat'
      },
      TOKEN
    );
    await fillAll(final.id);
    const finalHeats = await heatsOfRound(final.id);
    expect(finalHeats.length).toBe(1);
    const finalLineup = finalHeats[0].lineup;
    expect(finalLineup.length).toBe(2);

    // The two finalists are the winners of the two distinct semis heats (the FromHeatWinners carry):
    // each finalist came from a different semis heat, and both were semis pilots.
    const heatOf = (c: string) => semisHeats.findIndex((h) => h.lineup.includes(c));
    expect(finalLineup.every((c) => heatOf(c) >= 0)).toBe(true);
    expect(heatOf(finalLineup[0])).not.toBe(heatOf(finalLineup[1]));

    // 5) Run the final → its ranking's position-1 is the champion (one of the two finalists).
    await runAndFinalize(finalHeats[0].id, 2);
    const finalRanking = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, final.id);
    expect(finalRanking[0].position).toBe(1);
    expect(finalLineup).toContain(finalRanking[0].competitor as string);
  });
});
