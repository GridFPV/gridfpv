/**
 * Words and small decisions for the Server page (the link to a remote server). Pure, so
 * they can be tested alone.
 */
import type { LinkEvent, LinkSettings, LinkState, LinkTrust, LinkView } from '@gridfpv/types';

type Tone = 'neutral' | 'success' | 'warn' | 'danger' | 'info';

/** The link's state in a word or two. */
export function linkLabel(view: LinkView): string {
  const state = view.state;
  if (state === 'NotSet') return 'Not set';
  if (state === 'Off') return 'Off';
  if (state === 'Connecting') return 'Connecting';
  if (state === 'Connected') return view.club ? `Connected to ${view.club.name}` : 'Connected';
  return 'Not connected';
}

export function linkTone(state: LinkState): Tone {
  if (state === 'Connected') return 'success';
  if (state === 'Connecting') return 'info';
  if (state === 'NotSet' || state === 'Off') return 'neutral';
  return 'danger';
}

/** Why the link is not connected, when the Director has said. */
export function linkProblem(state: LinkState): string | undefined {
  return typeof state === 'object' && 'Failed' in state ? state.Failed.message : undefined;
}

/** How one event stands on the server, in a few words. */
export function eventLabel(event: LinkEvent): string {
  const state = event.state;
  if (state === 'Sent') return 'On the server';
  if (state === 'Sending') return `Sending, ${event.sent} of ${event.len}`;
  if (state === 'Different') return 'A different event is on the server';
  if (state === 'Removed') return 'Removed on the server';
  return 'Not sent';
}

export function eventTone(event: LinkEvent): Tone {
  const state = event.state;
  if (state === 'Sent') return 'success';
  if (state === 'Sending') return 'info';
  if (state === 'Different' || state === 'Removed') return 'warn';
  return 'danger';
}

export function eventProblem(event: LinkEvent): string | undefined {
  const state = event.state;
  return typeof state === 'object' && 'Failed' in state ? state.Failed.message : undefined;
}

/** How often to read the link again, in milliseconds: sooner while something is moving. */
export function refreshEvery(view: LinkView | undefined): number {
  if (!view || !view.on) return 5000;
  const settled =
    view.state === 'Connected' &&
    view.events.every((event) => event.state === 'Sent' || event.state === 'Removed');
  return settled ? 4000 : 1000;
}

/** An address as typed, tidied the way the Director tidies it: no spaces, no slash after. */
const tidy = (url: string): string => url.trim().replace(/\/+$/, '');

/** The form on the page. The token box is empty unless the director is typing a new one. */
export interface LinkForm {
  url: string;
  token: string;
  on: boolean;
  /** Whether extra certificate authorities are used. */
  extra: boolean;
  pem: string;
}

export function formOf(view: LinkView): LinkForm {
  const trust = view.trust;
  const pem =
    typeof trust === 'object' && 'ExtraAuthorities' in trust ? trust.ExtraAuthorities.pem : '';
  return { url: view.url, token: '', on: view.on, extra: pem !== '', pem };
}

/** The trust a form asks for. */
export function trustOf(form: LinkForm): LinkTrust {
  return form.extra && form.pem.trim() !== ''
    ? { ExtraAuthorities: { pem: form.pem.trim() } }
    : 'System';
}

/**
 * What to send for a form. The token is left out when the box is empty, which keeps the
 * one already set.
 */
export function settingsOf(form: LinkForm): LinkSettings {
  const token = form.token.trim();
  return {
    url: tidy(form.url),
    ...(token === '' ? {} : { token }),
    on: form.on,
    trust: trustOf(form)
  };
}

/** Whether the form differs from what is set. */
export function changed(form: LinkForm, view: LinkView): boolean {
  const set = formOf(view);
  return (
    tidy(form.url) !== tidy(set.url) ||
    form.token.trim() !== '' ||
    JSON.stringify(trustOf(form)) !== JSON.stringify(trustOf(set))
  );
}

/**
 * What is wrong with the form, in words, before it is sent. The Director checks it all
 * again; this is so the director sees it while typing.
 */
export function formProblem(form: LinkForm, view: LinkView): string | undefined {
  const url = tidy(form.url);
  if (url !== '' && !/^https?:\/\//i.test(url)) {
    return 'The address should start with https://';
  }
  const token = form.token.trim();
  if (token !== '' && !token.startsWith('gfs_')) {
    return 'A link token starts with gfs_. It is made on the server.';
  }
  if (form.on && (url === '' || (token === '' && !(view.has_token && url === tidy(view.url))))) {
    return 'Give the address and the link token first.';
  }
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
