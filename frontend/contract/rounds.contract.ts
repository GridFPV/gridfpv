/**
 * Round lifecycle contract (#418, #416) — over the real wire, against a real Director.
 *
 * Two bench failures, one screen apart:
 *
 * - **#418** — a practice round whose only heat was never armed could not be deleted, and the
 *   refusal advised *"discard its heats and re-use it"* through a route that has never existed.
 *   Asserted here: an all-`Scheduled` round deletes, **and its heats go with it** (they stop being
 *   served by `GET /events/{id}/heats`); a round with a heat in progress is still refused, names
 *   the heat, and recommends nothing impossible.
 * - **#416** — a stored round seating `node-6` on a four-node timer silently recorded nothing.
 *   Asserted here: `GET /events/{id}/round-issues` reports it, by friendly name.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createRound,
  createTimer,
  deleteRound,
  listHeats,
  listRoundIssues,
  setEventTimers
} from '../packages/protocol-client/dist/index.js';
import { rdControl, startDirectorWithEvent, type ContractDirector } from './harness.ts';

const TOKEN = 'rd-rounds-contract';

let director: ContractDirector;

beforeAll(async () => {
  director = await startDirectorWithEvent({ token: TOKEN });
});

afterAll(async () => {
  await director?.stop();
});

/** An open-practice round over the given node indices. */
function practiceRound(label: string, nodes: number[]) {
  return {
    label,
    classes: [],
    format: 'open_practice',
    params: {},
    seeding: { ActiveNodes: { nodes } }
  };
}

describe('#418 — a round with only unstarted heats deletes, heats included', () => {
  it('deletes the round and stops serving its heats', async () => {
    const round = await createRound(
      director.baseUrl,
      director.event,
      practiceRound('Throwaway', [0]),
      TOKEN
    );

    // Fill it: one heat, scheduled and never touched again.
    const ack = await rdControl(director, TOKEN, {
      FillRound: { round: round.id, mode: 'Next' }
    });
    expect(ack.ok).toBe(true);
    const filled = await listHeats(director.baseUrl, director.event);
    const heat = filled.find((h) => h.round === round.id);
    expect(heat, 'the round has a heat to strand').toBeDefined();
    expect(heat!.phase).toBe('Scheduled');

    // It deletes. The old gate refused on the mere EXISTENCE of a heat, which made a round
    // permanently undeletable the moment it was filled.
    const meta = await deleteRound(director.baseUrl, director.event, round.id, TOKEN);
    expect((meta.rounds ?? []).some((r) => r.id === round.id)).toBe(false);

    // And its heats went with it: the log still carries the `HeatScheduled` (it is append-only),
    // but a heat whose round the event no longer defines is not served — it has no name, no win
    // condition and no scoring left to resolve through.
    const after = await listHeats(director.baseUrl, director.event);
    expect(after.some((h) => h.heat === heat!.heat)).toBe(false);
  });

  it('still refuses a round with a heat IN PROGRESS, names it, and advises nothing impossible', async () => {
    const round = await createRound(
      director.baseUrl,
      director.event,
      practiceRound('Live Practice', [0]),
      TOKEN
    );
    const filled = await rdControl(director, TOKEN, {
      FillRound: { round: round.id, mode: 'Next' }
    });
    expect(filled.ok).toBe(true);
    const heats = await listHeats(director.baseUrl, director.event);
    const heat = heats.find((h) => h.round === round.id)!;

    // Stage it — the heat is now on the timer with its channels read off.
    const selected = await rdControl(director, TOKEN, {
      SetCurrentHeat: { heat: heat.heat }
    });
    expect(selected.ok).toBe(true);
    const stage = await rdControl(director, TOKEN, { Stage: { heat: heat.heat } });
    expect(stage.ok).toBe(true);

    await expect(deleteRound(director.baseUrl, director.event, round.id, TOKEN)).rejects.toThrow();

    // The refusal itself: names the blocking heat by its friendly name, and recommends no route.
    const resp = await fetch(`${director.baseUrl}/events/${director.event}/rounds/${round.id}`, {
      method: 'DELETE',
      headers: { Authorization: `Bearer ${TOKEN}` }
    });
    expect(resp.status).toBe(400);
    const body = (await resp.json()) as { message: string };
    expect(body.message).toContain('in progress');
    expect(body.message).toContain('Practice Heat');
    // "discard its heats and re-use it" pointed at a route that does not exist.
    expect(body.message).not.toContain('discard');
    // A raw heat id must never reach a user (repo display rule).
    expect(body.message).not.toContain(heat.heat);

    // Clean up so the staged heat does not bleed into the next test.
    await rdControl(director, TOKEN, { Abort: { heat: heat.heat } });
  });
});

describe('#416 — a stored round seating a node the timer does not have is reported on READ', () => {
  it('names the round, the timer and the 1-based node — never a raw ref', async () => {
    // A four-node timer, selected (and so primary) on the event. A Mock stands in for the bench's
    // RotorHazard: an RH timer is only selectable once its GridFPV plugin has been probed (#405),
    // and the node arithmetic under test is the timer's width, which is the same either way.
    const timer = await createTimer(
      director.baseUrl,
      { name: 'Bench Timer', kind: { Mock: { laps: 3, lap_ms: 1000 } }, node_count: 4 },
      TOKEN
    );
    await setEventTimers(director.baseUrl, director.event, [timer!.id], TOKEN);

    // #412 refuses this at WRITE — which is the point: the round on the bench predates that fix,
    // so the write path is exactly where it cannot be caught.
    await expect(
      createRound(director.baseUrl, director.event, practiceRound('Impossible', [6]), TOKEN)
    ).rejects.toThrow();

    // A round authored while the event had no timer is the stored shape that slipped through.
    await setEventTimers(director.baseUrl, director.event, [], TOKEN);
    const round = await createRound(
      director.baseUrl,
      director.event,
      practiceRound('Impossible', [6]),
      TOKEN
    );
    expect(await listRoundIssues(director.baseUrl, director.event)).toEqual([]);

    // Now the timer is back in the picture, and the seat becomes checkable — and impossible.
    await setEventTimers(director.baseUrl, director.event, [timer!.id], TOKEN);
    const issues = await listRoundIssues(director.baseUrl, director.event);
    const issue = issues.find((i) => i.round === round.id);
    expect(issue, 'the impossible seat is reported').toBeDefined();
    expect(issue!.problem).toBe('NoSuchNode');
    expect(issue!.node).toBe(6);
    expect(issue!.node_label).toBe('Node 7');
    expect(issue!.round_label).toBe('Impossible');
    expect(issue!.timer_name).toBe('Bench Timer');
    expect(issue!.detail).toContain('Node 7');
    expect(issue!.detail).toContain('Bench Timer');
    expect(issue!.detail).not.toContain('node-6');

    await deleteRound(director.baseUrl, director.event, round.id, TOKEN);
  });
});
