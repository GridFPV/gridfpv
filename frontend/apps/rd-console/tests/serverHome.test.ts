/** The small decisions of the page a remote server shows (`lib/serverHome.ts`). */
import { describe, expect, it } from 'vitest';
import {
  eventDetail,
  formatServerHash,
  parseServerHash,
  pilotHere
} from '../src/lib/serverHome.js';

describe('parseServerHash', () => {
  it('reads the list from an empty address', () => {
    for (const hash of [
      '',
      '#',
      '#/',
      '#/nothing',
      '#/e',
      '#/e/club/x',
      '#/c/north',
      '#/event',
      '#/event/club',
      '#/event/club/event/more'
    ]) {
      expect(parseServerHash(hash)).toEqual({ kind: 'list' });
    }
  });

  it('reads one event of one club', () => {
    expect(parseServerHash('#/event/club/friday-night-764c5x')).toEqual({
      kind: 'event',
      club: 'club',
      event: 'friday-night-764c5x'
    });
    // With or without the slashes a person might leave or add.
    expect(parseServerHash('#event/club/x/')).toEqual({ kind: 'event', club: 'club', event: 'x' });
  });

  it('undoes what an address has to write another way', () => {
    expect(parseServerHash('#/event/north%20club/a%2Fb')).toEqual({
      kind: 'event',
      club: 'north club',
      event: 'a/b'
    });
  });

  it('takes a badly written address as the list', () => {
    expect(parseServerHash('#/event/club/%E0%A4%A')).toEqual({ kind: 'list' });
  });
});

describe('formatServerHash', () => {
  it('goes there and back', () => {
    for (const place of [
      { kind: 'list' } as const,
      { kind: 'event', club: 'club', event: 'friday-night-764c5x' } as const,
      { kind: 'event', club: 'north club', event: 'a/b?c#d' } as const,
      { kind: 'results' } as const,
      { kind: 'club', word: 'north club' } as const,
      { kind: 'clubs' } as const
    ]) {
      expect(parseServerHash(formatServerHash(place))).toEqual(place);
    }
    expect(formatServerHash({ kind: 'list' })).toBe('#/');
    expect(formatServerHash({ kind: 'results' })).toBe('#/results');
    // Nothing hangs off the results page.
    expect(parseServerHash('#/results/more')).toEqual({ kind: 'list' });
  });
});

describe('eventDetail', () => {
  it('gives the date and the place, whichever were given', () => {
    expect(eventDetail({ date: '2026-11-27', location: 'Main field' })).toBe(
      '2026-11-27 · Main field'
    );
    expect(eventDetail({ date: '2026-11-27' })).toBe('2026-11-27');
    expect(eventDetail({ location: 'Main field' })).toBe('Main field');
    expect(eventDetail({})).toBe('');
    expect(eventDetail({ date: '  ', location: ' Main field ' })).toBe('Main field');
  });
});

describe('pilotHere', () => {
  const NORTH = { id: 'north', name: 'North FPV' };
  const SOUTH = { id: 'south', name: 'South FPV' };
  const event = [
    { id: 'p-ace', callsign: 'Acey' },
    { id: 'p-bee', callsign: 'Bee' }
  ];
  const holds = (club: typeof NORTH, id: string, held = true) => ({
    club,
    id,
    callsign: 'As the club has it',
    held
  });

  it('is the pilot the account holds in that club, as the event names them', () => {
    expect(pilotHere({ pilots: [holds(NORTH, 'p-ace')] }, 'north', event)).toEqual({
      id: 'p-ace',
      callsign: 'Acey'
    });
  });

  it('is nobody for someone who is only looking', () => {
    expect(pilotHere(undefined, 'north', event)).toBeUndefined();
    expect(pilotHere(null, 'north', event)).toBeUndefined();
    expect(pilotHere({ pilots: [] }, 'north', event)).toBeUndefined();
  });

  it("is nobody when the pilot is another club's, is not in the event, or is only asked for", () => {
    // The same id in another club is another pilot.
    expect(pilotHere({ pilots: [holds(SOUTH, 'p-ace')] }, 'north', event)).toBeUndefined();
    expect(pilotHere({ pilots: [holds(NORTH, 'p-zed')] }, 'north', event)).toBeUndefined();
    expect(pilotHere({ pilots: [holds(NORTH, 'p-ace', false)] }, 'north', event)).toBeUndefined();
  });

  it('picks the one that is in the event when the account holds several', () => {
    const account = {
      pilots: [holds(NORTH, 'p-zed'), holds(SOUTH, 'p-ace'), holds(NORTH, 'p-bee')]
    };
    expect(pilotHere(account, 'north', event)?.id).toBe('p-bee');
  });
});
