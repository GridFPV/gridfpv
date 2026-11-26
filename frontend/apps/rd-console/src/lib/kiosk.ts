/**
 * The kiosk (docs/primitives.md §11.4): a tablet the director sets up at the race
 * director's stand, for a pilot with no phone. Pure.
 *
 * The director types a **kiosk code** on the tablet once. It is used up, and the tablet is
 * a kiosk from then on. A pilot proves who they are there with their own code, signs up,
 * and walks away. The page must not leave them signed in for the next person, so it signs
 * them out when nobody has touched it for a while.
 */
import { tidyCode } from './pilotAccess.js';

/** The page address of the kiosk. */
export const KIOSK_HASH = '#/kiosk';

/** How long a pilot stays signed in at the kiosk with nobody touching it, in milliseconds. */
export const KIOSK_IDLE_MS = 45_000;
/** From how long before the end the page counts down out loud, in milliseconds. */
export const KIOSK_WARN_MS = 15_000;
/** How many letters and digits a kiosk code has. The Director decides; this saves a trip. */
export const KIOSK_CODE_LENGTH = 8;

const CODE_CHARS = /^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]+$/;

/** Whether a page address is the kiosk's. */
export function isKioskHash(hash: string): boolean {
  return (
    hash === KIOSK_HASH || hash.startsWith(`${KIOSK_HASH}/`) || hash.startsWith(`${KIOSK_HASH}?`)
  );
}

/** The address to open on the tablet, from the address other devices use. */
export function kioskLink(address: string): string {
  return `${address.replace(/#.*$/, '')}${KIOSK_HASH}`;
}

/** A kiosk code as it is shown: in two halves, so it can be read and typed. `ABCD 2345`. */
export function spellKioskCode(code: string): string {
  const half = Math.ceil(code.length / 2);
  return `${code.slice(0, half)} ${code.slice(half)}`.trim();
}

/**
 * What is wrong with a kiosk code as typed, in words, or `undefined` when it can be sent.
 * The Director checks it for real; this catches a slip before it counts as a wrong try.
 */
export function kioskCodeProblem(typed: string): string | undefined {
  const code = tidyCode(typed);
  if (code === '') return 'Type the kiosk code.';
  if (code.length !== KIOSK_CODE_LENGTH) {
    return `A kiosk code has ${KIOSK_CODE_LENGTH} letters and numbers.`;
  }
  if (!CODE_CHARS.test(code)) {
    return 'A kiosk code has capital letters and numbers, and never 0, O, 1 or I.';
  }
  return undefined;
}

/** When a kiosk was set up, in words: "today 3:42 PM", or the date for another day. */
export function setUpWords(seconds: number, now: Date = new Date()): string {
  const at = new Date(seconds * 1000);
  const time = at.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  const sameDay = at.toDateString() === now.toDateString();
  return sameDay
    ? `today ${time}`
    : `${at.toLocaleDateString([], { month: 'short', day: 'numeric' })} ${time}`;
}

/**
 * The code in what was typed into the kiosk. A handheld scanner that reads a pilot's QR
 * code types the whole sign-in link (`…#/join/ABC234`), so the code is taken out of that.
 */
export function typedCode(typed: string): string {
  const at = typed.lastIndexOf('/join/');
  return tidyCode(at === -1 ? typed : typed.slice(at + '/join/'.length));
}

/** How long is left before the kiosk signs the pilot out, in milliseconds. Never negative. */
export function kioskLeft(lastTouch: number, now: number, idle = KIOSK_IDLE_MS): number {
  // A clock that jumped back must not keep a pilot signed in for longer than `idle`.
  const waited = Math.max(0, now - lastTouch);
  return Math.max(0, idle - waited);
}

/** What the page says as the time runs out, or `undefined` while there is plenty left. */
export function kioskWarning(left: number, warn = KIOSK_WARN_MS): string | undefined {
  if (left > warn) return undefined;
  const seconds = Math.ceil(left / 1000);
  return `Signing out in ${seconds} s. Touch the screen to stay.`;
}
