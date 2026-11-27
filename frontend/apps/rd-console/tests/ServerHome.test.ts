/**
 * The page a remote server shows: its events, and one of them when it is picked.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/svelte';
import type { PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { eventView, heat, race, raceSeat } from './competition.js';
import { ACE, fakePeople, fakeServer } from './serverPeopleFake.js';
import { GATE, SIGNUP_PILOTS, signupEvent, signupSheet } from './signupFixtures.js';
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

const CLUB: PushClub = { id: 'club', name: 'Recon FPV' };
const FRIDAY: ServerEvent = {
  id: 'friday-night-764c5x',
  name: 'Friday Night Whoops',
  date: '2026-11-27',
  location: 'Main field',
  created_at: 300,
  hidden: false
};
const SUNDAY: ServerEvent = {
  id: 'sunday-cup-11aa22',
  name: 'Sunday Cup',
  created_at: 200,
  hidden: false
};

function racing() {
  const on = race('h1', 1, 'Running', {
    started_at: 100_000_000,
    seats: [raceSeat(0, 'p-ace', { finish: 1 })]
  });
  return eventView({
    now: 105_000_000,
    phases: [heat('h1', 'Qualifying Heat 1', ['p-ace'])],
    races: [on],
    current_race: on.race
  });
}

function open(
  lists: Record<string, ServerEvent[]> = { club: [FRIDAY, SUNDAY] },
  clubs: PushClub[] = [CLUB]
) {
  const stop = vi.fn();
  const load = {
    clubs: vi.fn(async (): Promise<PushClub[]> => clubs),
    events: vi.fn(async (club: string): Promise<ServerEvent[]> => lists[club] ?? [])
  };
  const sourceFor = vi.fn((_club: string, _event: string) =>
    fakeLiveSession(racing(), {
      listPilots: vi.fn(async () => [{ id: 'p-ace', callsign: 'Ace', vtx_types: [] }]),
      subscribeChanges: vi.fn(() => stop)
    })
  );
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    sourceFor,
    people: fakePeople(fakeServer({ providers: [] }))
  });
  return { load, sourceFor, stop };
}

const go = (hash: string) => {
  location.hash = hash;
  window.dispatchEvent(new Event('hashchange'));
};
const links = () => screen.getAllByTestId('server-event-link') as HTMLAnchorElement[];

describe('ServerHome', () => {
  it('lists the events under their club, in the order the server gave, each a link', async () => {
    open();
    await waitFor(() => expect(links()).toHaveLength(2));
    expect(screen.getByRole('heading', { name: 'Recon FPV' })).toBeVisible();
    expect(links()[0].textContent).toContain('Friday Night Whoops');
    expect(links()[0].textContent).toContain('2026-11-27 · Main field');
    expect(links()[0].getAttribute('href')).toBe('#/e/club/friday-night-764c5x');
    expect(links()[1].textContent).toContain('Sunday Cup');
    // An event is shown by its name. Its id is only in the address.
    expect(document.body.textContent).not.toContain('764c5x');
  });

  it('says so when there are no events', async () => {
    open({ club: [] });
    await waitFor(() => expect(screen.getByText('No events yet')).toBeVisible());
    expect(screen.queryByRole('heading', { name: 'Recon FPV' })).toBeNull();
  });

  it('shows each club that has events, and not one that has none', async () => {
    open({ north: [FRIDAY], south: [], east: [SUNDAY] }, [
      { id: 'north', name: 'North' },
      { id: 'south', name: 'South' },
      { id: 'east', name: 'East' }
    ]);
    await waitFor(() => expect(links()).toHaveLength(2));
    expect(screen.getByRole('heading', { name: 'North' })).toBeVisible();
    expect(screen.getByRole('heading', { name: 'East' })).toBeVisible();
    expect(screen.queryByRole('heading', { name: 'South' })).toBeNull();
    expect(links()[1].getAttribute('href')).toBe('#/e/east/sunday-cup-11aa22');
  });

  it('opens an event from its address, with its name and the race on it', async () => {
    location.hash = '#/e/club/friday-night-764c5x';
    const { sourceFor } = open();
    await waitFor(() =>
      expect(screen.getByTestId('server-event')).toHaveTextContent('Friday Night Whoops')
    );
    expect(sourceFor).toHaveBeenCalledWith('club', 'friday-night-764c5x');
    expect(screen.getByText('2026-11-27 · Main field')).toBeVisible();
    // The same view a visitor at the race gets: the heat, and who is in it.
    await waitFor(() => expect(document.body.textContent).toContain('Qualifying Heat 1'));
    await waitFor(() => expect(document.body.textContent).toContain('Ace'));
    expect(screen.getByRole('link', { name: '← All events' }).getAttribute('href')).toBe('#/');
    expect(screen.queryByTestId('server-event-link')).toBeNull();
  });

  it('shows who runs the event and what the director wrote about it, when given', async () => {
    location.hash = '#/e/club/friday-night-764c5x';
    open({
      club: [
        { ...FRIDAY, organizer: ' Recon FPV ', description: 'Whoops only.\nBring spare props.' },
        SUNDAY
      ]
    });
    await waitFor(() =>
      expect(screen.getByTestId('server-event-organizer')).toHaveTextContent('Run by Recon FPV')
    );
    expect(screen.getByTestId('server-event-description').textContent?.trim()).toBe(
      'Whoops only.\nBring spare props.'
    );
    // An event with neither has neither line.
    go('#/e/club/sunday-cup-11aa22');
    await waitFor(() => expect(screen.getByTestId('server-event')).toHaveTextContent('Sunday Cup'));
    expect(screen.queryByTestId('server-event-organizer')).toBeNull();
    expect(screen.queryByTestId('server-event-description')).toBeNull();
  });

  it('shows a description as text, whatever is in it', async () => {
    location.hash = '#/e/club/friday-night-764c5x';
    open({ club: [{ ...FRIDAY, description: '<img src=x onerror=alert(1)>', organizer: '   ' }] });
    await waitFor(() =>
      expect(screen.getByTestId('server-event-description').textContent).toContain('<img src=x')
    );
    expect(document.querySelector('img')).toBeNull();
    expect(screen.queryByTestId('server-event-organizer')).toBeNull();
  });

  it('goes from the list to an event and back, and stops reading the event it left', async () => {
    const { sourceFor, stop } = open();
    await waitFor(() => expect(links()).toHaveLength(2));
    expect(sourceFor).not.toHaveBeenCalled();

    go('#/e/club/sunday-cup-11aa22');
    await waitFor(() => expect(screen.getByTestId('server-event')).toHaveTextContent('Sunday Cup'));
    expect(sourceFor).toHaveBeenCalledTimes(1);

    go('#/e/club/friday-night-764c5x');
    await waitFor(() =>
      expect(screen.getByTestId('server-event')).toHaveTextContent('Friday Night Whoops')
    );
    expect(sourceFor).toHaveBeenCalledTimes(2);
    expect(stop).toHaveBeenCalledTimes(1);

    go('#/');
    await waitFor(() => expect(links()).toHaveLength(2));
    expect(stop).toHaveBeenCalledTimes(2);
  });

  it('says so when the address is for an event that is not here', async () => {
    location.hash = '#/e/club/gone-000000';
    open();
    await waitFor(() => expect(screen.getByText('This event is not here')).toBeVisible());
    expect(screen.getByRole('link', { name: '← All events' })).toBeVisible();
    // Another club's event under this club's address is not here either.
    go('#/e/other/friday-night-764c5x');
    await waitFor(() => expect(screen.getByText('This event is not here')).toBeVisible());
  });

  it('keeps what it has and says so when the server stops answering, then recovers', async () => {
    const { load } = open();
    await waitFor(() => expect(links()).toHaveLength(2));
    load.clubs.mockRejectedValue(new Error('offline'));
    await vi.advanceTimersByTimeAsync(15_100);
    await waitFor(() => expect(screen.getByTestId('server-offline')).toBeVisible());
    expect(links()).toHaveLength(2);

    load.clubs.mockResolvedValue([CLUB]);
    load.events.mockResolvedValue([FRIDAY]);
    await vi.advanceTimersByTimeAsync(15_100);
    await waitFor(() => expect(screen.queryByTestId('server-offline')).toBeNull());
    expect(links()).toHaveLength(1);
  });

  it('says it is not connected when the first read fails, with nothing else to show', async () => {
    const stop = vi.fn();
    const load = {
      clubs: vi.fn(async (): Promise<PushClub[]> => {
        throw new Error('offline');
      }),
      events: vi.fn(async (): Promise<ServerEvent[]> => [])
    };
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load,
      people: fakePeople(fakeServer({ providers: [] })),
      sourceFor: () => fakeLiveSession(racing(), { subscribeChanges: vi.fn(() => stop) })
    });
    await waitFor(() => expect(screen.getByTestId('server-offline')).toBeVisible());
    expect(screen.queryByText('Loading…')).toBeNull();
    expect(screen.queryByText('No events yet')).toBeNull();
  });

  it('shows names as text, whatever is in them', async () => {
    open({ club: [{ ...FRIDAY, name: '<img src=x onerror=alert(1)> & "Finals"' }] }, [
      { id: 'club', name: '<b>Club</b>' }
    ]);
    await waitFor(() => expect(links()).toHaveLength(1));
    expect(links()[0].textContent).toContain('<img src=x onerror=alert(1)> & "Finals"');
    expect(document.querySelector('img')).toBeNull();
    expect(document.querySelector('section b')).toBeNull();
  });

  it('has nothing on it that changes anything, and no way in for a director or a pilot', async () => {
    location.hash = '#/e/club/friday-night-764c5x';
    open();
    await waitFor(() => expect(screen.getByTestId('server-event')).toBeVisible());
    expect(screen.queryByRole('button', { name: /pilot|sign|director/i })).toBeNull();
    expect(document.body.textContent).not.toMatch(/director|sign in/i);
  });
});

// ── A pilot signing up on the server (docs/primitives.md §13.4) ─────────────

describe('signing up from the server', () => {
  const BEE = {
    ...ACE,
    name: 'Bea',
    clubs: [{ club: CLUB, role: 'Pilot' as const }],
    pilots: [{ club: CLUB, id: 'p-bee', callsign: 'Bee', held: true }]
  };

  /** Open Friday, whose qualifying has sign-up, as `account` (or as nobody). */
  function openSheet(account?: typeof BEE, calls = true) {
    let view = signupEvent(signupSheet());
    const reads = fakeLiveSession(view, {
      getEventState: vi.fn(async () => view),
      listPilots: vi.fn(async () => SIGNUP_PILOTS),
      listTimers: vi.fn(async () => [GATE])
    });
    const asks = {
      // The Director said yes. What it decided comes back with its next push.
      takeSpot: vi.fn(async () => {
        view = { ...view, cursor: view.cursor + 1 };
      }),
      takeNextSpot: vi.fn(async () => {
        view = { ...view, cursor: view.cursor + 1 };
      }),
      leaveSpot: vi.fn(async () => {
        view = { ...view, cursor: view.cursor + 1 };
      })
    };
    location.hash = '#/e/club/friday-night-764c5x';
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: {
        clubs: vi.fn(async (): Promise<PushClub[]> => [CLUB]),
        events: vi.fn(async (): Promise<ServerEvent[]> => [FRIDAY])
      },
      sourceFor: () => (calls ? { ...reads, ...asks } : reads),
      people: fakePeople(fakeServer({ account, providers: [] }))
    });
    return { reads, asks };
  }
  const signUpTab = () => screen.findByRole('tab', { name: 'Sign up' });

  it('someone who is only looking gets no Sign up tab', async () => {
    openSheet();
    await screen.findByRole('tab', { name: 'Results' });
    expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull();
    expect(screen.queryByRole('tab', { name: 'Me' })).toBeNull();
  });

  it('nor does someone signed in who is not a pilot in this event', async () => {
    // Signed in, with a pilot of the club that this event does not name.
    openSheet({ ...BEE, pilots: [{ club: CLUB, id: 'p-zed', callsign: 'Zed', held: true }] });
    await screen.findByTestId('server-who');
    await screen.findByRole('tab', { name: 'Results' });
    expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull();
  });

  it('nor someone who has only asked for the pilot', async () => {
    openSheet({ ...BEE, pilots: [{ club: CLUB, id: 'p-bee', callsign: 'Bee', held: false }] });
    await screen.findByTestId('server-who');
    await screen.findByRole('tab', { name: 'Results' });
    expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull();
  });

  it('a pilot in the event takes a seat, and the page waits for it to come back', async () => {
    const { asks, reads } = openSheet(BEE);
    await fireEvent.click(await signUpTab());
    const before = vi.mocked(reads.getEventState).mock.calls.length;
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    await waitFor(() =>
      expect(asks.takeSpot).toHaveBeenCalledWith('quali/heat-1', { adapter: 'gate', seat: '1' })
    );
    // The page read the event again until it had what the Director decided.
    await waitFor(() =>
      expect(vi.mocked(reads.getEventState).mock.calls.length).toBeGreaterThan(before)
    );
    expect(screen.queryByTestId('signup-error')).toBeNull();
  });

  it('a pilot leaves a seat and takes the next free one', async () => {
    const { asks } = openSheet({
      ...BEE,
      pilots: [{ club: CLUB, id: 'p-ace', callsign: 'Ace', held: true }]
    });
    await fireEvent.click(await signUpTab());
    await fireEvent.click(screen.getByRole('button', { name: 'Leave Qualifying Heat 1' }));
    await waitFor(() => expect(asks.leaveSpot).toHaveBeenCalledWith('quali/heat-1'));
    // One ask at a time: the next is taken once the first has come back.
    const next = await screen.findByRole('button', { name: 'Next free spot on Seat 3' });
    await waitFor(() => expect(next).toBeEnabled());
    await fireEvent.click(next);
    await waitFor(() =>
      expect(asks.takeNextSpot).toHaveBeenCalledWith('quali', { adapter: 'gate', seat: '2' })
    );
  });

  it("shows the Director's reason when it says no", async () => {
    const { asks } = openSheet(BEE);
    asks.takeSpot.mockRejectedValueOnce(
      new Error('The race computer is not connected right now. Try again in a moment.')
    );
    await fireEvent.click(await signUpTab());
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }));
    expect(await screen.findByTestId('signup-error')).toHaveTextContent(
      'The race computer is not connected right now.'
    );
  });

  it('only shows the sheet where there is no way to send an ask', async () => {
    openSheet(BEE, false);
    await screen.findByTestId('server-who');
    await fireEvent.click(await signUpTab());
    expect(screen.queryByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' })).toBeNull();
  });
});
