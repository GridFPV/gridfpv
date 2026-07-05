/**
 * Timer presentation helpers (issue #73, Slice 2a).
 *
 * Pure mappers shared by the app-level Timers management screen and the per-event timer
 * selector: the human label + Badge tone for a {@link TimerKind}, sensible defaults for a
 * new Mock, and the small "summary" line each kind shows (sim pace, or RotorHazard url).
 * No I/O — the session owns the protocol calls; these just shape `Timer`/`TimerKind` for the UI.
 */
import type { Timer, TimerKind } from '@gridfpv/types';

/** The two selectable kinds in the add/edit dialog (the discriminant tag). `'Unknown'` is the
 *  version-skew fallback: a NEWER Director may send a kind this console build doesn't model
 *  yet, and it must render labeled (not mislabeled as RotorHazard, and never crash on a field
 *  access) — the timer is still real and still selectable. */
export type TimerKindTag = 'Mock' | 'Rotorhazard' | 'Unknown';

/** Sensible defaults for a fresh **Mock** timer: a handful of laps at a one-minute-ish pace. */
export const DEFAULT_MOCK_LAPS = 3;
export const DEFAULT_MOCK_LAP_MS = 30_000;

/** The discriminant tag of a kind (`'Mock'` | `'Rotorhazard'`). */
export function kindTag(kind: TimerKind): TimerKindTag {
  if ('Mock' in kind) return 'Mock';
  if ('Rotorhazard' in kind) return 'Rotorhazard';
  return 'Unknown';
}

/** The short display label for the kind **badge** (RotorHazard is the brand spelling). */
export function kindLabel(kind: TimerKind): string {
  if ('Mock' in kind) return 'Mock';
  if ('Rotorhazard' in kind) return 'RotorHazard';
  // A newer Director's kind: show its discriminant verbatim rather than a wrong brand.
  return Object.keys(kind)[0] ?? 'Unknown';
}

/** The Badge `tone` for a kind: Mock is the brand accent; RotorHazard reads as informational. */
export function kindTone(kind: TimerKind): 'accent' | 'info' | 'neutral' {
  if ('Mock' in kind) return 'accent';
  if ('Rotorhazard' in kind) return 'info';
  return 'neutral';
}

/** A one-line summary of a kind's config for the timer row (the sim pace, or the RH url). */
export function kindSummary(kind: TimerKind): string {
  if ('Mock' in kind) {
    const { laps, lap_ms } = kind.Mock;
    const lapName = laps === 1 ? 'lap' : 'laps';
    return `${laps} ${lapName} · ${(lap_ms / 1000).toFixed(1)}s pace`;
  }
  if ('Rotorhazard' in kind) return kind.Rotorhazard.url || 'No URL set';
  return 'Unsupported by this console build — update the console';
}

/** Whether a timer is the undeletable built-in Mock (its reserved id). */
export const MOCK_TIMER_ID = 'mock';
export function isBuiltInMock(timer: Timer): boolean {
  return timer.id === MOCK_TIMER_ID;
}

/**
 * Whether a timer is usable right now — i.e. "connected" for the at-a-glance summary count.
 *
 * A built-in **Mock** reports `Ready` (it needs nothing external); a live **RotorHazard** reports
 * `Connected` once its socket is up. Both count. `Configured` (a RH timer that isn't dialed in yet),
 * `Connecting`, `Disconnected`, and `Error` do NOT — the timer isn't ready to run a heat. Centralized
 * here so the predicate can't drift between the hub and the timer screens.
 */
export function isTimerConnected(timer: Timer): boolean {
  return timer.status === 'Connected' || timer.status === 'Ready';
}
