import { describe, expect, it } from 'vitest';
import type { LinkPeople, LinkView, Pilot } from '@gridfpv/types';
import { addWords, heldElsewhere, serverLinked, toBringIn } from '../src/lib/serverPilots.js';

/** The link as it stands with Friday open. `sent` says whether Friday goes to a club. */
const view = (state: LinkView['state'], sent = true): LinkView => ({
  clubs: [{ id: 'k1', name: '', url: 'https://grid.example.org', trust: 'System' }],
  open: 'friday',
  ...(sent ? { club: 'k1' } : {}),
  state,
  events: []
});

const PIA = { id: 'acct_pia', name: 'Pia' };
const QUIN = { id: 'acct_quin', name: 'Quin' };
const RAY = { id: 'acct_ray', name: 'Ray' };
const mine = (id: string, callsign: string, over: Partial<Pilot> = {}): Pilot => ({
  id,
  callsign,
  vtx_types: [],
  ...over
});

describe('serverLinked', () => {
  it('is so once the open event goes to a club, answering or not', () => {
    expect(serverLinked(undefined)).toBe(false);
    // A club is saved, and the open event does not go to it.
    expect(serverLinked(view('NotSet', false))).toBe(false);
    // No event is open.
    expect(serverLinked({ clubs: view('NotSet').clubs, state: 'NotSet', events: [] })).toBe(false);
    expect(serverLinked(view('Connecting'))).toBe(true);
    expect(serverLinked(view('Connected'))).toBe(true);
    // A server that is away is still the one that is linked: asking it says what is wrong.
    expect(serverLinked(view({ Failed: { message: 'away' } }))).toBe(true);
  });
});

describe('toBringIn', () => {
  const people: LinkPeople = {
    members: [PIA, QUIN, RAY],
    pilots: [
      { id: 'cat-1', callsign: 'Cat', account: PIA },
      { id: 'dog-2', callsign: 'Dog' },
      { id: 'eel-3', callsign: 'Eel' }
    ]
  };

  it('offers the pilots the server has and this Director does not', () => {
    const found = toBringIn(people, [mine('dog-2', 'Dog')]);
    expect(found.pilots.map((p) => p.callsign)).toEqual(['Cat', 'Eel']);
  });

  it('offers a new pilot only for a member who holds none, there or here', () => {
    // Pia holds Cat on the server. Quin is tied to a pilot here that the server has not
    // heard of yet. Only Ray has no pilot anywhere.
    const found = toBringIn(people, [mine('quin-9', 'Quin', { server_account: QUIN })]);
    expect(found.members).toEqual([RAY]);
  });

  it('offers nothing when everything is here', () => {
    const all: LinkPeople = {
      members: [PIA],
      pilots: [{ id: 'cat-1', callsign: 'Cat', account: PIA }]
    };
    expect(toBringIn(all, [mine('cat-1', 'Cat')])).toEqual({ pilots: [], members: [] });
    expect(toBringIn({ members: [], pilots: [] }, [])).toEqual({ pilots: [], members: [] });
  });
});

describe('heldElsewhere', () => {
  it('names the other pilot a member already holds, and not the one being set', () => {
    const people: LinkPeople = {
      members: [PIA, QUIN],
      pilots: [
        { id: 'cat-1', callsign: 'Cat', account: PIA },
        { id: 'dog-2', callsign: 'Dog', account: QUIN },
        { id: 'eel-3', callsign: 'Eel' }
      ]
    };
    expect([...heldElsewhere(people, 'cat-1')]).toEqual([['acct_quin', 'Dog']]);
    expect([...heldElsewhere(people, 'eel-3')]).toEqual([
      ['acct_pia', 'Cat'],
      ['acct_quin', 'Dog']
    ]);
  });
});

describe('addWords', () => {
  it('counts what would be added', () => {
    expect(addWords(0)).toBe('Add pilots');
    expect(addWords(1)).toBe('Add 1 pilot');
    expect(addWords(7)).toBe('Add 7 pilots');
  });
});
