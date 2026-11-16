/**
 * The kiosk (docs/primitives.md §11.4): the visitor page on a shared tablet. Pure.
 *
 * A pilot proves who they are with their code, does what they came to do, and walks away.
 * The page must not leave them signed in for the next person, so it signs them out when
 * nobody has touched it for a while.
 */
import { tidyCode } from './pilotAccess.js';

/** The page address that makes the visitor page a kiosk. */
export const KIOSK_HASH = '#/kiosk';

/** How long a pilot stays signed in at the kiosk with nobody touching it, in milliseconds. */
export const KIOSK_IDLE_MS = 45_000;
/** From how long before the end the page counts down out loud, in milliseconds. */
export const KIOSK_WARN_MS = 15_000;

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
