/**
 * Heats listing contract (race redesign Slice 3b): `GET /events/{id}/heats` + the real
 * `listHeats` client helper.
 *
 * Sets up a real class + roster + membership + round on the Practice event (ScheduleHeat now
 * validates its round/class tag + membership against the event meta, #335), schedules a
 * **round/class-tagged** heat over the real control path (`Command::ScheduleHeat`), then reads it
 * back through the real `@gridfpv/protocol-client`'s `listHeats`. Asserts the served `HeatSummary`
 * round-trips the tag (round + class), the lineup, and a derived status — the exact shape the
 * Heats UI groups by round. If the new endpoint, the binding, or the tag plumbing were wrong, the
 * heat would not come back tagged. Also pins the #335 rejections: a dangling round tag and a
 * non-member pilot on a tagged heat are failed acks (nothing scheduled).
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createClass,
  createPilot,
  createRound,
  listHeats,
  setClassMembership,
  setEventClasses,
  setEventRoster
} from '../packages/protocol-client/dist/index.js';
import { rdControl, startDirectorWithEvent, type ContractDirector } from './harness.ts';

const TOKEN = 'rd-heats-contract';
const HEAT = 'q-1';
const LABEL = 'Featured Heat';

let director: ContractDirector;

beforeAll(async () => {
  director = await startDirectorWithEvent({ token: TOKEN });
});

afterAll(async () => {
  await director?.stop();
});

describe('GET /heats serves the round-tagged scheduled heats', () => {
  it('listHeats returns a tagged HeatSummary with lineup and a derived status', async () => {
    // Real directory setup: a class + two pilots, selected + rostered + membered on Practice —
    // a tagged ScheduleHeat is validated against exactly this meta (#335).
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
    const lineup = [pilotA.id, pilotB.id];

    // Schedule a heat tagged with the round + class + a custom label over the real control path.
    const ack = await rdControl(director, TOKEN, {
      ScheduleHeat: { heat: HEAT, lineup, class: klass.id, round: round.id, label: LABEL }
    });
    expect(ack.ok).toBe(true);

    // Read the heats list back through the real client helper.
    const heats = await listHeats(director.baseUrl, director.event);
    const summary = heats.find((h) => h.heat === HEAT);
    expect(summary).toBeDefined();
    expect(summary!.lineup).toEqual(lineup);
    expect(summary!.round).toBe(round.id);
    expect(summary!.class).toBe(klass.id);
    // The custom build-heat label round-trips on the wire (overrides the derived name in the UI).
    expect(summary!.label).toBe(LABEL);
    // Freshly scheduled and on the timer: Scheduled phase, marked current.
    expect(summary!.phase).toBe('Scheduled');
    expect(summary!.is_current).toBe(true);

    // #335 rejections over the wire: a dangling round tag is a failed ack …
    const dangling = await rdControl(director, TOKEN, {
      ScheduleHeat: { heat: 'q-bad-round', lineup, round: 'no-such-round' }
    });
    expect(dangling.ok).toBe(false);

    // … and a directory pilot who is NOT a member of the round's classes is rejected too.
    const outsider = await createPilot(director.baseUrl, { callsign: 'outsider' }, TOKEN);
    const nonMember = await rdControl(director, TOKEN, {
      ScheduleHeat: { heat: 'q-bad-member', lineup: [outsider.id], round: round.id }
    });
    expect(nonMember.ok).toBe(false);

    // Neither rejected command scheduled anything.
    const after = await listHeats(director.baseUrl, director.event);
    expect(after.some((h) => h.heat === 'q-bad-round' || h.heat === 'q-bad-member')).toBe(false);
  });
});
