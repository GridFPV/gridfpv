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
  setServerClubArchived,
  setServerPrimaryClub,
  setServerProfile,
  getServerInfo,
  setServerOpenSignUp,
  setServerClubOpen,
  joinServerClub,
  leaveServerClub,
  listClubJoinAsks,
  answerClubJoinAsk,
  findClubPeople,
  addClubMember,
  signInWithPassword,
  signUpWithPassword,
  setServerPassword,
  setServerPasswordSignIn,
  setServerEmail,
  removeServerEmail,
  confirmServerEmail,
  forgotServerPassword,
  serverMail,
  setServerMail,
  removeServerMail,
  testServerMail
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
  EmailConfirmed,
  ServerEmail,
  ServerEmailChange,
  MyResult,
  PasswordChange,
  PasswordSignUp,
  PersonView,
  PilotProfile,
  ServerEvent,
  ServerInfo,
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
  /** Mark the club the signed-in account's front page opens on. */
  setPrimary(club: string): Promise<Me>;
  /** Set the signed-in account's pilot profile, all of it. A part left out is cleared. */
  setProfile(profile: PilotProfile): Promise<Me>;
  /** What anyone may know of how the server is run. */
  serverInfo(): Promise<ServerInfo>;
  /** Say whether someone with no invite may make an account. For an owner. */
  setOpenSignUp(open: boolean): Promise<ServerInfo>;
  /** Make a club public (anyone with an account may join) or private. For those who run it. */
  setClubOpen(club: string, open: boolean): Promise<ServerClub>;
  /** Join a public club, or ask to join a private one. */
  joinClub(club: string): Promise<Me>;
  /** Take back an ask to join a club, or leave it. */
  leaveClub(club: string): Promise<Me>;
  /** Who has asked to join a club, the first to ask at the top. For those who run it. */
  joinAsks(club: string): Promise<PersonView[]>;
  /** Say yes or no to someone who asked to join. */
  answerJoinAsk(club: string, account: string, yes: boolean): Promise<void>;
  /** Accounts not in a club, by name or callsign. Nobody for fewer than two letters. */
  findPeople(club: string, text: string): Promise<PersonView[]>;
  /** Add an account to a club, as a pilot. */
  addMember(club: string, account: string): Promise<void>;
  /** Sign in with a username and a password the server keeps. */
  passwordSignIn(username: string, password: string): Promise<Me>;
  /** Make an account with a username and password. A re-connect link sets a way back in. */
  passwordSignUp(given: PasswordSignUp): Promise<Me>;
  /** Add a username and password to the signed-in account, or change its password. */
  setPassword(change: PasswordChange): Promise<Me>;
  /** Switch signing in with a password on or off. For an owner. */
  setPasswordSignIn(on: boolean): Promise<ServerInfo>;
  /** Give the signed-in account an email address. A link is sent to confirm it. */
  setEmail(address: string): Promise<Me>;
  /** Take the signed-in account's email address off. */
  removeEmail(): Promise<Me>;
  /** Confirm an email address with the link that was sent to it. */
  confirmEmail(secret: string): Promise<EmailConfirmed>;
  /** Ask for a link to set a new password, by username or email address. */
  forgotPassword(who: string): Promise<void>;
  /** The email account the server sends from, for an owner. Never its password. */
  mailSettings(): Promise<ServerEmail>;
  setMailSettings(change: ServerEmailChange): Promise<ServerEmail>;
  /** Forget what was set on the page. Settings in the server's own file are used again. */
  removeMailSettings(): Promise<ServerEmail>;
  /** Send one mail to check the settings. It throws with the email server's own words. */
  testMail(to: string): Promise<void>;
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
    setPrimary: (club) => setServerPrimaryClub(baseUrl, club),
    setProfile: (profile) => setServerProfile(baseUrl, profile),
    serverInfo: () => getServerInfo(baseUrl),
    setOpenSignUp: (open) => setServerOpenSignUp(baseUrl, open),
    setClubOpen: (club, open) => setServerClubOpen(baseUrl, club, open),
    joinClub: (club) => joinServerClub(baseUrl, club),
    leaveClub: (club) => leaveServerClub(baseUrl, club),
    joinAsks: (club) => listClubJoinAsks(baseUrl, club),
    answerJoinAsk: (club, account, yes) => answerClubJoinAsk(baseUrl, club, account, yes),
    findPeople: (club, text) => findClubPeople(baseUrl, club, text),
    addMember: (club, account) => addClubMember(baseUrl, club, account),
    passwordSignIn: (username, password) => signInWithPassword(baseUrl, { username, password }),
    passwordSignUp: (given) => signUpWithPassword(baseUrl, given),
    setPassword: (change) => setServerPassword(baseUrl, change),
    setPasswordSignIn: (on) => setServerPasswordSignIn(baseUrl, on),
    setEmail: (address) => setServerEmail(baseUrl, address),
    removeEmail: () => removeServerEmail(baseUrl),
    confirmEmail: (secret) => confirmServerEmail(baseUrl, secret),
    forgotPassword: (who) => forgotServerPassword(baseUrl, who),
    mailSettings: () => serverMail(baseUrl),
    setMailSettings: (change) => setServerMail(baseUrl, change),
    removeMailSettings: () => removeServerMail(baseUrl),
    testMail: (to) => testServerMail(baseUrl, to),
    removeEvent: (club, event) => removeServerEvent(baseUrl, club, event)
  };
}

/** The form for a pilot profile: every box as text, empty for a part that is not set. */
export interface ProfileForm {
  callsign: string;
  color: string;
  multigp_id: string;
  velocidrone_id: string;
}

export function profileForm(profile: PilotProfile | undefined): ProfileForm {
  return {
    callsign: profile?.callsign ?? '',
    color: profile?.color ?? '',
    multigp_id: profile?.multigp_id ?? '',
    velocidrone_id: profile?.velocidrone_id ?? ''
  };
}

/** What to send for a form: each part tidied, and an empty one left out. */
export function profileOf(form: ProfileForm): PilotProfile {
  const out: PilotProfile = {};
  const callsign = form.callsign.trim();
  const color = form.color.trim().toUpperCase();
  const multigp = form.multigp_id.trim();
  const velocidrone = form.velocidrone_id.trim();
  if (callsign !== '') out.callsign = callsign;
  if (color !== '') out.color = color;
  if (multigp !== '') out.multigp_id = multigp;
  if (velocidrone !== '') out.velocidrone_id = velocidrone;
  return out;
}

/**
 * What is wrong with a profile form, in words, before it is sent. The server checks it
 * all again; this is so the pilot sees it while typing.
 */
export function profileProblem(form: ProfileForm): string | undefined {
  const profile = profileOf(form);
  if ([...(profile.callsign ?? '')].length > 40) return 'A callsign is 40 letters at most.';
  if (profile.color !== undefined && !/^#[0-9A-F]{6}$/.test(profile.color)) {
    return 'A colour is a # and six letters or digits, like #FF8800.';
  }
  for (const [id, what] of [
    [profile.multigp_id, 'A MultiGP id'],
    [profile.velocidrone_id, 'A Velocidrone id']
  ] as const) {
    if (id === undefined) continue;
    if ([...id].length > 40) return `${what} is 40 letters at most.`;
    if (/\s/.test(id)) return `${what} has no spaces in it.`;
  }
  return undefined;
}

/** Whether a form differs from the profile that is kept. */
export function profileChanged(form: ProfileForm, profile: PilotProfile | undefined): boolean {
  return JSON.stringify(profileOf(form)) !== JSON.stringify(profileOf(profileForm(profile)));
}

/**
 * What a pilot is called on a server: the callsign in their profile, or else their
 * account's name.
 */
export function pilotName(account: { name: string; profile?: PilotProfile }): string {
  return account.profile?.callsign?.trim() || account.name;
}

/** Where an account stands with a club it may want to join. */
export type JoinStanding =
  /** Nobody is signed in. */
  | 'signed-out'
  /** Already in the club. */
  | 'member'
  /** Asked, and waiting for a director. */
  | 'asked'
  /** A public club: joining is one press. */
  | 'can-join'
  /** A private club: a director says yes. */
  | 'can-ask'
  /** An archived club takes nobody. */
  | 'closed';

export function joinStanding(
  account: Pick<MyAccount, 'clubs' | 'join_asks'> | null | undefined,
  club: { id: string; open?: boolean; archived?: boolean }
): JoinStanding {
  if (club.archived) return 'closed';
  if (!account) return 'signed-out';
  if (account.clubs.some((entry) => entry.club.id === club.id)) return 'member';
  if ((account.join_asks ?? []).some((asked) => asked.id === club.id)) return 'asked';
  return club.open ? 'can-join' : 'can-ask';
}

/** Someone as a director is shown them: their name, and their callsign when it differs. */
export function personLabel(person: { name: string; callsign?: string }): string {
  const callsign = person.callsign?.trim();
  return callsign && callsign !== person.name ? `${person.name} (${callsign})` : person.name;
}

/**
 * What the sign-in page says of making an account. Only a server that asks for an invite
 * says anything: where anyone may sign up, the way in is already on the page.
 */
export function signUpLine(open: boolean | undefined): string {
  return open === false ? 'New here? You need an invite.' : '';
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
  return `${origin.replace(/\/$/, '')}/#/club/${encodeURIComponent(word)}`;
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
/** A club as a list has it: its friendly address and standing too, when the list says. */
export interface ClubRef {
  id: string;
  name: string;
  word?: string;
  archived?: boolean;
  open?: boolean;
}

export interface LookedAfter {
  club: ClubRef;
  /** Runs the club: members, invites and pilots. Otherwise only says yes or no to claims. */
  runs: boolean;
}

/**
 * The clubs an account looks after: every club for an owner, else the ones it is a
 * director of (it runs those) or an approver of (it answers claims there).
 */
export function lookedAfterClubs(
  account: MyAccount | undefined,
  all: readonly ClubRef[]
): LookedAfter[] {
  if (!account) return [];
  if (account.owner) return all.map((club) => ({ club, runs: true }));
  return account.clubs
    .filter((entry) => entry.role !== 'Pilot')
    .map((entry) => ({
      // The club as the list has it, which knows its friendly address too.
      club: all.find((club) => club.id === entry.club.id) ?? entry.club,
      runs: entry.role === 'Director'
    }));
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
  if (id === PASSWORD_PROVIDER) return 'Username and password';
  return providers.find((provider) => provider.id === id)?.name ?? id;
}

/** The `provider` of a sign-in that is a username and a password kept on the server. */
export const PASSWORD_PROVIDER = 'password';

/** What is wrong with an email address, in words, before it is sent. */
export function emailProblem(address: string): string | undefined {
  const text = address.trim();
  if (text === '') return undefined;
  if (!/^[^\s@<>]+@[^\s@<>]+\.[^\s@<>]+$/.test(text)) {
    return 'That is not an email address. It looks like you@example.org';
  }
  return undefined;
}

/** What is said after someone asks for a link to set a new password, whoever they named. */
export const FORGOT_SENT =
  'If that account has a confirmed email address, a link is on its way. It works once, and for one hour.';

/** The shortest password a server takes. */
export const PASSWORD_MIN = 10;

/**
 * What is wrong with a username, in words, before it is sent. The server checks it again.
 */
export function usernameProblem(username: string): string | undefined {
  const text = username.trim();
  if (text === '') return undefined;
  if (!/^[A-Za-z0-9._-]{3,32}$/.test(text)) {
    return 'A username is 3 to 32 letters and digits. A dot, a dash and an underscore are fine too.';
  }
  return undefined;
}

/**
 * What is wrong with a new password, in words. `again` is the password typed a second
 * time, when the form asks for it.
 */
export function passwordProblem(password: string, again?: string): string | undefined {
  if (password === '') return undefined;
  if ([...password].length < PASSWORD_MIN) {
    return `A password is at least ${PASSWORD_MIN} characters. A few words in a row is a good one.`;
  }
  if ([...password].length > 200) return 'A password is 200 characters at most.';
  if (again !== undefined && again !== '' && again !== password) {
    return 'The two passwords are not the same.';
  }
  return undefined;
}
