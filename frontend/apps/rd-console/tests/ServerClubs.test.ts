/**
 * More than one club on a server (docs/primitives.md §13.2): the owner's list of clubs, a
 * club's friendly address, an archived club, and what people are told about owners.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, ServerClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { formatServerHash, parseServerHash } from '../src/lib/serverHome.js';
import { OWNERS_SEE_ALL, clubLink } from '../src/lib/serverPeople.js';
import { eventView } from './competition.js';
import { ACE, fakePeople, fakeServer, serverEvent, type FakeServer } from './serverPeopleFake.js';
import { fakeLiveSession } from './support.js';

const OZ: MyAccount = {
  id: 'acct_oz',
  name: 'Oz',
  owner: true,
  sign_ins: [{ provider: 'discord', subject: '900', label: 'oz' }],
  clubs: [],
  pilots: [],
  profile: {},
  join_asks: []
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

/** The page, reading its clubs and events from the stand-in server as the real one would. */
function open(hash: string, server: FakeServer) {
  location.hash = hash;
  const people = fakePeople(server);
  const load = {
    clubs: vi.fn(() => people.serverClubs()),
    clubByWord: vi.fn(
      async (word: string): Promise<ServerClub | undefined> =>
        server.clubList.find((club) => club.word === word.toLowerCase())
    ),
    events: vi.fn(async (club: string): Promise<ServerEvent[]> => server.events[club] ?? [])
  };
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server, load };
}

const owned = () => screen.getAllByTestId('owned-club');
const names = () => owned().map((row) => within(row).getByRole('link').textContent);

describe('a club address', () => {
  it('goes there and back, and is safe whatever the word', () => {
    expect(clubLink('https://grid.example.org/', 'recon-fpv')).toBe(
      'https://grid.example.org/#/club/recon-fpv'
    );
    for (const word of ['recon-fpv', 'ålesund-drone', 'a b/c']) {
      const hash = formatServerHash({ kind: 'club', word });
      expect(parseServerHash(hash)).toEqual({ kind: 'club', word });
      expect(parseServerHash(new URL(clubLink('https://x.example', word)).hash)).toEqual({
        kind: 'club',
        word
      });
    }
    expect(parseServerHash('#/c')).toEqual({ kind: 'list' });
    expect(parseServerHash('#/club/a/b')).toEqual({ kind: 'list' });
  });
});

describe("the owner's list of clubs", () => {
  it('is on the Server part, for an owner, with each club and its address', async () => {
    open('#/admin', fakeServer({ account: OZ }));
    const card = await screen.findByTestId('server-clubs');
    await within(card).findAllByTestId('owned-club');
    expect(names()).toEqual(['North FPV', 'South FPV']);
    expect(within(owned()[0]).getByTestId('club-address')).toHaveTextContent(
      `${location.origin}/#/club/north-fpv`
    );
    expect(within(owned()[0]).getByRole('link').getAttribute('href')).toBe('#/manage/north-fpv');
    // Never the club's id as words on the page.
    expect(card.textContent).not.toMatch(/\bnorth\b(?!-)/);
  });

  it('is not shown to anyone who is not an owner', async () => {
    const { people } = open('#/admin', fakeServer({ account: ACE }));
    await screen.findByTestId('server-who');
    expect(screen.queryByTestId('server-clubs')).toBeNull();
    expect(screen.queryByRole('link', { name: 'Server' })).toBeNull();
    expect(people.makeClub).not.toHaveBeenCalled();
  });

  it('makes a club, which then shows in the list with its own address', async () => {
    const { people, server } = open('#/admin', fakeServer({ account: OZ }));
    const card = await screen.findByTestId('server-clubs');
    await within(card).findAllByTestId('owned-club');
    const make = within(card).getByRole('button', { name: 'Make the club' });
    expect(make).toBeDisabled();
    await fireEvent.input(within(card).getByLabelText('New club'), {
      target: { value: '  West FPV ' }
    });
    await fireEvent.click(make);
    await waitFor(() => expect(names()).toEqual(['North FPV', 'South FPV', 'West FPV']));
    expect(people.makeClub).toHaveBeenCalledWith('West FPV');
    expect(server.clubList).toHaveLength(3);
    expect(within(owned()[2]).getByTestId('club-address')).toHaveTextContent('/#/club/west-fpv');
    expect(within(card).getByLabelText('New club')).toHaveValue('');
  });

  it("shows the server's words when a club cannot be made, and keeps what was typed", async () => {
    const { people } = open('#/admin', fakeServer({ account: OZ }));
    const card = await screen.findByTestId('server-clubs');
    await within(card).findAllByTestId('owned-club');
    people.makeClub.mockRejectedValueOnce(new Error("A club's name is 80 characters at most."));
    await fireEvent.input(within(card).getByLabelText('New club'), { target: { value: 'West' } });
    await fireEvent.click(within(card).getByRole('button', { name: 'Make the club' }));
    expect(await within(card).findByTestId('clubs-error')).toHaveTextContent(
      "A club's name is 80 characters at most."
    );
    expect(within(card).getByLabelText('New club')).toHaveValue('West');
    expect(names()).toEqual(['North FPV', 'South FPV']);
  });

  it('asks before archiving, says what it does, and does nothing on Cancel', async () => {
    const { people } = open('#/admin', fakeServer({ account: OZ }));
    await screen.findAllByTestId('owned-club');
    await fireEvent.click(screen.getByRole('button', { name: 'Archive North FPV' }));
    const sure = await screen.findByRole('dialog');
    expect(within(sure).getByRole('heading', { name: 'Archive North FPV?' })).toBeInTheDocument();
    expect(sure).toHaveTextContent('nothing in it can be changed');
    expect(sure).toHaveTextContent('Its events still open by their links.');
    expect(sure).toHaveTextContent('You can bring it back at any time.');
    await fireEvent.click(within(sure).getByRole('button', { name: 'Cancel' }));
    expect(people.archiveClub).not.toHaveBeenCalled();
    expect(screen.queryByText('Archived')).toBeNull();
  });

  it('archives a club, marks it, and brings it back', async () => {
    const { people, server } = open('#/admin', fakeServer({ account: OZ }));
    await screen.findAllByTestId('owned-club');
    await fireEvent.click(screen.getByRole('button', { name: 'Archive North FPV' }));
    await fireEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Archive' })
    );
    await waitFor(() => expect(owned()[0]).toHaveTextContent('Archived'));
    expect(people.archiveClub).toHaveBeenCalledWith('north', true);
    expect(server.clubList[0].archived).toBe(true);
    expect(screen.queryByRole('button', { name: 'Archive North FPV' })).toBeNull();
    expect(owned()[1]).not.toHaveTextContent('Archived');

    await fireEvent.click(screen.getByRole('button', { name: 'Bring back North FPV' }));
    await waitFor(() => expect(owned()[0]).not.toHaveTextContent('Archived'));
    expect(people.archiveClub).toHaveBeenLastCalledWith('north', false);
    expect(screen.getByRole('button', { name: 'Archive North FPV' })).toBeInTheDocument();
  });
});

describe('an archived club', () => {
  const archived = (account: MyAccount | undefined) => {
    const server = fakeServer({ account });
    server.clubList[0].archived = true;
    server.events = {
      north: [serverEvent('friday', 'Friday Night')],
      south: [serverEvent('sunday', 'Sunday Race')]
    };
    return server;
  };

  it('is off the front page for a visitor', async () => {
    open('#/', archived(undefined));
    await screen.findByText('Sunday Race');
    expect(screen.queryByText('North FPV')).toBeNull();
    expect(screen.queryByText('Friday Night')).toBeNull();
  });

  it('is on the front page for an owner, marked', async () => {
    open('#/', archived(OZ));
    const section = await screen.findByRole('region', { name: 'North FPV' });
    expect(section).toHaveTextContent('Archived');
    expect(section).toHaveTextContent('Friday Night');
    expect(screen.getByRole('region', { name: 'South FPV' })).not.toHaveTextContent('Archived');
  });

  it('still opens by its friendly address, marked, with its events', async () => {
    open('#/club/north-fpv', archived(undefined));
    const club = await screen.findByTestId('server-one-club');
    expect(screen.getByRole('heading', { name: 'North FPV' })).toBeVisible();
    expect(screen.getByText('Archived')).toBeVisible();
    const link = within(club).getByTestId('server-event-link');
    expect(link).toHaveTextContent('Friday Night');
    expect(link.getAttribute('href')).toBe('#/event/north/friday');
  });

  it('says so on its own page, for the owner who can bring it back', async () => {
    open('#/manage/north', archived(OZ));
    expect(await screen.findByTestId('club-archived')).toHaveTextContent(
      'Nothing in it can be changed'
    );
  });
});

describe("a club's friendly address", () => {
  it('opens that club alone, with a link to each of its events by the id that never changes', async () => {
    const server = fakeServer();
    server.events = {
      north: [serverEvent('friday', 'Friday Night'), serverEvent('sat', 'Saturday')],
      south: [serverEvent('sunday', 'Sunday Race')]
    };
    const { load } = open('#/club/South-FPV', server);
    const club = await screen.findByTestId('server-one-club');
    expect(load.clubByWord).toHaveBeenCalledWith('South-FPV');
    expect(club).toHaveTextContent('South FPV');
    expect(club).not.toHaveTextContent('Archived');
    const links = within(club).getAllByTestId('server-event-link');
    expect(links.map((link) => link.getAttribute('href'))).toEqual(['#/event/south/sunday']);
    expect(screen.queryByText('Friday Night')).toBeNull();
  });

  it('says so when the club has no events yet', async () => {
    open('#/club/north-fpv', fakeServer());
    expect(await screen.findByTestId('server-one-club')).toHaveTextContent('No events yet.');
  });

  it('says so when no club has that address', async () => {
    open('#/club/nowhere', fakeServer());
    expect(await screen.findByText('This club is not here')).toBeInTheDocument();
    expect(screen.queryByTestId('server-one-club')).toBeNull();
  });

  it("is shown on the club's own page to those who run it, to hand out", async () => {
    open('#/manage/north/settings', fakeServer({ account: ACE }));
    expect(await screen.findByTestId('club-own-address')).toHaveTextContent(
      `${location.origin}/#/club/north-fpv`
    );
  });
});

describe('what people are told about owners', () => {
  it('is said where someone signs in', async () => {
    open('#/sign-in', fakeServer());
    expect(await screen.findByTestId('owners-see-all')).toHaveTextContent(OWNERS_SEE_ALL);
    expect(OWNERS_SEE_ALL).toBe('The owners of this server can see and change everything on it.');
  });

  it('is said where someone accepts an invite', async () => {
    const server = fakeServer();
    server.links.gfi_x = {
      id: 'inv',
      kind: 'Personal',
      club: { id: 'north', name: 'North FPV' },
      role: 'Pilot',
      label: 'Ace',
      max_uses: 1,
      uses: 0,
      revoked: false,
      works: true
    };
    open('#/invite/gfi_x', server);
    expect(await screen.findByTestId('owners-see-all')).toHaveTextContent(OWNERS_SEE_ALL);
  });
});

describe('the primary club', () => {
  /** The row of pages at the top. */
  const pages = () => within(screen.getByRole('navigation', { name: 'Pages' }));
  const BOTH: MyAccount = {
    ...ACE,
    clubs: [
      { club: { id: 'north', name: 'North FPV' }, role: 'Director' },
      { club: { id: 'south', name: 'South FPV' }, role: 'Pilot' }
    ],
    primary: 'north'
  };
  const withEvents = (account: MyAccount | undefined) => {
    const server = fakeServer({ account });
    server.events = {
      north: [serverEvent('friday', 'Friday Night')],
      south: [serverEvent('sunday', 'Sunday Race')]
    };
    return server;
  };

  it('is marked on the account page, and another club can be made primary', async () => {
    const { people, server } = open('#/account/clubs', fakeServer({ account: BOTH }));
    await screen.findByTestId('server-account');
    const rows = () => screen.getAllByTestId('my-club');
    expect(rows()[0]).toHaveTextContent('North FPV');
    expect(rows()[0]).toHaveTextContent('Primary');
    // The primary club has nothing to press about being primary.
    expect(within(rows()[0]).queryByRole('button', { name: /primary/ })).toBeNull();
    expect(rows()[1]).not.toHaveTextContent('Primary');
    expect(screen.getByLabelText('About the primary club')).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Make South FPV your primary club' }));
    await waitFor(() => expect(rows()[1]).toHaveTextContent('Primary'));
    expect(people.setPrimary).toHaveBeenCalledWith('south');
    expect(server.account?.primary).toBe('south');
    expect(rows()[0]).not.toHaveTextContent('Primary');
    expect(
      screen.getByRole('button', { name: 'Make North FPV your primary club' })
    ).toBeInTheDocument();
  });

  it('is not offered to someone in one club: there is nothing to pick', async () => {
    open('#/account/clubs', fakeServer({ account: { ...ACE, primary: 'north' } }));
    await screen.findByTestId('server-account');
    expect(screen.getByTestId('my-club')).not.toHaveTextContent('Primary');
    expect(screen.queryByRole('button', { name: /primary club/ })).toBeNull();
    expect(screen.queryByLabelText('About the primary club')).toBeNull();
  });

  it("shows the server's words when it refuses", async () => {
    const { people } = open('#/account/clubs', fakeServer({ account: BOTH }));
    await screen.findByTestId('server-account');
    people.setPrimary.mockRejectedValueOnce(new Error('Pick a club you are in.'));
    await fireEvent.click(screen.getByRole('button', { name: 'Make South FPV your primary club' }));
    expect(await screen.findByText('Pick a club you are in.')).toBeInTheDocument();
    expect(screen.getAllByTestId('my-club')[0]).toHaveTextContent('Primary');
  });

  it('is what the front page opens on, with a way to every club', async () => {
    open('#/', withEvents(BOTH));
    await screen.findByText('Friday Night');
    expect(screen.queryByText('Sunday Race')).toBeNull();
    expect(screen.queryByRole('region', { name: 'South FPV' })).toBeNull();
    // The page is the club's own, and every club is one press away.
    expect(screen.getByRole('heading', { name: 'North FPV', level: 1 })).toBeVisible();
    expect(pages().getByRole('link', { name: 'Clubs' }).getAttribute('href')).toBe('#/clubs');
  });

  it('gives way to every club at its own address, with a way back', async () => {
    open('#/clubs', withEvents(BOTH));
    await screen.findByText('Friday Night');
    expect(screen.getByText('Sunday Race')).toBeInTheDocument();
    expect(pages().getByRole('link', { name: 'Events' }).getAttribute('href')).toBe('#/');
  });

  it('says so when the primary club has no events, and does not show the others', async () => {
    const server = withEvents({ ...BOTH, primary: 'north' });
    server.events.north = [];
    open('#/', server);
    expect(await screen.findByTestId('no-events')).toHaveTextContent('No events yet.');
    expect(screen.getByText('North FPV')).toBeInTheDocument();
    expect(screen.queryByText('Sunday Race')).toBeNull();
  });

  it('shows a visitor every club, and offers them no primary club', async () => {
    open('#/', withEvents(undefined));
    await screen.findByText('Friday Night');
    expect(screen.getByText('Sunday Race')).toBeInTheDocument();
    expect(screen.queryByTestId('all-clubs')).toBeNull();
  });

  it('shows every club when the primary one is not on the list any more', async () => {
    const server = withEvents({ ...BOTH, primary: 'gone' });
    open('#/', server);
    await screen.findByText('Friday Night');
    expect(screen.getByText('Sunday Race')).toBeInTheDocument();
    expect(screen.queryByTestId('all-clubs')).toBeNull();
  });

  it('reads the address of every club there and back', () => {
    expect(parseServerHash('#/clubs')).toEqual({ kind: 'clubs' });
    expect(formatServerHash({ kind: 'clubs' })).toBe('#/clubs');
  });
});
