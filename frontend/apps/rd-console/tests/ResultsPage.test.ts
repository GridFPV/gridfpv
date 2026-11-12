/**
 * The Results page: each class's standings, and one phase's ranking at a time with the
 * numbers behind each place. Rendered against a stand-in live store. Every order shown is
 * the Director's; the page ranks nobody.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import type { Class, EventView, Format, Pilot } from '@gridfpv/types';
import ResultsPage from '../src/screens/ResultsPage.svelte';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import {
  eventView,
  FIRST_TO_3,
  group,
  heat,
  lap,
  OPEN_PRACTICE,
  placed,
  race,
  raceSeat,
  TWO_MINUTE_QUALI
} from './competition.js';
import { openLive } from './support.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] },
  { id: 'p-3c', callsign: 'Cat', vtx_types: [] }
];
const CLASSES: Class[] = [{ id: 'cls-open', name: 'Open', source: 'Custom' }];

async function open(view: EventView, over: Partial<LiveSession> = {}) {
  const { live } = await openLive(view, {
    listPilots: vi.fn(async () => PILOTS),
    listClasses: vi.fn(async () => CLASSES),
    ...over
  });
  render(ResultsPage, { live });
}

/** A table as text: one string per row, cells joined by " | ". */
const rowsOf = (table: HTMLElement): string[] =>
  within(table)
    .getAllByRole('row')
    .map((row) =>
      [...row.querySelectorAll('th, td')]
        .map((cell) => cell.textContent?.replace(/\s+/g, ' ').trim())
        .join(' | ')
        .trim()
    );

const picker = () => screen.getByLabelText('Phase') as HTMLSelectElement;

function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of ['q-1', 'q-2', 'f-1', '#1', 'bench', 'seat-1', 'p-1a', 'cls-']) {
    expect(text).not.toContain(raw);
  }
}

/**
 * Qualifying on best 3 laps in a row, over two heats. Ace's best run is in heat 2
 * (30 s), Bee's in heat 1 (33 s). The final is one race to 3 laps.
 */
function raceDay(over: Partial<EventView> = {}): EventView {
  const quali = group('quali', 'Qualifying', undefined, {
    format: TWO_MINUTE_QUALI,
    ranking: [
      placed('p-1a', 1, { laps: 4, best_lap_micros: 9_500_000 }),
      placed('p-2b', 2, { laps: 3, best_lap_micros: 10_500_000 }),
      placed('p-3c', 3, { flew: false })
    ]
  });
  return eventView({
    phases: [
      quali,
      heat('q-1', 'Qualifying Heat 1', ['p-1a', 'p-2b'], {
        parent: 'quali',
        format: TWO_MINUTE_QUALI,
        ranking: [placed('p-2b', 1, { laps: 3 }), placed('p-1a', 2, { laps: 3 })]
      }),
      heat('q-2', 'Qualifying Heat 2', ['p-1a'], {
        parent: 'quali',
        format: TWO_MINUTE_QUALI,
        ranking: [placed('p-1a', 1, { laps: 4 })]
      }),
      heat('f-1', 'Final', ['p-1a', 'p-2b'], {
        complete: true,
        ranking: [
          placed('p-2b', 1, { laps: 3, wins: 1, best_lap_micros: 10_000_000, position_set: true }),
          placed('p-1a', 2, { laps: 2, disqualified: true })
        ]
      })
    ],
    races: [
      race('q-1', 1, 'Saved', {
        seats: [
          raceSeat(0, 'p-1a', { laps: [lap(1, 12), lap(2, 12), lap(3, 12)], finish: 2 }),
          raceSeat(1, 'p-2b', { laps: [lap(4, 11), lap(5, 11), lap(6, 11)], finish: 1 })
        ]
      }),
      race('q-2', 1, 'Saved', {
        seats: [
          raceSeat(0, 'p-1a', {
            laps: [lap(7, 14), lap(8, 10), lap(9, 10), lap(10, 10)],
            finish: 1
          })
        ]
      }),
      race('f-1', 1, 'Discarded', { note: 'Gate fell over' }),
      race('f-1', 2, 'Saved', {
        seats: [
          raceSeat(0, 'p-1a', { laps: [lap(11, 12), lap(12, 12)], finish: 2 }),
          raceSeat(1, 'p-2b', { laps: [lap(13, 10), lap(14, 10), lap(15, 10)], finish: 1 })
        ]
      })
    ],
    ...over
  });
}

describe('ResultsPage', () => {
  it('says so when no ranked phase has a result yet', async () => {
    await open(
      eventView({
        phases: [
          // A heat with a ranked format but nothing raced, and a practice heat that ranks nobody.
          heat('q-1', 'Qualifying Heat 1', ['p-1a']),
          heat('pr-1', 'Practice', ['p-1a'], {
            format: OPEN_PRACTICE,
            ranking: [placed('p-1a', 1)]
          })
        ]
      })
    );
    expect(screen.getByTestId('no-results')).toHaveTextContent('No ranked phase has a result yet.');
    expect(screen.queryByLabelText('Phase')).toBeNull();
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.getByRole('link', { name: 'Help for Results' })).toHaveAttribute(
      'href',
      '#/help/results'
    );
  });

  it('says so when the event cannot be read', async () => {
    await open(eventView(), {
      getEventState: vi.fn(async () => Promise.reject(new Error('The Director is not answering.')))
    });
    expect(screen.getByRole('alert')).toHaveTextContent('The Director is not answering.');
    expect(screen.queryByTestId('no-results')).toBeNull();
  });

  it('offers every ranked phase by its place in the tree and opens on the phase raced last', async () => {
    await open(raceDay());
    expect([...picker().options].map((option) => option.textContent)).toEqual([
      'Qualifying',
      'Qualifying › Qualifying Heat 1',
      'Qualifying › Qualifying Heat 2',
      'Final'
    ]);
    // The last saved race was in the Final.
    expect(picker()).toHaveValue('f-1');
    expect(screen.getByRole('table', { name: 'Results of Final' })).toBeVisible();
    expectNoRawIds();
  });

  it('opens on the phase that holds the heat raced last: it is the fuller table', async () => {
    const day = raceDay();
    await open({ ...day, races: day.races.slice(0, 2) });
    expect(picker()).toHaveValue('quali');
  });

  it("shows a phase's ranking in the Director's order with the numbers behind each place", async () => {
    const day = raceDay();
    await open({ ...day, races: day.races.slice(0, 2) });
    expect(rowsOf(screen.getByRole('table', { name: 'Results of Qualifying' }))).toEqual([
      'Place | Pilot | Best 3 in a row | Best lap | Most laps | Notes',
      // Ace's fastest three in a row are the last three laps of heat 2, not the 36 s of heat 1.
      '1 | Ace | 30.000 | 9.500 | 4 |',
      '2 | Bee | 33.000 | 10.500 | 3 |',
      '3 | Cat | — | — | 0 | Did not fly'
    ]);
    const state = screen.getByTestId('result-state');
    expect(state).toHaveTextContent('Provisional');
    expect(state).toHaveTextContent('Still being raced.');
    expect(state).toHaveTextContent('2 minute qualifying');
    // A phase that holds heats lists no races of its own.
    expect(screen.queryByRole('list', { name: /^Races of/ })).toBeNull();
  });

  it('shows the columns the format ranks by, and marks hand-set and disqualified places', async () => {
    const wins: Format = { ...FIRST_TO_3, ranking: { sort: ['Wins', 'LastFinish'] } };
    const day = raceDay();
    day.phases[3] = { ...day.phases[3], format: wins };
    await open(day);
    expect(rowsOf(screen.getByRole('table', { name: 'Results of Final' }))).toEqual([
      'Place | Pilot | Wins | Best lap | Most laps | Notes',
      '1 | Bee | 1 | 10.000 | 3 | Set by hand',
      '2 | Ace | 0 | — | 2 | Disqualified'
    ]);
    expect(screen.getByTestId('result-state')).toHaveTextContent('Complete, not locked yet.');
  });

  it('shows a points column only for a format ranked on points', async () => {
    const points: Format = {
      ...FIRST_TO_3,
      ranking: { sort: ['Points', 'BestFinish'] },
      points: [10, 8]
    };
    const day = raceDay();
    day.phases[3] = {
      ...day.phases[3],
      format: points,
      ranking: [placed('p-2b', 1, { points: 18, laps: 3 }), placed('p-1a', 2, { points: 16 })]
    };
    await open(day);
    expect(rowsOf(screen.getByRole('table', { name: 'Results of Final' })).slice(0, 2)).toEqual([
      'Place | Pilot | Points | Best lap | Most laps | Notes',
      '1 | Bee | 18 | — | 3 |'
    ]);
  });

  it("lists a heat's races newest first, each with its finish order and a link to Marshal", async () => {
    await open(raceDay());
    const items = within(screen.getByRole('list', { name: 'Races of Final' })).getAllByRole(
      'listitem'
    );
    expect(items.map((item) => item.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'Race 2 Saved 1. Bee (3 laps)2. Ace (2 laps) Open in Marshal',
      'Race 1 Discarded “Gate fell over” Open in Marshal'
    ]);
    // The address holds the race's id; nothing a person reads does.
    expect(within(items[0]).getByRole('link', { name: 'Open in Marshal' })).toHaveAttribute(
      'href',
      `#/event/marshal/${encodeURIComponent('f-1#2')}`
    );
    expectNoRawIds();
  });

  it('shows another phase when the director picks it', async () => {
    await open(raceDay());
    await fireEvent.change(picker(), { target: { value: 'q-1' } });
    expect(rowsOf(screen.getByRole('table', { name: 'Results of Qualifying Heat 1' }))).toEqual([
      'Place | Pilot | Best 3 in a row | Best lap | Most laps | Notes',
      '1 | Bee | 33.000 | — | 3 |',
      '2 | Ace | 36.000 | — | 3 |'
    ]);
    expect(screen.getByRole('list', { name: 'Races of Qualifying Heat 1' })).toBeVisible();
  });

  it('says a locked phase is official', async () => {
    const day = raceDay();
    day.phases[3] = { ...day.phases[3], locked: true, locked_here: true };
    await open(day);
    const state = screen.getByTestId('result-state');
    expect(state).toHaveTextContent('Official');
    expect(state).toHaveTextContent('Locked.');
    expect(state).not.toHaveTextContent('Provisional');
  });
});

describe('class standings', () => {
  it('shows each class from 1 to n with the phase that placed each pilot', async () => {
    await open(
      raceDay({
        rosters: [
          {
            class: 'cls-open',
            pilots: ['p-1a', 'p-2b', 'p-3c'],
            counted_phases: ['f-1', 'quali'],
            standings: [
              { pilot: 'p-2b', position: 1, placed_by: 'f-1' },
              { pilot: 'p-1a', position: 2, placed_by: 'quali' },
              { pilot: 'p-3c', position: 3 }
            ]
          }
        ]
      })
    );
    expect(screen.getByRole('heading', { name: 'Open standings' })).toBeVisible();
    expect(screen.getByText('From Final, then Qualifying.')).toBeVisible();
    expect(rowsOf(screen.getByRole('table', { name: 'Standings of Open' }))).toEqual([
      'Place | Pilot | Placed by',
      '1 | Bee | Final',
      '2 | Ace | Qualifying',
      '3 | Cat | Not placed yet'
    ]);
    expectNoRawIds();
  });

  it('says so when no phase counts toward a class, and points at the Event page', async () => {
    await open(
      eventView({
        rosters: [{ class: 'cls-open', pilots: ['p-1a'], counted_phases: [], standings: [] }]
      })
    );
    expect(screen.getByText(/No phase counts toward this class yet\./)).toBeVisible();
    expect(screen.getByRole('link', { name: 'Event page' })).toHaveAttribute('href', '#/event');
    expect(screen.queryByRole('table', { name: 'Standings of Open' })).toBeNull();
  });
});
