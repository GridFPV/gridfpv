/**
 * Event **channel layout** contract (#117 S2) — over the real wire, against a real Director.
 *
 * A layout is one complete tuning of the event's timer (`node → channel`, one channel per enabled
 * node) drawn from what the RD ticked for that timer globally. Three things are asserted here that
 * a mocked seam structurally cannot check:
 *
 *  - the response really is `bindings/ChannelLayouts.ts` — the expectation is **generated** from the
 *    ts-rs binding rather than hand-written, which is the #410 failure mode one level down;
 *  - the **seed** path: a create with no `nodes` comes back tuned from the timer's allowed set, and
 *    the timer record is **unchanged** afterwards (the whole point of layouts being event state);
 *  - the split between the one **error** (two nodes on one channel → a 400 naming both) and the one
 *    **warning** (channel reuse *between* layouts → a 200 carrying `overlaps`).
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import {
  createChannelLayout,
  createRound,
  createTimer,
  deleteChannelLayout,
  listChannelLayouts,
  listHeats,
  listRoundIssues,
  listTimers,
  setEventTimers,
  updateChannelLayout,
  updateTimer
} from '../packages/protocol-client/dist/index.js';
import {
  eventRoot,
  postControl,
  startDirectorWithEvent,
  type ContractDirector
} from './harness.ts';
import { wireShapeProblems } from './wire-shape.ts';

const TOKEN = 'rd-layouts-contract';

/** Raceband R1–R4 — a four-channel allowed set for a four-node timer (the bracket strategy). */
const ALLOWED = [5658, 5695, 5732, 5769];

let director: ContractDirector;

beforeAll(async () => {
  director = await startDirectorWithEvent({ token: TOKEN });
  // A four-node timer allowing exactly R1–R4, selected by the contract event. A layout tunes the
  // event's effective primary, so this is the timer every assertion below is about.
  const timer = await createTimer(
    director.baseUrl,
    {
      name: 'Layout Bench',
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

/** Remove every layout the event holds, so each test starts from a clean set. */
async function clearLayouts(): Promise<void> {
  const view = await listChannelLayouts(director.baseUrl, director.event, { token: TOKEN });
  for (const layout of view.layouts) {
    await deleteChannelLayout(director.baseUrl, director.event, layout.id, TOKEN);
  }
}

describe('#117 S2 — defining a layout', () => {
  it('serves exactly the generated ChannelLayouts shape', async () => {
    await clearLayouts();
    const view = await createChannelLayout(
      director.baseUrl,
      director.event,
      { name: 'Shape Check' },
      TOKEN
    );
    // Generated from bindings/ChannelLayouts.ts — never a second hand-written copy of the shape.
    expect(wireShapeProblems(view, 'ChannelLayouts')).toEqual([]);
  });

  it('seeds from the timer’s allowed set, and leaves the timer record alone', async () => {
    await clearLayouts();
    const before = (await listTimers(director.baseUrl)).find((t) => t.name === 'Layout Bench');

    const view = await createChannelLayout(
      director.baseUrl,
      director.event,
      { name: 'Bracket A' },
      TOKEN
    );
    expect(view.layouts).toHaveLength(1);
    // Enabled node `i` takes allowed channel `i`, in the RD's own preference order.
    expect(view.layouts[0].nodes).toEqual([
      { node: 0, channel: 5658 },
      { node: 1, channel: 5695 },
      { node: 2, channel: 5732 },
      { node: 3, channel: 5769 }
    ]);

    // The global record is the SEED, not the storage: defining a layout never edits a timer. This is
    // the bug the slice exists to close — the event workspace's channel checkboxes edit the global
    // `available_channels`, and a layout must not.
    const after = (await listTimers(director.baseUrl)).find((t) => t.name === 'Layout Bench');
    expect(after?.available_channels).toEqual(before?.available_channels);
    expect(after?.available_channels).toEqual(ALLOWED);
  });

  it('persists: the layout reads back on a fresh GET', async () => {
    await clearLayouts();
    await createChannelLayout(director.baseUrl, director.event, { name: 'Bracket A' }, TOKEN);
    const read = await listChannelLayouts(director.baseUrl, director.event, { token: TOKEN });
    expect(read.layouts.map((l) => l.name)).toEqual(['Bracket A']);
  });
});

describe('#117 S2 — the one hard rule inside a layout', () => {
  it('refuses two nodes on one channel, naming both nodes and the channel', async () => {
    await clearLayouts();
    await expect(
      createChannelLayout(
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
    await clearLayouts();
    await expect(
      createChannelLayout(
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
    await clearLayouts();
    await expect(
      createChannelLayout(
        director.baseUrl,
        director.event,
        { name: 'Phantom', nodes: [{ node: 9, channel: 5658 }] },
        TOKEN
      )
    ).rejects.toThrow(/Node 10 is not available/);
  });

  it('refuses an incomplete tuning — a layout tunes every enabled node', async () => {
    await clearLayouts();
    await expect(
      createChannelLayout(
        director.baseUrl,
        director.event,
        { name: 'Half', nodes: [{ node: 0, channel: 5658 }] },
        TOKEN
      )
    ).rejects.toThrow(/does not tune Node 2/);
  });
});

describe('#117 S2 — cross-layout overlap is a warning, not a rule', () => {
  it('accepts a second layout sharing channels, and reports the overlap on the 200', async () => {
    await clearLayouts();
    await createChannelLayout(director.baseUrl, director.event, { name: 'Bracket A' }, TOKEN);
    // The identical seeded tuning under a second name — the maximal overlap.
    const view = await createChannelLayout(
      director.baseUrl,
      director.event,
      { name: 'Bracket B' },
      TOKEN
    );

    // Accepted. An RD running a bracket off one layout does not care about reuse, so it never blocks.
    expect(view.layouts.map((l) => l.name)).toEqual(['Bracket A', 'Bracket B']);
    expect(view.overlaps).toHaveLength(1);
    expect(view.overlaps[0].layout).toBe(view.layouts[0].id);
    expect(view.overlaps[0].other).toBe(view.layouts[1].id);
    expect(view.overlaps[0].channels).toEqual(ALLOWED);
    // And the read agrees with the write — one computation of the rule, not two.
    const read = await listChannelLayouts(director.baseUrl, director.event, { token: TOKEN });
    expect(read).toEqual(view);
  });

  it('raises nothing for disjoint layouts', async () => {
    await clearLayouts();
    await createChannelLayout(
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
    const view = await listChannelLayouts(director.baseUrl, director.event, { token: TOKEN });
    expect(view.overlaps ?? []).toEqual([]);
  });
});

describe('#117 S2 — editing and removing', () => {
  it('replaces a layout wholesale, keeping its id', async () => {
    await clearLayouts();
    const created = await createChannelLayout(
      director.baseUrl,
      director.event,
      { name: 'Bracket A' },
      TOKEN
    );
    const id = created.layouts[0].id;
    const view = await updateChannelLayout(
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
    expect(view.layouts[0].id).toBe(id);
    expect(view.layouts[0].name).toBe('Mains');
    expect(view.layouts[0].nodes[0]).toEqual({ node: 0, channel: 5769 });
  });

  it('404s an unknown layout rather than succeeding silently', async () => {
    await clearLayouts();
    const resp = await fetch(
      `${eventRoot(director.baseUrl, director.event)}/layouts/never-existed`,
      {
        method: 'DELETE',
        headers: { Authorization: `Bearer ${TOKEN}` }
      }
    );
    expect(resp.status).toBe(404);
  });

  it('needs an RD token to write; the read is open', async () => {
    const open = await fetch(`${eventRoot(director.baseUrl, director.event)}/layouts`);
    expect(open.status).toBe(200);

    const tokenless = await fetch(`${eventRoot(director.baseUrl, director.event)}/layouts`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: 'Sneaky' })
    });
    expect(tokenless.status).toBe(401);
  });
});

// ── #117 S3: a round names layouts, a heat flies one ─────────────────────────────────────────
//
// The chain the RD asked for, over the real wire: *"I have no way to apply a layout to a
// round/heat currently, meaning I can not manually choose the channels for a round or heat at
// all."* Everything below is that gap closing.

describe('#117 S3 — a round names layouts, a heat flies one', () => {
  it('fills a heat on the channels the round’s layout puts each node on', async () => {
    await clearLayouts();
    const view = await createChannelLayout(
      director.baseUrl,
      director.event,
      {
        name: 'Bracket A',
        nodes: [
          { node: 0, channel: 5769 },
          { node: 1, channel: 5732 },
          { node: 2, channel: 5695 },
          { node: 3, channel: 5658 }
        ]
      },
      TOKEN
    );
    const layout = view.layouts[0].id;

    // An open-practice round over three seats, flying that layout. Practice is the sharpest case:
    // its seats NAME their nodes, and before S3 its heat carried empty frequencies by construction
    // — the whole of #402.
    const round = await createRound(
      director.baseUrl,
      director.event,
      {
        label: 'Practice',
        classes: [],
        format: 'open_practice',
        seeding: { AllChannels: { channels: [0, 1, 2] } },
        layouts: [layout]
      },
      TOKEN
    );
    expect(round.layouts).toEqual([layout]);

    const filled = await postControl(
      director,
      { FillRound: { round: round.id } },
      { token: TOKEN }
    );
    expect(filled.status).toBe(200);

    const heats = await listHeats(director.baseUrl, director.event);
    const heat = heats.find((h) => h.round === round.id);
    expect(heat).toBeDefined();
    // #402 closes here: every practice seat is on the channel its layout puts that node on, and the
    // heat records WHICH layout it flew.
    expect(heat?.frequencies).toEqual([
      ['node-0', 5769],
      ['node-1', 5732],
      ['node-2', 5695]
    ]);
    expect(heat?.layout).toBe(layout);
  });

  it('re-tunes a scheduled heat when its layout is edited, without rebuilding it', async () => {
    const heats = await listHeats(director.baseUrl, director.event);
    const before = heats.find((h) => h.round?.startsWith('practice'));
    expect(before).toBeDefined();
    const layout = before!.layout!;

    await updateChannelLayout(
      director.baseUrl,
      director.event,
      layout,
      {
        name: 'Bracket A',
        nodes: [
          { node: 0, channel: 5658 },
          { node: 1, channel: 5695 },
          { node: 2, channel: 5732 },
          { node: 3, channel: 5769 }
        ]
      },
      TOKEN
    );

    const after = (await listHeats(director.baseUrl, director.event)).find(
      (h) => h.heat === before!.heat
    );
    // Same heat, same id, new channels: the RD fixes a layout without deleting and rebuilding.
    expect(after?.heat).toBe(before!.heat);
    expect(after?.frequencies).toEqual([
      ['node-0', 5658],
      ['node-1', 5695],
      ['node-2', 5732]
    ]);
  });

  it('keeps a manual seating override across a re-fill', async () => {
    const heats = await listHeats(director.baseUrl, director.event);
    const heat = heats.find((h) => h.layout !== undefined)!;

    const set = await postControl(
      director,
      { OverrideHeatSeating: { heat: heat.heat, lineup: ['node-2', 'node-3'] } },
      { token: TOKEN }
    );
    expect(set.status).toBe(200);

    const seated = (await listHeats(director.baseUrl, director.event)).find(
      (h) => h.heat === heat.heat
    );
    expect(seated?.lineup).toEqual(['node-2', 'node-3']);
    // The RD's pilots, the layout's channels — an override that sets only the lineup does not mean
    // retyping every frequency.
    expect(seated?.frequencies).toEqual([
      ['node-2', 5732],
      ['node-3', 5769]
    ]);

    // Re-fill the round. The override is STICKY: losing it here is the failure #419 called worse
    // than having no override at all.
    await postControl(director, { FillRound: { round: heat.round! } }, { token: TOKEN });
    const after = (await listHeats(director.baseUrl, director.event)).find(
      (h) => h.heat === heat.heat
    );
    expect(after?.lineup).toEqual(['node-2', 'node-3']);
  });

  it('refuses a layout the round does not fly, by name', async () => {
    const other = await createChannelLayout(
      director.baseUrl,
      director.event,
      { name: 'Whoop pack' },
      TOKEN
    );
    const whoops = other.layouts.find((l) => l.name === 'Whoop pack')!.id;
    const heat = (await listHeats(director.baseUrl, director.event)).find(
      (h) => h.layout !== undefined
    )!;

    const refused = await postControl(
      director,
      { SetHeatLayout: { heat: heat.heat, layout: whoops } },
      { token: TOKEN }
    );
    const message = JSON.stringify(refused.body);
    expect(message).toContain('Whoop pack');
    expect(message).toContain('Practice');
    // Friendly names only — never the raw handles (CLAUDE.md).
    expect(message).not.toContain(whoops);
    expect(message).not.toContain(heat.heat);
  });

  it('reports a stale layout on round-issues rather than at arm time', async () => {
    // Untick a channel the layout uses AFTER it was written. #412 and #416 both landed on
    // "validate stored config on read", and this is that same read.
    const timer = (await listTimers(director.baseUrl)).find((t) => t.name === 'Layout Bench')!;
    await updateTimer(
      director.baseUrl,
      timer.id,
      { available_channels: [5658, 5695, 5732] },
      TOKEN
    );

    const issues = await listRoundIssues(director.baseUrl, director.event);
    const stale = issues.filter((i) => i.problem === 'LayoutChannelNotAllowed');
    expect(stale.length).toBeGreaterThan(0);
    expect(stale[0].layout_name).toBe('Bracket A');
    expect(stale[0].node_label).toBe('Node 4');
    // Every noun a person reads is a friendly name — and the channel is a band+channel label, never
    // a bare 5769.
    expect(stale[0].detail).toContain('Raceband R4');
    expect(stale[0].detail).not.toContain('5769');

    await updateTimer(director.baseUrl, timer.id, { available_channels: ALLOWED }, TOKEN);
  });
});
