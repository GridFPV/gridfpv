/**
 * Words and small decisions for the Server page: the clubs Grid can send to, and which
 * club each event goes to (docs/primitives.md §13). Pure, so they can be tested alone.
 */
import type {
  ClubSettings,
  LinkClub,
  LinkEvent,
  LinkState,
  LinkTrust,
  LinkView
} from '@gridfpv/types';

type Tone = 'neutral' | 'success' | 'warn' | 'danger' | 'info';

/** A web address without its `https://`, for when it stands in for a name. */
function hostOf(url: string): string {
  return url.replace(/^https?:\/\//i, '').replace(/\/+$/, '');
}

/**
 * What a saved club is called: the name the director gave it, else the club's own name on
 * the server, else the server's address. Never the club's id.
 */
export function clubName(club: LinkClub): string {
  return club.name.trim() || club.club?.name || hostOf(club.url);
}

/** The saved club with this id, when it is still saved. */
export function clubById(view: LinkView, id: string | undefined): LinkClub | undefined {
  return id === undefined ? undefined : view.clubs.find((club) => club.id === id);
}

/** The event that is open, as the link knows it. */
export function openEvent(view: LinkView): LinkEvent | undefined {
  return view.events.find((event) => event.id === view.open);
}

/** The connection's state in a word or two. */
export function linkLabel(view: LinkView): string {
  const state = view.state;
  if (state === 'NotSet') return view.open === undefined ? 'No event open' : 'Not sent';
  if (state === 'Connecting') return 'Connecting';
  if (state === 'Connected') {
    const club = clubById(view, view.club);
    return club ? `Connected to ${clubName(club)}` : 'Connected';
  }
  return 'Not connected';
}

export function linkTone(state: LinkState): Tone {
  if (state === 'Connected') return 'success';
  if (state === 'Connecting') return 'info';
  if (state === 'NotSet') return 'neutral';
  return 'danger';
}

/** Why the link is not connected, when the Director has said. */
export function linkProblem(state: LinkState): string | undefined {
  return typeof state === 'object' && 'Failed' in state ? state.Failed.message : undefined;
}

/** Where the open event goes, as a sentence. */
export function linkLine(view: LinkView): string {
  const open = openEvent(view);
  if (!open) return 'Open an event to send it. Grid connects to the club of the open event only.';
  const club = clubById(view, view.club);
  if (club) return `${open.name} is sent to ${clubName(club)}.`;
  return view.clubs.length === 0
    ? `${open.name} is not sent to a server. Save a club first.`
    : `${open.name} is not sent to a server. Pick a club for it under Events.`;
}

/** How one event stands on the server, in a few words. */
export function eventLabel(event: LinkEvent): string {
  const state = event.state;
  if (state === 'NoClub') return 'Not sent';
  if (state === 'Waiting') return 'Waiting';
  if (state === 'Sent') return 'On the server';
  if (state === 'Sending') return `Sending, ${event.sent} of ${event.len}`;
  if (state === 'Different') return 'A different event is on the server';
  if (state === 'Removed') return 'Removed on the server';
  return 'Could not be sent';
}

export function eventTone(event: LinkEvent): Tone {
  const state = event.state;
  if (state === 'NoClub' || state === 'Waiting') return 'neutral';
  if (state === 'Sent') return 'success';
  if (state === 'Sending') return 'info';
  if (state === 'Different' || state === 'Removed') return 'warn';
  return 'danger';
}

/**
 * What else to say of an event: why it could not be sent, or what it is waiting for. An
 * event of the connected club that waits is only waiting its turn, and needs no words.
 */
export function eventNote(event: LinkEvent, view: LinkView): string | undefined {
  const state = event.state;
  if (typeof state === 'object' && 'Failed' in state) return state.Failed.message;
  if (state === 'Waiting' && event.club !== view.club) {
    const club = clubById(view, event.club);
    return club
      ? `Sent when an event of ${clubName(club)} is open.`
      : 'Sent when an event of its club is open.';
  }
  return undefined;
}

/** How many events go to a saved club. */
export function eventsOf(view: LinkView, club: string): number {
  return view.events.filter((event) => event.club === club).length;
}

/** How often to read the link again, in milliseconds: sooner while something is moving. */
export function refreshEvery(view: LinkView | undefined): number {
  if (!view || view.club === undefined) return 5000;
  const settled =
    view.state === 'Connected' &&
    view.events
      .filter((event) => event.club === view.club)
      .every((event) => event.state === 'Sent' || event.state === 'Removed');
  return settled ? 4000 : 1000;
}

/** An address as typed, tidied the way the Director tidies it: no spaces, no slash after. */
const tidy = (url: string): string => url.trim().replace(/\/+$/, '');

/** The form for one club. The token box is empty unless the director is typing one. */
export interface ClubForm {
  name: string;
  url: string;
  token: string;
  /** Whether extra certificate authorities are used. */
  extra: boolean;
  pem: string;
}

/** The form for a saved club, or an empty one for a new club. */
export function formOf(club: LinkClub | undefined): ClubForm {
  if (!club) return { name: '', url: '', token: '', extra: false, pem: '' };
  const trust = club.trust;
  const pem =
    typeof trust === 'object' && 'ExtraAuthorities' in trust ? trust.ExtraAuthorities.pem : '';
  return { name: club.name, url: club.url, token: '', extra: pem !== '', pem };
}

/** The trust a form asks for. */
export function trustOf(form: ClubForm): LinkTrust {
  return form.extra && form.pem.trim() !== ''
    ? { ExtraAuthorities: { pem: form.pem.trim() } }
    : 'System';
}

/**
 * What to send for a form. The token is left out when the box is empty, which keeps the
 * one a saved club already has.
 */
export function settingsOf(form: ClubForm): ClubSettings {
  const token = form.token.trim();
  return {
    name: form.name.trim(),
    url: tidy(form.url),
    ...(token === '' ? {} : { token }),
    trust: trustOf(form)
  };
}

/** Whether the form differs from the saved club. A new club's form always does. */
export function changed(form: ClubForm, club: LinkClub | undefined): boolean {
  if (!club) return true;
  const set = formOf(club);
  return (
    form.name.trim() !== set.name.trim() ||
    tidy(form.url) !== tidy(set.url) ||
    form.token.trim() !== '' ||
    JSON.stringify(trustOf(form)) !== JSON.stringify(trustOf(set))
  );
}

/**
 * What is wrong with the form, in words, before it is sent. The Director checks it all
 * again; this is so the director sees it while typing. `club` is the saved club being
 * changed, or nothing for a new one.
 */
export function formProblem(form: ClubForm, club: LinkClub | undefined): string | undefined {
  const url = tidy(form.url);
  if (url === '') return "Give the server's address.";
  if (!/^https?:\/\//i.test(url)) return 'The address should start with https://';
  const token = form.token.trim();
  if (token !== '' && !token.startsWith('gfs_')) {
    return 'A link token starts with gfs_. It is made on the server.';
  }
  // A saved club keeps its token, unless the address is another server's.
  if (token === '' && !(club && url === tidy(club.url))) {
    return club
      ? 'Give the link token for this address. The old one is not sent to another server.'
      : 'Give the link token. It is made on the server, for your club.';
  }
  if ([...form.name.trim()].length > 60) return 'That name is too long. Keep it to 60 letters.';
  if (form.extra && form.pem.trim() !== '' && !form.pem.includes('-----BEGIN CERTIFICATE-----')) {
    return 'That is not a certificate. It should start with -----BEGIN CERTIFICATE-----';
  }
  return undefined;
}

/** What a look at a server's certificate found, as the page says it. */
export type CertificateVerdict =
  /** Grid takes it already. Nothing to accept. */
  | 'trusted'
  /** The server made it for itself. Accepting this one certificate is enough. */
  | 'accept'
  /** Signed by an authority this computer does not know, or for another address. */
  | 'authority';

export function certificateVerdict(look: {
  trusted: boolean;
  stands_alone: boolean;
}): CertificateVerdict {
  if (look.trusted) return 'trusted';
  return look.stands_alone ? 'accept' : 'authority';
}

/**
 * A fingerprint in short rows, so two people can read it to each other: eight pairs to a
 * row.
 */
export function fingerprintRows(fingerprint: string): string[] {
  const pairs = fingerprint.split(':').filter((pair) => pair !== '');
  const rows: string[] = [];
  for (let at = 0; at < pairs.length; at += 8) rows.push(pairs.slice(at, at + 8).join(':'));
  return rows;
}
