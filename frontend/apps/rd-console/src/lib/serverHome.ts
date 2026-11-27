/**
 * Small decisions for the page a remote server shows (docs/primitives.md §13): where in it
 * the address points, and how an event is listed. Pure, so they can be tested alone.
 */
import type { ServerEvent } from '@gridfpv/types';

/** Where the page is. */
export type ServerPlace =
  /** The list of events. */
  | { kind: 'list' }
  /** One event of one club. */
  | { kind: 'event'; club: string; event: string }
  /** Signing in. `problem` is why the last try did not work, as the server said. */
  | { kind: 'sign-in'; problem?: string }
  /** An invite link. */
  | { kind: 'invite'; secret: string }
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
 * - `#/e/‹club›/‹event›`: one event
 * - `#/sign-in`, `#/sign-in?problem=‹code›`
 * - `#/invite/‹secret›`
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
    if (first === 'sign-in' && rest.length === 0) {
      const problem = new URLSearchParams(query).get('problem') ?? undefined;
      return problem ? { kind: 'sign-in', problem } : { kind: 'sign-in' };
    }
    if (first === 'invite' && rest.length === 1) return { kind: 'invite', secret: rest[0] };
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
    case 'sign-in':
      return place.problem ? `#/sign-in?problem=${encodeURIComponent(place.problem)}` : '#/sign-in';
    case 'invite':
      return `#/invite/${encodeURIComponent(place.secret)}`;
    case 'account':
      return '#/account';
    case 'club':
      return `#/club/${encodeURIComponent(place.club)}`;
    case 'results':
      return '#/results';
  }
}

/** The line under an event's name: its date and its place, whichever were given. */
export function eventDetail(event: Pick<ServerEvent, 'date' | 'location'>): string {
  return [event.date, event.location]
    .map((part) => part?.trim())
    .filter((part) => part !== undefined && part !== '')
    .join(' · ');
}
