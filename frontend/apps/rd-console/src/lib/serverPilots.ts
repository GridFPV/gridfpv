/**
 * Pilots and the linked server (docs/primitives.md §13.7): the small decisions the Pilots
 * page makes. Pure, so they can be tested alone.
 */
import type { LinkPeople, LinkView, Pilot, PushAccount, PushPilot } from '@gridfpv/types';

/**
 * Whether a server is linked and switched on, so the Pilots page can offer what needs
 * one. A server that is not answering right now still counts: asking it says so.
 */
export function serverLinked(view: LinkView | undefined): boolean {
  return view !== undefined && view.state !== 'NotSet' && view.state !== 'Off';
}

/** What can be brought in from the server. */
export interface ToBringIn {
  /** Pilots the club has there and this Director does not. */
  pilots: PushPilot[];
  /** Members who hold no pilot, there or here. A new pilot would be made for each. */
  members: PushAccount[];
}

/**
 * Work out what the server has that this Director does not.
 *
 * A member is offered a new pilot only when they hold none on the server and none of the
 * pilots here is tied to them: someone who already has a pilot does not need a second.
 */
export function toBringIn(people: LinkPeople, mine: readonly Pilot[]): ToBringIn {
  const here = new Set(mine.map((pilot) => pilot.id));
  const holders = new Set<string>();
  for (const pilot of people.pilots) if (pilot.account) holders.add(pilot.account.id);
  for (const pilot of mine) if (pilot.server_account) holders.add(pilot.server_account.id);
  return {
    pilots: people.pilots.filter((pilot) => !here.has(pilot.id)),
    members: people.members.filter((member) => !holders.has(member.id))
  };
}

/**
 * Which of the club's pilots each member already holds, by member, other than `pilot`
 * itself: the callsign to show beside a member's name in the picker, so a director sees
 * that picking them moves nothing by surprise.
 */
export function heldElsewhere(people: LinkPeople, pilot: string): Map<string, string> {
  const held = new Map<string, string>();
  for (const other of people.pilots) {
    if (other.account && other.id !== pilot && !held.has(other.account.id)) {
      held.set(other.account.id, other.callsign);
    }
  }
  return held;
}

/** How many pilots a choice would add, in words for the button. */
export function addWords(count: number): string {
  if (count === 0) return 'Add pilots';
  return count === 1 ? 'Add 1 pilot' : `Add ${count} pilots`;
}
