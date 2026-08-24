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

/**
 * Whether this timer has a connection the RD can **manually hold** (issue #383).
 *
 * Only a **RotorHazard** timer does: it is the one that dials something over the network, and so
 * the only one where "is this URL right? is it reachable? does it have the plugin?" is a question
 * worth asking. The built-in Mock needs nothing external (the Director answers its `connect` with
 * a **400**), so the control is not offered for it at all rather than offered and then rejected. An
 * unknown (newer-Director) kind is likewise left alone — this console can't reason about it.
 */
export function isConnectable(timer: Timer): boolean {
  return kindTag(timer.kind) === 'Rotorhazard';
}

/** Whether the RD is currently **holding** a manual connection to this timer (#383). */
export function isManuallyHeld(timer: Timer): boolean {
  return isConnectable(timer) && timer.manual_connect === true;
}

/**
 * The **label** for the connect control, driven by the server-authoritative `manual_connect` hold
 * rather than by `status` (issue #383).
 *
 * `status` is the *result* of a hold and moves on its own (`Connecting` → `Connected` →
 * `Disconnected` on a drop, and the dialer retries behind that). Keying the button off it would
 * make the control flicker between "Connect" and "Disconnect" while the Director retries a bad
 * URL — exactly the moment the RD needs a stable thing to press. The hold is the RD's *intent* and
 * changes only when they press the button, so that is what the button reflects.
 */
export function connectActionLabel(timer: Timer): 'Connect' | 'Disconnect' {
  return isManuallyHeld(timer) ? 'Disconnect' : 'Connect';
}

/**
 * A short plain-language reading of a **manually held** RotorHazard timer's status (#383) — the
 * one-liner under the row while the RD is testing a timer at a venue, phrased as the question they
 * are actually asking ("is it reachable?") rather than as the enum.
 *
 * `undefined` when there is nothing to add (no hold, or a timer that can't be held): the row's
 * existing `StatusPill` and plugin badge already carry the state, and this only adds the sentence
 * that turns a status into an instruction.
 */
export function connectionHint(timer: Timer): string | undefined {
  if (!isManuallyHeld(timer)) return undefined;
  switch (timer.status) {
    // `Configured` is the resting status a just-held timer still reads until the reconciler's next
    // tick picks it up — to the RD that is indistinguishable from "connecting", so say so.
    case 'Configured':
    case 'Connecting':
      return 'Connecting…';
    case 'Connected':
      return 'Reachable — this timer is answering.';
    case 'Error':
      return 'Could not reach this timer. Check the URL, and that RotorHazard is running.';
    case 'Disconnected':
      return 'The connection dropped. Retrying…';
    default:
      return undefined;
  }
}
