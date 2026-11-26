/**
 * Small decisions for the page a remote server shows (docs/primitives.md §13): where in it
 * the address points, and how an event is listed. Pure, so they can be tested alone.
 */
import type { ServerEvent } from '@gridfpv/types';

/** Where the page is: the list of events, or one event of one club. */
export type ServerPlace = { kind: 'list' } | { kind: 'event'; club: string; event: string };

/**
 * Read the part of the address after the `#`. `#/e/‹club›/‹event›` is one event; anything
 * else is the list.
 */
export function parseServerHash(hash: string): ServerPlace {
  const parts = hash
    .replace(/^#\/?/, '')
    .split('/')
    .filter((part) => part !== '');
  if (parts.length === 3 && parts[0] === 'e') {
    try {
      return {
        kind: 'event',
        club: decodeURIComponent(parts[1]),
        event: decodeURIComponent(parts[2])
      };
    } catch {
      // A badly written address is the list.
    }
  }
  return { kind: 'list' };
}

export function formatServerHash(place: ServerPlace): string {
  return place.kind === 'list'
    ? '#/'
    : `#/e/${encodeURIComponent(place.club)}/${encodeURIComponent(place.event)}`;
}

/** The line under an event's name: its date and its place, whichever were given. */
export function eventDetail(event: Pick<ServerEvent, 'date' | 'location'>): string {
  return [event.date, event.location]
    .map((part) => part?.trim())
    .filter((part) => part !== undefined && part !== '')
    .join(' · ');
}
