/**
 * Event **channel layer** contract (#117 S2) — over the real wire, against a real Director.
 *
 * A layer is one complete tuning of the event's timer (`node → channel`, one channel per enabled
 * node) drawn from what the RD ticked for that timer globally. Three things are asserted here that
 * a mocked seam structurally cannot check:
 *
 *  - the response really is `bindings/ChannelLayers.ts` — the expectation is **generated** from the
 *    ts-rs binding rather than hand-written, which is the #410 failure mode one level down;
 *  - the **seed** path: a create with no `nodes` comes back tuned from the timer's allowed set, and
 *    the timer record is **unchanged** afterwards (the whole point of layers being event state);
 *  - the split between the one **error** (two nodes on one channel → a 400 naming both) and the one
 *    **warning** (channel reuse *between* layers → a 200 carrying `overlaps`).
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createChannelLayer,
  createTimer,
  deleteChannelLayer,
  listChannelLayers,
  listTimers,
  setEventTimers,
  updateChannelLayer
} from '../packages/protocol-client/dist/index.js';
import { eventRoot, startDirectorWithEvent, type ContractDirector } from './harness.ts';
import { wireShapeProblems } from './wire-shape.ts';

const TOKEN = 'rd-layers-contract';

/** Raceband R1–R4 — a four-channel allowed set for a four-node timer (the bracket strategy). */
const ALLOWED = [5658, 5695, 5732, 5769];

let director: ContractDirector;

beforeAll(async () => {
  director = await startDirectorWithEvent({ token: TOKEN });
  // A four-node timer allowing exactly R1–R4, selected by the contract event. A layer tunes the
  // event's effective primary, so this is the timer every assertion below is about.
  const timer = await createTimer(
    director.baseUrl,
    {
      name: 'Layer Bench',
      kind: { Mock: { laps: 3, lap_ms: 30000 } },
      node_count: 4,
      available_channels: ALLOWED
    },
    TOKEN
  );
  await setEventTimers(director.baseUrl, director.event, [timer.id], TOKEN);
});

afterAll(async () => {
  await director?.stop();
});

/** Remove every layer the event holds, so each test starts from a clean set. */
async function clearLayers(): Promise<void> {
  const view = await listChannelLayers(director.baseUrl, director.event, { token: TOKEN });
  for (const layer of view.layers) {
    await deleteChannelLayer(director.baseUrl, director.event, layer.id, TOKEN);
  }
}

describe('#117 S2 — defining a layer', () => {
  it('serves exactly the generated ChannelLayers shape', async () => {
    await clearLayers();
    const view = await createChannelLayer(
      director.baseUrl,
      director.event,
      { name: 'Shape Check' },
      TOKEN
    );
    // Generated from bindings/ChannelLayers.ts — never a second hand-written copy of the shape.
    expect(wireShapeProblems(view, 'ChannelLayers')).toEqual([]);
  });

  it('seeds from the timer’s allowed set, and leaves the timer record alone', async () => {
    await clearLayers();
    const before = (await listTimers(director.baseUrl)).find((t) => t.name === 'Layer Bench');

    const view = await createChannelLayer(
      director.baseUrl,
      director.event,
      { name: 'Bracket A' },
      TOKEN
    );
    expect(view.layers).toHaveLength(1);
    // Enabled node `i` takes allowed channel `i`, in the RD's own preference order.
    expect(view.layers[0].nodes).toEqual([
      { node: 0, channel: 5658 },
      { node: 1, channel: 5695 },
      { node: 2, channel: 5732 },
      { node: 3, channel: 5769 }
    ]);

    // The global record is the SEED, not the storage: defining a layer never edits a timer. This is
    // the bug the slice exists to close — the event workspace's channel checkboxes edit the global
    // `available_channels`, and a layer must not.
    const after = (await listTimers(director.baseUrl)).find((t) => t.name === 'Layer Bench');
    expect(after?.available_channels).toEqual(before?.available_channels);
    expect(after?.available_channels).toEqual(ALLOWED);
  });

  it('persists: the layer reads back on a fresh GET', async () => {
    await clearLayers();
    await createChannelLayer(director.baseUrl, director.event, { name: 'Bracket A' }, TOKEN);
    const read = await listChannelLayers(director.baseUrl, director.event, { token: TOKEN });
    expect(read.layers.map((l) => l.name)).toEqual(['Bracket A']);
  });
});

describe('#117 S2 — the one hard rule inside a layer', () => {
  it('refuses two nodes on one channel, naming both nodes and the channel', async () => {
    await clearLayers();
    await expect(
      createChannelLayer(
        director.baseUrl,
        director.event,
        {
          name: 'Clash',
          nodes: [
            { node: 0, channel: 5658 },
            { node: 1, channel: 5658 },
            { node: 2, channel: 5732 },
            { node: 3, channel: 5769 }
          ]
        },
        TOKEN
      )
      // Friendly names, never an index or a bare MHz — the message is read by an RD at a venue.
    ).rejects.toThrow(/Node 1 and Node 2 are both on Raceband R1/);
  });

  it('refuses a channel the timer is not allowed to use', async () => {
    await clearLayers();
    await expect(
      createChannelLayer(
        director.baseUrl,
        director.event,
        {
          name: 'Off-pool',
          nodes: [
            { node: 0, channel: 5658 },
            { node: 1, channel: 5695 },
            { node: 2, channel: 5732 },
            // Fatshark F4 — a real catalog channel, but not one the RD ticked for this timer.
            { node: 3, channel: 5800 }
          ]
        },
        TOKEN
      )
    ).rejects.toThrow(/Fatshark F4 is not one of the channels/);
  });

  it('refuses a node the timer does not have', async () => {
    await clearLayers();
    await expect(
      createChannelLayer(
        director.baseUrl,
        director.event,
        { name: 'Phantom', nodes: [{ node: 9, channel: 5658 }] },
        TOKEN
      )
    ).rejects.toThrow(/Node 10 is not available/);
  });

  it('refuses an incomplete tuning — a layer tunes every enabled node', async () => {
    await clearLayers();
    await expect(
      createChannelLayer(
        director.baseUrl,
        director.event,
        { name: 'Half', nodes: [{ node: 0, channel: 5658 }] },
        TOKEN
      )
    ).rejects.toThrow(/does not tune Node 2/);
  });
});

describe('#117 S2 — cross-layer overlap is a warning, not a rule', () => {
  it('accepts a second layer sharing channels, and reports the overlap on the 200', async () => {
    await clearLayers();
    await createChannelLayer(director.baseUrl, director.event, { name: 'Bracket A' }, TOKEN);
    // The identical seeded tuning under a second name — the maximal overlap.
    const view = await createChannelLayer(
      director.baseUrl,
      director.event,
      { name: 'Bracket B' },
      TOKEN
    );

    // Accepted. An RD running a bracket off one layer does not care about reuse, so it never blocks.
    expect(view.layers.map((l) => l.name)).toEqual(['Bracket A', 'Bracket B']);
    expect(view.overlaps).toHaveLength(1);
    expect(view.overlaps[0].layer).toBe(view.layers[0].id);
    expect(view.overlaps[0].other).toBe(view.layers[1].id);
    expect(view.overlaps[0].channels).toEqual(ALLOWED);
    // And the read agrees with the write — one computation of the rule, not two.
    const read = await listChannelLayers(director.baseUrl, director.event, { token: TOKEN });
    expect(read).toEqual(view);
  });

  it('raises nothing for disjoint layers', async () => {
    await clearLayers();
    await createChannelLayer(
      director.baseUrl,
      director.event,
      {
        name: 'Pack A',
        nodes: [
          { node: 0, channel: 5658 },
          { node: 1, channel: 5695 },
          { node: 2, channel: 5732 },
          { node: 3, channel: 5769 }
        ]
      },
      TOKEN
    );
    const view = await listChannelLayers(director.baseUrl, director.event, { token: TOKEN });
    expect(view.overlaps ?? []).toEqual([]);
  });
});

describe('#117 S2 — editing and removing', () => {
  it('replaces a layer wholesale, keeping its id', async () => {
    await clearLayers();
    const created = await createChannelLayer(
      director.baseUrl,
      director.event,
      { name: 'Bracket A' },
      TOKEN
    );
    const id = created.layers[0].id;
    const view = await updateChannelLayer(
      director.baseUrl,
      director.event,
      id,
      {
        name: 'Mains',
        nodes: [
          { node: 0, channel: 5769 },
          { node: 1, channel: 5732 },
          { node: 2, channel: 5695 },
          { node: 3, channel: 5658 }
        ]
      },
      TOKEN
    );
    expect(view.layers[0].id).toBe(id);
    expect(view.layers[0].name).toBe('Mains');
    expect(view.layers[0].nodes[0]).toEqual({ node: 0, channel: 5769 });
  });

  it('404s an unknown layer rather than succeeding silently', async () => {
    await clearLayers();
    const resp = await fetch(
      `${eventRoot(director.baseUrl, director.event)}/layers/never-existed`,
      {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${TOKEN}` }
      }
    );
    expect(resp.status).toBe(404);
  });

  it('needs an RD token to write; the read is open', async () => {
    const open = await fetch(`${eventRoot(director.baseUrl, director.event)}/layers`);
    expect(open.status).toBe(200);

    const tokenless = await fetch(`${eventRoot(director.baseUrl, director.event)}/layers`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: 'Sneaky' })
    });
    expect(tokenless.status).toBe(401);
  });
});
