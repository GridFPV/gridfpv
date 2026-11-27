/**
 * Small decisions for the page a remote server shows (docs/primitives.md §13): where in it
 * the address points, and how an event is listed. Pure, so they can be tested alone.
 */
import type { MyAccount, Pilot, PublicPilot, ServerEvent } from '@gridfpv/types';

/** Where the page is. */
export type ServerPlace =
  /** The front page: the signed-in person's primary club, or every club. */
  | { kind: 'list' }
  /** Every club on the server, with its events. */
  | { kind: 'clubs' }
  /** One club, by its friendly address (§13.2). */
  | { kind: 'c'; word: string }
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
  /** The signed-in account. */
  | { kind: 'account' }
  /** A club's members, invites and pilots, for those who look after it. */
  | { kind: 'club'; club: string }
  /** The events the signed-in account's pilots flew. */
  | { kind: 'results' };

/**
 * Read the part of the address after the `#`. Anything that is not one of the pages is the
 * list.
 *
 * - `#/clubs`: every club
 * - `#/c/‹word›`: one club, by its friendly address
 * - `#/e/‹club›/‹event›`: one event
 * - `#/here/‹club›/‹event›/‹code›`: one event, with the race code shown at the race
 * - `#/sign-in`, `#/sign-in?problem=‹code›`
 * - `#/invite/‹secret›`
 * - `#/reconnect/‹secret›`
 * - `#/account`
 * - `#/club/‹club›`
 * - `#/results`
 */
export function parseServerHash(hash: string): ServerPlace {
  const [path, query = ''] = hash.replace(/^#\/?/, '').split('?');
  const parts = path.split('/').filter((part) => part !== '');
  try {
    const [first, ...rest] = parts.map((part) => decodeURIComponent(part));
    if (first === 'e' && rest.length === 2) return { kind: 'event', club: rest[0], event: rest[1] };
    if (first === 'clubs' && rest.length === 0) return { kind: 'clubs' };
    if (first === 'c' && rest.length === 1) return { kind: 'c', word: rest[0] };
    if (first === 'here' && rest.length === 3) {
      return { kind: 'here', club: rest[0], event: rest[1], code: rest[2] };
    }
    if (first === 'sign-in' && rest.length === 0) {
      const problem = new URLSearchParams(query).get('problem') ?? undefined;
      return problem ? { kind: 'sign-in', problem } : { kind: 'sign-in' };
    }
    if (first === 'invite' && rest.length === 1) return { kind: 'invite', secret: rest[0] };
    if (first === 'reconnect' && rest.length === 1) return { kind: 'reconnect', secret: rest[0] };
    if (first === 'account' && rest.length === 0) return { kind: 'account' };
    if (first === 'club' && rest.length === 1) return { kind: 'club', club: rest[0] };
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
      return `#/e/${encodeURIComponent(place.club)}/${encodeURIComponent(place.event)}`;
    case 'clubs':
      return '#/clubs';
    case 'c':
      return `#/c/${encodeURIComponent(place.word)}`;
    case 'here':
      return `#/here/${encodeURIComponent(place.club)}/${encodeURIComponent(place.event)}/${encodeURIComponent(place.code)}`;
    case 'sign-in':
      return place.problem ? `#/sign-in?problem=${encodeURIComponent(place.problem)}` : '#/sign-in';
    case 'invite':
      return `#/invite/${encodeURIComponent(place.secret)}`;
    case 'reconnect':
      return `#/reconnect/${encodeURIComponent(place.secret)}`;
    case 'account':
      return '#/account';
    case 'club':
      return `#/club/${encodeURIComponent(place.club)}`;
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
