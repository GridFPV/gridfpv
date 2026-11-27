/**
 * A stand-in for a remote server's accounts, for the page tests. It keeps the rules the
 * real one keeps closely enough to drive the pages: an invite works once, a club keeps a
 * director, a link is shown once.
 */
import { vi } from 'vitest';
import type {
  AccountSignIns,
  ServerClub,
  MySignIn,
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
  MyResult,
  PasswordChange,
  PasswordSignUp,
  PersonView,
  PilotProfile,
  ServerEvent,
  ServerInfo,
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
  clubs: [{ club: NORTH, role: 'Director' }],
  pilots: [],
  profile: {},
  join_asks: []
};

/** When a re-connect link made here stops working: 4 December 2026, midday. */
export const LINK_UNTIL = Date.UTC(2026, 11, 4, 12);

export interface FakeServer {
  account: MyAccount | undefined;
  providers: SignInProvider[];
  members: Record<string, MemberView[]>;
  invites: Record<string, InviteView[]>;
  /** Invite links that work: secret to what they are for. */
  links: Record<string, InviteView>;
  /**
   * Each club's pilots, with who holds and who asked. What a reader is shown of it is
   * worked out as the real server does, from who is signed in.
   */
  pilots: Record<string, ClubPilotView[]>;
  results: MyResult[];
  /** Each club's link tokens, newest first. */
  tokens: Record<string, LinkTokenView[]>;
  /** The tokens themselves, by id: what a Director would paste. */
  secrets: Record<string, string>;
  /** Each club's events, hidden ones too. */
  events: Record<string, ServerEvent[]>;
  /** Events removed from the server, as `club/event`. */
  removed: string[];
  /** The clubs on the server, archived ones too. */
  clubList: ServerClub[];
  /** How each account signs in, by account id. */
  signIns: Record<string, MySignIn[]>;
  /** Re-connect links that work: secret to the account it is for. */
  reconnects: Record<string, string>;
  /** Accounts that were signed out everywhere, by an owner taking a sign-in off. */
  signedOut: string[];
  /** Whether someone with no invite may make an account. */
  openSignUp: boolean;
  /** Who has asked to join each club, the first to ask at the top. */
  joinAsks: Record<string, PersonView[]>;
  /** Everyone with an account on the server, in a club or not. */
  people: PersonView[];
  /** Whether people may sign in with a username and password. */
  passwordOn: boolean;
  /** Each username's password, and the account it signs in to. */
  passwords: Record<string, { password: string; account: MyAccount }>;
}

/** An event a club holds on the server. */
export const serverEvent = (
  id: string,
  name: string,
  over: Partial<ServerEvent> = {}
): ServerEvent => ({ id, name, created_at: 1, hidden: false, ...over });

/** A pilot of a club that nobody holds or has asked for. */
export const clubPilot = (
  id: string,
  callsign: string,
  over: Partial<ClubPilotView> = {}
): ClubPilotView => ({ id, callsign, taken: false, mine: false, asked: false, asks: [], ...over });

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
    pilots: { north: [clubPilot('p-cat', 'Cat'), clubPilot('p-dog', 'Dog')] },
    results: [],
    tokens: { north: [{ id: 'tok1', label: 'Club laptop', created_at: 1_700_000_000_000 }] },
    secrets: { tok1: 'gfs_first' },
    events: { north: [] },
    removed: [],
    signIns: {
      acct_ace: [{ provider: 'discord', subject: '111', label: 'ace_fpv' }],
      acct_bee: [
        { provider: 'discord', subject: '222', label: 'bee_fpv' },
        { provider: 'google', subject: 'g-bee', label: 'Bee B' }
      ]
    },
    reconnects: {},
    signedOut: [],
    openSignUp: false,
    joinAsks: {},
    people: [],
    passwordOn: false,
    passwords: {},
    clubList: [
      { ...NORTH, word: 'north-fpv', archived: false, open: false },
      { ...SOUTH, word: 'south-fpv', archived: false, open: false }
    ],
    ...over
  };
}

const ARCHIVED =
  'This club is archived, so nothing in it can be changed. An owner of this server can bring it back.';

const PASSWORDS_OFF = 'Signing in with a password is switched off on this server.';

const refuse = (message: string): never => {
  throw new Error(message);
};

export function fakePeople(server: FakeServer) {
  let made = 0;
  const me = (): Me => (server.account ? { account: server.account } : {});
  const info = (): ServerInfo => ({
    open_sign_up: server.openSignUp,
    password_sign_in: server.passwordOn
  });
  const clubs: Record<string, { id: string; name: string }> = {
    north: { ...NORTH },
    south: { ...SOUTH }
  };
  const nameOf = (club: string, account: string): string =>
    server.members[club]?.find((m) => m.id === account)?.name ??
    (server.account?.id === account ? server.account.name : account);
  /** Whether the signed-in account is told who holds and who asked. */
  const seesWho = (club: string): boolean => {
    const account = server.account;
    if (!account) return false;
    if (account.owner) return true;
    const role = account.clubs.find((c) => c.club.id === club)?.role;
    return role === 'Director' || role === 'Approver';
  };
  const roleIn = (club: string): ClubRole | undefined =>
    server.account?.clubs.find((c) => c.club.id === club)?.role;
  /** Only a director of the club, or an owner, runs it. */
  const runsClub = (club: string) => {
    if (server.account?.owner !== true && roleIn(club) !== 'Director') {
      refuse('You are not allowed to do that.');
    }
  };
  const pilotOf = (club: string, id: string): ClubPilotView =>
    server.pilots[club]?.find((p) => p.id === id) ?? refuse('This club has no such pilot.');
  /** Keep the signed-in account's own list in step with the club's. */
  const sync = () => {
    const account = server.account;
    if (!account) return;
    const pilots = Object.entries(server.pilots).flatMap(([club, list]) =>
      list
        .filter((p) => p.holder?.id === account.id || p.asks.some((a) => a.id === account.id))
        .map((p) => ({
          club: clubs[club],
          id: p.id,
          callsign: p.callsign,
          held: p.holder?.id === account.id
        }))
    );
    server.account = { ...account, pilots };
  };
  const accountName = (account: string): string =>
    Object.values(server.members)
      .flat()
      .find((m) => m.id === account)?.name ?? account;
  /** Only an owner of the server helps an account. */
  const signInsOf = (account: string): AccountSignIns => {
    if (server.account?.owner !== true) refuse('Only an owner of this server can do that.');
    const held = server.signIns[account] ?? refuse('There is no such account.');
    const waiting = Object.values(server.reconnects).includes(account);
    return {
      id: account,
      name: accountName(account),
      sign_ins: [...held],
      ...(waiting ? { link_until: LINK_UNTIL } : {})
    };
  };
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
        ...(request.pilot
          ? { pilot: { id: request.pilot, callsign: pilotOf(club, request.pilot).callsign } }
          : {}),
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
    }),
    clubPilots: vi.fn(async (club: string): Promise<ClubPilotView[]> => {
      const list = server.pilots[club] ?? refuse('Only a member of this club can do that.');
      const mine = server.account?.id;
      const sees = seesWho(club);
      return list.map((p) => {
        const { holder, ...rest } = p;
        return {
          ...rest,
          taken: holder !== undefined,
          mine: holder?.id === mine,
          asked: p.asks.some((a) => a.id === mine),
          asks: sees ? [...p.asks] : [],
          ...(sees && holder ? { holder } : {})
        };
      });
    }),
    askFor: vi.fn(async (club: string, pilot: string) => {
      const account = server.account ?? refuse('Sign in first.');
      const target = pilotOf(club, pilot);
      if (target.holder) return refuse('That pilot already belongs to an account.');
      if (!target.asks.some((a) => a.id === account.id)) {
        target.asks = [...target.asks, { id: account.id, name: account.name }];
      }
      sync();
    }),
    dropClaim: vi.fn(async (club: string, pilot: string) => {
      const account = server.account ?? refuse('Sign in first.');
      const target = pilotOf(club, pilot);
      if (target.holder?.id === account.id) delete target.holder;
      target.asks = target.asks.filter((a) => a.id !== account.id);
      sync();
    }),
    giveTo: vi.fn(async (club: string, pilot: string, account: string) => {
      const target = pilotOf(club, pilot);
      target.holder = { id: account, name: nameOf(club, account) };
      target.asks = [];
      sync();
    }),
    freePilot: vi.fn(async (club: string, pilot: string) => {
      delete pilotOf(club, pilot).holder;
      sync();
    }),
    refuseAsk: vi.fn(async (club: string, pilot: string, account: string) => {
      const target = pilotOf(club, pilot);
      target.asks = target.asks.filter((a) => a.id !== account);
      sync();
    }),
    results: vi.fn(async () => [...server.results]),
    renameClub: vi.fn(async (club: string, name: string) => {
      runsClub(club);
      if (name.trim() === '') refuse('A club needs a name.');
      clubs[club].name = name.trim();
      const account = server.account;
      if (account) {
        server.account = {
          ...account,
          clubs: account.clubs.map((c) =>
            c.club.id === club ? { ...c, club: { ...c.club, name: name.trim() } } : c
          )
        };
      }
    }),
    linkTokens: vi.fn(async (club: string): Promise<LinkTokenView[]> => {
      runsClub(club);
      return (server.tokens[club] ?? []).map((token) => ({ ...token }));
    }),
    makeLinkToken: vi.fn(async (club: string, label: string): Promise<LinkTokenMade> => {
      runsClub(club);
      if (label.trim() === '') refuse('A token needs a label, so it can be told from the others.');
      made += 1;
      const token: LinkTokenView = {
        id: `tok-new-${made}`,
        label: label.trim(),
        created_at: 1_700_000_000_000 + made
      };
      server.tokens[club] = [token, ...(server.tokens[club] ?? [])];
      server.secrets[token.id] = `gfs_secret-${made}`;
      return { token: { ...token }, secret: server.secrets[token.id] };
    }),
    revokeLinkToken: vi.fn(async (club: string, id: string) => {
      runsClub(club);
      const token = server.tokens[club]?.find((t) => t.id === id && t.revoked_at === undefined);
      if (!token) refuse('This club has no such link token, or it was already taken back.');
      else token.revoked_at = 1_700_000_100_000;
    }),
    clubEvents: vi.fn(async (club: string): Promise<ServerEvent[]> => {
      const all = server.events[club] ?? [];
      const runs = server.account?.owner === true || roleIn(club) === 'Director';
      return all.filter((event) => runs || !event.hidden).map((event) => ({ ...event }));
    }),
    hideEvent: vi.fn(async (club: string, event: string, hidden: boolean) => {
      runsClub(club);
      const found = server.events[club]?.find((e) => e.id === event);
      if (!found) refuse('This server holds no such event.');
      else found.hidden = hidden;
    }),
    removeEvent: vi.fn(async (club: string, event: string) => {
      runsClub(club);
      if (!server.events[club]?.some((e) => e.id === event)) {
        refuse('This server holds no such event.');
      }
      server.events[club] = server.events[club].filter((e) => e.id !== event);
      server.removed.push(`${club}/${event}`);
    }),
    accountSignIns: vi.fn(async (account: string) => signInsOf(account)),
    dropSignIn: vi.fn(async (account: string, provider: string, subject: string) => {
      const before = signInsOf(account).sign_ins;
      if (account === server.account?.id && before.length <= 1) {
        return refuse('An account must keep at least one way to sign in.');
      }
      server.signIns[account] = before.filter(
        (s) => !(s.provider === provider && s.subject === subject)
      );
      if (server.signIns[account].length < before.length) server.signedOut.push(account);
      return signInsOf(account);
    }),
    makeReconnect: vi.fn(async (account: string): Promise<ReconnectMade> => {
      signInsOf(account);
      // An account has one link at a time.
      for (const [secret, whose] of Object.entries(server.reconnects)) {
        if (whose === account) delete server.reconnects[secret];
      }
      made += 1;
      const secret = `gfr_secret${made}`;
      server.reconnects[secret] = account;
      return { secret, expires_at: LINK_UNTIL };
    }),
    revokeReconnect: vi.fn(async (account: string) => {
      const found = Object.entries(server.reconnects).find(([, whose]) => whose === account);
      if (!found) return refuse('That re-connect link does not work.');
      delete server.reconnects[found[0]];
    }),
    reconnect: vi.fn(async (secret: string): Promise<ReconnectView> => {
      const account = server.reconnects[secret] ?? refuse('That re-connect link does not work.');
      return { name: accountName(account) };
    }),
    serverClubs: vi.fn(async (): Promise<ServerClub[]> => {
      const owner = server.account?.owner === true;
      return server.clubList.filter((club) => owner || !club.archived).map((c) => ({ ...c }));
    }),
    makeClub: vi.fn(async (name: string): Promise<ServerClub> => {
      if (server.account?.owner !== true) refuse('Only an owner of this server can do that.');
      if (name.trim() === '') refuse('A club needs a name.');
      made += 1;
      const base = name
        .trim()
        .toLowerCase()
        .replace(/[^a-z0-9]+/g, '-')
        .replace(/^-|-$/g, '');
      const taken = server.clubList.some((c) => c.word === base);
      const club = {
        id: `cmade${made}`,
        name: name.trim(),
        word: taken ? `${base}-2` : base,
        archived: false,
        open: false
      };
      server.clubList.push(club);
      return { ...club };
    }),
    setPrimary: vi.fn(async (club: string) => {
      const account = server.account ?? refuse('Sign in first.');
      if (!account.owner && !account.clubs.some((entry) => entry.club.id === club)) {
        refuse('Pick a club you are in. An archived club cannot be picked.');
      }
      server.account = { ...account, primary: club };
      return me();
    }),
    setClubOpen: vi.fn(async (club: string, open: boolean): Promise<ServerClub> => {
      runsClub(club);
      const found =
        server.clubList.find((c) => c.id === club) ?? refuse('This server has no such club.');
      if (found.archived) refuse(ARCHIVED);
      found.open = open;
      return { ...found };
    }),
    joinClub: vi.fn(async (club: string) => {
      const account = server.account ?? refuse('Sign in first.');
      const found =
        server.clubList.find((c) => c.id === club) ?? refuse('This server has no such club.');
      if (found.archived) refuse(ARCHIVED);
      if (account.clubs.some((entry) => entry.club.id === club)) return me();
      const listed = { id: found.id, name: found.name };
      if (found.open) {
        server.account = {
          ...account,
          clubs: [...account.clubs, { club: listed, role: 'Pilot' }],
          join_asks: account.join_asks.filter((asked) => asked.id !== club)
        };
        (server.members[club] ??= []).push({ id: account.id, name: account.name, role: 'Pilot' });
      } else if (!account.join_asks.some((asked) => asked.id === club)) {
        server.account = { ...account, join_asks: [...account.join_asks, listed] };
        (server.joinAsks[club] ??= []).push({
          id: account.id,
          name: account.name,
          ...(account.profile.callsign ? { callsign: account.profile.callsign } : {}),
          asked_at: 1_700_000_000_000
        });
      }
      return me();
    }),
    leaveClub: vi.fn(async (club: string) => {
      const account = server.account ?? refuse('Sign in first.');
      const asked = account.join_asks.some((entry) => entry.id === club);
      const mine = account.clubs.find((entry) => entry.club.id === club);
      if (!asked && !mine) refuse('You are not in that club, and have not asked to join it.');
      const directors = (server.members[club] ?? []).filter((m) => m.role === 'Director').length;
      if (mine?.role === 'Director' && directors <= 1) {
        refuse('A club must keep at least one director.');
      }
      server.account = {
        ...account,
        clubs: account.clubs.filter((entry) => entry.club.id !== club),
        join_asks: account.join_asks.filter((entry) => entry.id !== club),
        pilots: account.pilots.filter((pilot) => pilot.club.id !== club)
      };
      server.joinAsks[club] = (server.joinAsks[club] ?? []).filter((p) => p.id !== account.id);
      server.members[club] = (server.members[club] ?? []).filter((m) => m.id !== account.id);
      return me();
    }),
    joinAsks: vi.fn(async (club: string): Promise<PersonView[]> => {
      runsClub(club);
      return (server.joinAsks[club] ?? []).map((person) => ({ ...person }));
    }),
    answerJoinAsk: vi.fn(async (club: string, account: string, yes: boolean) => {
      runsClub(club);
      const list = server.joinAsks[club] ?? [];
      const person =
        list.find((p) => p.id === account) ??
        refuse('That account has not asked to join the club.');
      server.joinAsks[club] = list.filter((p) => p.id !== account);
      if (yes)
        (server.members[club] ??= []).push({ id: person.id, name: person.name, role: 'Pilot' });
    }),
    findPeople: vi.fn(async (club: string, text: string): Promise<PersonView[]> => {
      runsClub(club);
      const wanted = text.trim().toLowerCase();
      if (wanted.length < 2) return [];
      const inside = new Set((server.members[club] ?? []).map((m) => m.id));
      return server.people
        .filter((p) => !inside.has(p.id))
        .filter(
          (p) =>
            p.name.toLowerCase().includes(wanted) ||
            (p.callsign ?? '').toLowerCase().includes(wanted)
        )
        .map((p) => ({ ...p }));
    }),
    addMember: vi.fn(async (club: string, account: string) => {
      runsClub(club);
      const person =
        server.people.find((p) => p.id === account) ?? refuse('There is no such account.');
      const list = (server.members[club] ??= []);
      if (!list.some((m) => m.id === account)) {
        list.push({ id: person.id, name: person.name, role: 'Pilot' });
      }
      server.joinAsks[club] = (server.joinAsks[club] ?? []).filter((p) => p.id !== account);
    }),
    setProfile: vi.fn(async (profile: PilotProfile) => {
      const account = server.account ?? refuse('Sign in first.');
      if (profile.color !== undefined && !/^#[0-9A-Fa-f]{6}$/.test(profile.color)) {
        refuse('A colour is a # and six letters or digits, like #FF8800.');
      }
      server.account = { ...account, profile: { ...profile } };
      return me();
    }),
    serverInfo: vi.fn(async (): Promise<ServerInfo> => info()),
    setOpenSignUp: vi.fn(async (open: boolean): Promise<ServerInfo> => {
      if (server.account?.owner !== true) refuse('Only an owner of this server can do that.');
      server.openSignUp = open;
      return info();
    }),
    setPasswordSignIn: vi.fn(async (on: boolean): Promise<ServerInfo> => {
      const account = server.account;
      if (account?.owner !== true) refuse('Only an owner of this server can do that.');
      if (!on && account?.sign_ins.every((held) => held.provider === 'password')) {
        refuse(
          'A password is the only way you sign in. Add another way to sign in first, or you would lock yourself out.'
        );
      }
      server.passwordOn = on;
      return info();
    }),
    passwordSignIn: vi.fn(async (username: string, password: string) => {
      if (!server.passwordOn) refuse(PASSWORDS_OFF);
      const held = server.passwords[username.trim().toLowerCase()];
      if (!held || held.password !== password) refuse('That username and password do not match.');
      server.account = held.account;
      return me();
    }),
    passwordSignUp: vi.fn(async (given: PasswordSignUp) => {
      if (!server.passwordOn) refuse(PASSWORDS_OFF);
      const username = given.username.trim().toLowerCase();
      const held: MySignIn = { provider: 'password', subject: username, label: username };
      const secret = given.invite;
      let account: MyAccount;
      if (secret?.startsWith('gfr_')) {
        // A re-connect link: the username and password become a way in to that account.
        const id = server.reconnects[secret] ?? refuse('That re-connect link does not work.');
        if (server.passwords[username] && server.passwords[username].account.id !== id) {
          refuse('That sign-in already belongs to an account.');
        }
        delete server.reconnects[secret];
        for (const [name, kept] of Object.entries(server.passwords)) {
          if (kept.account.id === id) delete server.passwords[name];
        }
        account = {
          ...ACE,
          id,
          name: accountName(id),
          sign_ins: [...(server.signIns[id] ?? []).filter((s) => s.provider !== 'password'), held]
        };
      } else {
        if (server.passwords[username]) refuse('That sign-in already belongs to an account.');
        const invite = secret === undefined ? undefined : server.links[secret];
        if (secret !== undefined && !invite) refuse('That invite does not work.');
        if (secret === undefined && !server.openSignUp) {
          refuse('You need an invite to make an account here. Ask your club for a link.');
        }
        if (secret !== undefined) delete server.links[secret];
        account = {
          ...ACE,
          id: `acct_${username}`,
          name: given.name ?? username,
          sign_ins: [held],
          clubs: invite?.club ? [{ club: invite.club, role: invite.role ?? 'Pilot' }] : []
        };
      }
      server.passwords[username] = { password: given.password, account };
      server.account = account;
      return me();
    }),
    setPassword: vi.fn(async (change: PasswordChange) => {
      if (!server.passwordOn) refuse(PASSWORDS_OFF);
      const account = server.account ?? refuse('Sign in first.');
      const mine = Object.entries(server.passwords).find(
        ([, kept]) => kept.account.id === account.id
      );
      if (mine) {
        if (mine[1].password !== change.current) refuse('That is not the password you have now.');
        mine[1].password = change.password;
        return me();
      }
      const username = (change.username ?? '').trim().toLowerCase();
      if (username === '') refuse('A username is 3 to 32 letters and digits.');
      if (server.passwords[username]) refuse('That sign-in already belongs to an account.');
      const next = {
        ...account,
        sign_ins: [
          ...account.sign_ins,
          { provider: 'password', subject: username, label: username }
        ]
      };
      server.account = next;
      server.passwords[username] = { password: change.password, account: next };
      return me();
    }),
    archiveClub: vi.fn(async (club: string, archived: boolean): Promise<ServerClub> => {
      if (server.account?.owner !== true) refuse('Only an owner of this server can do that.');
      const found =
        server.clubList.find((c) => c.id === club) ?? refuse('This server has no such club.');
      found.archived = archived;
      return { ...found };
    })
  } satisfies PeopleApi;
  return api;
}
