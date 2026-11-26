/**
 * Words for the Network page: what each door's listener is doing, said plainly. Pure, so
 * the page only lays them out.
 */
import type { DoorState, DoorView, NetworkView } from '@gridfpv/types';

/** The lowest and highest port that can be picked. The Director refuses anything else. */
export const LOWEST_PORT = 1;
export const HIGHEST_PORT = 65535;
/** The fewest and most characters an access token the director types may have. */
export const TOKEN_MIN = 8;
export const TOKEN_MAX = 64;

/** The state in a word or two, for the pill beside a door's switch. */
export function stateLabel(state: DoorState): string {
  switch (state) {
    case 'Off':
      return 'Off';
    case 'Starting':
      return 'Starting';
    case 'On':
      return 'On';
    case 'Failed':
      return 'Not working';
  }
}

/** The pill's colour. */
export function stateTone(state: DoorState): 'neutral' | 'success' | 'warn' | 'danger' {
  switch (state) {
    case 'Off':
      return 'neutral';
    case 'Starting':
      return 'warn';
    case 'On':
      return 'success';
    case 'Failed':
      return 'danger';
  }
}

/** Whether other devices can come in by this door right now. */
export function reachable(door: DoorView): boolean {
  return door.state === 'On' && door.addresses.length > 0;
}

/**
 * What is wrong with a port as typed, in words, or `undefined` when it can be sent. The
 * Director checks again; this only saves a round trip for the plain mistakes.
 */
export function portProblem(text: string): string | undefined {
  const trimmed = text.trim();
  if (trimmed === '') return 'Type a port number.';
  if (!/^\d+$/.test(trimmed)) return 'A port is a whole number.';
  const port = Number(trimmed);
  if (port < LOWEST_PORT || port > HIGHEST_PORT) {
    return `Pick a port from ${LOWEST_PORT} to ${HIGHEST_PORT}.`;
  }
  return undefined;
}

/**
 * What is wrong with an access token as typed, or `undefined` when it can be sent. Nothing
 * typed is fine: Grid then makes one.
 */
export function tokenProblem(text: string): string | undefined {
  const trimmed = text.trim();
  if (trimmed === '') return undefined;
  if (/\s/.test(trimmed)) return 'An access token cannot have spaces in it.';
  const length = [...trimmed].length;
  if (length < TOKEN_MIN) return `An access token needs at least ${TOKEN_MIN} characters.`;
  if (length > TOKEN_MAX) return `An access token can have ${TOKEN_MAX} characters at most.`;
  return undefined;
}

/** The address a director opens on another device: the door's own, with the sign-in page. */
export function hostOf(address: string): string {
  return address.replace(/^https?:\/\//, '').replace(/\/$/, '');
}

/** How often to read the setting again, in milliseconds: sooner while something is changing. */
export function refreshEvery(view: NetworkView | undefined): number {
  const moving = (door: DoorView | undefined) =>
    door?.state === 'Starting' || door?.state === 'Failed';
  return moving(view?.public) || moving(view?.remote) ? 1000 : 5000;
}
