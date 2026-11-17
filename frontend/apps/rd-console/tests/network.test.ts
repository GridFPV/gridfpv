/**
 * The words on the Network page, and the port check.
 */
import { describe, expect, it } from 'vitest';
import type { NetworkState, NetworkView } from '@gridfpv/types';
import {
  HIGHEST_PORT,
  LOWEST_PORT,
  portProblem,
  reachable,
  refreshEvery,
  stateLabel,
  stateTone
} from '../src/lib/network.js';

const STATES: NetworkState[] = ['Off', 'Starting', 'On', 'Failed', 'Always'];
const view = (over: Partial<NetworkView> = {}): NetworkView => ({
  enabled: true,
  port: 8081,
  state: 'On',
  addresses: ['http://192.168.1.20:8081/'],
  ...over
});

describe('the state in words', () => {
  it('names every state, and calls an always-on Director on', () => {
    expect(STATES.map(stateLabel)).toEqual(['Off', 'Starting', 'On', 'Not working', 'On']);
  });

  it('colours every state', () => {
    expect(STATES.map(stateTone)).toEqual(['neutral', 'warn', 'success', 'danger', 'success']);
  });
});

describe('whether other devices can open Grid', () => {
  it('needs the listener up and an address to give', () => {
    expect(reachable(view())).toBe(true);
    expect(reachable(view({ state: 'Always' }))).toBe(true);
    expect(reachable(view({ addresses: [] }))).toBe(false);
    expect(reachable(view({ state: 'Always', addresses: [] }))).toBe(false);
    for (const state of ['Off', 'Starting', 'Failed'] as const) {
      expect(reachable(view({ state }))).toBe(false);
    }
  });
});

describe('the port check', () => {
  it('takes any whole number in range, with spaces around it', () => {
    for (const text of ['1024', '8081', '65535', ' 9000 ', '09000']) {
      expect(portProblem(text), text).toBeUndefined();
    }
  });

  it('asks for a number when there is none', () => {
    for (const text of ['', ' ', '\t']) {
      expect(portProblem(text), JSON.stringify(text)).toBe('Type a port number.');
    }
  });

  it('refuses anything that is not a whole number', () => {
    for (const text of [
      'abc',
      '80a',
      '8081.5',
      '-1',
      '+8081',
      '8 081',
      '1e4',
      '0x1F90',
      '８０８１'
    ]) {
      expect(portProblem(text), text).toBe('A port is a whole number.');
    }
  });

  it('refuses a number out of range, at both edges', () => {
    const range = `Pick a port from ${LOWEST_PORT} to ${HIGHEST_PORT}.`;
    for (const text of ['0', '1', '80', '443', '1023', '65536', '70000', '99999999999999999999']) {
      expect(portProblem(text), text).toBe(range);
    }
  });
});

describe('how often the page reads the setting again', () => {
  it('is sooner while the listener is starting or has failed', () => {
    expect(refreshEvery('Starting')).toBe(1000);
    expect(refreshEvery('Failed')).toBe(1000);
    for (const state of ['Off', 'On', 'Always', undefined] as const) {
      expect(refreshEvery(state)).toBe(5000);
    }
  });
});
