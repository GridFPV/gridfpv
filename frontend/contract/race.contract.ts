/**
 * Seam 8: the full sim race — the end-to-end client↔server contract in one test.
 *
 * guards: the whole integration (every v0.4 bug lived at a mocked boundary). This drives a
 * real race through the real control path (register → schedule → stage/arm/start → finish →
 * finalize) while the **real `@gridfpv/protocol-client`** holds a `/stream` subscription, and
 * asserts the client's *exposed converged state* tracks the race: current heat appears,
 * per-pilot laps climb, the phase walks Scheduled → Running → Unofficial → Final. If any seam
 * (StreamMessage unwrap, cursor/sequence axis, bigint numbers, control headers) were wrong,
 * the client would not converge.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { connect } from '../packages/protocol-client/dist/index.js';
import {
  driveToRunning,
  rdControl,
  startDirectorWithEvent,
  type ContractDirector
} from './harness.ts';

const TOKEN = 'rd-race-contract';
const HEAT = 'final';
const LINEUP = ['A', 'B'];

let director: ContractDirector;

beforeAll(async () => {
  // A brisk sim (small lap time) so the race completes well inside the test timeout.
  director = await startDirectorWithEvent({ token: TOKEN, simLaps: 3, simLapMs: 40 });
});

afterAll(async () => {
  await director?.stop();
});

interface LiveBody {
  LiveRaceState?: {
    current_heat?: string;
    phase: string;
    progress?: Array<{ competitor: string; laps_completed: number; last_lap_micros?: number }>;
  };
}

describe('seam 8: a full race converges through the real protocol-client', () => {
  it('current_heat appears, laps climb, and the result is reached', async () => {
    // Bind pilots, then schedule the heat (over the real control path).
    await rdControl(director, TOKEN, {
      Register: { adapter: 'sim', competitor: 'A', pilot: 'ace' }
    });
    await rdControl(director, TOKEN, {
      Register: { adapter: 'sim', competitor: 'B', pilot: 'bee' }
    });
    await rdControl(director, TOKEN, { ScheduleHeat: { heat: HEAT, lineup: LINEUP } });

    // The real client subscribes to the heat scope.
    const client = connect({
      baseUrl: director.baseUrl,
      eventId: director.event,
      scope: { Heat: { heat: HEAT } }
    });
    try {
      await waitForState(client, (s) => s.body !== undefined);
      // current_heat is set once the heat is scheduled and folded.
      const initial = (client.getState().body as LiveBody).LiveRaceState;
      expect(initial?.current_heat).toBe(HEAT);

      // Drive the heat to Running; the sim emits laps in real time.
      await driveToRunning(director, TOKEN, HEAT);

      // The client's exposed progress must climb to ≥ 2 laps for at least one pilot.
      await waitForState(
        client,
        (s) => {
          const ls = (s.body as LiveBody).LiveRaceState;
          return ls?.phase === 'Running' && (ls.progress ?? []).some((p) => p.laps_completed >= 2);
        },
        12_000
      );
      const running = (client.getState().body as LiveBody).LiveRaceState!;
      expect(running.phase).toBe('Running');
      const laps = (running.progress ?? []).map((p) => p.laps_completed);
      expect(Math.max(...laps)).toBeGreaterThanOrEqual(2);
      // The last-lap timing is a plain number wherever a lap has been banked.
      for (const p of running.progress ?? []) {
        if (p.laps_completed >= 1) expect(typeof p.last_lap_micros).toBe('number');
      }

      // ForceEnd (the completion override) + Finalize; the client converges to the finalized phase.
      await rdControl(director, TOKEN, { ForceEnd: { heat: HEAT } });
      await rdControl(director, TOKEN, { Finalize: { heat: HEAT } });
      await waitForState(
        client,
        (s) => (s.body as LiveBody).LiveRaceState?.phase === 'Final',
        6_000
      );
      expect((client.getState().body as LiveBody).LiveRaceState?.phase).toBe('Final');

      // Revert re-opens the finalized result for correction (Final → Unofficial), then
      // Finalize again locks it back in — the new Slice-1 off-ramp, end to end.
      await rdControl(director, TOKEN, { Revert: { heat: HEAT } });
      await waitForState(
        client,
        (s) => (s.body as LiveBody).LiveRaceState?.phase === 'Unofficial',
        6_000
      );
      expect((client.getState().body as LiveBody).LiveRaceState?.phase).toBe('Unofficial');
      await rdControl(director, TOKEN, { Finalize: { heat: HEAT } });
      await waitForState(
        client,
        (s) => (s.body as LiveBody).LiveRaceState?.phase === 'Final',
        6_000
      );
      expect((client.getState().body as LiveBody).LiveRaceState?.phase).toBe('Final');

      // And the scored HeatResult is served on the read path (a result exists).
      const result = (await (
        await fetch(`${director.eventRoot}/snapshot/heat/${HEAT}?projection=result`)
      ).json()) as { body: { HeatResult: { places: unknown[] } } };
      expect(result.body.HeatResult.places.length).toBe(LINEUP.length);
    } finally {
      client.close();
    }
  });
});

/** Resolve once the client's exposed state satisfies `pred` (or reject on timeout). */
function waitForState(
  client: ReturnType<typeof connect>,
  pred: (state: ReturnType<typeof client.getState>) => boolean,
  timeoutMs = 5_000
): Promise<void> {
  return new Promise((res, rej) => {
    let unsub: () => void = () => {};
    const to = setTimeout(() => {
      unsub();
      rej(
        new Error(
          `client state never satisfied predicate within ${timeoutMs}ms; last=${JSON.stringify(
            client.getState().body
          )}`
        )
      );
    }, timeoutMs);
    unsub = client.onState((s) => {
      if (pred(s)) {
        clearTimeout(to);
        unsub();
        res();
      }
    });
  });
}
