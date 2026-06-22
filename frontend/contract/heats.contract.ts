/**
 * Heats listing contract (race redesign Slice 3b): `GET /events/{id}/heats` + the real
 * `listHeats` client helper.
 *
 * Schedules a **round/class-tagged** heat over the real control path (`Command::ScheduleHeat`),
 * then reads it back through the real `@gridfpv/protocol-client`'s `listHeats`. Asserts the served
 * `HeatSummary` round-trips the tag (round + class), the lineup, and a derived status — the exact
 * shape the Heats UI groups by round. If the new endpoint, the binding, or the tag plumbing were
 * wrong, the heat would not come back tagged.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { listHeats, PRACTICE_EVENT_ID } from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { rdControl, startContractDirector } from './harness.ts';

const TOKEN = 'rd-heats-contract';
const HEAT = 'q-1';
const LINEUP = ['A', 'B'];
const ROUND = 'r1';
const CLASS = 'open';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({ token: TOKEN });
});

afterAll(async () => {
  await director?.stop();
});

describe('GET /heats serves the round-tagged scheduled heats', () => {
  it('listHeats returns a tagged HeatSummary with lineup and a derived status', async () => {
    // Schedule a heat tagged with a round + class over the real control path.
    const ack = await rdControl(director.baseUrl, TOKEN, {
      ScheduleHeat: { heat: HEAT, lineup: LINEUP, class: CLASS, round: ROUND }
    });
    expect(ack.ok).toBe(true);

    // Read the heats list back through the real client helper.
    const heats = await listHeats(director.baseUrl, PRACTICE_EVENT_ID);
    const summary = heats.find((h) => h.heat === HEAT);
    expect(summary).toBeDefined();
    expect(summary!.lineup).toEqual(LINEUP);
    expect(summary!.round).toBe(ROUND);
    expect(summary!.class).toBe(CLASS);
    // Freshly scheduled and on the timer: Scheduled phase, marked current.
    expect(summary!.phase).toBe('Scheduled');
    expect(summary!.is_current).toBe(true);
  });
});
