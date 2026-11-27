/**
 * Checking in to an open seat from a remote server's page (docs/primitives.md §11.9,
 * §13.13): the seat buttons, the race code a pilot types once, and the link from the QR
 * code shown at the race.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { EventView, MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import {
  NEEDS_RACE_CODE,
  dropRaceCode,
  keepRaceCode,
  keptRaceCode,
  needsRaceCode,
  type CodeStore
} from '../src/lib/raceCode.js';
import { formatServerHash, parseServerHash } from '../src/lib/serverHome.js';
import { eventView, heat, race, raceSeat } from './competition.js';
import { fakePeople, fakeServer } from './serverPeopleFake.js';
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
const FRIDAY: ServerEvent = { id: 'friday', name: 'Friday Night', created_at: 1, hidden: false };
const PILOTS = [
  { id: 'p-bee', callsign: 'Bee', vtx_types: [] },
  { id: 'p-cat', callsign: 'Cat', vtx_types: [] },
  { id: 'p-dan', callsign: 'Dan', vtx_types: [] }
];
const BEE: MyAccount = {
  id: 'acct_bea',
  name: 'Bea',
  owner: false,
  sign_ins: [{ provider: 'discord', subject: '1', label: 'bea' }],
  clubs: [{ club: CLUB, role: 'Pilot' }],
  pilots: [{ club: CLUB, id: 'p-bee', callsign: 'Bee', held: true }],
  profile: {},
  join_asks: []
};

/** A store that works like the browser's, kept by the test. */
function memory(start: Record<string, string> = {}): CodeStore & { all: Record<string, string> } {
  const all = { ...start };
  return {
    all,
    getItem: (key) => all[key] ?? null,
    setItem: (key, value) => {
      all[key] = value;
    },
    removeItem: (key) => {
      delete all[key];
    }
  };
}

/** A refusal as the client throws it for a status. */
function refusal(status: number, message: string): Error {
  return Object.assign(new Error(message), { status });
}

/** Practice with two open seats and one with Cat named on it. `holder` has seat 2. */
function practice(holder?: string): EventView {
  return eventView({
    phases: [heat('practice', 'Open Practice', ['Open', 'Open', 'p-cat'])],
    races: [
      race('practice', 1, 'Running', {
        seats: [raceSeat(0), raceSeat(1, undefined, holder ? { holder } : {}), raceSeat(2, 'p-cat')]
      })
    ]
  });
}

/** Open Friday as `account`, at the race code `right`, with `kept` already in the browser. */
function open({
  // `null` is nobody signed in.
  account = BEE as MyAccount | null,
  right = 'ABC234',
  kept = undefined as string | undefined,
  hash = '#/event/club/friday',
  start = practice()
} = {}) {
  let view = start;
  const codes = memory(kept ? { 'gridfpv.race-code.club/friday': kept } : {});
  const reads = fakeLiveSession(view, {
    getEventState: vi.fn(async () => view),
    listPilots: vi.fn(async () => PILOTS)
  });
  const seats = {
    // The Director: it takes the seat only with the race code.
    take: vi.fn(
      async (
        _club: string,
        _event: string,
        _race: string,
        seat: { adapter: string; seat: string },
        code: string | undefined
      ) => {
        if (!code) throw refusal(NEEDS_RACE_CODE, 'Type the race code first.');
        if (code.replace(/[\s-]/g, '').toUpperCase() !== right) {
          throw refusal(NEEDS_RACE_CODE, 'That is not the race code.');
        }
        view = {
          ...practice(),
          cursor: view.cursor + 1,
          races: view.races.map((r) => ({
            ...r,
            seats: r.seats.map((s) =>
              s.seat === seat.seat
                ? { ...s, holder: 'p-bee' }
                : s.holder === 'p-bee'
                  ? { ...s, holder: undefined }
                  : s
            )
          }))
        };
      }
    ),
    drop: vi.fn(async () => {
      view = { ...practice(), cursor: view.cursor + 1 };
    })
  };
  location.hash = hash;
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load: {
      clubs: vi.fn(async (): Promise<PushClub[]> => [CLUB]),
      events: vi.fn(async (): Promise<ServerEvent[]> => [FRIDAY])
    },
    sourceFor: () => reads,
    people: fakePeople(fakeServer({ account: account ?? undefined, providers: [] })),
    seats,
    codes
  });
  return { seats, codes, reads };
}

const flyNow = async () => {
  await fireEvent.click(await screen.findByRole('tab', { name: 'Me' }));
  return screen.findByRole('region', { name: 'Fly now' });
};
const typeCode = async (code: string) => {
  const dialog = await screen.findByRole('dialog');
  await fireEvent.input(within(dialog).getByLabelText('Race code'), { target: { value: code } });
  return dialog;
};
const send = (dialog: HTMLElement) =>
  within(dialog).getByRole('button', { name: 'Take the seat' }) as HTMLButtonElement;

describe('taking a seat from the server', () => {
  it('asks for the race code the first time, and takes the seat with it', async () => {
    const { seats, codes } = open();
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    // Nothing is asked of the Director before the pilot has typed a code.
    const dialog = await screen.findByRole('dialog');
    expect(dialog.textContent).toContain('Are you at the race?');
    expect(seats.take).not.toHaveBeenCalled();
    expect(send(dialog).disabled).toBe(true);

    await typeCode('abc-234');
    expect(send(dialog).disabled).toBe(false);
    await fireEvent.click(send(dialog));
    await waitFor(() =>
      expect(seats.take).toHaveBeenCalledWith(
        'club',
        'friday',
        'practice#1',
        expect.objectContaining({ adapter: 'bench', seat: 'seat-1' }),
        'abc-234'
      )
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    // The page shows the seat as theirs once the Director's push is back.
    await waitFor(() => expect(region.textContent).toContain('You have Seat 1'));
    // The code is kept, tidied, for the next seat.
    expect(codes.all).toEqual({ 'gridfpv.race-code.club/friday': 'ABC234' });
  });

  it('does not ask again once a code is kept', async () => {
    const { seats } = open({ kept: 'ABC234' });
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 2' }));
    await waitFor(() => expect(seats.take).toHaveBeenCalledTimes(1));
    expect(seats.take.mock.calls[0][4]).toBe('ABC234');
    await waitFor(() => expect(region.textContent).toContain('You have Seat 2'));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('catches a slip in the code before it is sent, so it is not a wrong try', async () => {
    const { seats } = open();
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    for (const slip of ['ABC', 'ABC2345', 'ABC10O']) {
      const dialog = await typeCode(slip);
      expect(send(dialog).disabled).toBe(true);
      expect(within(dialog).getByRole('status').textContent).toMatch(/A code has/);
    }
    // Enter does not send it either.
    await fireEvent.submit(screen.getByRole('dialog').querySelector('form')!);
    expect(seats.take).not.toHaveBeenCalled();
  });

  it("shows the Director's words for a wrong code, and lets the pilot try again", async () => {
    const { seats, codes } = open();
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    let dialog = await typeCode('ZZZ999');
    await fireEvent.click(send(dialog));
    expect(await screen.findByTestId('race-code-why')).toHaveTextContent(
      'That is not the race code.'
    );
    expect(codes.all).toEqual({});
    expect(region.textContent).not.toContain('You have');

    dialog = await typeCode('ABC234');
    await fireEvent.click(send(dialog));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(seats.take).toHaveBeenCalledTimes(2);
    await waitFor(() => expect(region.textContent).toContain('You have Seat 1'));
  });

  it('asks for the new code when the one it kept is an old one, and says why', async () => {
    const { seats, codes } = open({ kept: 'OLD222' });
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    // The kept code was tried, refused and forgotten.
    expect(await screen.findByTestId('race-code-why')).toHaveTextContent(
      'That is not the race code.'
    );
    expect(seats.take).toHaveBeenCalledTimes(1);
    expect(codes.all).toEqual({});

    const dialog = await typeCode('ABC234');
    await fireEvent.click(send(dialog));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(codes.all).toEqual({ 'gridfpv.race-code.club/friday': 'ABC234' });
  });

  it('takes no seat when the pilot closes the question', async () => {
    const { seats } = open();
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    const dialog = await screen.findByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(seats.take).not.toHaveBeenCalled();
    // The buttons work again: the page is not left busy.
    await waitFor(() =>
      expect(
        (within(region).getByRole('button', { name: 'Take Seat 1' }) as HTMLButtonElement).disabled
      ).toBe(false)
    );
    expect(within(region).queryByRole('alert')).toBeNull();
  });

  it("shows the Director's own reason, and keeps a code that was not the problem", async () => {
    const { seats, codes } = open({ kept: 'ABC234' });
    seats.take.mockRejectedValueOnce(refusal(409, 'That race is over.'));
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    expect(await within(region).findByRole('alert')).toHaveTextContent('That race is over.');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(codes.all).toEqual({ 'gridfpv.race-code.club/friday': 'ABC234' });
  });

  it("shows the Director's own reason in the question too, when a code was just typed", async () => {
    const { seats } = open();
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    const dialog = await typeCode('ABC234');
    seats.take.mockRejectedValueOnce(refusal(409, 'The race computer did not answer.'));
    await fireEvent.click(send(dialog));
    expect(await screen.findByTestId('race-code-why')).toHaveTextContent(
      'The race computer did not answer.'
    );
  });

  it('drops a seat with no code', async () => {
    const { seats } = open({ start: practice('p-bee') });
    const region = await flyNow();
    expect(region.textContent).toContain('You have Seat 2');
    await fireEvent.click(within(region).getByRole('button', { name: 'Drop Seat 2' }));
    await waitFor(() => expect(seats.drop).toHaveBeenCalledWith('club', 'friday', 'practice#1'));
    await waitFor(() => expect(region.textContent).not.toContain('You have'));
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('is not offered to someone who is only looking', async () => {
    open({ account: null });
    await screen.findByRole('tab', { name: 'Results' });
    expect(screen.queryByRole('tab', { name: 'Me' })).toBeNull();
    expect(screen.queryByRole('button', { name: /Take Seat/ })).toBeNull();
  });
});

describe('the link in the QR code shown at the race', () => {
  it('keeps the code, opens the event, and takes the code out of the address', async () => {
    const { seats, codes } = open({ hash: '#/here/club/friday/abc234' });
    expect(await screen.findByTestId('server-event')).toHaveTextContent('Friday Night');
    expect(location.hash).toBe('#/event/club/friday');
    expect(codes.all).toEqual({ 'gridfpv.race-code.club/friday': 'ABC234' });

    // So a seat is taken with no typing.
    const region = await flyNow();
    await fireEvent.click(within(region).getByRole('button', { name: 'Take Seat 1' }));
    await waitFor(() => expect(seats.take).toHaveBeenCalledTimes(1));
    expect(seats.take.mock.calls[0][4]).toBe('ABC234');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('works for someone not signed in yet: the code waits in the browser', async () => {
    const { codes } = open({ account: null, hash: '#/here/club/friday/ABC234' });
    expect(await screen.findByTestId('server-event')).toHaveTextContent('Friday Night');
    expect(codes.all).toEqual({ 'gridfpv.race-code.club/friday': 'ABC234' });
  });

  it('is read from the address, and nothing else is', () => {
    expect(parseServerHash('#/here/club/friday/ABC234')).toEqual({
      kind: 'here',
      club: 'club',
      event: 'friday',
      code: 'ABC234'
    });
    expect(formatServerHash({ kind: 'here', club: 'c d', event: 'e', code: 'X/Y' })).toBe(
      '#/here/c%20d/e/X%2FY'
    );
    expect(parseServerHash('#/here/c%20d/e/X%2FY')).toEqual({
      kind: 'here',
      club: 'c d',
      event: 'e',
      code: 'X/Y'
    });
    for (const not of ['#/here', '#/here/club', '#/here/club/friday', '#/here/a/b/c/d']) {
      expect(parseServerHash(not)).toEqual({ kind: 'list' });
    }
  });
});

describe('keeping the race code in the browser', () => {
  it('keeps one code for each event, tidied', () => {
    const store = memory();
    expect(keptRaceCode('club', 'friday', store)).toBeUndefined();
    keepRaceCode('club', 'friday', ' abc-234 ', store);
    keepRaceCode('club', 'sunday', 'XYZ789', store);
    keepRaceCode('other', 'friday', 'QQQ222', store);
    expect(keptRaceCode('club', 'friday', store)).toBe('ABC234');
    expect(keptRaceCode('club', 'sunday', store)).toBe('XYZ789');
    expect(keptRaceCode('other', 'friday', store)).toBe('QQQ222');
    dropRaceCode('club', 'friday', store);
    expect(keptRaceCode('club', 'friday', store)).toBeUndefined();
    expect(keptRaceCode('club', 'sunday', store)).toBe('XYZ789');
  });

  it('keeps nothing for an empty code', () => {
    const store = memory();
    keepRaceCode('club', 'friday', '  - ', store);
    expect(store.all).toEqual({});
  });

  it('carries on when the browser will not store anything', () => {
    const broken: CodeStore = {
      getItem: () => {
        throw new Error('closed');
      },
      setItem: () => {
        throw new Error('full');
      },
      removeItem: () => {
        throw new Error('closed');
      }
    };
    expect(() => keepRaceCode('club', 'friday', 'ABC234', broken)).not.toThrow();
    expect(keptRaceCode('club', 'friday', broken)).toBeUndefined();
    expect(() => dropRaceCode('club', 'friday', broken)).not.toThrow();
  });

  it('knows a refusal about the race code from any other', () => {
    expect(needsRaceCode(refusal(NEEDS_RACE_CODE, 'x'))).toBe(true);
    expect(needsRaceCode(refusal(409, 'x'))).toBe(false);
    expect(needsRaceCode(refusal(403, 'x'))).toBe(false);
    expect(needsRaceCode(new Error('offline'))).toBe(false);
    expect(needsRaceCode('no')).toBe(false);
  });
});
