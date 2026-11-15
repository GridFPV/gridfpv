/** The 5.8 GHz channel table: a frequency's channel name, and the picker's list. */
import { describe, expect, it } from 'vitest';
import {
  BANDS,
  CHANNEL_GROUPS,
  channelLabel,
  formatMhz,
  frequencyLabel
} from '../src/lib/channels.js';

describe('channelLabel', () => {
  it('names every Raceband and Fatshark channel', () => {
    const raceband = [5658, 5695, 5732, 5769, 5806, 5843, 5917];
    expect(raceband.map(channelLabel)).toEqual(['R1', 'R2', 'R3', 'R4', 'R5', 'R6', 'R8']);
    const fatshark = [5740, 5760, 5780, 5800, 5820, 5840, 5860];
    expect(fatshark.map(channelLabel)).toEqual(['F1', 'F2', 'F3', 'F4', 'F5', 'F6', 'F7']);
  });

  it('names the Boscam bands', () => {
    expect(channelLabel(5865)).toBe('A1');
    expect(channelLabel(5725)).toBe('A8');
    expect(channelLabel(5733)).toBe('B1');
    expect(channelLabel(5866)).toBe('B8');
    expect(channelLabel(5705)).toBe('E1');
    expect(channelLabel(5945)).toBe('E8');
  });

  it('gives both names to the frequency two bands share', () => {
    expect(channelLabel(5880)).toBe('R7 / F8');
  });

  it('has no name for a frequency that is not in the table', () => {
    expect(channelLabel(5700)).toBeUndefined();
    expect(channelLabel(0)).toBeUndefined();
  });
});

describe('frequency words', () => {
  it('writes MHz', () => {
    expect(formatMhz(5658)).toBe('5658 MHz');
  });

  it('puts the channel name first when there is one', () => {
    expect(frequencyLabel(5658)).toBe('R1 · 5658 MHz');
    expect(frequencyLabel(5700)).toBe('5700 MHz');
  });
});

describe('the picker list', () => {
  it('has five bands of eight channels', () => {
    expect(BANDS.map((band) => band.name)).toEqual([
      'Raceband',
      'Fatshark',
      'Boscam A',
      'Boscam B',
      'Boscam E'
    ]);
    for (const band of BANDS) expect(band.mhz).toHaveLength(8);
  });

  it('offers each frequency once, grouped by band, Raceband first', () => {
    expect(CHANNEL_GROUPS[0].band).toBe('Raceband');
    expect(CHANNEL_GROUPS[0].choices[0]).toEqual({ mhz: 5658, label: 'R1 · 5658 MHz' });
    const all = CHANNEL_GROUPS.flatMap((group) => group.choices.map((choice) => choice.mhz));
    expect(new Set(all).size).toBe(all.length);
    expect(all).toHaveLength(39);
    // The shared frequency is under Raceband with both names, and not again under Fatshark.
    expect(CHANNEL_GROUPS[0].choices[6]).toEqual({ mhz: 5880, label: 'R7 / F8 · 5880 MHz' });
    expect(CHANNEL_GROUPS[1].choices.map((choice) => choice.mhz)).not.toContain(5880);
  });
});
