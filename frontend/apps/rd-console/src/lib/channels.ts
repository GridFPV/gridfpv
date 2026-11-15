/**
 * The standard 5.8 GHz video channels, and the one place a frequency gets its channel name.
 *
 * A seat on a video timer watches one frequency. People know those frequencies by band and
 * channel ("R1", "F4"), so wherever a frequency is shown its channel name goes beside it. A
 * frequency that is not in the table is still a fine frequency: it shows as plain MHz and can
 * be typed by hand.
 */

/** One band of eight channels. */
export interface Band {
  /** The letter people write before the channel number: "R". */
  letter: string;
  /** The band's name: "Raceband". */
  name: string;
  /** The eight frequencies in MHz, channel 1 first. */
  mhz: readonly number[];
}

/** The bands, in the order they are offered. Raceband is first: it is what races fly. */
export const BANDS: readonly Band[] = [
  { letter: 'R', name: 'Raceband', mhz: [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917] },
  { letter: 'F', name: 'Fatshark', mhz: [5740, 5760, 5780, 5800, 5820, 5840, 5860, 5880] },
  { letter: 'A', name: 'Boscam A', mhz: [5865, 5845, 5825, 5805, 5785, 5765, 5745, 5725] },
  { letter: 'B', name: 'Boscam B', mhz: [5733, 5752, 5771, 5790, 5809, 5828, 5847, 5866] },
  { letter: 'E', name: 'Boscam E', mhz: [5705, 5685, 5665, 5645, 5885, 5905, 5925, 5945] }
];

/** Every channel name a frequency has, in band order. 5880 MHz is both R7 and F8. */
function namesFor(mhz: number): string[] {
  const names: string[] = [];
  for (const band of BANDS) {
    const index = band.mhz.indexOf(mhz);
    if (index >= 0) names.push(`${band.letter}${index + 1}`);
  }
  return names;
}

/**
 * The channel name of a frequency: "R1". A frequency two bands share carries both names
 * ("R7 / F8"). A frequency that is not in the table has no name, and the answer is
 * `undefined`.
 */
export function channelLabel(mhz: number): string | undefined {
  const names = namesFor(mhz);
  return names.length === 0 ? undefined : names.join(' / ');
}

/** A frequency in words: "5658 MHz". */
export function formatMhz(mhz: number): string {
  return `${mhz} MHz`;
}

/** A frequency with its channel name when it has one: "R1 · 5658 MHz", or "5700 MHz". */
export function frequencyLabel(mhz: number): string {
  const label = channelLabel(mhz);
  return label ? `${label} · ${formatMhz(mhz)}` : formatMhz(mhz);
}

/** One choice in a frequency picker. */
export interface ChannelChoice {
  mhz: number;
  /** What the choice reads as: "R1 · 5658 MHz". */
  label: string;
}

/** A band's choices in a frequency picker. */
export interface ChannelGroup {
  /** The band's name: "Raceband". */
  band: string;
  choices: ChannelChoice[];
}

/**
 * The list a frequency picker offers, grouped by band. Each frequency is offered once: one
 * that two bands share is listed under the first band, with both names.
 */
export const CHANNEL_GROUPS: readonly ChannelGroup[] = (() => {
  const seen = new Set<number>();
  return BANDS.map((band) => ({
    band: band.name,
    choices: band.mhz
      .filter((mhz) => {
        if (seen.has(mhz)) return false;
        seen.add(mhz);
        return true;
      })
      .map((mhz) => ({ mhz, label: frequencyLabel(mhz) }))
  }));
})();
