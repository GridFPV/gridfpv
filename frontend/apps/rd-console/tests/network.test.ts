/**
 * The words on the Network page, and the port check.
 */
import { describe, expect, it } from 'vitest';
import type { DoorState, DoorView, NetworkView } from '@gridfpv/types';
import {
  HIGHEST_PORT,
  LOWEST_PORT,
  hostOf,
  portProblem,
  reachable,
  refreshEvery,
  stateLabel,
  stateTone,
  tokenProblem
} from '../src/lib/network.js';

const STATES: DoorState[] = ['Off', 'Starting', 'On', 'Failed'];
const door = (over: Partial<DoorView> = {}): DoorView => ({
  enabled: true,
  port: 8081,
  port_picked: false,
  state: 'On',
  addresses: ['http://192.168.1.20:8081/'],
  ...over
});
const SHUT = door({ enabled: false, state: 'Off', addresses: [] });
const both = (pub: DoorView, remote: DoorView): NetworkView => ({ public: pub, remote });

describe('the state in words', () => {
  it('names every state', () => {
    expect(STATES.map(stateLabel)).toEqual(['Off', 'Starting', 'On', 'Not working']);
  });

  it('colours every state', () => {
    expect(STATES.map(stateTone)).toEqual(['neutral', 'warn', 'success', 'danger']);
  });
});

describe('whether other devices can come in by a door', () => {
  it('needs the listener up and an address to give', () => {
    expect(reachable(door())).toBe(true);
    expect(reachable(door({ addresses: [] }))).toBe(false);
    for (const state of ['Off', 'Starting', 'Failed'] as const) {
      expect(reachable(door({ state }))).toBe(false);
    }
  });
});

describe('an address as it is shown', () => {
  it('drops the scheme and the last slash, and keeps a port and a page', () => {
    expect(hostOf('http://192.168.1.20/')).toBe('192.168.1.20');
    expect(hostOf('http://192.168.1.20:8081/')).toBe('192.168.1.20:8081');
    expect(hostOf('https://grid.race.lan/')).toBe('grid.race.lan');
    expect(hostOf('http://192.168.1.20/#/kiosk')).toBe('192.168.1.20/#/kiosk');
  });
});

describe('the access token check', () => {
  it('takes nothing typed, since Grid then makes one', () => {
    for (const text of ['', '  ', '\t']) expect(tokenProblem(text)).toBeUndefined();
  });

  it('takes eight to sixty-four characters, with spaces around them', () => {
    for (const text of ['12345678', ' sunday-race-7 ', 'x'.repeat(64)]) {
      expect(tokenProblem(text), text).toBeUndefined();
    }
  });

  it('refuses one that is too short, too long, or has a space in it', () => {
    expect(tokenProblem('short')).toBe('An access token needs at least 8 characters.');
    expect(tokenProblem('x'.repeat(65))).toBe('An access token can have 64 characters at most.');
    expect(tokenProblem('two words here')).toBe('An access token cannot have spaces in it.');
  });
});

describe('the port check', () => {
  it('takes any whole number in range, with spaces around it', () => {
    for (const text of ['1', '80', '443', '1024', '8081', '65535', ' 9000 ', '09000']) {
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
    for (const text of ['0', '65536', '70000', '99999999999999999999']) {
      expect(portProblem(text), text).toBe(range);
    }
  });
});

describe('how often the page reads the setting again', () => {
  it('is sooner while either door is starting or has failed', () => {
    for (const moving of ['Starting', 'Failed'] as const) {
      expect(refreshEvery(both(door({ state: moving }), SHUT))).toBe(1000);
      expect(refreshEvery(both(SHUT, door({ state: moving })))).toBe(1000);
      expect(refreshEvery(both(door(), door({ state: moving })))).toBe(1000);
    }
    expect(refreshEvery(both(SHUT, SHUT))).toBe(5000);
    expect(refreshEvery(both(door(), door()))).toBe(5000);
    expect(refreshEvery(undefined)).toBe(5000);
  });
});
