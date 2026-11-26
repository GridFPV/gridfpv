/** The small decisions of the page a remote server shows (`lib/serverHome.ts`). */
import { describe, expect, it } from 'vitest';
import { eventDetail, formatServerHash, parseServerHash } from '../src/lib/serverHome.js';

describe('parseServerHash', () => {
  it('reads the list from an empty address', () => {
    for (const hash of ['', '#', '#/', '#/nothing', '#/e', '#/e/club', '#/e/club/event/more']) {
      expect(parseServerHash(hash)).toEqual({ kind: 'list' });
    }
  });

  it('reads one event of one club', () => {
    expect(parseServerHash('#/e/club/friday-night-764c5x')).toEqual({
      kind: 'event',
      club: 'club',
      event: 'friday-night-764c5x'
    });
    // With or without the slashes a person might leave or add.
    expect(parseServerHash('#e/club/x/')).toEqual({ kind: 'event', club: 'club', event: 'x' });
  });

  it('undoes what an address has to write another way', () => {
    expect(parseServerHash('#/e/north%20club/a%2Fb')).toEqual({
      kind: 'event',
      club: 'north club',
      event: 'a/b'
    });
  });

  it('takes a badly written address as the list', () => {
    expect(parseServerHash('#/e/club/%E0%A4%A')).toEqual({ kind: 'list' });
  });
});

describe('formatServerHash', () => {
  it('goes there and back', () => {
    for (const place of [
      { kind: 'list' } as const,
      { kind: 'event', club: 'club', event: 'friday-night-764c5x' } as const,
      { kind: 'event', club: 'north club', event: 'a/b?c#d' } as const
    ]) {
      expect(parseServerHash(formatServerHash(place))).toEqual(place);
    }
    expect(formatServerHash({ kind: 'list' })).toBe('#/');
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
