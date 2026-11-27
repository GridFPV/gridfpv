/**
 * The race code on a remote server's page (docs/primitives.md §13.13).
 *
 * A pilot who checks in to an open seat from the server must show they are at the race.
 * The director shows a code there. The pilot types it once, or scans a QR code that holds
 * it, and this browser keeps it for that event so the next seat needs no typing.
 *
 * The code is kept in this browser only. It is sent with each ask for a seat, and the
 * Director is the one that says whether it is right.
 */
import { isRequestFailure } from '@gridfpv/protocol-client';
import { tidyCode } from './pilotAccess.js';

/** What a kept code is stored in. A test passes its own. */
export type CodeStore = Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>;

const key = (club: string, event: string): string => `gridfpv.race-code.${club}/${event}`;

/** This browser's storage, or `undefined` where it may not be used (a private window). */
function browserStore(): CodeStore | undefined {
  try {
    return globalThis.localStorage;
  } catch {
    return undefined;
  }
}

/** The code kept for an event, if one is. */
export function keptRaceCode(
  club: string,
  event: string,
  store: CodeStore | undefined = browserStore()
): string | undefined {
  try {
    return store?.getItem(key(club, event)) || undefined;
  } catch {
    return undefined;
  }
}

/** Keep a code for an event, tidied the way the Director reads it. */
export function keepRaceCode(
  club: string,
  event: string,
  code: string,
  store: CodeStore | undefined = browserStore()
): void {
  const tidy = tidyCode(code);
  if (tidy === '') return;
  try {
    store?.setItem(key(club, event), tidy);
  } catch {
    // Storage is full or closed. The pilot types the code again next time.
  }
}

/** Stop keeping an event's code: the Director said it is not right any more. */
export function dropRaceCode(
  club: string,
  event: string,
  store: CodeStore | undefined = browserStore()
): void {
  try {
    store?.removeItem(key(club, event));
  } catch {
    // Nothing to do.
  }
}

/** The status a server answers with when the Director's refusal is about the race code. */
export const NEEDS_RACE_CODE = 428;

/** Whether a failed ask was refused over the race code: none was sent, or it was not right. */
export function needsRaceCode(err: unknown): boolean {
  return isRequestFailure(err) && err.status === NEEDS_RACE_CODE;
}
