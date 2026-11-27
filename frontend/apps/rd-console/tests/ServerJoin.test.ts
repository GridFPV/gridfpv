/**
 * Getting into a club on a remote server (docs/primitives.md §13.7): joining a public
 * club, asking to join a private one, leaving, and what a director does about it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { joinStanding, personLabel } from '../src/lib/serverPeople.js';
import { eventView } from './competition.js';
import { ACE, NORTH, SOUTH, fakePeople, fakeServer, type FakeServer } from './serverPeopleFake.js';
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

const club = (base: PushClub, over: Partial<ServerClub> = {}): ServerClub => ({
  ...base,
  word: `${base.id}-fpv`,
  archived: false,
  open: false,
  ...over
});

function open(hash: string, server: FakeServer) {
  location.hash = hash;
  const people = fakePeople(server);
  const load = {
    clubs: vi.fn(async (): Promise<ServerClub[]> => server.clubList.map((c) => ({ ...c }))),
    events: vi.fn(async (): Promise<ServerEvent[]> => []),
    clubByWord: vi.fn(async (word: string): Promise<ServerClub> => {
      const found = server.clubList.find((c) => c.word === word);
      if (!found) throw new Error('This server has no such club.');
      return { ...found };
    })
  };
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server };
}

/** Pia: an account in no club, as someone who signed up with no invite is. */
const PIA: MyAccount = {
  ...ACE,
  id: 'acct_pia',
  name: 'Pia',
  clubs: [],
  profile: { callsign: 'PiaFPV' }
};

const joinCard = () => screen.findByTestId('join-clubs');
const rowOf = (card: HTMLElement, name: string) =>
  within(card)
    .getAllByTestId('join-club')
    .find((row) => row.textContent?.includes(name)) as HTMLElement;

describe('where an account stands with a club', () => {
  const account = (over: Partial<MyAccount> = {}) => ({ ...PIA, ...over });

  it('is worked out from the account and the club', () => {
    expect(joinStanding(null, club(NORTH))).toBe('signed-out');
    expect(joinStanding(undefined, club(NORTH))).toBe('signed-out');
    expect(joinStanding(account(), club(NORTH))).toBe('can-ask');
    expect(joinStanding(account(), club(NORTH, { open: true }))).toBe('can-join');
    expect(joinStanding(account({ join_asks: [NORTH] }), club(NORTH))).toBe('asked');
    expect(
      joinStanding(
        account({ clubs: [{ club: NORTH, role: 'Pilot' }] }),
        club(NORTH, { open: true })
      )
    ).toBe('member');
    // Being in one club says nothing of another.
    expect(joinStanding(account({ clubs: [{ club: SOUTH, role: 'Pilot' }] }), club(NORTH))).toBe(
      'can-ask'
    );
  });

  it('takes nobody into an archived club, whoever they are', () => {
    const archived = club(NORTH, { archived: true, open: true });
    expect(joinStanding(account(), archived)).toBe('closed');
    expect(joinStanding(null, archived)).toBe('closed');
    expect(joinStanding(account({ join_asks: [NORTH] }), archived)).toBe('closed');
  });

  it('treats an account from an older server, with no list of asks, as having asked nothing', () => {
    const { join_asks: _asks, ...old } = account();
    expect(joinStanding(old as MyAccount, club(NORTH))).toBe('can-ask');
  });

  it('names a person, with their callsign when it says more', () => {
    expect(personLabel({ name: 'Pia', callsign: 'PiaFPV' })).toBe('Pia (PiaFPV)');
    expect(personLabel({ name: 'Pia' })).toBe('Pia');
    expect(personLabel({ name: 'Pia', callsign: 'Pia' })).toBe('Pia');
    expect(personLabel({ name: 'Pia', callsign: '  ' })).toBe('Pia');
  });
});

describe('joining a club from the account page', () => {
  it('lists the clubs the account is not in, and says which are public', async () => {
    const server = fakeServer({ account: { ...PIA, clubs: [{ club: SOUTH, role: 'Pilot' }] } });
    server.clubList = [
      club(NORTH, { open: true }),
      club(SOUTH),
      club({ id: 'east', name: 'East FPV' }),
      club({ id: 'old', name: 'Old Club' }, { archived: true })
    ];
    open('#/account/clubs', server);
    const card = await joinCard();
    const rows = within(card).getAllByTestId('join-club');
    // Not the club she is in, and not the archived one.
    expect(rows.map((row) => row.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'North FPV Public Join',
      'East FPV Private Ask to join A director of the club says yes.'
    ]);
  });

  it('joins a public club with one press', async () => {
    const server = fakeServer({ account: PIA });
    server.clubList = [club(NORTH, { open: true }), club(SOUTH)];
    const { people } = open('#/account/clubs', server);
    const card = await joinCard();
    expect(screen.getByText(/You are not in a club yet/)).toBeTruthy();

    await fireEvent.click(within(card).getByRole('button', { name: 'Join North FPV' }));
    await waitFor(() => expect(people.joinClub).toHaveBeenCalledWith('north'));
    // She is in: the club is in her list, and no longer offered.
    await waitFor(() => expect(screen.getByTestId('my-club')).toHaveTextContent('North FPV'));
    expect(screen.getByTestId('my-club')).toHaveTextContent('Pilot');
    expect(within(await joinCard()).queryByText('North FPV')).toBeNull();
    expect(within(await joinCard()).getByText('South FPV')).toBeTruthy();
    expect(screen.queryByText(/You are not in a club yet/)).toBeNull();
  });

  it('asks a private club, says so, and takes the ask back', async () => {
    const server = fakeServer({ account: PIA });
    const { people } = open('#/account/clubs', server);
    const card = await joinCard();
    await fireEvent.click(within(card).getByRole('button', { name: 'Ask to join North FPV' }));
    await waitFor(() => expect(people.joinClub).toHaveBeenCalledWith('north'));
    const row = () => rowOf(card, 'North FPV');
    await waitFor(() =>
      expect(row()).toHaveTextContent('You asked to join. A director of the club will answer.')
    );
    // Asking put her in no club.
    expect(screen.queryByTestId('my-club')).toBeNull();
    expect(server.joinAsks.north).toEqual([
      { id: 'acct_pia', name: 'Pia', callsign: 'PiaFPV', asked_at: 1_700_000_000_000 }
    ]);
    // Nothing more to ask of that club. The other is still there to ask.
    expect(within(row()).queryByRole('button', { name: /Ask to join/ })).toBeNull();
    expect(within(card).getByRole('button', { name: 'Ask to join South FPV' })).toBeTruthy();

    await fireEvent.click(
      within(row()).getByRole('button', { name: 'Take back your ask to join North FPV' })
    );
    await waitFor(() => expect(people.leaveClub).toHaveBeenCalledWith('north'));
    await waitFor(() =>
      expect(within(row()).getByRole('button', { name: 'Ask to join North FPV' })).toBeTruthy()
    );
    expect(server.joinAsks.north).toEqual([]);
  });

  it('shows an ask that was made before, when the page is opened again', async () => {
    open('#/account/clubs', fakeServer({ account: { ...PIA, join_asks: [NORTH] } }));
    const card = await joinCard();
    expect(rowOf(card, 'North FPV')).toHaveTextContent('You asked to join.');
  });

  it("says the server's words when joining is refused", async () => {
    const server = fakeServer({ account: PIA });
    const { people } = open('#/account/clubs', server);
    people.joinClub.mockRejectedValueOnce(new Error('This club is archived.'));
    const card = await joinCard();
    await fireEvent.click(within(card).getByRole('button', { name: 'Ask to join North FPV' }));
    expect(await within(card).findByRole('alert')).toHaveTextContent('This club is archived.');
    expect(screen.queryByTestId('my-club')).toBeNull();
  });

  it('is not shown to someone in every club there is', async () => {
    const both: MyAccount = {
      ...ACE,
      clubs: [
        { club: NORTH, role: 'Director' },
        { club: SOUTH, role: 'Pilot' }
      ]
    };
    open('#/account/clubs', fakeServer({ account: both }));
    await screen.findByRole('list', { name: 'Your clubs' });
    expect(screen.queryByTestId('join-clubs')).toBeNull();
  });

  it('says there is no club to join when the server has none for someone in no club', async () => {
    const server = fakeServer({ account: PIA });
    server.clubList = [];
    open('#/account/clubs', server);
    expect(await joinCard()).toHaveTextContent('There is no club to join on this server yet.');
  });
});

describe('leaving a club', () => {
  const pilot: MyAccount = { ...ACE, clubs: [{ club: NORTH, role: 'Pilot' }] };

  it('is asked about first, and then the club is offered to join again', async () => {
    const server = fakeServer({ account: pilot });
    server.clubList = [club(NORTH, { open: true })];
    const { people } = open('#/account/clubs', server);
    await fireEvent.click(await screen.findByRole('button', { name: 'Leave North FPV' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('Leave North FPV?');
    expect(dialog).toHaveTextContent('Pilots you hold in this club are let go');
    expect(people.leaveClub).not.toHaveBeenCalled();

    // Cancel does nothing.
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.leaveClub).not.toHaveBeenCalled();
    expect(screen.getByTestId('my-club')).toBeTruthy();

    await fireEvent.click(screen.getByRole('button', { name: 'Leave North FPV' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Leave' })
    );
    await waitFor(() => expect(people.leaveClub).toHaveBeenCalledWith('north'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(screen.queryByTestId('my-club')).toBeNull();
    expect(within(await joinCard()).getByRole('button', { name: 'Join North FPV' })).toBeTruthy();
  });

  it('says why the only director cannot leave, and keeps them in', async () => {
    const server = fakeServer({ account: ACE });
    open('#/account/clubs', server);
    await fireEvent.click(await screen.findByRole('button', { name: 'Leave North FPV' }));
    const dialog = screen.getByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Leave' }));
    expect(await within(dialog).findByText('A club must keep at least one director.')).toBeTruthy();
    expect(screen.getByTestId('my-club')).toHaveTextContent('North FPV');
  });
});

describe("joining from a club's own address", () => {
  it('offers a visitor to sign in, and says the club by name', async () => {
    open('#/club/north-fpv', fakeServer());
    const control = await screen.findByTestId('club-join');
    const link = within(control).getByRole('link', { name: 'Sign in to join North FPV' });
    expect(link.getAttribute('href')).toBe('#/sign-in');
  });

  it('lets a signed-in person ask, right there', async () => {
    const { people } = open('#/club/north-fpv', fakeServer({ account: PIA }));
    const control = await screen.findByTestId('club-join');
    await fireEvent.click(within(control).getByRole('button', { name: 'Ask to join North FPV' }));
    await waitFor(() => expect(people.joinClub).toHaveBeenCalledWith('north'));
    await waitFor(() =>
      expect(screen.getByTestId('club-join')).toHaveTextContent('You asked to join.')
    );
  });

  it('says so to someone who is already in the club', async () => {
    open('#/club/north-fpv', fakeServer({ account: ACE }));
    expect(await screen.findByTestId('club-join')).toHaveTextContent('You are in this club.');
  });

  it('offers nothing at an archived club', async () => {
    const server = fakeServer({ account: PIA });
    server.clubList = [club(NORTH, { archived: true, open: true })];
    open('#/club/north-fpv', server);
    await screen.findByTestId('server-one-club');
    expect(screen.queryByTestId('club-join')).toBeNull();
  });
});

describe("who can join, on a club's own page", () => {
  const setting = () => screen.findByTestId('club-join-setting');
  const flip = (card: HTMLElement) =>
    within(card).getByRole('switch', {
      name: 'Let anyone with an account join'
    }) as HTMLInputElement;
  const asking = () => screen.findByTestId('club-join-asks');
  const adding = () => screen.findByTestId('club-add-someone');
  const PEOPLE = [
    { id: 'acct_pia', name: 'Pia', callsign: 'PiaFPV' },
    { id: 'acct_quin', name: 'Quin' },
    { id: 'acct_bee', name: 'Bee' }
  ];

  it('starts private, and a director makes it public and private again', async () => {
    const { people, server } = open('#/manage/north/settings', fakeServer({ account: ACE }));
    // The page is drawn again after each change, so the card is looked up each time.
    const state = async () => within(await setting()).getByTestId('club-open-state');
    expect(await state()).toHaveTextContent('Private');

    await fireEvent.click(flip(await setting()));
    await waitFor(() => expect(people.setClubOpen).toHaveBeenCalledWith('north', true));
    await waitFor(async () => expect(await state()).toHaveTextContent('Public'));
    expect(server.clubList[0].open).toBe(true);

    await fireEvent.click(flip(await setting()));
    await waitFor(() => expect(people.setClubOpen).toHaveBeenCalledWith('north', false));
    await waitFor(async () => expect(await state()).toHaveTextContent('Private'));
    expect(server.clubList[0].open).toBe(false);
  });

  it('opens on a club that is already public', async () => {
    const server = fakeServer({ account: ACE });
    server.clubList = [club(NORTH, { open: true }), club(SOUTH)];
    open('#/manage/north/settings', server);
    expect(within(await setting()).getByTestId('club-open-state')).toHaveTextContent('Public');
  });

  it('says nobody is waiting when nobody is', async () => {
    open('#/manage/north', fakeServer({ account: ACE }));
    expect(await within(await asking()).findByTestId('no-join-asks')).toHaveTextContent(
      'Nobody is waiting.'
    );
  });

  it('lets in someone who asked, and they show among the members', async () => {
    const server = fakeServer({ account: ACE });
    server.joinAsks = {
      north: [
        { id: 'acct_pia', name: 'Pia', callsign: 'PiaFPV', asked_at: 1 },
        { id: 'acct_quin', name: 'Quin', asked_at: 2 }
      ]
    };
    const { people } = open('#/manage/north', server);
    const card = await asking();
    const asks = await within(card).findAllByTestId('join-ask');
    // The first to ask is at the top. A callsign shows beside the name.
    expect(asks.map((row) => row.querySelector('.name')?.textContent)).toEqual([
      'Pia (PiaFPV)',
      'Quin'
    ]);

    await fireEvent.click(within(card).getByRole('button', { name: 'Let Pia (PiaFPV) in' }));
    await waitFor(() =>
      expect(people.answerJoinAsk).toHaveBeenCalledWith('north', 'acct_pia', true)
    );
    await waitFor(() => expect(within(card).getAllByTestId('join-ask')).toHaveLength(1));
    const members = screen.getByRole('list', { name: 'Members' });
    await waitFor(() => expect(members).toHaveTextContent('Pia'));

    // No: gone from the list, and not a member.
    await fireEvent.click(within(card).getByRole('button', { name: 'Say no to Quin' }));
    await waitFor(() =>
      expect(people.answerJoinAsk).toHaveBeenCalledWith('north', 'acct_quin', false)
    );
    await waitFor(() => expect(within(card).getByTestId('no-join-asks')).toBeTruthy());
    expect(members).not.toHaveTextContent('Quin');
  });

  it('reads the list again when an answer is refused, so a stale row does not stay', async () => {
    const server = fakeServer({ account: ACE });
    server.joinAsks = { north: [{ id: 'acct_pia', name: 'Pia', asked_at: 1 }] };
    open('#/manage/north', server);
    const card = await asking();
    await within(card).findByTestId('join-ask');
    // She took her ask back a moment ago.
    server.joinAsks.north = [];
    await fireEvent.click(within(card).getByRole('button', { name: 'Let Pia in' }));
    expect(await within(card).findByText(/has not asked to join the club/)).toBeTruthy();
    await waitFor(() => expect(within(card).getByTestId('no-join-asks')).toBeTruthy());
  });

  it('finds someone by name or callsign and adds them as a pilot', async () => {
    const server = fakeServer({ account: ACE, people: PEOPLE });
    const { people } = open('#/manage/north', server);
    const card = await adding();
    const find = within(card).getByRole('button', { name: 'Find' }) as HTMLButtonElement;
    const box = within(card).getByLabelText('Name or callsign') as HTMLInputElement;
    // One letter is too little to look with.
    await fireEvent.input(box, { target: { value: 'p' } });
    expect(find.disabled).toBe(true);
    expect(people.findPeople).not.toHaveBeenCalled();

    await fireEvent.input(box, { target: { value: ' fpv ' } });
    expect(find.disabled).toBe(false);
    await fireEvent.click(find);
    await waitFor(() => expect(people.findPeople).toHaveBeenCalledWith('north', 'fpv'));
    const rows = await within(card).findAllByTestId('found-person');
    expect(rows).toHaveLength(1);
    expect(rows[0]).toHaveTextContent('Pia (PiaFPV)');

    await fireEvent.click(
      within(card).getByRole('button', { name: 'Add Pia (PiaFPV) to North FPV' })
    );
    await waitFor(() => expect(people.addMember).toHaveBeenCalledWith('north', 'acct_pia'));
    // She is gone from what was found, and is among the members.
    await waitFor(() => expect(within(card).queryAllByTestId('found-person')).toHaveLength(0));
    await waitFor(() =>
      expect(screen.getByRole('list', { name: 'Members' })).toHaveTextContent('Pia')
    );
  });

  it('does not offer someone who is already in the club, and says when nobody matches', async () => {
    const server = fakeServer({ account: ACE, people: PEOPLE });
    open('#/manage/north', server);
    const card = await adding();
    const box = within(card).getByLabelText('Name or callsign');
    // Bee is a member of North already.
    await fireEvent.input(box, { target: { value: 'bee' } });
    await fireEvent.click(within(card).getByRole('button', { name: 'Find' }));
    expect(await within(card).findByTestId('nobody-found')).toHaveTextContent(
      'Nobody outside the club matches “bee”.'
    );
    // Typing more does not change what the last look said until Find is pressed again.
    await fireEvent.input(box, { target: { value: 'quin' } });
    expect(within(card).getByTestId('nobody-found')).toHaveTextContent('“bee”');
    await fireEvent.click(within(card).getByRole('button', { name: 'Find' }));
    expect(await within(card).findByTestId('found-person')).toHaveTextContent('Quin');
  });

  it("says the server's words when adding is refused", async () => {
    const server = fakeServer({ account: ACE, people: PEOPLE });
    const { people } = open('#/manage/north', server);
    const card = await adding();
    await fireEvent.input(within(card).getByLabelText('Name or callsign'), {
      target: { value: 'quin' }
    });
    await fireEvent.click(within(card).getByRole('button', { name: 'Find' }));
    await within(card).findByTestId('found-person');
    people.addMember.mockRejectedValueOnce(new Error('There is no such account.'));
    await fireEvent.click(within(card).getByRole('button', { name: 'Add Quin to North FPV' }));
    expect(await within(card).findByText('There is no such account.')).toBeTruthy();
    // Still offered: nothing was added.
    expect(within(card).getByTestId('found-person')).toHaveTextContent('Quin');
  });

  it('is not shown to a claims approver, who does not run the club', async () => {
    open(
      '#/manage/north',
      fakeServer({ account: { ...ACE, clubs: [{ club: NORTH, role: 'Approver' }] } })
    );
    await screen.findByTestId('server-club');
    expect(screen.queryByTestId('club-join-asks')).toBeNull();
    expect(screen.queryByTestId('club-add-someone')).toBeNull();
  });

  it('can change nothing in an archived club', async () => {
    const server = fakeServer({ account: { ...ACE, owner: true } });
    server.clubList = [club(NORTH, { archived: true })];
    server.joinAsks = { north: [{ id: 'acct_pia', name: 'Pia', asked_at: 1 }] };
    open('#/manage/north', server);
    const card = await asking();
    await within(card).findByTestId('join-ask');
    for (const button of within(card).getAllByRole('button')) {
      expect((button as HTMLButtonElement).disabled, button.textContent ?? '').toBe(true);
    }
  });
});
