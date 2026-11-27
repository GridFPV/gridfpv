/** The words and small decisions of the Server page (`lib/serverLink.ts`). */
import { describe, expect, it } from 'vitest';
import type { LinkClub, LinkEvent, LinkView } from '@gridfpv/types';
import {
  certificateVerdict,
  changed,
  clubById,
  clubName,
  eventLabel,
  eventNote,
  eventTone,
  eventsOf,
  fingerprintRows,
  formOf,
  formProblem,
  linkLabel,
  linkLine,
  linkProblem,
  linkTone,
  openEvent,
  refreshEvery,
  settingsOf,
  trustOf
} from '../src/lib/serverLink.js';

const RECON: LinkClub = {
  id: 'k1',
  name: '',
  url: 'https://grid.example.org',
  trust: 'System',
  club: { id: 'club', name: 'Recon FPV' }
};
const NORTH: LinkClub = {
  id: 'k2',
  name: 'North (mine)',
  url: 'https://north.example.org',
  trust: 'System',
  club: { id: 'c9', name: 'North FPV' }
};
const event = (over: Partial<LinkEvent> = {}): LinkEvent => ({
  id: 'friday',
  name: 'Friday Night',
  club: 'k1',
  len: 10,
  sent: 10,
  state: 'Sent',
  ...over
});
/** Friday is open and goes to Recon, which is connected. */
const view = (over: Partial<LinkView> = {}): LinkView => ({
  clubs: [RECON, NORTH],
  open: 'friday',
  club: 'k1',
  state: 'Connected',
  events: [event()],
  ...over
});
/** Friday is open and goes to no club. */
const notSent = (over: Partial<LinkView> = {}): LinkView => {
  const { club: _club, ...rest } = view({
    state: 'NotSet',
    events: [event({ club: undefined, state: 'NoClub', len: 0, sent: 0 })],
    ...over
  });
  return rest;
};
const PEM = '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----';

describe('what a saved club is called', () => {
  it("uses the director's name, then the club's own, then the address", () => {
    expect(clubName(NORTH)).toBe('North (mine)');
    expect(clubName(RECON)).toBe('Recon FPV');
    expect(clubName({ ...RECON, name: '   ' })).toBe('Recon FPV');
    // Never heard from the server, and not named: the address stands in.
    expect(
      clubName({ id: 'k3', name: '', url: 'https://grid.example.org/', trust: 'System' })
    ).toBe('grid.example.org');
    expect(clubName({ id: 'k3', name: '', url: 'http://localhost:8470', trust: 'System' })).toBe(
      'localhost:8470'
    );
  });

  it('is never the id', () => {
    for (const club of [RECON, NORTH, { ...RECON, club: undefined }]) {
      expect(clubName(club)).not.toContain(club.id);
    }
  });

  it('finds a saved club by its id, and nothing for one that is gone', () => {
    expect(clubById(view(), 'k2')).toBe(NORTH);
    expect(clubById(view(), 'gone')).toBeUndefined();
    expect(clubById(view(), undefined)).toBeUndefined();
  });
});

describe('the link in words', () => {
  it('names each state, and the club once connected', () => {
    expect(linkLabel(notSent())).toBe('Not sent');
    expect(linkLabel(notSent({ open: undefined, events: [] }))).toBe('No event open');
    expect(linkLabel(view({ state: 'Connecting' }))).toBe('Connecting');
    expect(linkLabel(view())).toBe('Connected to Recon FPV');
    expect(linkLabel(view({ club: 'k2' }))).toBe('Connected to North (mine)');
    // The club was removed a moment ago: still a sentence.
    expect(linkLabel(view({ club: 'gone' }))).toBe('Connected');
    expect(linkLabel(view({ state: { Failed: { message: 'x' } } }))).toBe('Not connected');
  });

  it('colours each state', () => {
    expect(linkTone('Connected')).toBe('success');
    expect(linkTone('Connecting')).toBe('info');
    expect(linkTone('NotSet')).toBe('neutral');
    expect(linkTone({ Failed: { message: 'x' } })).toBe('danger');
  });

  it('gives the reason only when there is one', () => {
    expect(linkProblem({ Failed: { message: 'The server could not be reached.' } })).toBe(
      'The server could not be reached.'
    );
    expect(linkProblem('Connected')).toBeUndefined();
    expect(linkProblem('NotSet')).toBeUndefined();
  });

  it('finds the open event', () => {
    expect(openEvent(view())?.name).toBe('Friday Night');
    expect(openEvent(view({ open: undefined }))).toBeUndefined();
    expect(openEvent(view({ open: 'gone' }))).toBeUndefined();
  });

  it('says where the open event goes, and what to do when it goes nowhere', () => {
    expect(linkLine(view())).toBe('Friday Night is sent to Recon FPV.');
    expect(linkLine(notSent())).toBe(
      'Friday Night is not sent to a server. Pick a club for it under Events.'
    );
    expect(linkLine(notSent({ clubs: [] }))).toBe(
      'Friday Night is not sent to a server. Save a club first.'
    );
    expect(linkLine(notSent({ open: undefined, events: [] }))).toContain('Open an event');
    // An event is named, never shown by its id.
    expect(linkLine(view())).not.toContain('friday');
    expect(linkLine(view())).not.toContain('k1');
  });
});

describe('an event in words', () => {
  it('names each state', () => {
    expect(eventLabel(event())).toBe('On the server');
    expect(eventLabel(event({ state: 'Sending', sent: 3, len: 10 }))).toBe('Sending, 3 of 10');
    expect(eventLabel(event({ state: 'Different' }))).toBe('A different event is on the server');
    expect(eventLabel(event({ state: 'Removed' }))).toBe('Removed on the server');
    expect(eventLabel(event({ state: 'NoClub' }))).toBe('Not sent');
    expect(eventLabel(event({ state: 'Waiting' }))).toBe('Waiting');
    expect(eventLabel(event({ state: { Failed: { message: 'x' } } }))).toBe('Could not be sent');
  });

  it('colours each state', () => {
    expect(eventTone(event())).toBe('success');
    expect(eventTone(event({ state: 'Sending' }))).toBe('info');
    expect(eventTone(event({ state: 'Different' }))).toBe('warn');
    expect(eventTone(event({ state: 'Removed' }))).toBe('warn');
    expect(eventTone(event({ state: 'NoClub' }))).toBe('neutral');
    expect(eventTone(event({ state: 'Waiting' }))).toBe('neutral');
    expect(eventTone(event({ state: { Failed: { message: 'x' } } }))).toBe('danger');
  });

  it('says why an event could not be sent', () => {
    expect(eventNote(event({ state: { Failed: { message: 'disk full' } } }), view())).toBe(
      'disk full'
    );
    expect(eventNote(event(), view())).toBeUndefined();
    expect(eventNote(event({ state: 'NoClub', club: undefined }), view())).toBeUndefined();
  });

  it("says what an event of another club is waiting for, by the club's name", () => {
    const sunday = event({ id: 'sunday', name: 'Sunday Cup', club: 'k2', state: 'Waiting' });
    expect(eventNote(sunday, view())).toBe('Sent when an event of North (mine) is open.');
    // An event of the connected club that waits is only waiting its turn.
    expect(eventNote(event({ state: 'Waiting' }), view())).toBeUndefined();
    // With nothing open, every event with a club is waiting for one to be opened.
    expect(eventNote(event({ state: 'Waiting' }), notSent({ open: undefined }))).toBe(
      'Sent when an event of Recon FPV is open.'
    );
    // A club that is gone is not named by its id.
    expect(eventNote({ ...sunday, club: 'gone' }, view())).toBe(
      'Sent when an event of its club is open.'
    );
  });

  it('counts the events that go to a club', () => {
    const all = view({
      events: [
        event(),
        event({ id: 'a', club: 'k1' }),
        event({ id: 'b', club: 'k2' }),
        event({ id: 'c', club: undefined })
      ]
    });
    expect(eventsOf(all, 'k1')).toBe(2);
    expect(eventsOf(all, 'k2')).toBe(1);
    expect(eventsOf(all, 'k3')).toBe(0);
  });
});

describe('how often to look again', () => {
  it('looks often while something is moving, and seldom once settled', () => {
    expect(refreshEvery(undefined)).toBe(5000);
    expect(refreshEvery(notSent())).toBe(5000);
    expect(refreshEvery(view())).toBe(4000);
    expect(refreshEvery(view({ events: [event({ state: 'Removed' })] }))).toBe(4000);
    expect(refreshEvery(view({ state: 'Connecting' }))).toBe(1000);
    expect(refreshEvery(view({ events: [event({ state: 'Sending' })] }))).toBe(1000);
    expect(refreshEvery(view({ state: { Failed: { message: 'x' } } }))).toBe(1000);
  });

  it('is not kept busy by events of a club that is not connected', () => {
    const waiting = event({ id: 'sunday', club: 'k2', state: 'Waiting' });
    const nowhere = event({ id: 'garage', club: undefined, state: 'NoClub' });
    expect(refreshEvery(view({ events: [event(), waiting, nowhere] }))).toBe(4000);
    // One of the connected club's own that still waits is.
    expect(refreshEvery(view({ events: [event({ state: 'Waiting' })] }))).toBe(1000);
  });
});

describe('the club form', () => {
  it('starts empty for a new club, and from the saved club for a change', () => {
    expect(formOf(undefined)).toEqual({ name: '', url: '', token: '', extra: false, pem: '' });
    expect(formOf(NORTH)).toEqual({
      name: 'North (mine)',
      url: 'https://north.example.org',
      token: '',
      extra: false,
      pem: ''
    });
    const own = formOf({ ...RECON, trust: { ExtraAuthorities: { pem: PEM } } });
    expect(own.extra).toBe(true);
    expect(own.pem).toBe(PEM);
  });

  it('sends the token only when one was typed, and tidies the rest', () => {
    const form = { ...formOf(RECON), name: '  Mine ', url: ' https://grid.example.org/ ' };
    expect(settingsOf(form)).toEqual({
      name: 'Mine',
      url: 'https://grid.example.org',
      trust: 'System'
    });
    expect('token' in settingsOf(form)).toBe(false);
    expect(settingsOf({ ...form, token: '  gfs_abc\n' }).token).toBe('gfs_abc');
  });

  it('asks for extra trust only when the box is ticked and holds something', () => {
    expect(trustOf({ ...formOf(RECON), extra: true, pem: `\n${PEM}\n` })).toEqual({
      ExtraAuthorities: { pem: PEM }
    });
    expect(trustOf({ ...formOf(RECON), extra: true, pem: '  ' })).toBe('System');
    expect(trustOf({ ...formOf(RECON), extra: false, pem: PEM })).toBe('System');
  });

  it('knows whether anything was changed', () => {
    expect(changed(formOf(RECON), RECON)).toBe(false);
    // A slash after the same address is not a change.
    expect(changed({ ...formOf(RECON), url: 'https://grid.example.org/' }, RECON)).toBe(false);
    expect(changed({ ...formOf(RECON), url: 'https://other.example.org' }, RECON)).toBe(true);
    expect(changed({ ...formOf(RECON), token: 'gfs_new' }, RECON)).toBe(true);
    expect(changed({ ...formOf(RECON), name: 'Mine' }, RECON)).toBe(true);
    expect(changed({ ...formOf(RECON), extra: true, pem: PEM }, RECON)).toBe(true);
    // Ticking the box with nothing in it changes nothing.
    expect(changed({ ...formOf(RECON), extra: true }, RECON)).toBe(false);
    // A new club is always something to save.
    expect(changed(formOf(undefined), undefined)).toBe(true);
  });

  it('says what is wrong with a new club', () => {
    const blank = formOf(undefined);
    expect(formProblem(blank, undefined)).toContain('address');
    expect(formProblem({ ...blank, url: 'grid.example.org' }, undefined)).toContain('https://');
    expect(formProblem({ ...blank, url: 'https://grid.example.org' }, undefined)).toContain(
      'Give the link token'
    );
    expect(
      formProblem({ ...blank, url: 'https://grid.example.org', token: 'nope' }, undefined)
    ).toContain('gfs_');
    const good = { ...blank, url: 'https://grid.example.org', token: 'gfs_abc' };
    expect(formProblem(good, undefined)).toBeUndefined();
    // A server on this same computer may use http.
    expect(formProblem({ ...good, url: 'http://localhost:8470' }, undefined)).toBeUndefined();
    expect(formProblem({ ...good, name: 'x'.repeat(61) }, undefined)).toContain('too long');
    expect(formProblem({ ...good, name: 'é'.repeat(60) }, undefined)).toBeUndefined();
    expect(formProblem({ ...good, extra: true, pem: 'hello' }, undefined)).toContain(
      'not a certificate'
    );
    expect(formProblem({ ...good, extra: true, pem: PEM }, undefined)).toBeUndefined();
    expect(formProblem({ ...good, extra: true, pem: '' }, undefined)).toBeUndefined();
  });

  it('keeps the token of a saved club, but not for another address', () => {
    expect(formProblem(formOf(RECON), RECON)).toBeUndefined();
    expect(formProblem({ ...formOf(RECON), url: 'https://grid.example.org/' }, RECON)).toBe(
      undefined
    );
    const moved = { ...formOf(RECON), url: 'https://other.example.org' };
    expect(formProblem(moved, RECON)).toContain('not sent to another server');
    expect(formProblem({ ...moved, token: 'gfs_new' }, RECON)).toBeUndefined();
    expect(formProblem({ ...formOf(RECON), url: '' }, RECON)).toContain('address');
  });
});

describe("the server's certificate", () => {
  it('says whether it needs accepting, and whether accepting would help', () => {
    expect(certificateVerdict({ trusted: true, stands_alone: false })).toBe('trusted');
    expect(certificateVerdict({ trusted: true, stands_alone: true })).toBe('trusted');
    expect(certificateVerdict({ trusted: false, stands_alone: true })).toBe('accept');
    expect(certificateVerdict({ trusted: false, stands_alone: false })).toBe('authority');
  });

  it('breaks a fingerprint into rows short enough to read out', () => {
    const fingerprint = Array.from({ length: 32 }, (_, i) =>
      i.toString(16).toUpperCase().padStart(2, '0')
    ).join(':');
    const rows = fingerprintRows(fingerprint);
    expect(rows).toHaveLength(4);
    expect(rows[0]).toBe('00:01:02:03:04:05:06:07');
    expect(rows.join(':')).toBe(fingerprint);
    expect(fingerprintRows('')).toEqual([]);
    expect(fingerprintRows('AB:CD')).toEqual(['AB:CD']);
  });
});
