/**
 * Timer presentation helpers (issue #73, Slice 2a).
 *
 * Pure mappers shared by the app-level Timers management screen and the per-event timer
 * selector: the human label + Badge tone for a {@link TimerKind}, sensible defaults for a
 * new Mock, and the small "summary" line each kind shows (sim pace, or RotorHazard url).
 * No I/O — the session owns the protocol calls; these just shape `Timer`/`TimerKind` for the UI.
 */
import type { Timer, TimerKind } from '@gridfpv/types';

/** The two selectable kinds in the add/edit dialog (the discriminant tag). */
export type TimerKindTag = 'Mock' | 'Rotorhazard';

/** Sensible defaults for a fresh **Mock** timer: a handful of laps at a one-minute-ish pace. */
export const DEFAULT_MOCK_LAPS = 3;
export const DEFAULT_MOCK_LAP_MS = 30_000;

/** The discriminant tag of a kind (`'Mock'` | `'Rotorhazard'`). */
export function kindTag(kind: TimerKind): TimerKindTag {
  return 'Mock' in kind ? 'Mock' : 'Rotorhazard';
}

/** The short display label for the kind **badge** (RotorHazard is the brand spelling). */
export function kindLabel(kind: TimerKind): string {
  return 'Mock' in kind ? 'Mock' : 'RotorHazard';
}

/** The Badge `tone` for a kind: Mock is the brand accent; RotorHazard reads as informational. */
export function kindTone(kind: TimerKind): 'accent' | 'info' {
  return 'Mock' in kind ? 'accent' : 'info';
}

/** A one-line summary of a kind's config for the timer row (the sim pace, or the RH url). */
export function kindSummary(kind: TimerKind): string {
  if ('Mock' in kind) {
    const { laps, lap_ms } = kind.Mock;
    const lapName = laps === 1 ? 'lap' : 'laps';
    return `${laps} ${lapName} · ${(lap_ms / 1000).toFixed(1)}s pace`;
  }
  return kind.Rotorhazard.url || 'No URL set';
}

/** Whether a timer is the undeletable built-in Mock (its reserved id). */
export const MOCK_TIMER_ID = 'mock';
export function isBuiltInMock(timer: Timer): boolean {
  return timer.id === MOCK_TIMER_ID;
}
