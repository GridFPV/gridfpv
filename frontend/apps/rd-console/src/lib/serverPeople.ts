/**
 * Accounts on a remote server (docs/primitives.md §13.8): what the pages ask the server
 * for, and the words they use. The words are pure, so they can be tested alone.
 */
import {
  acceptServerInvite,
  askForClubPilot,
  dropClubPilotClaim,
  freeClubPilot,
  getServerInvite,
  getServerMe,
  getServerMyResults,
  listClubInvites,
  listClubLinkTokens,
  listServerEvents,
  makeClubLinkToken,
  removeServerEvent,
  renameServerClub,
  revokeClubLinkToken,
  setServerEventHidden,
  listClubMembers,
  listClubPilots,
  listSignInProviders,
  makeClubInvite,
  refuseClubPilotAsk,
  removeClubMember,
  removeServerSignIn,
  renameServerMe,
  revokeClubInvite,
  setClubPilotHolder,
  setClubRole,
  signInUrl,
  signOutOfServer,
  getAccountSignIns,
  dropAccountSignIn,
  makeAccountReconnect,
  revokeAccountReconnect,
  getServerReconnect,
  listServerClubs,
  makeServerClub,
  setServerClubArchived
} from '@gridfpv/protocol-client';
import type {
  AccountSignIns,
  ServerClub,
  ReconnectMade,
  ReconnectView,
  ClubPilotView,
  ClubRole,
  InviteMade,
  InviteRequest,
  InviteView,
  LinkTokenMade,
  LinkTokenView,
  Me,
  MemberView,
  MyAccount,
  MyClassResult,
  MyResult,
  ServerEvent,
  SignInProvider
} from '@gridfpv/types';
import { ordinal } from './names.js';

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
  /** A club's pilots, and which are taken. For anyone in the club. */
  clubPilots(club: string): Promise<ClubPilotView[]>;
  /** Ask for a pilot. Nothing is held until someone allowed says yes. */
  askFor(club: string, pilot: string): Promise<void>;
  /** Stop asking for a pilot, or let go of one that is held. */
  dropClaim(club: string, pilot: string): Promise<void>;
  /** Say an account holds a pilot: the yes to an ask, or a director giving it. */
  giveTo(club: string, pilot: string, account: string): Promise<void>;
  /** Take a pilot from its holder. */
  freePilot(club: string, pilot: string): Promise<void>;
  /** Say no to an account that asked. */
  refuseAsk(club: string, pilot: string, account: string): Promise<void>;
  /** The events the account's pilots flew, newest first. */
  results(): Promise<MyResult[]>;
  /** Give a club a new name. For those who run it, as is everything below. */
  renameClub(club: string, name: string): Promise<void>;
  /** A club's link tokens, newest first. Never the tokens themselves. */
  linkTokens(club: string): Promise<LinkTokenView[]>;
  /** Make a link token. The token itself is in the answer, this once. */
  makeLinkToken(club: string, label: string): Promise<LinkTokenMade>;
  revokeLinkToken(club: string, id: string): Promise<void>;
  /** A club's events as the person signed in may see them: hidden ones too, for those who run it. */
  clubEvents(club: string): Promise<ServerEvent[]>;
  hideEvent(club: string, event: string, hidden: boolean): Promise<void>;
  removeEvent(club: string, event: string): Promise<void>;
  /** How an account signs in, and whether a re-connect link waits. For an owner, as is everything below. */
  accountSignIns(account: string): Promise<AccountSignIns>;
  /** Take a sign-in off someone's account, even their last. They are signed out everywhere. */
  dropSignIn(account: string, provider: string, subject: string): Promise<AccountSignIns>;
  /** Make a link that lets someone back in. The secret is in the answer, this once. */
  makeReconnect(account: string): Promise<ReconnectMade>;
  revokeReconnect(account: string): Promise<void>;
  /** Whose account a re-connect link is for. For whoever opens the link. */
  reconnect(secret: string): Promise<ReconnectView>;
  /** The clubs on the server. An owner is given the archived ones too. */
  serverClubs(): Promise<ServerClub[]>;
  /** Make a club. For an owner of the server, as is archiving one. */
  makeClub(name: string): Promise<ServerClub>;
  /** Archive a club, or bring it back. */
  archiveClub(club: string, archived: boolean): Promise<ServerClub>;
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
    removeMember: (club, account) => removeClubMember(baseUrl, club, account),
    clubPilots: (club) => listClubPilots(baseUrl, club),
    askFor: (club, pilot) => askForClubPilot(baseUrl, club, pilot),
    dropClaim: (club, pilot) => dropClubPilotClaim(baseUrl, club, pilot),
    giveTo: (club, pilot, account) => setClubPilotHolder(baseUrl, club, pilot, account),
    freePilot: (club, pilot) => freeClubPilot(baseUrl, club, pilot),
    refuseAsk: (club, pilot, account) => refuseClubPilotAsk(baseUrl, club, pilot, account),
    results: () => getServerMyResults(baseUrl),
    renameClub: async (club, name) => {
      await renameServerClub(baseUrl, club, name);
    },
    linkTokens: (club) => listClubLinkTokens(baseUrl, club),
    makeLinkToken: (club, label) => makeClubLinkToken(baseUrl, club, label),
    revokeLinkToken: (club, id) => revokeClubLinkToken(baseUrl, club, id),
    clubEvents: (club) => listServerEvents(baseUrl, club),
    hideEvent: (club, event, hidden) => setServerEventHidden(baseUrl, club, event, hidden),
    accountSignIns: (account) => getAccountSignIns(baseUrl, account),
    dropSignIn: (account, provider, subject) =>
      dropAccountSignIn(baseUrl, account, provider, subject),
    makeReconnect: (account) => makeAccountReconnect(baseUrl, account),
    revokeReconnect: (account) => revokeAccountReconnect(baseUrl, account),
    reconnect: (secret) => getServerReconnect(baseUrl, secret),
    serverClubs: () => listServerClubs(baseUrl),
    makeClub: (name) => makeServerClub(baseUrl, name),
    archiveClub: (club, archived) => setServerClubArchived(baseUrl, club, archived),
    removeEvent: (club, event) => removeServerEvent(baseUrl, club, event)
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
    case 'link':
      return 'That re-connect link does not work. It may have been used already, taken back, or have run out. Ask for a new one.';
    case 'refused':
      return 'You were not signed in. Try again when you are ready.';
    case 'expired':
      return 'That took too long, or was started somewhere else. Try again.';
    case 'taken':
      return 'That sign-in already belongs to another account here. Pick one that no account here uses.';
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

/** A club's friendly address, as a person would open it. */
export function clubLink(origin: string, word: string): string {
  return `${origin.replace(/\/$/, '')}/#/c/${encodeURIComponent(word)}`;
}

/** What everyone who signs in or runs a club here is told about the server's owners. */
export const OWNERS_SEE_ALL = 'The owners of this server can see and change everything on it.';

/** The link for a re-connect link's secret, as a person would open it. */
export function reconnectLink(origin: string, secret: string): string {
  return `${origin.replace(/\/$/, '')}/#/reconnect/${encodeURIComponent(secret)}`;
}

/** The day a re-connect link stops working, in words: "until 4 December". */
export function untilDay(at: number, locale?: string): string {
  return `until ${new Date(at).toLocaleDateString(locale, { day: 'numeric', month: 'long' })}`;
}

/** The link for an invite's secret, as a person would open it. */
export function inviteLink(origin: string, secret: string): string {
  return `${origin.replace(/\/$/, '')}/#/invite/${encodeURIComponent(secret)}`;
}

/** A club an account looks after, and how far its say goes there. */
export interface LookedAfter {
  club: { id: string; name: string };
  /** Runs the club: members, invites and pilots. Otherwise only says yes or no to claims. */
  runs: boolean;
}

/**
 * The clubs an account looks after: every club for an owner, else the ones it is a
 * director of (it runs those) or an approver of (it answers claims there).
 */
export function lookedAfterClubs(
  account: MyAccount | undefined,
  all: readonly { id: string; name: string }[]
): LookedAfter[] {
  if (!account) return [];
  if (account.owner) return all.map((club) => ({ club, runs: true }));
  return account.clubs
    .filter((entry) => entry.role !== 'Pilot')
    .map((entry) => ({ club: entry.club, runs: entry.role === 'Director' }));
}

/** How far an account's say goes in one club. `undefined` when it has none. */
export function sayIn(
  account: MyAccount | undefined,
  club: string
): 'runs' | 'answers' | undefined {
  if (!account) return undefined;
  if (account.owner) return 'runs';
  const role = account.clubs.find((entry) => entry.club.id === club)?.role;
  return role === 'Director' ? 'runs' : role === 'Approver' ? 'answers' : undefined;
}

/** Where a pilot stands in a class, in words: "2nd of 12", or that racing has not placed them. */
export function placeWords(result: Pick<MyClassResult, 'position' | 'of' | 'placed'>): string {
  if (!result.placed) return 'Not placed yet';
  return `${ordinal(result.position)} of ${result.of}`;
}

/** How a pilot of a club stands for the person looking, in a few words. */
export function pilotStanding(pilot: Pick<ClubPilotView, 'mine' | 'asked' | 'taken'>): string {
  if (pilot.mine) return 'Yours';
  if (pilot.asked) return 'You asked';
  if (pilot.taken) return 'Taken';
  return '';
}

/** How a link token stands, in a few words. */
export function tokenStanding(token: Pick<LinkTokenView, 'revoked_at'>): string {
  return token.revoked_at === undefined ? 'Works' : 'Taken back';
}

/** A provider's name for a sign-in an account holds, when the server still offers it. */
export function providerName(id: string, providers: readonly SignInProvider[]): string {
  return providers.find((provider) => provider.id === id)?.name ?? id;
}
