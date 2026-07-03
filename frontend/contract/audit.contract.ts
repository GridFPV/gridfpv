/**
 * Event-wide audit contract: `GET /events/{id}/audit` plus the real `eventAudit` client helper.
 *
 * Drives a real Director end-to-end: directory setup (class + pilots + round), schedule + run +
 * finalize a heat, then two marshaling rulings — a penalty (heat-tagged) and a detection void
 * (target-addressed, aimed at a real pass offset read back from the laps projection). The
 * event-wide audit must then serve both rulings through the real `@gridfpv/protocol-client`,
 * each **tagged with the heat** it rules on and in **newest-first** order (descending global
 * append offset) — the same window-attributed entries Marshaling's per-heat `?projection=audit`
 * serves, so the Audit page and Marshaling can never disagree. If the route, the flattened
 * `EventAuditEntry` binding, or the merge/sort were wrong, the shapes would not round-trip.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createClass,
  createPilot,
  createRound,
  eventAudit,
  PRACTICE_EVENT_ID,
  setClassMembership,
  setEventClasses,
  setEventRoster
} from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { driveToRunning, eventRoot, rdControl, startContractDirector } from './harness.ts';

const TOKEN = 'rd-audit-contract';
const HEAT = 'a-1';

let director: Director;

beforeAll(async () => {
  // A brisk sim (one quick lap each) so the heat closes fast and the rulings can land.
  director = await startContractDirector({ token: TOKEN, simLaps: 1, simLapMs: 30 });
});

afterAll(async () => {
  await director?.stop();
});

describe('GET /events/{id}/audit serves the heat-tagged, newest-first event audit', () => {
  it('rulings on a finalized heat arrive heat-tagged, newest first', async () => {
    // Real directory setup: a class + two pilots, selected + rostered + membered on Practice.
    const klass = await createClass(director.baseUrl, { name: 'Open' }, TOKEN);
    const pilotA = await createPilot(director.baseUrl, { callsign: 'alpha' }, TOKEN);
    const pilotB = await createPilot(director.baseUrl, { callsign: 'bravo' }, TOKEN);
    await setEventClasses(director.baseUrl, PRACTICE_EVENT_ID, [klass.id], TOKEN);
    await setEventRoster(director.baseUrl, PRACTICE_EVENT_ID, [pilotA.id, pilotB.id], TOKEN);
    await setClassMembership(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      klass.id,
      [pilotA.id, pilotB.id],
      TOKEN
    );
    const round = await createRound(
      director.baseUrl,
      PRACTICE_EVENT_ID,
      {
        label: 'Qualifying',
        classes: [klass.id],
        format: 'timed_qual',
        params: { rounds: '1' },
        win_condition: 'BestLap',
        time_limit_secs: 60,
        seeding: 'FromRoster'
      },
      TOKEN
    );

    // An empty event has an empty audit (a 200 [], not an error).
    expect(await eventAudit(director.baseUrl, PRACTICE_EVENT_ID)).toEqual([]);

    // Schedule + run + finalize the round's heat.
    expect(
      (
        await rdControl(director.baseUrl, TOKEN, {
          ScheduleHeat: {
            heat: HEAT,
            lineup: [pilotA.id, pilotB.id],
            class: klass.id,
            round: round.id
          }
        })
      ).ok
    ).toBe(true);
    await driveToRunning(director.baseUrl, TOKEN, HEAT);
    await waitForBothLaps(director.baseUrl, HEAT);
    expect((await rdControl(director.baseUrl, TOKEN, { ForceEnd: { heat: HEAT } })).ok).toBe(true);
    expect((await rdControl(director.baseUrl, TOKEN, { Finalize: { heat: HEAT } })).ok).toBe(true);

    // Two rulings on the finalized heat: a heat-tagged penalty, then a target-addressed void
    // aimed at a REAL pass offset (pilot A's first lap's end pass, read from the laps projection).
    expect(
      (
        await rdControl(director.baseUrl, TOKEN, {
          ApplyPenalty: {
            heat: HEAT,
            competitor: pilotB.id,
            penalty: { Disqualify: { reason: 'contract: flew the wrong course' } }
          }
        })
      ).ok
    ).toBe(true);
    const voidTarget = await firstLapEndRef(director.baseUrl, HEAT);
    expect(
      (await rdControl(director.baseUrl, TOKEN, { VoidDetection: { target: voidTarget } })).ok
    ).toBe(true);

    // The event-wide audit, through the real client helper.
    const trail = await eventAudit(director.baseUrl, PRACTICE_EVENT_ID);
    expect(trail).toHaveLength(2);

    // Newest first: descending global append offset — the void (appended last) leads.
    expect(trail[0].at_ref).toBeGreaterThan(trail[1].at_ref);
    expect(trail[0].kind).toBe('Voided');
    expect(trail[1].kind).toBe('PenaltyApplied');

    // Every entry carries the heat it rules on (the flattened EventAuditEntry shape: the per-heat
    // AuditEntry fields plus `heat`), with the structured competitor on the penalty.
    for (const entry of trail) expect(entry.heat).toBe(HEAT);
    expect(trail[1].competitor).toBe(pilotB.id);
    expect(trail[1].summary).toContain('DQ applied');
    expect(trail[0].summary).toContain(`(ref ${voidTarget})`);
  });
});

/** Poll the heat's `laps` projection until every competitor present has at least one completed lap. */
async function waitForBothLaps(baseUrl: string, heat: string, timeoutMs = 10_000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const competitors = await lapCompetitors(baseUrl, heat);
    if (competitors.length >= 2 && competitors.every((c) => c.laps.length >= 1)) return;
    await new Promise((r) => setTimeout(r, 30));
  }
  throw new Error(`both pilots did not bank a lap within ${timeoutMs}ms`);
}

/** The heat's first competitor's first-lap end pass offset — a real `LogRef` a void can target. */
async function firstLapEndRef(baseUrl: string, heat: string): Promise<number> {
  const competitors = await lapCompetitors(baseUrl, heat);
  const endRef = competitors[0]?.laps[0]?.end_ref;
  if (typeof endRef !== 'number') throw new Error('no lap end_ref to void');
  return endRef;
}

/** The heat's `?projection=laps` competitor list (empty on a non-2xx read). */
async function lapCompetitors(
  baseUrl: string,
  heat: string
): Promise<Array<{ laps: Array<{ end_ref: number }> }>> {
  const resp = await fetch(`${eventRoot(baseUrl)}/snapshot/heat/${heat}?projection=laps`);
  if (!resp.ok) return [];
  const snap = (await resp.json()) as {
    body: { LapList?: { competitors: Array<{ laps: Array<{ end_ref: number }> }> } };
  };
  return snap.body.LapList?.competitors ?? [];
}
