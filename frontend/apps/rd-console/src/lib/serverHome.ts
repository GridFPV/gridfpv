/**
 * Small decisions for the page a remote server shows (docs/primitives.md §13): where in it
 * the address points, and how an event is listed. Pure, so they can be tested alone.
 */
import type { MyAccount, Pilot, PublicPilot, ServerEvent } from '@gridfpv/types';

/** The pages of the signed-in account. */
export const ACCOUNT_PAGES = ['profile', 'clubs', 'sign-in'] as const;
export type AccountPage = (typeof ACCOUNT_PAGES)[number];

/** The pages a club is run from, for those who look after it. */
export const MANAGE_PAGES = [
  'members',
  'invites',
  'pilots',
  'events',
  'tokens',
  'settings'
] as const;
export type ManagePage = (typeof MANAGE_PAGES)[number];

/** The pages the server itself is run from, for its owners. */
export const ADMIN_PAGES = ['clubs', 'sign-in', 'email'] as const;
export type AdminPage = (typeof ADMIN_PAGES)[number];

/** Where the page is. */
export type ServerPlace =
  /** The front page: the signed-in person's primary club, or every club. */
  | { kind: 'list' }
  /** Every club on the server. */
  | { kind: 'clubs' }
  /** One club, by its friendly address (§13.2): its events, and the way in. */
  | { kind: 'club'; word: string }
  /** One event of one club. */
  | { kind: 'event'; club: string; event: string }
  /**
   * One event, opened from the race code's QR at the race (§13.13). The page keeps the
   * code and goes on to the event.
   */
  | { kind: 'here'; club: string; event: string; code: string }
  /** Signing in. `problem` is why the last try did not work, as the server said. */
  | { kind: 'sign-in'; problem?: string }
  /** An invite link. */
  | { kind: 'invite'; secret: string }
  /** A re-connect link, for someone who can no longer sign in (§13.8). */
  | { kind: 'reconnect'; secret: string }
  /** The link sent to an email address, to confirm it. */
  | { kind: 'confirm-email'; secret: string }
  /** The signed-in account's own settings. */
  | { kind: 'account'; page: AccountPage }
  /** Running a club, for those who look after it. `club` is its friendly address. */
  | { kind: 'manage'; club: string; page: ManagePage }
  /** Running the server, for its owners. */
  | { kind: 'admin'; page: AdminPage }
  /** The way in to the admin area. The page goes on to the first thing the person runs. */
  | { kind: 'admin-home' }
  /** The events the signed-in account's pilots flew. */
  | { kind: 'results' };

/** The four parts of the page. Each has a row of pages of its own. */
export type ServerSide = 'events' | 'account' | 'club' | 'server';

/** Which part a place belongs to. A link someone was sent (an invite, say) is on none. */
export function sideOfPlace(place: ServerPlace): ServerSide | undefined {
  switch (place.kind) {
    case 'list':
    case 'clubs':
    case 'club':
    case 'event':
    case 'here':
    case 'results':
      return 'events';
    case 'account':
      return 'account';
    case 'manage':
      return 'club';
    case 'admin':
      return 'server';
    default:
      return undefined;
  }
}

/**
 * The two areas of the page. The pilot area is what everyone sees: events, clubs, results
 * and a person's own account. The admin area is apart from it, for those who run a club or
 * the server. Every address of the admin area starts `#/admin`.
 */
export type ServerMode = 'pilot' | 'admin';

/** Which area a place is in. */
export function modeOfPlace(place: ServerPlace): ServerMode {
  return place.kind === 'manage' || place.kind === 'admin' || place.kind === 'admin-home'
    ? 'admin'
    : 'pilot';
}

/** A page of a part, when the word after it names one. The first page when there is none. */
function pageOf<T extends string>(pages: readonly T[], word: string | undefined): T | undefined {
  if (word === undefined) return pages[0];
  return pages.find((page) => page === word);
}

/**
 * Read the part of the address after the `#`. Anything that is not one of the pages is the
 * list.
 *
 * - `#/clubs`: every club
 * - `#/club/‹word›`: one club, by its friendly address
 * - `#/event/‹club›/‹event›`: one event
 * - `#/here/‹club›/‹event›/‹code›`: one event, with the race code shown at the race
 * - `#/results`
 * - `#/sign-in`, `#/sign-in?problem=‹code›`
 * - `#/invite/‹secret›`, `#/reconnect/‹secret›`, `#/confirm-email/‹secret›`
 * - `#/account`, `#/account/clubs`, `#/account/sign-in`
 * - `#/admin`: the admin area
 * - `#/admin/club/‹word›`, `#/admin/club/‹word›/‹page›`: running a club
 * - `#/admin/server`, `#/admin/server/sign-in`, `#/admin/server/email`: running the server
 */
export function parseServerHash(hash: string): ServerPlace {
  const [path, query = ''] = hash.replace(/^#\/?/, '').split('?');
  const parts = path.split('/').filter((part) => part !== '');
  try {
    const [first, ...rest] = parts.map((part) => decodeURIComponent(part));
    if (first === 'event' && rest.length === 2) {
      return { kind: 'event', club: rest[0], event: rest[1] };
    }
    if (first === 'clubs' && rest.length === 0) return { kind: 'clubs' };
    if (first === 'club' && rest.length === 1) return { kind: 'club', word: rest[0] };
    if (first === 'here' && rest.length === 3) {
      return { kind: 'here', club: rest[0], event: rest[1], code: rest[2] };
    }
    if (first === 'sign-in' && rest.length === 0) {
      const problem = new URLSearchParams(query).get('problem') ?? undefined;
      return problem ? { kind: 'sign-in', problem } : { kind: 'sign-in' };
    }
    if (first === 'invite' && rest.length === 1) return { kind: 'invite', secret: rest[0] };
    if (first === 'reconnect' && rest.length === 1) return { kind: 'reconnect', secret: rest[0] };
    if (first === 'confirm-email' && rest.length === 1) {
      return { kind: 'confirm-email', secret: rest[0] };
    }
    if (first === 'account' && rest.length <= 1) {
      const page = pageOf(ACCOUNT_PAGES, rest[0]);
      if (page) return { kind: 'account', page };
    }
    if (first === 'admin') {
      const [area, ...inside] = rest;
      if (area === undefined) return { kind: 'admin-home' };
      if (area === 'club' && (inside.length === 1 || inside.length === 2)) {
        const page = pageOf(MANAGE_PAGES, inside[1]);
        if (page) return { kind: 'manage', club: inside[0], page };
      }
      if (area === 'server' && inside.length <= 1) {
        const page = pageOf(ADMIN_PAGES, inside[0]);
        if (page) return { kind: 'admin', page };
      }
      // Anything else under `#/admin` is still the admin area, not the list of events.
      return { kind: 'admin-home' };
    }
    if (first === 'results' && rest.length === 0) return { kind: 'results' };
  } catch {
    // A badly written address is the list.
  }
  return { kind: 'list' };
}

export function formatServerHash(place: ServerPlace): string {
  switch (place.kind) {
    case 'list':
      return '#/';
    case 'event':
      return `#/event/${encodeURIComponent(place.club)}/${encodeURIComponent(place.event)}`;
    case 'clubs':
      return '#/clubs';
    case 'club':
      return `#/club/${encodeURIComponent(place.word)}`;
    case 'here':
      return `#/here/${encodeURIComponent(place.club)}/${encodeURIComponent(place.event)}/${encodeURIComponent(place.code)}`;
    case 'sign-in':
      return place.problem ? `#/sign-in?problem=${encodeURIComponent(place.problem)}` : '#/sign-in';
    case 'invite':
      return `#/invite/${encodeURIComponent(place.secret)}`;
    case 'reconnect':
      return `#/reconnect/${encodeURIComponent(place.secret)}`;
    case 'confirm-email':
      return `#/confirm-email/${encodeURIComponent(place.secret)}`;
    case 'account':
      return place.page === ACCOUNT_PAGES[0] ? '#/account' : `#/account/${place.page}`;
    case 'manage': {
      const club = `#/admin/club/${encodeURIComponent(place.club)}`;
      return place.page === MANAGE_PAGES[0] ? club : `${club}/${place.page}`;
    }
    case 'admin':
      return place.page === ADMIN_PAGES[0] ? '#/admin/server' : `#/admin/server/${place.page}`;
    case 'admin-home':
      return '#/admin';
    case 'results':
      return '#/results';
  }
}

/**
 * The pilot the signed-in account is in one event (docs/primitives.md §13.7): a pilot of
 * that club the account holds, and that the event names. `undefined` for anyone else, who
 * only looks.
 *
 * A pilot the account has only asked for is not theirs yet. The callsign is the one the
 * event has for the pilot, which is what its heats show.
 */
export function pilotHere(
  account: Pick<MyAccount, 'pilots'> | null | undefined,
  club: string,
  eventPilots: readonly Pick<Pilot, 'id' | 'callsign'>[]
): PublicPilot | undefined {
  for (const mine of account?.pilots ?? []) {
    if (!mine.held || mine.club.id !== club) continue;
    const here = eventPilots.find((pilot) => pilot.id === mine.id);
    if (here) return { id: here.id, callsign: here.callsign };
  }
  return undefined;
}

/** The line under an event's name: its date and its place, whichever were given. */
export function eventDetail(event: Pick<ServerEvent, 'date' | 'location'>): string {
  return [event.date, event.location]
    .map((part) => part?.trim())
    .filter((part) => part !== undefined && part !== '')
    .join(' · ');
}
