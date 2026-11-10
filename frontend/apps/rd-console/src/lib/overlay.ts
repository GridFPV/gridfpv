/**
 * Overlays: pages for an OBS browser source (`#/overlay/‹name›`), and the pure helpers
 * behind them.
 *
 * An overlay only reads. It is a route of the console drawn bare on a see-through page. It
 * follows the Director's active event and shows what that event's snapshot says, so closing
 * OBS, or the browser source crashing, cannot touch a race.
 *
 * The event page's Overlays card (`screens/EventOverlays.svelte`) lists the catalogue below
 * with each page's URL. The pages themselves are `screens/*Overlay.svelte`.
 */
import type { EventView, Format, LapView, PhaseId, PilotId, RaceView } from '@gridfpv/types';
import type { Names } from './names.js';
import { currentRace } from './phases.js';
import { formatHash, type OverlayName, type OverlayTheme } from './route.js';
import { raceSeatRows } from './run.js';

/** One overlay, as the Overlays card lists it. */
export interface OverlayInfo {
  name: OverlayName;
  title: string;
  /** One or two sentences on what the page shows. */
  description: string;
}

/** The overlays the console has, in the order the card lists them. */
export const OVERLAY_CATALOG: readonly OverlayInfo[] = [
  {
    name: 'race',
    title: 'Race',
    description:
      'The heat being raced: its name, the race clock, and each pilot with their place, laps, ' +
      'last lap and best lap. A heat with one pilot shows their last few laps in full.'
  }
];

/**
 * The URL to paste into OBS for an overlay, built from where the console itself is served.
 * `theme` picks the contrast: `'dark'` (the default) is light text on a dark panel, for dark
 * video; `'light'` is dark text on a light panel, for bright video.
 */
export function overlayUrl(
  loc: { origin: string; pathname: string },
  overlay: OverlayName,
  theme: OverlayTheme = 'dark'
): string {
  const hash = formatHash(
    theme === 'light' ? { kind: 'overlay', overlay, theme } : { kind: 'overlay', overlay }
  );
  return `${loc.origin}${loc.pathname}${hash}`;
}

// ── Which race to show ───────────────────────────────────────────────────────

/** How long a saved result stays in the video once no race is on: one minute. */
export const RESULT_LINGER_MICROS = 60_000_000;

/** The race the overlay shows. */
export interface OverlaySubject {
  race: RaceView;
  /** True for the event's current race; false for a saved result still being shown. */
  current: boolean;
}

/**
 * The result to fall back on when no race is on: the most recently saved race of the heat
 * that was raced last. The snapshot lists races oldest first, so the last one names that
 * heat. A discarded race is never shown; a heat with nothing saved gives `undefined`.
 */
export function lastResult(view: EventView): RaceView | undefined {
  const heat = view.races.at(-1)?.heat;
  if (heat === undefined) return undefined;
  return view.races.filter((race) => race.heat === heat && race.state === 'Saved').at(-1);
}

/**
 * Which race the overlay shows at `now` (Director clock, microseconds), or `undefined` when
 * it should go quiet.
 *
 * - The current race (counting down, running or stopped), whenever there is one.
 * - Otherwise the last result, for {@link RESULT_LINGER_MICROS}. The time counts from
 *   `decidedAt`, the moment this overlay saw the current race go away. An overlay that was
 *   opened after that never saw it, so it counts from when the race stopped instead (the
 *   snapshot does not say when a race was saved).
 */
export function overlaySubject(
  view: EventView | undefined,
  now: number,
  decidedAt?: number
): OverlaySubject | undefined {
  if (!view) return undefined;
  const current = currentRace(view);
  if (current) return { race: current, current: true };
  const result = lastResult(view);
  if (!result) return undefined;
  const since = Math.max(result.stopped_at ?? Number.NEGATIVE_INFINITY, decidedAt ?? 0);
  if (!Number.isFinite(since) || since <= 0) return undefined;
  return now - since < RESULT_LINGER_MICROS ? { race: result, current: false } : undefined;
}

/** The race's state in one word, as the overlay shows it. */
export function overlayStateWord(race: RaceView): string {
  switch (race.state) {
    case 'Staging':
      return 'Starting';
    case 'Running':
      return 'Live';
    case 'Stopped':
      return 'Finished';
    case 'Saved':
      return 'Result';
    case 'Discarded':
      return 'Discarded';
  }
}

// ── Best N laps in a row ─────────────────────────────────────────────────────

/**
 * How many laps in a row a heat is ranked on: `n` when its ranking rule sorts by best `n`
 * consecutive first, `undefined` for every other rule (and for a heat with no ranking).
 */
export function consecutiveLaps(format: Format | undefined): number | undefined {
  const first = format?.ranking?.sort[0];
  if (typeof first === 'object' && first !== null && 'BestConsecutive' in first) {
    return first.BestConsecutive > 0 ? first.BestConsecutive : undefined;
  }
  return undefined;
}

/** A run of laps in a row. */
export interface LapRun {
  /** Where the run starts among the seat's counted laps, counted from 0. */
  start: number;
  /** How many laps it holds. */
  length: number;
  /** What its laps add up to, in microseconds. */
  totalMicros: number;
}

/**
 * The fastest run of `n` laps in a row among a seat's laps, or `undefined` when there is no
 * such run. `laps` is the seat's list as the snapshot gives it, marked restarts and timer
 * restarts included: a run never crosses one, which is the engine's rule too. Two equal runs
 * give the earlier one.
 */
export function bestLapRun(laps: readonly LapView[], n: number): LapRun | undefined {
  if (!Number.isInteger(n) || n <= 0) return undefined;
  let best: LapRun | undefined;
  /** The laps since the last restart, and where the first of them sits among counted laps. */
  let stretch: number[] = [];
  let counted = 0;
  for (const lap of laps) {
    if (lap.restart || lap.timer_restarted) {
      stretch = [];
      continue;
    }
    stretch.push(lap.micros);
    counted += 1;
    if (stretch.length < n) continue;
    const totalMicros = stretch.slice(-n).reduce((sum, micros) => sum + micros, 0);
    if (!best || totalMicros < best.totalMicros) {
      best = { start: counted - n, length: n, totalMicros };
    }
  }
  return best;
}

/**
 * A pilot's fastest run of `n` laps in a row across a heat: every saved race of the heat,
 * and `shown` (the race on screen) whatever its state. In microseconds, or `undefined` when
 * no race holds such a run. A run never joins laps from two races.
 */
export function bestRunInHeat(
  view: EventView,
  heat: PhaseId,
  pilot: PilotId,
  n: number,
  shown?: RaceView
): number | undefined {
  let best: number | undefined;
  for (const race of view.races) {
    if (race.heat !== heat) continue;
    if (race.state !== 'Saved' && race.race !== shown?.race) continue;
    for (const seat of race.seats) {
      if (seat.pilot !== pilot) continue;
      const run = bestLapRun(seat.laps, n);
      if (run && (best === undefined || run.totalMicros < best)) best = run.totalMicros;
    }
  }
  return best;
}

// ── The rows ─────────────────────────────────────────────────────────────────

/** One seat's line on the overlay. */
export interface OverlayRow {
  /** Stable key for the list. A wire handle: never shown. */
  key: string;
  /** The pilot's callsign, or the seat's name ("Seat 2") for an open seat. */
  name: string;
  pilot?: PilotId;
  /** Place in the race so far, 1 first. */
  place?: number;
  /** How many laps count. */
  laps: number;
  /** Every counted lap, in order, in microseconds. */
  lapMicros: number[];
  lastLapMicros?: number;
  bestLapMicros?: number;
  /** The fastest run of N laps in a row, when the heat is ranked on one. */
  bestRun?: LapRun;
}

/**
 * The overlay's rows for a race: one per seat, in lineup order. `runLength` is the N of a
 * heat ranked on best N consecutive (see {@link consecutiveLaps}); leave it out otherwise.
 */
export function overlayRows(race: RaceView, names: Names, runLength?: number): OverlayRow[] {
  return raceSeatRows(race, names).map((row, index) => ({
    key: `${row.adapter}\u0000${row.seat}`,
    name: row.pilot === undefined ? row.seatName : row.who,
    pilot: row.pilot,
    place: row.place,
    laps: row.laps.length,
    lapMicros: row.laps.map((lap) => lap.micros),
    lastLapMicros: row.lastLapMicros,
    bestLapMicros: row.bestLapMicros,
    bestRun: runLength === undefined ? undefined : bestLapRun(race.seats[index].laps, runLength)
  }));
}

/** Whether lap `index` (among counted laps, from 0) is part of `run`. */
export function inRun(index: number, run: LapRun | undefined): boolean {
  return run !== undefined && index >= run.start && index < run.start + run.length;
}
