/**
 * Words for the Network page: what the listener for other devices is doing, said plainly.
 * Pure, so the page only lays them out.
 */
import type { NetworkState, NetworkView } from '@gridfpv/types';

/** The lowest and highest port that can be picked. The Director refuses anything else. */
export const LOWEST_PORT = 1024;
export const HIGHEST_PORT = 65535;

/** The state in a word or two, for the pill at the top of the card. */
export function stateLabel(state: NetworkState): string {
  switch (state) {
    case 'Off':
      return 'Off';
    case 'Starting':
      return 'Starting';
    case 'On':
    case 'Always':
      return 'On';
    case 'Failed':
      return 'Not working';
  }
}

/** The pill's colour. */
export function stateTone(state: NetworkState): 'neutral' | 'success' | 'warn' | 'danger' {
  switch (state) {
    case 'Off':
      return 'neutral';
    case 'Starting':
      return 'warn';
    case 'On':
    case 'Always':
      return 'success';
    case 'Failed':
      return 'danger';
  }
}

/** Whether other devices can open Grid right now. */
export function reachable(view: NetworkView): boolean {
  return (view.state === 'On' || view.state === 'Always') && view.addresses.length > 0;
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

/** How often to read the setting again, in milliseconds: sooner while something is changing. */
export function refreshEvery(state: NetworkState | undefined): number {
  return state === 'Starting' || state === 'Failed' ? 1000 : 5000;
}
