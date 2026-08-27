/**
 * Channel catalog contract (race redesign Slice 4b): `GET /channels` + the real `listChannels`
 * client helper.
 *
 * Reads the standard FPV channel catalog back through the real `@gridfpv/protocol-client`'s
 * `listChannels` (an open read, no token), and asserts the served `ChannelCatalogEntry[]` is the
 * band/channel ↔ raw-MHz vocabulary the Channels UI offers and labels heats with. If the new
 * route, the binding, or the `catalog()` plumbing were wrong, the catalog would not come back.
 */
import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { listChannels, rateChannels } from '../packages/protocol-client/dist/index.js';
import { type Director } from '../test-harness/director.ts';
import { startContractDirector } from './harness.ts';

let director: Director;

beforeAll(async () => {
  director = await startContractDirector({});
});

afterAll(async () => {
  await director?.stop();
});

describe('GET /channels serves the standard FPV channel catalog', () => {
  it('listChannels returns the band/channel/MHz catalog, open (no token)', async () => {
    const catalog = await listChannels(director.baseUrl);
    expect(catalog.length).toBeGreaterThan(0);

    // Every entry is a band + channel label + a plausible 5.8 GHz centre frequency.
    for (const entry of catalog) {
      expect(typeof entry.band).toBe('string');
      expect(entry.band.length).toBeGreaterThan(0);
      expect(typeof entry.channel).toBe('string');
      expect(entry.channel.length).toBeGreaterThan(0);
      expect(entry.mhz).toBeGreaterThanOrEqual(5600);
      expect(entry.mhz).toBeLessThanOrEqual(6000);
    }

    // The de-facto Raceband default is present in channel order (R1 = 5658, R8 = 5917) — the UI
    // resolves a heat's raw 5658 MHz back to this "Raceband R1" label.
    const raceband = catalog.filter((e) => e.band === 'Raceband');
    expect(raceband.map((e) => e.channel)).toEqual([
      'R1',
      'R2',
      'R3',
      'R4',
      'R5',
      'R6',
      'R7',
      'R8'
    ]);
    expect(raceband[0].mhz).toBe(5658);
    expect(raceband[7].mhz).toBe(5917);
  });
});

describe('GET /channels/imd serves IMDTabler’s own rating (#117 S4)', () => {
  it('rateChannels answers with the number an RD reads off RotorHazard, open (no token)', async () => {
    // RotorHazard’s default IMD6C profile. This exact integer is the point of #430: the
    // Director’s port must reproduce IMDTabler, or the console is showing the RD a second
    // opinion dressed up as the standard.
    const imd6c = await rateChannels(director.baseUrl, [5658, 5695, 5760, 5800, 5880, 5917]);
    expect(imd6c.rating).toBe(29);
    // ...and it names the worst offender, whose arithmetic must actually hold.
    const worst = imd6c.worst;
    expect(worst).toBeDefined();
    if (!worst) return;
    expect(2 * worst.doubled - worst.subtracted).toBe(worst.product);
    expect(Math.abs(worst.product - worst.lands_on)).toBe(worst.gap_mhz);
    expect(worst.gap_mhz).toBeLessThan(35);

    // The canonical table, end to end over the wire.
    expect((await rateChannels(director.baseUrl, [5658, 5732, 5843, 5917])).rating).toBe(100);
    expect(
      (await rateChannels(director.baseUrl, [5645, 5685, 5760, 5805, 5905, 5945])).rating
    ).toBe(67);
    expect(
      (await rateChannels(director.baseUrl, [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917]))
        .rating
    ).toBe(-635);
  });

  it('a clean set has no offender to name', async () => {
    // Racebnd4 rates the ceiling because nothing lands within 35 MHz. Reporting a nearest miss
    // here would tell the RD a clean set has a problem.
    const clean = await rateChannels(director.baseUrl, [5658, 5732, 5843, 5917]);
    expect(clean.rating).toBe(100);
    expect(clean.worst).toBeUndefined();
  });
});
