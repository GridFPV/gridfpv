/**
 * Getting back in when a sign-in is lost (docs/primitives.md §13.8): the dialog an owner
 * of the server helps a member from, and the page a re-connect link opens.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { parseServerHash } from '../src/lib/serverHome.js';
import { reconnectLink, untilDay } from '../src/lib/serverPeople.js';
import { eventView } from './competition.js';
import {
  ACE,
  LINK_UNTIL,
  NORTH,
  SOUTH,
  fakePeople,
  fakeServer,
  type FakeServer
} from './serverPeopleFake.js';
import { fakeLiveSession } from './support.js';

const OZ: MyAccount = {
  id: 'acct_oz',
  name: 'Oz',
  owner: true,
  sign_ins: [{ provider: 'discord', subject: '900', label: 'oz' }],
  clubs: [],
  pilots: []
};

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  location.hash = '';
});
afterEach(() => {
  cleanup();
  location.hash = '';
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function open(hash: string, server: FakeServer) {
  location.hash = hash;
  const people = fakePeople(server);
  const load = {
    clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH, SOUTH]),
    events: vi.fn(async (): Promise<ServerEvent[]> => [])
  };
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server };
}

/** The owner opens North's page and the dialog for one member. */
async function help(name: string, server: FakeServer = fakeServer({ account: OZ })) {
  const opened = open('#/club/north', server);
  await fireEvent.click(await screen.findByRole('button', { name: `How ${name} signs in` }));
  const dialog = await screen.findByRole('dialog');
  await within(dialog).findByText('Re-connect link');
  return { ...opened, dialog };
}

describe('reconnectLink and untilDay', () => {
  it('makes the address a person opens, which the page reads back', () => {
    const link = reconnectLink('https://grid.example.org/', 'gfr_a-b_C');
    expect(link).toBe('https://grid.example.org/#/reconnect/gfr_a-b_C');
    expect(parseServerHash(new URL(link).hash)).toEqual({ kind: 'reconnect', secret: 'gfr_a-b_C' });
  });

  it('says the day a link runs out', () => {
    expect(untilDay(Date.UTC(2026, 11, 4, 12), 'en-GB')).toBe('until 4 December');
  });
});

describe('who is offered it', () => {
  it('gives an owner a button for each member', async () => {
    open('#/club/north', fakeServer({ account: OZ }));
    expect(await screen.findByRole('button', { name: 'How Ace signs in' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'How Bee signs in' })).toBeInTheDocument();
  });

  it("gives a club's director none: an account is for the whole server", async () => {
    const { people } = open('#/club/north', fakeServer({ account: ACE }));
    await screen.findByRole('button', { name: 'Remove Bee' });
    expect(screen.queryByRole('button', { name: /signs in$/ })).toBeNull();
    expect(people.accountSignIns).not.toHaveBeenCalled();
  });
});

describe("an owner looking at a member's sign-ins", () => {
  it('lists them by the name of the provider, never its id', async () => {
    const { dialog, people } = await help('Bee');
    expect(people.accountSignIns).toHaveBeenCalledWith('acct_bee');
    const rows = within(dialog).getAllByTestId('member-sign-in');
    expect(rows.map((row) => row.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'Discord bee_fpv Take off',
      'Google Bee B Take off'
    ]);
    expect(dialog).not.toHaveTextContent('g-bee');
    expect(dialog).not.toHaveTextContent('222');
    expect(within(dialog).getByRole('heading', { name: 'How Bee signs in' })).toBeInTheDocument();
  });

  it('asks before taking one off, says what it does, and does nothing on Cancel', async () => {
    const { dialog, people, server } = await help('Bee');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take off Discord bee_fpv' }));
    expect(within(dialog).getByTestId('drop-sure')).toHaveTextContent(
      "Take Discord bee_fpv off Bee's account?"
    );
    expect(dialog).toHaveTextContent('Bee is signed out everywhere');
    expect(dialog).not.toHaveTextContent('their only one');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(people.dropSignIn).not.toHaveBeenCalled();
    expect(server.signIns.acct_bee).toHaveLength(2);
    expect(within(dialog).getAllByTestId('member-sign-in')).toHaveLength(2);
  });

  it('takes one off and shows what is left', async () => {
    const { dialog, people, server } = await help('Bee');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take off Discord bee_fpv' }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take it off' }));
    await waitFor(() => expect(within(dialog).getAllByTestId('member-sign-in')).toHaveLength(1));
    expect(people.dropSignIn).toHaveBeenCalledWith('acct_bee', 'discord', '222');
    expect(server.signedOut).toEqual(['acct_bee']);
    expect(within(dialog).getByTestId('member-sign-in')).toHaveTextContent('Google');
  });

  it('warns when it is their only one, and then says they have no way to sign in', async () => {
    const server = fakeServer({ account: OZ });
    server.members.north = [{ id: 'acct_ace', name: 'Ace', role: 'Director' }];
    const { dialog } = await help('Ace', server);
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take off Discord ace_fpv' }));
    expect(dialog).toHaveTextContent(
      'It is their only one, so they cannot sign in until you send them a re-connect link.'
    );
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take it off' }));
    expect(await within(dialog).findByTestId('no-sign-ins')).toHaveTextContent(
      'Ace has no way to sign in. Send them a re-connect link.'
    );
    expect(server.signIns.acct_ace).toEqual([]);
  });

  it("offers no way to take off the owner's own last sign-in", async () => {
    const server = fakeServer({ account: OZ });
    server.members.north.push({ id: 'acct_oz', name: 'Oz', role: 'Pilot' });
    server.signIns.acct_oz = [...OZ.sign_ins];
    const { dialog } = await help('Oz', server);
    expect(within(dialog).getAllByTestId('member-sign-in')).toHaveLength(1);
    expect(within(dialog).queryByRole('button', { name: /^Take off/ })).toBeNull();
  });

  it("shows the server's words when it refuses, and keeps the list", async () => {
    const { dialog, people } = await help('Bee');
    people.dropSignIn.mockRejectedValueOnce(new Error('The server could not write its accounts.'));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take off Google Bee B' }));
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take it off' }));
    expect(await within(dialog).findByTestId('sign-ins-error')).toHaveTextContent(
      'The server could not write its accounts.'
    );
    expect(within(dialog).getAllByTestId('member-sign-in')).toHaveLength(2);
  });

  it('says so when the account could not be read', async () => {
    const server = fakeServer({ account: OZ });
    delete server.signIns.acct_bee;
    open('#/club/north', server);
    await fireEvent.click(await screen.findByRole('button', { name: 'How Bee signs in' }));
    const dialog = await screen.findByRole('dialog');
    expect(await within(dialog).findByTestId('sign-ins-error')).toHaveTextContent(
      'There is no such account.'
    );
    expect(within(dialog).queryByRole('button', { name: 'Make a re-connect link' })).toBeNull();
  });
});

describe('a re-connect link', () => {
  it('is made, shown once with who to send it to, and can be copied', async () => {
    const write = vi.fn(async () => undefined);
    Object.defineProperty(navigator, 'clipboard', {
      value: { writeText: write },
      configurable: true
    });
    const { dialog, people, server } = await help('Bee');
    expect(within(dialog).queryByTestId('reconnect-waiting')).toBeNull();
    expect(dialog).toHaveTextContent('send it only to Bee');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Make a re-connect link' }));
    const made = await within(dialog).findByTestId('reconnect-made');
    expect(people.makeReconnect).toHaveBeenCalledWith('acct_bee');
    const secret = Object.keys(server.reconnects)[0];
    const link = `${location.origin}/#/reconnect/${secret}`;
    expect(within(made).getByTestId('reconnect-link')).toHaveTextContent(link);
    expect(made).toHaveTextContent('Send this link to Bee. It is shown this once.');
    await fireEvent.click(within(made).getByRole('button', { name: 'Copy' }));
    expect(write).toHaveBeenCalledWith(link);
    expect(within(dialog).getByTestId('reconnect-waiting')).toHaveTextContent(
      `A link is waiting, ${untilDay(LINK_UNTIL)}.`
    );
  });

  it('is not shown again when the dialog is opened later, only that one is waiting', async () => {
    const server = fakeServer({ account: OZ, reconnects: { gfr_old: 'acct_bee' } });
    const { dialog } = await help('Bee', server);
    expect(within(dialog).getByTestId('reconnect-waiting')).toBeInTheDocument();
    expect(within(dialog).queryByTestId('reconnect-made')).toBeNull();
    expect(dialog).not.toHaveTextContent('gfr_old');
    expect(dialog).toHaveTextContent('A new link stops the one that is waiting.');
    // A new one takes the old one's place.
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Make a new link' }));
    await within(dialog).findByTestId('reconnect-made');
    expect(Object.keys(server.reconnects)).toEqual(['gfr_secret1']);
  });

  it('is taken back', async () => {
    const server = fakeServer({ account: OZ, reconnects: { gfr_old: 'acct_bee' } });
    const { dialog, people } = await help('Bee', server);
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Take back the link' }));
    await waitFor(() => expect(within(dialog).queryByTestId('reconnect-waiting')).toBeNull());
    expect(people.revokeReconnect).toHaveBeenCalledWith('acct_bee');
    expect(server.reconnects).toEqual({});
    expect(within(dialog).getByRole('button', { name: 'Make a re-connect link' })).toBeEnabled();
  });

  it('is gone from the page when the dialog is closed and opened for someone else', async () => {
    const { dialog } = await help('Bee');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Make a re-connect link' }));
    await within(dialog).findByTestId('reconnect-made');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(screen.queryByTestId('member-sign-ins')).toBeNull());
    expect(document.body).not.toHaveTextContent('gfr_secret1');
    await fireEvent.click(screen.getByRole('button', { name: 'How Ace signs in' }));
    const again = await screen.findByRole('dialog');
    await within(again).findByText('Re-connect link');
    expect(within(again).queryByTestId('reconnect-made')).toBeNull();
    expect(within(again).getByRole('heading', { name: 'How Ace signs in' })).toBeInTheDocument();
  });
});

describe('the page a re-connect link opens', () => {
  it('says whose account it is for and sends the person to sign in with the link along', async () => {
    const { people } = open('#/reconnect/gfr_x', fakeServer({ reconnects: { gfr_x: 'acct_bee' } }));
    expect(await screen.findByTestId('reconnect-for')).toHaveTextContent('Get back in to Bee');
    expect(people.reconnect).toHaveBeenCalledWith('gfr_x');
    const page = screen.getByTestId('server-reconnect');
    expect(page).toHaveTextContent('Your clubs, pilots and results stay as they are.');
    expect(page).not.toHaveTextContent('gfr_x');
    const discord = within(page).getByRole('link', { name: 'Continue with Discord' });
    expect(discord.getAttribute('href')).toBe('/auth/discord/start?invite=gfr_x');
    expect(
      within(page).getByRole('link', { name: 'Continue with Google' }).getAttribute('href')
    ).toBe('/auth/google/start?invite=gfr_x');
    expect(screen.queryByTestId('reconnect-signed-in')).toBeNull();
  });

  it('tells someone signed in as another account what going on will do', async () => {
    open('#/reconnect/gfr_x', fakeServer({ account: OZ, reconnects: { gfr_x: 'acct_bee' } }));
    expect(await screen.findByTestId('reconnect-signed-in')).toHaveTextContent(
      'This browser is signed in as Oz now. Going on signs it in as Bee.'
    );
  });

  it('says so when the link does not work, and offers no way to sign in with it', async () => {
    open('#/reconnect/gfr_gone', fakeServer());
    expect(await screen.findByText('This link does not work')).toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /Continue with/ })).toBeNull();
    expect(screen.getByTestId('server-reconnect')).toHaveTextContent('Ask an owner of this server');
  });

  it('says why when the person comes back and it did not go through', async () => {
    open('#/sign-in?problem=link', fakeServer());
    expect(await screen.findByTestId('sign-in-problem')).toHaveTextContent(
      'That re-connect link does not work.'
    );
  });
});
