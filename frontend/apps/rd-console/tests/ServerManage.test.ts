/**
 * Running a club from the server's page: its events (hide, show, remove), its link
 * tokens, and its name.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerClubManage from '../src/screens/ServerClubManage.svelte';
import ServerHome from '../src/screens/ServerHome.svelte';
import { tokenStanding } from '../src/lib/serverPeople.js';
import { eventView } from './competition.js';
import {
  ACE,
  NORTH,
  SOUTH,
  fakePeople,
  fakeServer,
  serverEvent,
  type FakeServer
} from './serverPeopleFake.js';
import { fakeLiveSession } from './support.js';

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

const BEE: MyAccount = {
  id: 'acct_bee',
  name: 'Bee',
  owner: false,
  sign_ins: [{ provider: 'discord', subject: '222', label: 'bee_fpv' }],
  clubs: [{ club: NORTH, role: 'Pilot' }],
  pilots: [],
  profile: {},
  join_asks: []
};
const APP: MyAccount = {
  ...BEE,
  id: 'acct_app',
  name: 'App',
  clubs: [{ club: NORTH, role: 'Approver' }]
};
const OZ: MyAccount = { ...BEE, id: 'acct_oz', name: 'Oz', owner: true, clubs: [] };

const TWO_EVENTS = (): Record<string, ServerEvent[]> => ({
  north: [
    serverEvent('summer', 'Summer Race', { created_at: 2 }),
    serverEvent('test-day', 'Test Day', { hidden: true })
  ]
});

/** The server's page, reading its lists from the same stand-in the account pages use. */
function open(hash: string, server: FakeServer) {
  location.hash = hash;
  const people = fakePeople(server);
  const load = {
    clubs: vi.fn(async (): Promise<PushClub[]> => {
      // The stand-in keeps a renamed club's name on the account.
      const mine = server.account?.clubs.find((c) => c.club.id === 'north')?.club;
      return [mine ?? NORTH, SOUTH];
    }),
    events: vi.fn((club: string) => people.clubEvents(club)),
    // By its address an event is read by anyone, hidden or not.
    event: vi.fn(async (club: string, id: string) =>
      server.events[club]?.find((event) => event.id === id)
    )
  };
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server, load };
}

/** Go to another page, as a link would. */
function go(hash: string) {
  location.hash = hash;
  window.dispatchEvent(new HashChangeEvent('hashchange'));
}

const events = () => screen.queryAllByTestId('manage-event');
const tokens = () => screen.queryAllByTestId('link-token');

describe("a club's events, for those who run it", () => {
  it('lists every event with a link to it, and marks the hidden ones', async () => {
    open('#/admin/club/north/events', fakeServer({ account: ACE, events: TWO_EVENTS() }));
    await waitFor(() => expect(events()).toHaveLength(2));
    const [summer, test] = events();
    expect(within(summer).getByRole('link', { name: 'Summer Race' }).getAttribute('href')).toBe(
      '#/event/north/summer'
    );
    expect(summer.textContent).not.toContain('Hidden');
    expect(within(summer).getByRole('button', { name: 'Hide Summer Race' })).toBeTruthy();
    expect(test.textContent).toContain('Hidden');
    expect(within(test).getByRole('button', { name: 'Show Test Day' })).toBeTruthy();
    // Never an id on the page.
    expect(screen.getByTestId('server-club').textContent).not.toContain('test-day');
  });

  it('says so when the club has no events yet', async () => {
    open('#/admin/club/north/events', fakeServer({ account: ACE }));
    expect(await screen.findByText(/No events yet/)).toBeTruthy();
    expect(events()).toHaveLength(0);
  });

  it('hides an event and shows it again, and reads the lists again each time', async () => {
    const { people, server, load } = open(
      '#/admin/club/north/events',
      fakeServer({ account: ACE, events: TWO_EVENTS() })
    );
    await waitFor(() => expect(events()).toHaveLength(2));
    const before = load.events.mock.calls.length;

    await fireEvent.click(screen.getByRole('button', { name: 'Hide Summer Race' }));
    await waitFor(() => expect(people.hideEvent).toHaveBeenCalledWith('north', 'summer', true));
    await waitFor(() =>
      expect(screen.getByRole('button', { name: 'Show Summer Race' })).toBeTruthy()
    );
    expect(server.events.north[0].hidden).toBe(true);
    await waitFor(() => expect(load.events.mock.calls.length).toBeGreaterThan(before));

    await fireEvent.click(screen.getByRole('button', { name: 'Show Test Day' }));
    await waitFor(() => expect(people.hideEvent).toHaveBeenCalledWith('north', 'test-day', false));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Hide Test Day' })).toBeTruthy());
    // Nothing was removed by hiding.
    expect(events()).toHaveLength(2);
    expect(people.removeEvent).not.toHaveBeenCalled();
  });

  it('removes an event only after asking, in a dialog of the page', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const { people, server } = open(
      '#/admin/club/north/events',
      fakeServer({ account: ACE, events: TWO_EVENTS() })
    );
    await waitFor(() => expect(events()).toHaveLength(2));
    await fireEvent.click(screen.getByRole('button', { name: 'Remove Summer Race' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Remove Summer Race from this server?');
    // It says what happens to the director's own copy.
    expect(dialog.textContent).toContain('own copy in Grid is not touched');
    expect(people.removeEvent).not.toHaveBeenCalled();

    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(events()).toHaveLength(2);
    expect(people.removeEvent).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: 'Remove Summer Race' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Remove' })
    );
    await waitFor(() => expect(people.removeEvent).toHaveBeenCalledWith('north', 'summer'));
    await waitFor(() => expect(events()).toHaveLength(1));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(server.removed).toEqual(['north/summer']);
    expect(events()[0].textContent).toContain('Test Day');
    expect(confirm).not.toHaveBeenCalled();
  });

  it("shows the server's words when a change is refused, and what is really there", async () => {
    const { people, server } = open(
      '#/admin/club/north/events',
      fakeServer({ account: ACE, events: TWO_EVENTS() })
    );
    await waitFor(() => expect(events()).toHaveLength(2));
    // Someone else removed it a moment ago.
    server.events.north = server.events.north.filter((e) => e.id !== 'summer');
    await fireEvent.click(screen.getByRole('button', { name: 'Hide Summer Race' }));
    await waitFor(() =>
      expect(screen.getByTestId('manage-error').textContent).toContain(
        'This server holds no such event.'
      )
    );
    await waitFor(() => expect(events()).toHaveLength(1));
    expect(people.hideEvent).toHaveBeenCalledTimes(1);
  });

  it('says so when the tokens and events cannot be read', async () => {
    const server = fakeServer({ account: ACE, events: TWO_EVENTS() });
    location.hash = '#/admin/club/north/events';
    const people = fakePeople(server);
    people.linkTokens.mockRejectedValue(new Error('The server did not answer.'));
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: { clubs: async () => [NORTH, SOUTH], events: async () => [] },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    expect(
      await screen.findByText("This club's tokens and events could not be read.")
    ).toBeTruthy();
    expect(screen.getByTestId('server-club').textContent).toContain('The server did not answer.');
  });
});

describe('who sees a hidden event in the list of events', () => {
  it('a director of the club does, marked as hidden', async () => {
    open('#/', fakeServer({ account: ACE, events: TWO_EVENTS() }));
    const links = await screen.findAllByTestId('server-event-link');
    expect(links.map((l) => l.textContent?.trim())).toEqual([
      expect.stringContaining('Summer Race'),
      expect.stringContaining('Test Day')
    ]);
    expect(links[0].textContent).not.toContain('Hidden');
    expect(links[1].textContent).toContain('Hidden');
  });

  it('an owner does', async () => {
    open('#/', fakeServer({ account: OZ, events: TWO_EVENTS() }));
    await waitFor(() => expect(screen.getAllByTestId('server-event-link')).toHaveLength(2));
  });

  it.each([
    ['a pilot of the club', BEE],
    ['an approver', APP],
    ['someone not signed in', undefined]
  ])('%s does not', async (_who, account) => {
    open('#/', fakeServer({ account, events: TWO_EVENTS() }));
    const links = await screen.findAllByTestId('server-event-link');
    expect(links).toHaveLength(1);
    expect(links[0].textContent).toContain('Summer Race');
    expect(document.body.textContent).not.toContain('Test Day');
  });
});

describe('opening an event by its address', () => {
  it('opens a hidden event for someone who is not signed in', async () => {
    const { load } = open('#/event/north/test-day', fakeServer({ events: TWO_EVENTS() }));
    expect(await screen.findByTestId('server-event')).toHaveTextContent('Test Day');
    expect(load.event).toHaveBeenCalledWith('north', 'test-day');
    expect(screen.queryByText('This event is not here')).toBeNull();
  });

  it('does not ask for an event the list already has', async () => {
    const { load } = open('#/event/north/summer', fakeServer({ events: TWO_EVENTS() }));
    expect(await screen.findByTestId('server-event')).toHaveTextContent('Summer Race');
    expect(load.event).not.toHaveBeenCalled();
  });

  it('says an event is not here only once the server has said so', async () => {
    const { load } = open('#/event/north/long-gone', fakeServer({ events: TWO_EVENTS() }));
    expect(await screen.findByText('This event is not here')).toBeTruthy();
    expect(load.event).toHaveBeenCalledWith('north', 'long-gone');
    expect(screen.queryByTestId('server-event')).toBeNull();
  });

  it('does not call an event gone when the server could not be asked', async () => {
    const server = fakeServer({ events: TWO_EVENTS() });
    location.hash = '#/event/north/test-day';
    const people = fakePeople(server);
    const event = vi.fn(async (): Promise<ServerEvent | undefined> => {
      throw new Error('offline');
    });
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: { clubs: async () => [NORTH, SOUTH], events: async () => [], event },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    await waitFor(() => expect(event).toHaveBeenCalled());
    await new Promise((done) => setTimeout(done, 20));
    expect(screen.queryByText('This event is not here')).toBeNull();
  });

  it('says so when an event is removed while it is open', async () => {
    const { server } = open('#/event/north/test-day', fakeServer({ events: TWO_EVENTS() }));
    expect(await screen.findByTestId('server-event')).toHaveTextContent('Test Day');
    server.events.north = [];
    // The list is read again every so often, and the event is asked for again with it.
    await vi.advanceTimersByTimeAsync(16_000);
    expect(await screen.findByText('This event is not here')).toBeTruthy();
    expect(screen.queryByTestId('server-event')).toBeNull();
  });
});

describe("a club's link tokens", () => {
  it('lists them with what each is for and whether it works, and never a token', async () => {
    const server = fakeServer({ account: ACE });
    server.tokens.north.push({
      id: 'tok0',
      label: 'Old laptop',
      created_at: 1_600_000_000_000,
      revoked_at: 1_600_000_500_000
    });
    open('#/admin/club/north/tokens', server);
    await waitFor(() => expect(tokens()).toHaveLength(2));
    const [works, spent] = tokens();
    expect(works.textContent).toContain('Club laptop');
    expect(works.textContent).toContain('Works');
    expect(within(works).getByRole('button', { name: 'Take back the token for Club laptop' }));
    expect(spent.textContent).toContain('Old laptop');
    expect(spent.textContent).toContain('Taken back');
    expect(within(spent).queryByRole('button')).toBeNull();
    expect(document.body.textContent).not.toContain('gfs_');
    expect(screen.queryByTestId('token-made')).toBeNull();
  });

  it('makes one and shows the token once', async () => {
    const { people, server } = open('#/admin/club/north/tokens', fakeServer({ account: ACE }));
    await waitFor(() => expect(tokens()).toHaveLength(1));
    const make = screen.getByRole('button', { name: 'Make a link token' }) as HTMLButtonElement;
    expect(make.disabled).toBe(true);
    await fireEvent.input(screen.getByLabelText('What it is for'), { target: { value: '   ' } });
    expect(make.disabled).toBe(true);

    await fireEvent.input(screen.getByLabelText('What it is for'), {
      target: { value: " Dee's laptop " }
    });
    await fireEvent.click(make);
    await waitFor(() => expect(people.makeLinkToken).toHaveBeenCalledWith('north', "Dee's laptop"));
    const box = await screen.findByTestId('token-made');
    const secret = server.secrets['tok-new-1'];
    expect(screen.getByTestId('token-secret').textContent).toBe(secret);
    expect(box.textContent).toContain("Dee's laptop");
    expect(box.textContent).toContain('shown this once');
    await waitFor(() => expect(tokens()).toHaveLength(2));
    expect(tokens()[0].textContent).toContain("Dee's laptop");
    // The box is emptied for the next one.
    expect((screen.getByLabelText('What it is for') as HTMLInputElement).value).toBe('');

    await fireEvent.click(within(box).getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(screen.queryByTestId('token-made')).toBeNull());
    expect(document.body.textContent).not.toContain(secret);
    expect(tokens()).toHaveLength(2);
  });

  it('copies the token', async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    const { server } = open('#/admin/club/north/tokens', fakeServer({ account: ACE }));
    await waitFor(() => expect(tokens()).toHaveLength(1));
    await fireEvent.input(screen.getByLabelText('What it is for'), { target: { value: 'Pi' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Make a link token' }));
    const box = await screen.findByTestId('token-made');
    await fireEvent.click(within(box).getByRole('button', { name: 'Copy' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(server.secrets['tok-new-1']));
  });

  it('takes one back only after asking, and then it has no button', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const { people } = open('#/admin/club/north/tokens', fakeServer({ account: ACE }));
    await waitFor(() => expect(tokens()).toHaveLength(1));
    const name = 'Take back the token for Club laptop';
    await fireEvent.click(screen.getByRole('button', { name }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Take back the token for Club laptop?');
    expect(dialog.textContent).toContain('stops working straight away');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.revokeLinkToken).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Take back' })
    );
    await waitFor(() => expect(people.revokeLinkToken).toHaveBeenCalledWith('north', 'tok1'));
    await waitFor(() => expect(tokens()[0].textContent).toContain('Taken back'));
    expect(screen.queryByRole('button', { name })).toBeNull();
    expect(confirm).not.toHaveBeenCalled();
  });

  it('stops showing a token that was just made when it is taken back', async () => {
    open('#/admin/club/north/tokens', fakeServer({ account: ACE }));
    await waitFor(() => expect(tokens()).toHaveLength(1));
    await fireEvent.input(screen.getByLabelText('What it is for'), { target: { value: 'Oops' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Make a link token' }));
    await screen.findByTestId('token-made');
    await fireEvent.click(screen.getByRole('button', { name: 'Take back the token for Oops' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Take back' })
    );
    await waitFor(() => expect(screen.queryByTestId('token-made')).toBeNull());
    expect(document.body.textContent).not.toContain('gfs_');
  });

  it('says how a token stands', () => {
    expect(tokenStanding({})).toBe('Works');
    expect(tokenStanding({ revoked_at: 5 })).toBe('Taken back');
  });
});

describe("a club's name", () => {
  it('saves a new name, trimmed, and the page shows it', async () => {
    const { people } = open('#/admin/club/north/settings', fakeServer({ account: ACE }));
    const box = (await screen.findByLabelText('Name')) as HTMLInputElement;
    await waitFor(() => expect(box.value).toBe('North FPV'));
    const form = screen.getByRole('form', { name: 'Club name' });
    const save = within(form).getByRole('button', { name: 'Save' }) as HTMLButtonElement;
    // Nothing to save until it differs, and never an empty name.
    expect(save.disabled).toBe(true);
    await fireEvent.input(box, { target: { value: '  ' } });
    expect(save.disabled).toBe(true);
    await fireEvent.input(box, { target: { value: ' North FPV ' } });
    expect(save.disabled).toBe(true);

    await fireEvent.input(box, { target: { value: ' Northern Lights ' } });
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);
    await waitFor(() => expect(people.renameClub).toHaveBeenCalledWith('north', 'Northern Lights'));
    await waitFor(() => expect(screen.getByText('Northern Lights')).toBeVisible());
    await waitFor(() => expect(box.value).toBe('Northern Lights'));
    expect(save.disabled).toBe(true);
  });

  it('keeps what is being typed when the page reads its lists again', async () => {
    const people = fakePeople(fakeServer({ account: ACE }));
    const view = render(ServerClubManage, {
      api: people,
      club: { id: 'north', name: 'North FPV' },
      section: 'settings'
    });
    const box = (await screen.findByLabelText('Name')) as HTMLInputElement;
    await fireEvent.input(box, { target: { value: 'Northern Li' } });
    // The same club, handed over again as a list that was read again has it.
    await view.rerender({ club: { id: 'north', name: 'North FPV' } });
    expect(box.value).toBe('Northern Li');
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(
      false
    );
    // A new name from the server does put the box right.
    await view.rerender({ club: { id: 'north', name: 'Northern Lights' } });
    await waitFor(() => expect(box.value).toBe('Northern Lights'));
  });

  it('follows the club to its new address when its name changes', async () => {
    const server = fakeServer({ account: ACE });
    const people = fakePeople(server);
    // The list gives each club its friendly address, which follows the name.
    let north = { ...NORTH, word: 'north-fpv' };
    location.hash = '#/admin/club/north-fpv/settings';
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: {
        clubs: vi.fn(async (): Promise<PushClub[]> => [north, SOUTH]),
        events: vi.fn(async () => [])
      },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    const box = (await screen.findByLabelText('Name')) as HTMLInputElement;
    people.renameClub.mockImplementationOnce(async () => {
      north = { ...north, name: 'Northern Lights', word: 'northern-lights' };
    });
    await fireEvent.input(box, { target: { value: 'Northern Lights' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(location.hash).toBe('#/admin/club/northern-lights/settings'));
    // Still the club's settings, not a page that says the club is gone.
    expect(await screen.findByLabelText('Name')).toBeTruthy();
    expect(screen.queryByText('This club is not here')).toBeNull();
  });

  it("shows the server's words when the name is refused, and keeps what was typed", async () => {
    const { people } = open('#/admin/club/north/settings', fakeServer({ account: ACE }));
    const box = (await screen.findByLabelText('Name')) as HTMLInputElement;
    await waitFor(() => expect(box.value).toBe('North FPV'));
    people.renameClub.mockRejectedValueOnce(
      new Error("A club's name is 80 characters at most, on one line.")
    );
    await fireEvent.input(box, { target: { value: 'Too long' } });
    await fireEvent.click(
      within(screen.getByRole('form', { name: 'Club name' })).getByRole('button', { name: 'Save' })
    );
    await waitFor(() =>
      expect(screen.getByTestId('manage-error').textContent).toContain('80 characters at most')
    );
    expect(box.value).toBe('Too long');
    expect(screen.getByText('North FPV')).toBeVisible();
  });
});

describe('who may run a club', () => {
  it('an owner runs a club they are not a member of', async () => {
    open('#/admin/club/north/events', fakeServer({ account: OZ, events: TWO_EVENTS() }));
    await waitFor(() => expect(events()).toHaveLength(2));
    go('#/admin/club/north/tokens');
    await waitFor(() => expect(tokens()).toHaveLength(1));
    go('#/admin/club/north/settings');
    expect(await screen.findByLabelText('Name')).toBeTruthy();
  });

  it('a claims approver is shown none of it, and none of it is asked for', async () => {
    const { people } = open(
      '#/admin/club/north',
      fakeServer({ account: APP, events: TWO_EVENTS() })
    );
    await screen.findByTestId('server-club');
    await waitFor(() => expect(people.clubPilots).toHaveBeenCalled());
    expect(screen.queryByText('Link tokens')).toBeNull();
    expect(screen.queryByText('Club name')).toBeNull();
    expect(events()).toHaveLength(0);
    expect(people.linkTokens).not.toHaveBeenCalled();
  });

  it('a pilot cannot open the page at all', async () => {
    const { people } = open(
      '#/admin/club/north',
      fakeServer({ account: BEE, events: TWO_EVENTS() })
    );
    expect(await screen.findByText(/Only those who look after this club/)).toBeTruthy();
    expect(people.linkTokens).not.toHaveBeenCalled();
  });
});
