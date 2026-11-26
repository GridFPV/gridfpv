/**
 * Accounts on a remote server (docs/primitives.md §13.8): what the pages ask the server
 * for, and the words they use. The words are pure, so they can be tested alone.
 */
import {
  acceptServerInvite,
  getServerInvite,
  getServerMe,
  listClubInvites,
  listClubMembers,
  listSignInProviders,
  makeClubInvite,
  removeClubMember,
  removeServerSignIn,
  renameServerMe,
  revokeClubInvite,
  setClubRole,
  signInUrl,
  signOutOfServer
} from '@gridfpv/protocol-client';
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

/** Everything the account pages ask a server for. A test passes its own. */
export interface PeopleApi {
  me(): Promise<Me>;
  providers(): Promise<SignInProvider[]>;
  /** Where to send the browser to sign in with a provider. */
  signInUrl(provider: string, how?: { invite?: string; add?: boolean }): string;
  rename(name: string): Promise<Me>;
  removeSignIn(provider: string, subject: string): Promise<Me>;
  signOut(): Promise<void>;
  invite(secret: string): Promise<InviteView>;
  acceptInvite(secret: string): Promise<Me>;
  clubInvites(club: string): Promise<InviteView[]>;
  makeInvite(club: string, request: InviteRequest): Promise<InviteMade>;
  revokeInvite(club: string, id: string): Promise<void>;
  members(club: string): Promise<MemberView[]>;
  setRole(club: string, account: string, role: ClubRole): Promise<void>;
  removeMember(club: string, account: string): Promise<void>;
}

/** The real thing, for the server at `baseUrl`. */
export function peopleApi(baseUrl: string): PeopleApi {
  return {
    me: () => getServerMe(baseUrl),
    providers: () => listSignInProviders(baseUrl),
    signInUrl: (provider, how) => signInUrl(baseUrl, provider, how),
    rename: (name) => renameServerMe(baseUrl, name),
    removeSignIn: (provider, subject) => removeServerSignIn(baseUrl, provider, subject),
    signOut: () => signOutOfServer(baseUrl),
    invite: (secret) => getServerInvite(baseUrl, secret),
    acceptInvite: (secret) => acceptServerInvite(baseUrl, secret),
    clubInvites: (club) => listClubInvites(baseUrl, club),
    makeInvite: (club, request) => makeClubInvite(baseUrl, club, request),
    revokeInvite: (club, id) => revokeClubInvite(baseUrl, club, id),
    members: (club) => listClubMembers(baseUrl, club),
    setRole: (club, account, role) => setClubRole(baseUrl, club, account, role),
    removeMember: (club, account) => removeClubMember(baseUrl, club, account)
  };
}

/** Why a sign-in did not work, in words, from the code the server put in the address. */
export function signInProblem(code: string | undefined): string | undefined {
  switch (code) {
    case undefined:
    case '':
      return undefined;
    case 'need-invite':
      return 'You need an invite to make an account here. Ask your club for a link.';
    case 'invite':
      return 'That invite does not work. It may have been used already, or have run out.';
    case 'refused':
      return 'You were not signed in. Try again when you are ready.';
    case 'expired':
      return 'That took too long, or was started somewhere else. Try again.';
    case 'taken':
      return 'That sign-in already belongs to another account here.';
    case 'not-signed-in':
      return 'Sign in first, then add another way to sign in.';
    case 'not-set-up':
      return 'Signing in that way is not set up on this server.';
    default:
      return 'Signing in did not work. Try again in a moment.';
  }
}

export const ROLES: readonly ClubRole[] = ['Pilot', 'Approver', 'Director'];

/** A role as a word. */
export function roleWord(role: ClubRole): string {
  return role === 'Director' ? 'Director' : role === 'Approver' ? 'Approver' : 'Pilot';
}

/** What an invite gives, as the end of "You are invited to …". */
export function inviteWords(invite: Pick<InviteView, 'kind' | 'club' | 'role'>): string {
  if (invite.kind === 'Owner') return 'be an owner of this server';
  const club = invite.club?.name ?? 'a club';
  if (invite.role === 'Director') return `join ${club} as a director`;
  if (invite.role === 'Approver') return `join ${club} as an approver`;
  return `join ${club}`;
}

/** How an invite stands, in a few words, for a director's list. */
export function inviteStanding(invite: InviteView, now: number): string {
  if (invite.revoked) return 'Taken back';
  if (invite.uses >= invite.max_uses) return invite.max_uses === 1 ? 'Used' : 'Full';
  if (invite.expires_at !== undefined && invite.expires_at <= now) return 'Ran out';
  if (invite.max_uses === 1) return 'Not used yet';
  return `${invite.uses} of ${invite.max_uses} used`;
}

/** The link for an invite's secret, as a person would open it. */
export function inviteLink(origin: string, secret: string): string {
  return `${origin.replace(/\/$/, '')}/#/invite/${encodeURIComponent(secret)}`;
}

/** The clubs an account may manage: all of them for an owner, else where it is a director. */
export function managedClubs(
  account: MyAccount | undefined,
  all: readonly { id: string; name: string }[]
): { id: string; name: string }[] {
  if (!account) return [];
  if (account.owner) return [...all];
  return account.clubs.filter((entry) => entry.role === 'Director').map((entry) => entry.club);
}

/** A provider's name for a sign-in an account holds, when the server still offers it. */
export function providerName(id: string, providers: readonly SignInProvider[]): string {
  return providers.find((provider) => provider.id === id)?.name ?? id;
}
