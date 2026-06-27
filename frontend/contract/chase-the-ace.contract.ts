/**
 * Chase-the-Ace final contract: the multi-race final format end to end against the real Director.
 * A `chase_the_ace` round races the field repeatedly — one race per `FillRound` — until a pilot has
 * `wins_to_win` race-wins (default 2), then the round completes and ranks that pilot champion.
 *
 * This proves the server↔engine path the unit tests can't: that `FillRound` single-steps the
 * generator (each next race only appears after the prior is scored), that the series terminates
 * deterministically, and that the round ranking crowns a single champion.
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

const TOKEN = 'rd-cta-contract';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 25 });
});
afterAll(async () => {
  await director?.stop();
});

async function heatsOfRound(roundId: string): Promise<string[]> {
  const heats = (await (await fetch(`${eventRoot(director.baseUrl)}/heats`)).json()) as Array<{
    heat: string;
    round?: string;
  }>;
  return heats.filter((h) => h.round === roundId).map((h) => h.heat);
}

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

describe('Chase the Ace final: race until first to N wins, then crown a champion', () => {
  it('single-steps one race per FillRound and terminates with one champion', async () => {
    const klass = await createClass(director.baseUrl, { name: 'Open' }, TOKEN);
    const ids = [];
    for (const cs of ['ace', 'rival']) {
      ids.push((await createPilot(director.baseUrl, { callsign: cs }, TOKEN)).id);
    }
    await setEventClasses(director.baseUrl, PRACTICE_EVENT_ID, [klass.id], TOKEN);
    await setEventRoster(director.baseUrl, PRACTICE_EVENT_ID, ids, TOKEN);
    await setClassMembership(director.baseUrl, PRACTICE_EVENT_ID, klass.id, ids, TOKEN);

    const cta = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Grand Final',
        classes: [klass.id],
        format: 'chase_the_ace',
        params: { wins_to_win: '2' },
        win_condition: 'BestLap',
        time_limit_secs: 60,
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      },
      TOKEN
    );

    // Drive the series: FillRound emits the next race only after the prior is scored. Loop until a
    // FillRound produces no new heat (the generator completed). A 2-pilot first-to-2 ends in ≤ 3.
    let lastCount = 0;
    let races = 0;
    for (let i = 0; i < 5; i++) {
      await rdControl(director.baseUrl, TOKEN, { FillRound: { round: cta.id, mode: 'Next' } });
      const heats = await heatsOfRound(cta.id);
      if (heats.length === lastCount) break; // no new race → series complete
      lastCount = heats.length;
      races = heats.length;
      // Each FillRound adds exactly one race (single-step), never a batch.
      expect(heats.length).toBe(i + 1);
      await runAndFinalize(heats[heats.length - 1], 2);
    }

    // A 2-pilot first-to-2 final is decided in 2 or 3 races, never more.
    expect(races).toBeGreaterThanOrEqual(2);
    expect(races).toBeLessThanOrEqual(3);

    // Exactly one champion: a clear position-1, and the runner-up strictly behind.
    const ranking = await roundRanking(director.baseUrl, PRACTICE_EVENT_ID, cta.id);
    expect(ranking.length).toBe(2);
    expect(ranking[0].position).toBe(1);
    expect(ranking[1].position).toBe(2);
    expect(ids).toContain(ranking[0].competitor as string);
  });
});
