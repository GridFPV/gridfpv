/** The words and small decisions of the account pages (`lib/serverPeople.ts`), and their routes. */
import { describe, expect, it } from 'vitest';
import type { InviteView, MyAccount } from '@gridfpv/types';
import { formatServerHash, parseServerHash, type ServerPlace } from '../src/lib/serverHome.js';
import {
  inviteLink,
  inviteStanding,
  inviteWords,
  lookedAfterClubs,
  pilotStanding,
  placeWords,
  sayIn,
  providerName,
  roleWord,
  signInProblem
} from '../src/lib/serverPeople.js';

const NOW = 1_800_000_000_000;
const invite = (over: Partial<InviteView> = {}): InviteView => ({
  id: 'abc',
  kind: 'Personal',
  club: { id: 'north', name: 'North FPV' },
  role: 'Pilot',
  label: 'Ace',
  max_uses: 1,
  uses: 0,
  revoked: false,
  works: true,
  ...over
});

describe('the account pages have addresses', () => {
  it('reads each page from its address', () => {
    expect(parseServerHash('#/sign-in')).toEqual({ kind: 'sign-in' });
    expect(parseServerHash('#/sign-in?problem=need-invite')).toEqual({
      kind: 'sign-in',
      problem: 'need-invite'
    });
    expect(parseServerHash('#/invite/gfi_abc-DEF_123')).toEqual({
      kind: 'invite',
      secret: 'gfi_abc-DEF_123'
    });
    expect(parseServerHash('#/account')).toEqual({ kind: 'account', page: 'profile' });
    expect(parseServerHash('#/account/clubs')).toEqual({ kind: 'account', page: 'clubs' });
    expect(parseServerHash('#/account/sign-in')).toEqual({ kind: 'account', page: 'sign-in' });
    expect(parseServerHash('#/manage/north')).toEqual({
      kind: 'manage',
      club: 'north',
      page: 'members'
    });
    expect(parseServerHash('#/manage/north/tokens')).toEqual({
      kind: 'manage',
      club: 'north',
      page: 'tokens'
    });
    expect(parseServerHash('#/admin')).toEqual({ kind: 'admin', page: 'clubs' });
    expect(parseServerHash('#/admin/email')).toEqual({ kind: 'admin', page: 'email' });
    // The server sends a browser back to `#/` after signing in.
    expect(parseServerHash('#/')).toEqual({ kind: 'list' });
  });

  it('takes anything that is not quite a page as the list', () => {
    for (const hash of [
      '#/invite',
      '#/invite/a/b',
      '#/club',
      '#/manage',
      '#/manage/a/b',
      '#/manage/a/members/b',
      '#/admin/x',
      '#/admin/clubs/x',
      '#/account/x',
      '#/sign-in/x',
      '#/Account',
      '#/invite/%E0%A4%A'
    ]) {
      expect(parseServerHash(hash)).toEqual({ kind: 'list' });
    }
    // An empty problem is no problem.
    expect(parseServerHash('#/sign-in?problem=')).toEqual({ kind: 'sign-in' });
    expect(parseServerHash('#/sign-in?other=1')).toEqual({ kind: 'sign-in' });
  });

  it('goes there and back', () => {
    const places: ServerPlace[] = [
      { kind: 'sign-in' },
      { kind: 'sign-in', problem: 'taken' },
      { kind: 'invite', secret: 'gfi_abc-DEF_123' },
      { kind: 'reconnect', secret: 'gfr_abc-DEF_123' },
      { kind: 'account', page: 'profile' },
      { kind: 'account', page: 'clubs' },
      { kind: 'account', page: 'sign-in' },
      { kind: 'manage', club: 'north club/1', page: 'members' },
      { kind: 'manage', club: 'north', page: 'settings' },
      { kind: 'admin', page: 'clubs' },
      { kind: 'admin', page: 'sign-in' }
    ];
    for (const place of places) expect(parseServerHash(formatServerHash(place))).toEqual(place);
  });
});

describe('signInProblem', () => {
  it('has words for every code the server sends, and none for no code', () => {
    expect(signInProblem(undefined)).toBeUndefined();
    expect(signInProblem('')).toBeUndefined();
    const codes = [
      'need-invite',
      'invite',
      'link',
      'refused',
      'expired',
      'taken',
      'not-signed-in',
      'not-set-up',
      'provider'
    ];
    const words = codes.map((code) => signInProblem(code));
    for (const text of words) expect(text && text.length > 10).toBe(true);
    expect(signInProblem('need-invite')).toContain('invite');
    expect(signInProblem('taken')).toContain('another account');
    expect(signInProblem('link')).toContain('re-connect link');
    // A code this build has never heard of still says something, and never the code itself.
    expect(signInProblem('<script>')).toBe('Signing in did not work. Try again in a moment.');
  });
});

describe('an invite in words', () => {
  it('says what it gives', () => {
    expect(inviteWords(invite())).toBe('join North FPV');
    expect(inviteWords(invite({ role: 'Director' }))).toBe('join North FPV as a director');
    expect(inviteWords(invite({ role: 'Approver' }))).toBe('join North FPV as an approver');
    expect(inviteWords(invite({ kind: 'Group' }))).toBe('join North FPV');
    expect(inviteWords({ kind: 'Owner' })).toBe('be an owner of this server');
    // A club that is gone from the server.
    expect(inviteWords({ kind: 'Personal', role: 'Pilot' })).toBe('join a club');
  });

  it('says how it stands', () => {
    expect(inviteStanding(invite(), NOW)).toBe('Not used yet');
    expect(inviteStanding(invite({ uses: 1, works: false }), NOW)).toBe('Used');
    expect(inviteStanding(invite({ kind: 'Group', max_uses: 20, uses: 3 }), NOW)).toBe(
      '3 of 20 used'
    );
    expect(inviteStanding(invite({ kind: 'Group', max_uses: 20, uses: 20 }), NOW)).toBe('Full');
    expect(inviteStanding(invite({ expires_at: NOW }), NOW)).toBe('Ran out');
    expect(inviteStanding(invite({ expires_at: NOW + 1 }), NOW)).toBe('Not used yet');
    // Taken back wins over everything else.
    expect(inviteStanding(invite({ revoked: true, uses: 1, expires_at: NOW - 1 }), NOW)).toBe(
      'Taken back'
    );
  });

  it('makes a link a person can open', () => {
    expect(inviteLink('https://grid.example.org', 'gfi_abc-DEF_1')).toBe(
      'https://grid.example.org/#/invite/gfi_abc-DEF_1'
    );
    expect(inviteLink('https://grid.example.org/', 'gfi_x')).toBe(
      'https://grid.example.org/#/invite/gfi_x'
    );
    const link = inviteLink('https://grid.example.org', 'gfi_x');
    expect(parseServerHash(new URL(link).hash)).toEqual({ kind: 'invite', secret: 'gfi_x' });
  });
});

describe('lookedAfterClubs', () => {
  const all = [
    { id: 'north', name: 'North' },
    { id: 'south', name: 'South' },
    { id: 'east', name: 'East' }
  ];
  const account = (over: Partial<MyAccount>): MyAccount => ({
    id: 'acct_1',
    name: 'Ace',
    owner: false,
    sign_ins: [],
    clubs: [],
    pilots: [],
    profile: {},
    join_asks: [],
    ...over
  });

  it('is where the account is a director or an approver, and every club for an owner', () => {
    expect(lookedAfterClubs(undefined, all)).toEqual([]);
    expect(lookedAfterClubs(account({}), all)).toEqual([]);
    const mixed = account({
      clubs: [
        { club: all[0], role: 'Director' },
        { club: all[1], role: 'Pilot' },
        { club: all[2], role: 'Approver' }
      ]
    });
    // A director runs the club. An approver only answers claims there. A pilot does neither.
    expect(lookedAfterClubs(mixed, all)).toEqual([
      { club: all[0], runs: true },
      { club: all[2], runs: false }
    ]);
    expect(lookedAfterClubs(account({ owner: true }), all)).toEqual(
      all.map((club) => ({ club, runs: true }))
    );
    expect(sayIn(mixed, 'north')).toBe('runs');
    expect(sayIn(mixed, 'south')).toBeUndefined();
    expect(sayIn(mixed, 'east')).toBe('answers');
    expect(sayIn(mixed, 'nowhere')).toBeUndefined();
    expect(sayIn(undefined, 'north')).toBeUndefined();
    // An owner runs every club, the ones they are not in too.
    expect(sayIn(account({ owner: true }), 'nowhere')).toBe('runs');
  });
});

describe('pilots and results in words', () => {
  it('says where a pilot stands, and never gives a place racing has not decided', () => {
    expect(placeWords({ position: 1, of: 12, placed: true })).toBe('1st of 12');
    expect(placeWords({ position: 2, of: 2, placed: true })).toBe('2nd of 2');
    expect(placeWords({ position: 11, of: 30, placed: true })).toBe('11th of 30');
    // Before racing places anyone, the position is only the order of the list.
    expect(placeWords({ position: 1, of: 12, placed: false })).toBe('Not placed yet');
  });

  it('says how a pilot stands for the one looking', () => {
    expect(pilotStanding({ mine: true, asked: false, taken: true })).toBe('Yours');
    expect(pilotStanding({ mine: false, asked: true, taken: false })).toBe('You asked');
    expect(pilotStanding({ mine: false, asked: false, taken: true })).toBe('Taken');
    expect(pilotStanding({ mine: false, asked: false, taken: false })).toBe('');
  });
});

describe('small words', () => {
  it('names a role and a provider', () => {
    expect(roleWord('Pilot')).toBe('Pilot');
    expect(roleWord('Approver')).toBe('Approver');
    expect(roleWord('Director')).toBe('Director');
    const providers = [{ id: 'discord', name: 'Discord' }];
    expect(providerName('discord', providers)).toBe('Discord');
    // One the server no longer offers is shown by what it was kept as.
    expect(providerName('google', providers)).toBe('google');
  });
});
