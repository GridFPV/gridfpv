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

import { listChannels } from '../packages/protocol-client/dist/index.js';
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
