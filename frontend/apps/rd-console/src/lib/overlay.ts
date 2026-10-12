/**
 * Overlays — the OBS browser-source pages (`#/overlay/<name>`), and the pure helpers they share.
 *
 * An overlay is a **read-only protocol client** (streaming.html §2): a route of the RD console SPA
 * rendered bare on a transparent document, following the Director's active event — *whatever* that
 * event is. It is deliberately not tied to a kind of event, a preset or a format: a club night's
 * heat of six, a one-pilot time trial at home, and an open-practice session all put the same page
 * in the video, and the page shows what the current heat has. Nothing here writes; closing OBS, or
 * the browser source crashing, cannot touch a race.
 *
 * The console reaches these pages from the event workspace's **Overlays** tab
 * (`screens/EventOverlays.svelte`), which lists the catalogue below with each page's URL, its
 * contrast picker and a live preview. The pages themselves live in `screens/*Overlay.svelte`.
 */

import type {
  CompetitorRef,
  EventMeta,
  HeatId,
  HeatSummary,
  LapList,
  Lap,
  RoundDef,
  RoundStanding,
  WinCondition
} from '@gridfpv/types';
import { formatHash, type OverlayName, type OverlayTheme } from './route.js';

/** One entry of the overlay catalogue: what the Overlays tab lists and links. */
export interface OverlayInfo {
  name: OverlayName;
  /** The display title — shown in the Overlays tab and as the page's accessible label. */
  title: string;
  /** One or two sentences on what the page shows, for the tab. */
  description: string;
}

/** The overlays the console serves, in the order the Overlays tab lists them. */
export const OVERLAY_CATALOG: readonly OverlayInfo[] = [
  {
    name: 'race',
    title: 'Race timer',
    description:
      'The race clock and phase, the heat name, and every pilot in the current heat with their ' +
      'laps — last lap, best lap, and the best run of laps in a row when the round scores one. ' +
      'A single pilot gets the full lap list.'
  }
];

/**
 * The OBS browser-source URL for an overlay, from the console's own location. `theme` picks the
 * panel's contrast for the footage underneath — light text on a dark panel (default) or dark text
 * on a light one (`#/overlay/<name>/light`, see `route.ts`).
 */
export function overlayUrl(
  loc: { origin: string; pathname: string },
  overlay: OverlayName,
  theme: OverlayTheme = 'dark'
): string {
  return `${loc.origin}${loc.pathname}${formatHash({ kind: 'overlay', overlay, theme })}`;
}

/** A consecutive-lap window: where it starts (0-based lap index) and what it sums to. */
export interface ConsecutiveWindow {
  /** Index of the window's first lap in the lap list it was found in. */
  start: number;
  /** The window's laps, in order (µs each). */
  laps: number[];
  /** The window's total (µs). */
  totalMicros: number;
}

/**
 * The fastest `n`-consecutive-lap window of a lap sequence (durations in µs), or `undefined` with
 * fewer than `n` laps. Ties go to the **earliest** window, matching the engine's tie-break by the
 * completion time of the window's last lap. Pure; the overlay runs it over the *served* lap list,
 * so a voided or edited lap is already reflected.
 */
export function bestConsecutive(
  lapMicros: readonly number[],
  n: number
): ConsecutiveWindow | undefined {
  if (n <= 0 || lapMicros.length < n) return undefined;
  let best: ConsecutiveWindow | undefined;
  for (let start = 0; start + n <= lapMicros.length; start += 1) {
    const laps = lapMicros.slice(start, start + n);
    const totalMicros = laps.reduce((sum, lap) => sum + lap, 0);
    if (!best || totalMicros < best.totalMicros) best = { start, laps, totalMicros };
  }
  return best;
}

/**
 * The consecutive-lap window a round scores — `n` for a `BestConsecutive { n }` win condition,
 * `undefined` for every other condition. The overlay shows a "best N in a row" figure only for a
 * round that ranks by one; a timed heat or a first-to-N race shows laps and best lap instead.
 */
export function consecutiveWindow(win: WinCondition | undefined): number | undefined {
  if (typeof win === 'object' && win !== null && 'BestConsecutive' in win) {
    return win.BestConsecutive.n;
  }
  return undefined;
}

/** The round a heat belongs to, by the heat's round tag — `undefined` for an untagged heat. */
export function heatRound(
  event: EventMeta | undefined,
  heats: readonly HeatSummary[],
  heat: HeatId | undefined
): RoundDef | undefined {
  if (!event || !heat) return undefined;
  const summary = heats.find((h) => h.heat === heat);
  if (!summary?.round) return undefined;
  return event.rounds?.find((r) => r.id === summary.round);
}

/** The served laps of one competitor off a heat's lap list, in order (`[]` when it has none). */
export function competitorLaps(list: LapList | undefined, ref: CompetitorRef): Lap[] {
  return list?.competitors.find((c) => c.competitor.competitor === ref)?.laps ?? [];
}

/**
 * The served round-wide figure (µs) for one competitor off the round standings: the best
 * consecutive window of a `BestConsecutive` round, or the best lap of a `BestLap` round.
 * `undefined` before one exists, and for a `MostLaps` round (laps are not a time).
 */
export function servedRoundBestMicros(
  standings: readonly RoundStanding[],
  ref: CompetitorRef
): number | undefined {
  const row = standings.find((s) => s.competitor === ref);
  if (!row) return undefined;
  const m = row.metric;
  if (typeof m !== 'object' || m === null) return undefined;
  if ('BestConsecutive' in m) return m.BestConsecutive.micros ?? undefined;
  if ('BestLap' in m) return m.BestLap.micros ?? undefined;
  return undefined;
}

/**
 * The order the overlay lists a heat's pilots in: the live **running order** when the stream
 * carries one, else the heat's active pilots, else its scheduled lineup, else whoever the lap list
 * names. Refs appearing in a later source but not an earlier one are appended, so a pilot never
 * vanishes from the frame because one projection lags another.
 */
export function overlayOrder(
  sources: readonly (readonly CompetitorRef[] | undefined)[]
): CompetitorRef[] {
  const out: CompetitorRef[] = [];
  for (const list of sources) {
    for (const ref of list ?? []) if (!out.includes(ref)) out.push(ref);
  }
  return out;
}
