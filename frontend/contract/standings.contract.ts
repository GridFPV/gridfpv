/**
 * Round-ranking + class-standings contract (race redesign Slice 5/6a): the two read routes
 * `GET /events/{id}/rounds/{round}/ranking` and `GET /events/{id}/classes/{class}/standings`,
 * plus the real `roundRanking` / `classStandings` client helpers.
 *
 * Sets up a real class + roster + membership + round on the Practice event through the real client
 * helpers, schedules that round's heat (tagged with the round + class), drives it to `Final` with
 * deterministic inserted laps (pilot A faster than B), then reads the two projections back through
 * the real `@gridfpv/protocol-client`. Asserts the round ranking comes back best-first (A then B)
 * and the class standings aggregate that round into one per-pilot row each, A leading on points. If
 * the routes, the bindings, or the season-join fold were wrong, the shapes would not round-trip.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  classStandings,
  createClass,
  createPilot,
  createRound,
  roundRanking,
  roundStandings,
  setClassMembership,
  setEventClasses,
  setEventRoster
} from '../packages/protocol-client/dist/index.js';
import {
  driveToRunning,
  rdControl,
  startDirectorWithEvent,
  type ContractDirector
} from './harness.ts';

const TOKEN = 'rd-standings-contract';
const HEAT = 's-1';

let director: ContractDirector;

beforeAll(async () => {
  // A brisk sim (one quick lap) so the heat scores fast; the sim makes the top seed faster than
  // the next, giving a deterministic A-before-B round ranking.
  director = await startDirectorWithEvent({ token: TOKEN, simLaps: 1, simLapMs: 30 });
});

afterAll(async () => {
  await director?.stop();
});

describe('GET round ranking + class standings serve the season-join projections', () => {
  it('ranking is best-first and standings aggregate the round per pilot', async () => {
    // Real directory setup: a class + two pilots, selected + rostered + membered on Practice.
    const klass = await createClass(director.baseUrl, { name: 'Open' }, TOKEN);
    const pilotA = await createPilot(director.baseUrl, { callsign: 'alpha' }, TOKEN);
    const pilotB = await createPilot(director.baseUrl, { callsign: 'bravo' }, TOKEN);

    await setEventClasses(director.baseUrl, director.event, [klass.id], TOKEN);
    await setEventRoster(director.baseUrl, director.event, [pilotA.id, pilotB.id], TOKEN);
    await setClassMembership(
      director.baseUrl,
      director.event,
      klass.id,
      [pilotA.id, pilotB.id],
      TOKEN
    );

    // A single-round timed_qual over the class (BestLap), seeded from the roster membership.
    const round = await createRound(
      director.baseUrl,
      director.event,
      {
        label: 'Qualifying',
        classes: [klass.id],
        format: 'timed_qual',
        params: { rounds: '1' },
        win_condition: 'BestLap',
        // Best Lap only ranks, so a scored round needs a race time to end (server validation).
        time_limit_secs: 60,
        seeding: 'FromRoster'
      },
      TOKEN
    );

    // Schedule the round's heat tagged with the round + class; lineup is the two members.
    expect(
      (
        await rdControl(director, TOKEN, {
          ScheduleHeat: {
            heat: HEAT,
            lineup: [pilotA.id, pilotB.id],
            class: klass.id,
            round: round.id
          }
        })
      ).ok
    ).toBe(true);

    // Drive the heat to Running; the sim bridge emits a holeshot + one lap per pilot, with the
    // top seed (A) faster than the next (B) — a deterministic round ranking.
    await driveToRunning(director, TOKEN, HEAT);

    // Wait until both pilots have banked their lap (a `laps` projection shows a completed lap each)
    // before closing the heat, so the scored result is final.
    await waitForBothLaps(director.eventRoot, HEAT);

    // Close the heat: ForceEnd (the completion override) → Finalize.
    expect((await rdControl(director, TOKEN, { ForceEnd: { heat: HEAT } })).ok).toBe(true);
    expect((await rdControl(director, TOKEN, { Finalize: { heat: HEAT } })).ok).toBe(true);

    // 1) The round ranking: best-first, A ahead of B, positions 1-based.
    const ranking = await roundRanking(director.baseUrl, director.event, round.id);
    expect(ranking.map((e) => e.competitor)).toEqual([pilotA.id, pilotB.id]);
    expect(ranking[0].position).toBe(1);

    // 1b) The round standings: same competitors + positions as the ranking, each with a best lap
    // and the BestLap win-condition metric (this round is scored under BestLap).
    const standingsByRound = await roundStandings(director.baseUrl, director.event, round.id);
    expect(standingsByRound.map((s) => [s.competitor, s.position])).toEqual(
      ranking.map((e) => [e.competitor, e.position])
    );
    expect(standingsByRound[0].best_lap_micros).toBeTypeOf('number');
    expect(standingsByRound[0].metric).toHaveProperty('BestLap');

    // 2) The class standings: one row per pilot, aggregated across the class's rounds, best first.
    const standings = await classStandings(director.baseUrl, director.event, klass.id);
    expect(standings.class).toBe(klass.id);
    expect(standings.standings.map((s) => s.competitor)).toEqual([pilotA.id, pilotB.id]);
    expect(standings.standings[0].position).toBe(1);
    // A leads on points (2-pilot field: winner = 2 points, runner-up = 1).
    expect(standings.standings[0].points).toBeGreaterThan(standings.standings[1].points);
    expect(standings.standings[0].rounds_entered).toBe(1);
  });
});

/** Poll the heat's `laps` projection until every competitor present has at least one completed lap. */
async function waitForBothLaps(eventRoot: string, heat: string, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const resp = await fetch(`${eventRoot}/snapshot/heat/${heat}?projection=laps`);
    if (resp.ok) {
      const snap = (await resp.json()) as {
        body: { LapList?: { competitors: Array<{ laps: unknown[] }> } };
      };
      const competitors = snap.body.LapList?.competitors ?? [];
      if (competitors.length >= 2 && competitors.every((c) => c.laps.length >= 1)) return;
    }
    await new Promise((r) => setTimeout(r, 30));
  }
  throw new Error(`both pilots did not bank a lap within ${timeoutMs}ms`);
}
