/**
 * Channel helper unit tests (race redesign Slice 4b): the catalog grouping/labelling the channel
 * picker and the per-heat channel display rely on.
 */
import { describe, expect, it } from 'vitest';
import type { ChannelCapability, ChannelCatalogEntry } from '@gridfpv/types';
import {
  assignChannelsRoundRobin,
  capabilityTag,
  catalogEntryFor,
  channelLabel,
  fixedAllowed,
  groupByBand,
  isCatalogChannel,
  isPlausibleMhz,
  nodeIndexOf,
  nodeSeatLabel,
  offeredCatalog,
  poolChannel
} from '../src/lib/channels.js';

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Raceband', channel: 'R2', mhz: 5695 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 },
  { band: 'DJI', channel: 'R1', mhz: 5660 }
];

describe('groupByBand', () => {
  it('groups entries into bands, preserving catalog order across and within bands', () => {
    const bands = groupByBand(CATALOG);
    expect(bands.map((b) => b.band)).toEqual(['Raceband', 'Fatshark', 'DJI']);
    expect(bands[0].entries.map((e) => e.channel)).toEqual(['R1', 'R2']);
    expect(bands[1].entries).toHaveLength(1);
  });

  it('is empty for an empty catalog', () => {
    expect(groupByBand([])).toEqual([]);
  });
});

describe('channelLabel', () => {
  it('resolves a known MHz to its band + channel', () => {
    expect(channelLabel(5658, CATALOG)).toBe('Raceband R1');
    expect(channelLabel(5800, CATALOG)).toBe('Fatshark F4');
  });

  it('falls back to a raw "<mhz> MHz" for a custom/unknown frequency', () => {
    expect(channelLabel(5685, CATALOG)).toBe('5685 MHz');
  });

  it('falls back to raw MHz when the catalog is unavailable', () => {
    expect(channelLabel(5800, [])).toBe('5800 MHz');
  });

  it('prefers the first catalog match (stable order) for a coincident frequency', () => {
    // Two bands could share a frequency; the earlier (Raceband) wins.
    const dupe: ChannelCatalogEntry[] = [
      { band: 'Raceband', channel: 'R1', mhz: 5658 },
      { band: 'HDZero', channel: 'R1', mhz: 5658 }
    ];
    expect(channelLabel(5658, dupe)).toBe('Raceband R1');
  });
});

describe('catalogEntryFor / isCatalogChannel', () => {
  it('finds the entry for a known MHz, and reports catalog membership', () => {
    expect(catalogEntryFor(5695, CATALOG)?.channel).toBe('R2');
    expect(isCatalogChannel(5695, CATALOG)).toBe(true);
    expect(catalogEntryFor(5685, CATALOG)).toBeUndefined();
    expect(isCatalogChannel(5685, CATALOG)).toBe(false);
  });
});

describe('capabilityTag / fixedAllowed', () => {
  it('reads Flexible and Fixed capabilities', () => {
    expect(capabilityTag('Flexible')).toBe('Flexible');
    expect(capabilityTag(undefined)).toBe('Flexible');
    const fixed: ChannelCapability = { Fixed: { channels: [5658, 5695] } };
    expect(capabilityTag(fixed)).toBe('Fixed');
    expect(fixedAllowed(fixed)).toEqual([5658, 5695]);
    expect(fixedAllowed('Flexible')).toEqual([]);
  });
});

describe('offeredCatalog', () => {
  it('offers the whole catalog for a Flexible timer', () => {
    expect(offeredCatalog('Flexible', CATALOG)).toHaveLength(CATALOG.length);
  });

  it('limits a Fixed timer to its built-in allowed set, in catalog order', () => {
    const fixed: ChannelCapability = { Fixed: { channels: [5800, 5658] } };
    const offered = offeredCatalog(fixed, CATALOG);
    expect(offered.map((e) => e.mhz)).toEqual([5658, 5800]); // catalog order, not allowed order
  });
});

describe('isPlausibleMhz', () => {
  it('accepts a 5.8 GHz centre and rejects out-of-band / non-integer values', () => {
    expect(isPlausibleMhz(5685)).toBe(true);
    expect(isPlausibleMhz(5300)).toBe(true);
    expect(isPlausibleMhz(6000)).toBe(true);
    expect(isPlausibleMhz(1234)).toBe(false);
    expect(isPlausibleMhz(6500)).toBe(false);
    expect(isPlausibleMhz(5800.5)).toBe(false);
    expect(isPlausibleMhz(Number.NaN)).toBe(false);
  });
});

describe('assignChannelsRoundRobin', () => {
  it('spreads channels first-fit, the i-th pilot getting pool[i]', () => {
    const out = assignChannelsRoundRobin(['a', 'b', 'c'], [5658, 5695, 5760]);
    expect([...out]).toEqual([
      ['a', 5658],
      ['b', 5695],
      ['c', 5760]
    ]);
  });

  it('wraps round-robin when there are more pilots than channels (repeats are fine)', () => {
    // 5 pilots across 2 channels → 5658,5695,5658,5695,5658 (the repeats fly in different heats).
    const out = assignChannelsRoundRobin(['p1', 'p2', 'p3', 'p4', 'p5'], [5658, 5695]);
    expect([...out.values()]).toEqual([5658, 5695, 5658, 5695, 5658]);
    // Every pilot is covered exactly once.
    expect(out.size).toBe(5);
  });

  it('is deterministic — same input order yields the same assignment', () => {
    const a = assignChannelsRoundRobin(['x', 'y', 'z'], [10, 20]);
    const b = assignChannelsRoundRobin(['x', 'y', 'z'], [10, 20]);
    expect([...a]).toEqual([...b]);
  });

  it('assigns nothing when the channel pool is empty', () => {
    expect(assignChannelsRoundRobin(['a', 'b'], []).size).toBe(0);
  });

  it('handles an empty pilot list', () => {
    expect(assignChannelsRoundRobin([], [5658]).size).toBe(0);
  });
});

describe('nodeIndexOf (open-practice node ref parsing)', () => {
  it('parses node-{i} into its index', () => {
    expect(nodeIndexOf('node-0')).toBe(0);
    expect(nodeIndexOf('node-7')).toBe(7);
  });

  it('returns undefined for a non-node ref', () => {
    expect(nodeIndexOf('ALICE')).toBeUndefined();
    expect(nodeIndexOf('node-')).toBeUndefined();
    expect(nodeIndexOf('node-x')).toBeUndefined();
  });
});

describe('nodeSeatLabel (open-practice seat label) — #416: node AND channel', () => {
  it('reads as node + channel, which is the pair the RD actually needs', () => {
    // The node number is what they look at on the hardware; the channel is what the pilot needs.
    expect(nodeSeatLabel(6, 5695, CATALOG)).toBe('Node 7 · Raceband R2');
    expect(nodeSeatLabel(0, 5658, CATALOG)).toBe('Node 1 · Raceband R1');
  });

  it('falls back to raw MHz for a non-catalog channel', () => {
    expect(nodeSeatLabel(0, 5111, CATALOG)).toBe('Node 1 · 5111 MHz');
  });

  it('is the 1-based node ALONE when the channel is genuinely unknown', () => {
    expect(nodeSeatLabel(2, undefined, CATALOG)).toBe('Node 3');
  });

  it('never renders a raw seat ref', () => {
    expect(nodeSeatLabel(6, undefined, CATALOG)).not.toContain('node-6');
    expect(nodeSeatLabel(6, 5695, CATALOG)).not.toContain('node-6');
  });
});

describe('poolChannel — an EMPTY pool is "unrestricted", not "none" (#413, #416)', () => {
  it('reads a configured pool positionally', () => {
    expect(poolChannel(0, [5658, 5800])).toBe(5658);
    expect(poolChannel(1, [5658, 5800])).toBe(5800);
  });

  it('answers undefined past the end of a configured pool', () => {
    expect(poolChannel(2, [5658, 5800])).toBeUndefined();
  });

  it('refuses to read an EMPTY pool at all', () => {
    // Every Flexible timer (both RotorHazard timers on the bench) reports `available_channels: []`,
    // where empty means "no restriction" rather than "no channels". Indexing it answers undefined
    // for every node — which reads as "this seat has no channel", a false statement about the
    // hardware, and the reason the seat label degraded to a bare `Node N` on all real timers.
    expect(poolChannel(0, [])).toBeUndefined();
    expect(poolChannel(0, undefined)).toBeUndefined();
  });
});
