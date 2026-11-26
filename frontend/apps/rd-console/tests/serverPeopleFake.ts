/**
 * A stand-in for a remote server's accounts, for the page tests. It keeps the rules the
 * real one keeps closely enough to drive the pages: an invite works once, a club keeps a
 * director, a link is shown once.
 */
import { vi } from 'vitest';
import type {
  ClubRole,
  InviteMade,
  InviteRequest,
  InviteView,
  Me,
  MemberView,
  MyAccount,
  SignInProvider
} from '@gridfpv/types';
import type { PeopleApi } from '../src/lib/serverPeople.js';

export const NORTH = { id: 'north', name: 'North FPV' };
export const SOUTH = { id: 'south', name: 'South FPV' };
export const PROVIDERS: SignInProvider[] = [
  { id: 'discord', name: 'Discord' },
  { id: 'google', name: 'Google' }
];

export const ACE: MyAccount = {
  id: 'acct_ace',
  name: 'Ace',
  owner: false,
  sign_ins: [{ provider: 'discord', subject: '111', label: 'ace_fpv' }],
  clubs: [{ club: NORTH, role: 'Director' }]
};

export interface FakeServer {
  account: MyAccount | undefined;
  providers: SignInProvider[];
  members: Record<string, MemberView[]>;
  invites: Record<string, InviteView[]>;
  /** Invite links that work: secret to what they are for. */
  links: Record<string, InviteView>;
}

export function fakeServer(over: Partial<FakeServer> = {}): FakeServer {
  return {
    account: undefined,
    providers: PROVIDERS,
    members: {
      north: [
        { id: 'acct_ace', name: 'Ace', role: 'Director' },
        { id: 'acct_bee', name: 'Bee', role: 'Pilot' }
      ]
    },
    invites: { north: [] },
    links: {},
    ...over
  };
}

const refuse = (message: string): never => {
  throw new Error(message);
};

export function fakePeople(server: FakeServer) {
  let made = 0;
  const me = (): Me => (server.account ? { account: server.account } : {});
  const api = {
    me: vi.fn(async () => me()),
    providers: vi.fn(async () => server.providers),
    signInUrl: vi.fn(
      (provider: string, how: { invite?: string; add?: boolean } = {}) =>
        `/auth/${provider}/start${how.add ? '?add=1' : how.invite ? `?invite=${how.invite}` : ''}`
    ),
    rename: vi.fn(async (name: string) => {
      if (!server.account) return refuse('Sign in first.');
      if (name.trim() === '') return refuse('An account needs a name.');
      server.account = { ...server.account, name: name.trim() };
      return me();
    }),
    removeSignIn: vi.fn(async (provider: string, subject: string) => {
      const account = server.account ?? refuse('Sign in first.');
      if (account.sign_ins.length <= 1) {
        return refuse('An account must keep at least one way to sign in.');
      }
      server.account = {
        ...account,
        sign_ins: account.sign_ins.filter(
          (s) => !(s.provider === provider && s.subject === subject)
        )
      };
      return me();
    }),
    signOut: vi.fn(async () => {
      server.account = undefined;
    }),
    invite: vi.fn(async (secret: string) => {
      return server.links[secret] ?? refuse('That invite does not work.');
    }),
    acceptInvite: vi.fn(async (secret: string) => {
      const account = server.account ?? refuse('Sign in first.');
      const invite = server.links[secret] ?? refuse('That invite does not work.');
      delete server.links[secret];
      if (invite.club) {
        server.account = {
          ...account,
          clubs: [...account.clubs, { club: invite.club, role: invite.role ?? 'Pilot' }]
        };
      }
      return me();
    }),
    clubInvites: vi.fn(async (club: string) => [...(server.invites[club] ?? [])]),
    makeInvite: vi.fn(async (club: string, request: InviteRequest): Promise<InviteMade> => {
      made += 1;
      const invite: InviteView = {
        id: `inv${made}`,
        kind: request.kind,
        club: club === 'north' ? NORTH : SOUTH,
        role: request.kind === 'Group' ? 'Pilot' : (request.role ?? 'Pilot'),
        label: request.label,
        max_uses: request.kind === 'Group' ? (request.max_uses ?? 0) : 1,
        uses: 0,
        revoked: false,
        works: true
      };
      server.invites[club] = [invite, ...(server.invites[club] ?? [])];
      return { invite, secret: `gfi_secret-${made}` };
    }),
    revokeInvite: vi.fn(async (club: string, id: string) => {
      server.invites[club] = (server.invites[club] ?? []).map((invite) =>
        invite.id === id ? { ...invite, revoked: true, works: false } : invite
      );
    }),
    members: vi.fn(async (club: string) => {
      return [...(server.members[club] ?? refuse('Only a director of this club can do that.'))];
    }),
    setRole: vi.fn(async (club: string, account: string, role: ClubRole) => {
      const list = server.members[club] ?? [];
      const target = list.find((m) => m.id === account) ?? refuse('That account is not a member.');
      const directors = list.filter((m) => m.role === 'Director').length;
      if (target.role === 'Director' && role !== 'Director' && directors <= 1) {
        return refuse('A club must keep at least one director.');
      }
      server.members[club] = list.map((m) => (m.id === account ? { ...m, role } : m));
    }),
    removeMember: vi.fn(async (club: string, account: string) => {
      const list = server.members[club] ?? [];
      const target = list.find((m) => m.id === account) ?? refuse('That account is not a member.');
      if (target.role === 'Director' && list.filter((m) => m.role === 'Director').length <= 1) {
        return refuse('A club must keep at least one director.');
      }
      server.members[club] = list.filter((m) => m.id !== account);
    })
  } satisfies PeopleApi;
  return api;
}
