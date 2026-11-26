/** The words and small decisions of the Server page (`lib/serverLink.ts`). */
import { describe, expect, it } from 'vitest';
import type { LinkEvent, LinkView } from '@gridfpv/types';
import {
  changed,
  eventLabel,
  eventProblem,
  eventTone,
  formOf,
  formProblem,
  linkLabel,
  linkProblem,
  linkTone,
  refreshEvery,
  settingsOf
} from '../src/lib/serverLink.js';

const view = (over: Partial<LinkView> = {}): LinkView => ({
  url: 'https://grid.example.org',
  has_token: true,
  on: true,
  trust: 'System',
  state: 'Connected',
  events: [],
  ...over
});
const event = (over: Partial<LinkEvent> = {}): LinkEvent => ({
  id: 'friday',
  name: 'Friday',
  len: 10,
  sent: 10,
  state: 'Sent',
  ...over
});
const PEM = '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----';

describe('the link in words', () => {
  it('names each state, and the club once connected', () => {
    expect(linkLabel(view({ state: 'NotSet' }))).toBe('Not set');
    expect(linkLabel(view({ state: 'Off' }))).toBe('Off');
    expect(linkLabel(view({ state: 'Connecting' }))).toBe('Connecting');
    expect(linkLabel(view())).toBe('Connected');
    expect(linkLabel(view({ club: { id: 'club', name: 'Recon FPV' } }))).toBe(
      'Connected to Recon FPV'
    );
    expect(linkLabel(view({ state: { Failed: { message: 'x' } } }))).toBe('Not connected');
  });

  it('colours each state', () => {
    expect(linkTone('Connected')).toBe('success');
    expect(linkTone('Connecting')).toBe('info');
    expect(linkTone('Off')).toBe('neutral');
    expect(linkTone('NotSet')).toBe('neutral');
    expect(linkTone({ Failed: { message: 'x' } })).toBe('danger');
  });

  it('gives the reason only when there is one', () => {
    expect(linkProblem({ Failed: { message: 'The server could not be reached.' } })).toBe(
      'The server could not be reached.'
    );
    for (const state of ['NotSet', 'Off', 'Connecting', 'Connected'] as const) {
      expect(linkProblem(state)).toBeUndefined();
    }
  });
});

describe('an event in words', () => {
  it('says where it stands', () => {
    expect(eventLabel(event())).toBe('On the server');
    expect(eventLabel(event({ state: 'Sending', sent: 4 }))).toBe('Sending, 4 of 10');
    expect(eventLabel(event({ state: 'Different' }))).toBe('A different event is on the server');
    expect(eventLabel(event({ state: { Failed: { message: 'x' } } }))).toBe('Not sent');
  });

  it('colours it, and gives a failure its reason', () => {
    expect(eventTone(event())).toBe('success');
    expect(eventTone(event({ state: 'Sending' }))).toBe('info');
    expect(eventTone(event({ state: 'Different' }))).toBe('warn');
    const failed = event({ state: { Failed: { message: 'It will be sent again.' } } });
    expect(eventTone(failed)).toBe('danger');
    expect(eventProblem(failed)).toBe('It will be sent again.');
    expect(eventProblem(event())).toBeUndefined();
    expect(eventProblem(event({ state: 'Different' }))).toBeUndefined();
  });
});

describe('refreshEvery', () => {
  it('looks often while something is moving, and seldom when settled or off', () => {
    expect(refreshEvery(undefined)).toBe(5000);
    expect(refreshEvery(view({ on: false, state: 'Off' }))).toBe(5000);
    expect(refreshEvery(view({ events: [event()] }))).toBe(4000);
    expect(refreshEvery(view({ state: 'Connecting' }))).toBe(1000);
    expect(refreshEvery(view({ state: { Failed: { message: 'x' } } }))).toBe(1000);
    expect(refreshEvery(view({ events: [event(), event({ id: 'b', state: 'Sending' })] }))).toBe(
      1000
    );
    expect(refreshEvery(view({ events: [event({ state: 'Different' })] }))).toBe(1000);
  });
});

describe('the form', () => {
  it('starts from what is set, with the token box empty', () => {
    expect(formOf(view())).toEqual({
      url: 'https://grid.example.org',
      token: '',
      on: true,
      extra: false,
      pem: ''
    });
    expect(formOf(view({ trust: { ExtraAuthorities: { pem: PEM } } }))).toMatchObject({
      extra: true,
      pem: PEM
    });
  });

  it('leaves the token out when the box is empty, so the one set is kept', () => {
    const form = formOf(view());
    expect(settingsOf(form)).toEqual({
      url: 'https://grid.example.org',
      on: true,
      trust: 'System'
    });
    expect('token' in settingsOf(form)).toBe(false);
    expect(settingsOf({ ...form, token: '  gfs_new \n' }).token).toBe('gfs_new');
    expect(settingsOf({ ...form, url: '  https://b.example/ ' }).url).toBe('https://b.example');
  });

  it('sends extra authorities only when ticked and filled', () => {
    const form = formOf(view());
    expect(settingsOf({ ...form, extra: true, pem: `\n${PEM}\n` }).trust).toEqual({
      ExtraAuthorities: { pem: PEM }
    });
    expect(settingsOf({ ...form, extra: true, pem: '   ' }).trust).toBe('System');
    // Unticked: what was pasted is not sent.
    expect(settingsOf({ ...form, extra: false, pem: PEM }).trust).toBe('System');
  });

  it('knows when it differs from what is set', () => {
    const set = view();
    const form = formOf(set);
    expect(changed(form, set)).toBe(false);
    expect(changed({ ...form, url: 'https://grid.example.org  ' }, set)).toBe(false);
    // A slash after the address is the same address, as the Director reads it.
    expect(changed({ ...form, url: 'https://grid.example.org/' }, set)).toBe(false);
    expect(formProblem({ ...form, url: 'https://grid.example.org/' }, set)).toBeUndefined();
    expect(changed({ ...form, url: 'https://b.example' }, set)).toBe(true);
    expect(changed({ ...form, token: 'gfs_x' }, set)).toBe(true);
    expect(changed({ ...form, extra: true, pem: PEM }, set)).toBe(true);
    // Ticked with nothing pasted is the same as not ticked.
    expect(changed({ ...form, extra: true }, set)).toBe(false);
    // The switch saves on its own: it is not an unsaved change.
    expect(changed({ ...form, on: false }, set)).toBe(false);
  });
});

describe('formProblem', () => {
  const set = view();
  const form = formOf(set);

  it('passes what is right', () => {
    expect(formProblem(form, set)).toBeUndefined();
    expect(formProblem({ ...form, token: 'gfs_abc' }, set)).toBeUndefined();
    expect(
      formProblem({ ...form, url: 'http://localhost:8470', token: 'gfs_abc' }, set)
    ).toBeUndefined();
    expect(formProblem({ ...form, extra: true, pem: PEM }, set)).toBeUndefined();
    // Nothing set and switched off is fine: there is nothing to send to.
    const empty = view({ url: '', has_token: false, on: false, state: 'NotSet' });
    expect(formProblem(formOf(empty), empty)).toBeUndefined();
  });

  it('catches an address with no https:// in front', () => {
    expect(formProblem({ ...form, url: 'grid.example.org' }, set)).toContain('https://');
  });

  it('catches something that is not a link token', () => {
    expect(formProblem({ ...form, token: 'K7QM-2XWD-9HRA' }, set)).toContain('gfs_');
  });

  it('will not switch on with no address or no token', () => {
    const empty = view({ url: '', has_token: false, on: false, state: 'NotSet' });
    const blank = formOf(empty);
    expect(formProblem({ ...blank, on: true }, empty)).toContain('first');
    expect(formProblem({ ...blank, on: true, url: 'https://a.example' }, empty)).toContain('first');
    expect(
      formProblem({ ...blank, on: true, url: 'https://a.example', token: 'gfs_x' }, empty)
    ).toBeUndefined();
  });

  it('wants the token again for another address, since the old one is not carried over', () => {
    expect(formProblem({ ...form, url: 'https://other.example' }, set)).toContain('first');
    expect(
      formProblem({ ...form, url: 'https://other.example', token: 'gfs_x' }, set)
    ).toBeUndefined();
    // Switched off, another address may be saved without one.
    expect(formProblem({ ...form, on: false, url: 'https://other.example' }, set)).toBeUndefined();
  });

  it('catches a pasted thing that is not a certificate', () => {
    expect(formProblem({ ...form, extra: true, pem: 'hello' }, set)).toContain('certificate');
    expect(formProblem({ ...form, extra: false, pem: 'hello' }, set)).toBeUndefined();
  });
});
