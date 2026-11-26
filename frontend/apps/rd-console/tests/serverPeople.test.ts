/** The words and small decisions of the account pages (`lib/serverPeople.ts`), and their routes. */
import { describe, expect, it } from 'vitest';
import type { InviteView, MyAccount } from '@gridfpv/types';
import { formatServerHash, parseServerHash, type ServerPlace } from '../src/lib/serverHome.js';
import {
  inviteLink,
  inviteStanding,
  inviteWords,
  managedClubs,
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
    expect(parseServerHash('#/account')).toEqual({ kind: 'account' });
    expect(parseServerHash('#/club/north')).toEqual({ kind: 'club', club: 'north' });
    // The server sends a browser back to `#/` after signing in.
    expect(parseServerHash('#/')).toEqual({ kind: 'list' });
  });

  it('takes anything that is not quite a page as the list', () => {
    for (const hash of [
      '#/invite',
      '#/invite/a/b',
      '#/club',
      '#/club/a/b',
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
      { kind: 'account' },
      { kind: 'club', club: 'north club/1' }
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

describe('managedClubs', () => {
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
    ...over
  });

  it('is where the account is a director, and every club for an owner', () => {
    expect(managedClubs(undefined, all)).toEqual([]);
    expect(managedClubs(account({}), all)).toEqual([]);
    const mixed = account({
      clubs: [
        { club: all[0], role: 'Director' },
        { club: all[1], role: 'Pilot' },
        { club: all[2], role: 'Approver' }
      ]
    });
    expect(managedClubs(mixed, all)).toEqual([all[0]]);
    expect(managedClubs(account({ owner: true }), all)).toEqual(all);
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
