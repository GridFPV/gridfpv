/**
 * The spectator's screen: the race on now, what is coming up, and the results.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { EventView, Pilot } from '@gridfpv/types';
import SpectatorView from '../src/screens/SpectatorView.svelte';
import {
  TWO_MINUTE_QUALI,
  eventView,
  group,
  heat,
  lap,
  placed,
  race,
  raceSeat
} from './competition.js';
import { SIGNUP_PILOTS, signupEvent, signupSheet } from './signupFixtures.js';
import { openLive } from './support.js';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const S = 1_000_000;
const pilot = (id: string, callsign: string, color?: string): Pilot => ({
  id,
  callsign,
  vtx_types: [],
  ...(color ? { color } : {})
});
const PILOTS = [pilot('p-ace', 'Ace', '#FF0000'), pilot('p-bee', 'Bee'), pilot('p-cat', 'Cat')];

/** A running race: Bee leads Ace on laps. The Director's clock is 12 s past the start. */
function running(): EventView {
  const on = race('h1', 1, 'Running', {
    started_at: 100 * S,
    tone_at: 100 * S,
    seats: [
      raceSeat(0, 'p-ace', {
        finish: 2,
        hole_shot: { crossing: 1, at: 101 * S },
        laps: [lap(2, 5.5)],
        best_lap_micros: 5.5 * S
      }),
      raceSeat(1, 'p-bee', {
        finish: 1,
        hole_shot: { crossing: 3, at: 101 * S },
        laps: [lap(4, 5), lap(5, 4.25)],
        best_lap_micros: 4.25 * S
      })
    ]
  });
  return eventView({
    now: 112 * S,
    phases: [
      heat('h1', 'Qualifying Heat 1', ['p-ace', 'p-bee']),
      heat('h2', 'Qualifying Heat 2', ['p-cat'])
    ],
    queue: ['h1', 'h2'],
    races: [on],
    current_race: on.race
  });
}

async function open(view: EventView, pilots: Pilot[] = PILOTS) {
  const { live, session } = await openLive(view, { listPilots: vi.fn(async () => pilots) });
  render(SpectatorView, { live });
  return { live, session };
}

const tab = (name: string | RegExp) => screen.getByRole('tab', { name });
describe('SpectatorView: the race on now', () => {
  it('names the heat, says it is racing, and runs a clock', async () => {
    await open(running());
    expect(tab('On now')).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByTestId('race-heat')).toHaveTextContent('Qualifying Heat 1');
    expect(screen.getByTestId('race-word')).toHaveTextContent('Racing');
    expect(screen.getByRole('timer')).toHaveTextContent(/^0:1\d\.\d$/);
  });

  it('lists the pilots in race order with laps, last lap and best lap', async () => {
    await open(running());
    const table = screen.getByRole('table', { name: 'Pilots in this race' });
    const names = within(table)
      .getAllByRole('rowheader')
      .map((cell) => cell.textContent?.trim());
    expect(names).toEqual(['Bee', 'Ace']);
    const bee = within(table).getByRole('row', { name: /Bee/ });
    expect(bee).toHaveTextContent('1st');
    expect(
      within(bee)
        .getAllByRole('cell')
        .map((cell) => cell.textContent?.trim())
    ).toEqual(['1st', '2', '4.250', '4.250']);
    const ace = within(table).getByRole('row', { name: /Ace/ });
    expect(
      within(ace)
        .getAllByRole('cell')
        .map((cell) => cell.textContent?.trim())
    ).toEqual(['2nd', '1', '5.500', '5.500']);
  });

  it("shows a pilot's colour as a dot, and none for a pilot without one", async () => {
    await open(running());
    const table = screen.getByRole('table', { name: 'Pilots in this race' });
    const ace = within(table).getByRole('rowheader', { name: /Ace/ });
    const bee = within(table).getByRole('rowheader', { name: /Bee/ });
    expect(ace.querySelector('.dot')).not.toBeNull();
    expect(bee.querySelector('.dot')).toBeNull();
  });

  it('keeps the clock moving while the race is on', async () => {
    await open(running());
    const first = screen.getByRole('timer').textContent;
    await vi.advanceTimersByTimeAsync(2500);
    expect(screen.getByRole('timer').textContent).not.toBe(first);
  });

  it('counts down to the start while the race is starting', async () => {
    const view = running();
    view.races[0] = { ...view.races[0], state: 'Staging', started_at: undefined, tone_at: 115 * S };
    await open(view);
    expect(screen.getByTestId('race-word')).toHaveTextContent('Starting');
    expect(screen.getByRole('timer')).toBeVisible();
  });

  it('a stopped race says finished and is still the race on top', async () => {
    const view = running();
    view.races[0] = { ...view.races[0], state: 'Stopped', stopped_at: 111 * S };
    await open(view);
    expect(screen.getByTestId('race-word')).toHaveTextContent('Finished');
    expect(tab('On now')).toBeVisible();
  });

  it('with no race on, shows how the last one ended, with no clock', async () => {
    const view = running();
    view.races[0] = { ...view.races[0], state: 'Saved', stopped_at: 111 * S };
    view.current_race = undefined;
    await open(view);
    expect(tab('Last race')).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByTestId('race-word')).toHaveTextContent('Result');
    expect(screen.queryByRole('timer')).toBeNull();
    expect(screen.getByRole('table', { name: 'Pilots in this race' })).toBeVisible();
  });

  it('says so when nothing has been raced', async () => {
    await open(eventView({ phases: [heat('h1', 'Heat 1', ['p-ace'])], queue: ['h1'] }));
    expect(screen.getByTestId('no-race')).toHaveTextContent('No race yet.');
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('a thrown-out race is not shown as a result', async () => {
    await open(
      eventView({ phases: [heat('h1', 'Heat 1', ['p-ace'])], races: [race('h1', 1, 'Discarded')] })
    );
    expect(screen.getByTestId('no-race')).toBeVisible();
  });

  it('shows the best run of laps when the heat is ranked on one', async () => {
    const view = running();
    view.phases[0] = { ...view.phases[0], format: TWO_MINUTE_QUALI };
    view.races[0].seats[1] = {
      ...view.races[0].seats[1],
      laps: [lap(4, 5), lap(5, 4), lap(6, 3)]
    };
    await open(view);
    const table = screen.getByRole('table', { name: 'Pilots in this race' });
    expect(within(table).getByRole('columnheader', { name: 'Best 3' })).toBeVisible();
    const bee = within(table).getByRole('row', { name: /Bee/ });
    expect(within(bee).getAllByRole('cell').at(-1)).toHaveTextContent('12.000');
    // Ace has one lap: no run of three yet.
    const ace = within(table).getByRole('row', { name: /Ace/ });
    expect(within(ace).getAllByRole('cell').at(-1)?.textContent?.trim()).not.toMatch(/\d/);
  });

  it('names an open seat by its seat', async () => {
    const view = running();
    view.races[0].seats.push(raceSeat(2));
    await open(view);
    const table = screen.getByRole('table', { name: 'Pilots in this race' });
    expect(within(table).getByRole('rowheader', { name: 'Seat 3' })).toBeVisible();
  });

  it('draws a name as text, whatever is in it', async () => {
    const view = running();
    await open(view, [pilot('p-ace', '<img src=x onerror=alert(1)>'), pilot('p-bee', 'Bee')]);
    expect(document.querySelector('img')).toBeNull();
    expect(screen.getByText('<img src=x onerror=alert(1)>')).toBeVisible();
  });
});

describe('SpectatorView: up next', () => {
  it('counts the heats coming up on the tab and lists who is in each', async () => {
    await open(running());
    const next = tab(/Up next/);
    expect(next).toHaveTextContent('1');
    await fireEvent.click(next);
    const list = screen.getByRole('region', { name: 'Up next' });
    const heat2 = within(list).getByRole('listitem', { name: 'Qualifying Heat 2' });
    expect(heat2).toHaveTextContent('Seat 1');
    expect(heat2).toHaveTextContent('Cat');
    // The heat on now is not "next".
    expect(within(list).queryByRole('listitem', { name: 'Qualifying Heat 1' })).toBeNull();
  });

  it('says so when nothing is lined up', async () => {
    await open(eventView());
    await fireEvent.click(tab(/Up next/));
    expect(screen.getByTestId('no-upcoming')).toHaveTextContent('Nothing is lined up yet.');
    expect(tab(/Up next/).textContent?.trim()).toBe('Up next');
  });
});

describe('SpectatorView: results', () => {
  function withResults(): EventView {
    return eventView({
      phases: [
        group('quali', 'Qualifying', undefined, {
          format: TWO_MINUTE_QUALI,
          ranking: [
            placed('p-bee', 1, { laps: 9, best_lap_micros: 4.25 * S }),
            placed('p-ace', 2, { laps: 7, best_lap_micros: 5.5 * S }),
            placed('p-cat', 3, { flew: false })
          ]
        }),
        heat('h1', 'Heat 1', ['p-ace', 'p-bee'], {
          parent: 'quali',
          ranking: [
            placed('p-ace', 1, { laps: 3 }),
            placed('p-bee', 2, { laps: 2, disqualified: true })
          ]
        })
      ],
      races: [race('h1', 1, 'Saved')]
    });
  }

  it('opens on the phase raced last and lists it in ranked order', async () => {
    await open(withResults());
    await fireEvent.click(tab('Results'));
    const pickerEl = screen.getByRole('combobox', { name: 'Results for' }) as HTMLSelectElement;
    expect(pickerEl.selectedOptions[0].textContent).toBe('Qualifying');
    const table = screen.getByRole('table', { name: 'Results' });
    expect(
      within(table)
        .getAllByRole('rowheader')
        .map((cell) => cell.textContent?.trim().replace(/\s+/g, ' '))
    ).toEqual(['Bee', 'Ace', 'Cat did not fly']);
    expect(within(table).getByRole('columnheader', { name: 'Best 3' })).toBeVisible();
    const bee = within(table).getByRole('row', { name: /Bee/ });
    expect(bee).toHaveTextContent('1st');
    expect(bee).toHaveTextContent('4.250');
  });

  it('switches to another phase, and marks a disqualified pilot', async () => {
    await open(withResults());
    await fireEvent.click(tab('Results'));
    const pickerEl = screen.getByRole('combobox', { name: 'Results for' });
    expect(
      within(pickerEl)
        .getAllByRole('option')
        .map((o) => o.textContent)
    ).toEqual(['Qualifying', 'Qualifying › Heat 1']);
    await fireEvent.change(pickerEl, { target: { value: 'h1' } });
    const table = screen.getByRole('table', { name: 'Results' });
    expect(within(table).getByRole('row', { name: /Bee/ })).toHaveTextContent('Disqualified');
    expect(within(table).queryByRole('columnheader', { name: 'Best 3' })).toBeNull();
  });

  it('says so when there are no results', async () => {
    await open(running());
    await fireEvent.click(tab('Results'));
    expect(screen.getByTestId('no-results')).toHaveTextContent('No results yet.');
  });

  it('a pilot with no best lap shows a dash, not a zero', async () => {
    await open(withResults());
    await fireEvent.click(tab('Results'));
    const cat = within(screen.getByRole('table', { name: 'Results' })).getByRole('row', {
      name: /Cat/
    });
    expect(within(cat).getAllByRole('cell').at(-1)).toHaveTextContent('—');
  });
});

describe('SpectatorView: the connection, and looking only', () => {
  it('has nothing on it that changes anything', async () => {
    await open(running());
    for (const name of ['On now', /Up next/, 'Results']) {
      await fireEvent.click(tab(name));
      // No button but the three tabs; no link; no text box.
      expect(screen.queryAllByRole('button')).toEqual([]);
      expect(screen.queryAllByRole('link')).toEqual([]);
      expect(screen.queryAllByRole('textbox')).toEqual([]);
    }
  });

  it('never sends a command', async () => {
    const { session } = await open(running());
    for (const name of ['On now', /Up next/, 'Results']) await fireEvent.click(tab(name));
    await vi.advanceTimersByTimeAsync(3000);
    expect(session.sendCommand).not.toHaveBeenCalled();
    expect(session.postPass).not.toHaveBeenCalled();
    expect(session.createBracket).not.toHaveBeenCalled();
  });

  it('says loading before the first snapshot, and that Grid cannot be reached when it fails', async () => {
    const loading = {
      view: undefined,
      names: (await openLive(eventView())).live.names,
      pilots: [],
      classes: [],
      timers: [],
      loadError: undefined as string | undefined,
      nowMicros: () => 0,
      reloadPilots: vi.fn(async () => {}),
      reloadClasses: vi.fn(async () => {}),
      reloadTimers: vi.fn(async () => {})
    };
    render(SpectatorView, { live: loading });
    expect(screen.getByRole('status')).toHaveTextContent('Loading…');
    cleanup();
    render(SpectatorView, { live: { ...loading, loadError: 'Failed to fetch' } });
    expect(screen.getByTestId('spectator-offline')).toHaveTextContent(
      'Could not reach Grid. Trying again.'
    );
    expect(screen.queryByRole('tablist')).toBeNull();
  });

  it('keeps showing the last snapshot when a later read fails, and says it is not live', async () => {
    const { live } = await openLive(running(), { listPilots: vi.fn(async () => PILOTS) });
    live.loadError = 'Failed to fetch';
    render(SpectatorView, { live });
    expect(screen.getByTestId('spectator-stale')).toHaveTextContent('Not live right now.');
    expect(screen.getByRole('table', { name: 'Pilots in this race' })).toBeVisible();
  });

  it('asks for names again when the snapshot names someone new', async () => {
    const listPilots = vi
      .fn<() => Promise<Pilot[]>>()
      .mockResolvedValueOnce([pilot('p-bee', 'Bee')])
      .mockResolvedValue(PILOTS);
    const { live } = await openLive(running(), { listPilots });
    render(SpectatorView, { live });
    // Ace was not in the first list: the page asks again and then has the name.
    await waitFor(() => expect(listPilots).toHaveBeenCalledTimes(2));
    await waitFor(() =>
      expect(
        within(screen.getByRole('table', { name: 'Pilots in this race' })).getByRole('rowheader', {
          name: /Ace/
        })
      ).toBeVisible()
    );
  });

  it('does not ask for names again when it has them all', async () => {
    const { session } = await open(running());
    await vi.advanceTimersByTimeAsync(3000);
    expect(session.listPilots).toHaveBeenCalledOnce();
    expect(session.listClasses).toHaveBeenCalledOnce();
    expect(session.listTimers).toHaveBeenCalledOnce();
  });
});

describe('SpectatorView: a signed-in pilot', () => {
  const ACE = { id: 'p-ace', callsign: 'Ace' };
  const CAT = { id: 'p-cat', callsign: 'Cat' };
  const CHANNELS = [{ adapter: 'bench', seat: 'seat-1', freq_mhz: 5658 }];

  async function openAs(
    view: EventView,
    who: { id: string; callsign: string } | undefined,
    channels = CHANNELS
  ) {
    const { live } = await openLive(view, { listPilots: vi.fn(async () => PILOTS) });
    const buzz = vi.fn();
    const rendered = render(SpectatorView, { live, pilot: who, channels, buzz });
    return { live, buzz, rendered };
  }

  /** Three heats queued, none on. Ace is in the first and the third. */
  function queued(): EventView {
    return eventView({
      phases: [
        heat('h1', 'Heat 1', ['p-ace', 'p-bee']),
        heat('h2', 'Heat 2', ['p-bee', 'p-cat']),
        heat('h3', 'Heat 3', ['p-ace'])
      ],
      queue: ['h1', 'h2', 'h3']
    });
  }

  it('a spectator has no Me tab and no alert', async () => {
    await openAs(queued(), undefined);
    expect(screen.queryByRole('tab', { name: /^Me/ })).toBeNull();
    expect(screen.queryByTestId('pilot-alert')).toBeNull();
  });

  it('a pilot lands on Me, with their heats in order and how far off each is', async () => {
    await openAs(queued(), ACE);
    expect(tab(/^Me/)).toHaveAttribute('aria-selected', 'true');
    expect(tab(/^Me/)).toHaveTextContent('2');
    const mine = within(screen.getByRole('list', { name: 'My heats' })).getAllByRole('listitem');
    expect(mine.map((item) => item.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'Next Heat 1 Seat 1 · R1 · 5658 MHz',
      '2 heats away Heat 3 Seat 1 · R1 · 5658 MHz'
    ]);
  });

  it('shows one tab at a time: Me has no race table and no results under it', async () => {
    const view = running();
    view.phases[0] = { ...view.phases[0], ranking: [placed('p-ace', 1, { laps: 1 })] };
    await openAs(view, ACE);
    expect(tab(/^Me/)).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('region', { name: 'Me' })).toBeVisible();
    expect(screen.queryByRole('region', { name: 'Results' })).toBeNull();
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByRole('combobox')).toBeNull();
    // And each other tab shows only itself.
    for (const [name, region] of [
      ['On now', 'On now'],
      [/Up next/, 'Up next'],
      ['Results', 'Results']
    ] as const) {
      await fireEvent.click(tab(name));
      expect(screen.getAllByRole('region').map((el) => el.getAttribute('aria-label'))).toEqual([
        region
      ]);
    }
  });

  it('shows no channel for a seat the timer has not spoken for', async () => {
    await openAs(queued(), ACE, []);
    expect(screen.queryByTestId('my-channel')).toBeNull();
    expect(screen.getByRole('list', { name: 'My heats' })).toHaveTextContent('Seat 1');
  });

  it('tells a pilot they are next, on every tab, and buzzes once', async () => {
    const { buzz } = await openAs(queued(), ACE);
    const alert = screen.getByTestId('pilot-alert');
    expect(alert).toHaveTextContent('You are next');
    expect(alert).toHaveTextContent('Heat 1 · Seat 1 · R1 · 5658 MHz');
    expect(alert).toHaveAttribute('data-nearness', 'next');
    for (const name of ['Last race', /Up next/, 'Results']) {
      await fireEvent.click(tab(name));
      expect(screen.getByTestId('pilot-alert')).toHaveTextContent('You are next');
    }
    await vi.advanceTimersByTimeAsync(3000);
    expect(buzz).toHaveBeenCalledOnce();
  });

  it('tells a pilot how many heats away they are, and nothing when that is far off', async () => {
    await openAs(queued(), CAT);
    expect(screen.getByTestId('pilot-alert')).toHaveTextContent('You are 1 heat away');
    expect(screen.getByTestId('pilot-alert')).toHaveAttribute('data-nearness', 'soon');
    cleanup();
    const far = queued();
    far.phases.push(heat('h4', 'Heat 4', ['p-bee']), heat('h5', 'Heat 5', ['p-cat']));
    far.phases[1] = heat('h2', 'Heat 2', ['p-bee']);
    far.queue = ['h1', 'h2', 'h3', 'h4', 'h5'];
    const { buzz } = await openAs(far, CAT);
    expect(screen.queryByTestId('pilot-alert')).toBeNull();
    expect(buzz).not.toHaveBeenCalled();
    // The heat is still listed for them.
    expect(screen.getByRole('list', { name: 'My heats' })).toHaveTextContent('4 heats away');
  });

  it('buzzes again when the pilot gets closer, and says they are flying when their race is on', async () => {
    const view = queued();
    const { live, buzz } = await openAs(view, CAT);
    expect(buzz).toHaveBeenCalledTimes(1);
    // Heat 1 is flown and saved: Heat 2, with Cat in it, is next.
    live.view = {
      ...view,
      cursor: 2,
      queue: ['h2', 'h3'],
      races: [race('h1', 1, 'Saved')]
    };
    await waitFor(() =>
      expect(screen.getByTestId('pilot-alert')).toHaveTextContent('You are next')
    );
    expect(buzz).toHaveBeenCalledTimes(2);
    // Heat 2 starts.
    const on = race('h2', 1, 'Running', {
      started_at: 100 * S,
      seats: [raceSeat(0, 'p-bee'), raceSeat(1, 'p-cat')]
    });
    live.view = {
      ...live.view,
      cursor: 3,
      races: [race('h1', 1, 'Saved'), on],
      current_race: on.race
    };
    await waitFor(() =>
      expect(screen.getByTestId('pilot-alert')).toHaveTextContent('You are flying now')
    );
    expect(screen.getByTestId('pilot-alert')).toHaveAttribute('data-nearness', 'now');
    expect(buzz).toHaveBeenCalledTimes(3);
    // The same snapshot again buzzes nobody.
    live.view = { ...live.view, cursor: 4 };
    await vi.advanceTimersByTimeAsync(1000);
    expect(buzz).toHaveBeenCalledTimes(3);
  });

  it('a phone that cannot buzz still shows the alert', async () => {
    const { live } = await openLive(queued(), { listPilots: vi.fn(async () => PILOTS) });
    render(SpectatorView, {
      live,
      pilot: ACE,
      channels: [],
      buzz: () => {
        throw new Error('NotAllowedError');
      }
    });
    expect(screen.getByTestId('pilot-alert')).toHaveTextContent('You are next');
  });

  it('says so when the pilot is in no heat that is coming up', async () => {
    await openAs(queued(), { id: 'p-new', callsign: 'New' });
    expect(screen.getByTestId('no-heats')).toHaveTextContent(
      'You are not in a heat that is coming up.'
    );
    expect(screen.getByTestId('no-races')).toBeVisible();
    expect(tab(/^Me/).textContent?.trim()).toBe('Me');
  });

  it("lists the pilot's races lap by lap, newest first, with the best lap marked", async () => {
    const view = queued();
    view.races = [
      race('h1', 1, 'Saved', {
        seats: [
          raceSeat(0, 'p-ace', {
            finish: 2,
            laps: [lap(1, 7), lap(2, 6.5), lap(3, 6.9)],
            best_lap_micros: 6.5 * S,
            total_micros: 20.4 * S
          })
        ]
      }),
      race('h3', 1, 'Saved', {
        seats: [raceSeat(0, 'p-ace', { finish: 1, false_start: true, laps: [] })]
      })
    ];
    await openAs(view, ACE);
    const races = within(screen.getByRole('list', { name: 'My races' }));
    const names = races.getAllByText(/· race \d/).map((el) => el.textContent);
    expect(names).toEqual(['Heat 3 · race 1', 'Heat 1 · race 1']);
    const laps = within(screen.getByRole('list', { name: 'Laps of Heat 1 · race 1' }))
      .getAllByRole('listitem')
      .map((item) => item.textContent?.replace(/\s+/g, ' ').trim());
    expect(laps).toEqual(['1 7.000', '2 6.500', '3 6.900']);
    const best = screen
      .getByRole('list', { name: 'Laps of Heat 1 · race 1' })
      .querySelectorAll('li.best');
    expect(best).toHaveLength(1);
    expect(best[0]).toHaveTextContent('6.500');
    const first = screen.getByText('Heat 1 · race 1').closest('li')!;
    expect(first).toHaveTextContent('2nd');
    expect(first).toHaveTextContent('Best 6.500');
    expect(first).toHaveTextContent('Total 20.400');
    const second = screen.getByText('Heat 3 · race 1').closest('li')!;
    expect(second).toHaveTextContent('False start');
    expect(second).toHaveTextContent('No laps.');
  });

  it("marks the pilot's own row in the race, in what is coming up, and in the results", async () => {
    const view = running();
    view.phases[0] = {
      ...view.phases[0],
      ranking: [placed('p-bee', 1, { laps: 2 }), placed('p-ace', 2, { laps: 1 })]
    };
    view.phases.push(heat('h3', 'Qualifying Heat 3', ['p-bee', 'p-ace']));
    view.queue = ['h1', 'h2', 'h3'];
    await openAs(view, ACE);
    await fireEvent.click(tab('On now'));
    const table = screen.getByRole('table', { name: 'Pilots in this race' });
    expect(within(table).getByRole('row', { name: /Ace/ })).toHaveClass('me');
    expect(within(table).getByRole('row', { name: /Bee/ })).not.toHaveClass('me');
    await fireEvent.click(tab(/Up next/));
    const heat3 = within(screen.getByRole('region', { name: 'Up next' })).getByRole('listitem', {
      name: 'Qualifying Heat 3'
    });
    expect(within(heat3).getByText('Ace').closest('li')).toHaveClass('me');
    expect(within(heat3).getByText('Bee').closest('li')).not.toHaveClass('me');
    await fireEvent.click(tab('Results'));
    const results = screen.getByRole('table', { name: 'Results' });
    expect(within(results).getByRole('row', { name: /Ace/ })).toHaveClass('me');
  });

  it("shows each seat's channel in what is coming up, to everyone", async () => {
    await openAs(queued(), undefined);
    await fireEvent.click(tab(/Up next/));
    const heat1 = within(screen.getByRole('region', { name: 'Up next' })).getByRole('listitem', {
      name: 'Heat 1'
    });
    expect(within(heat1).getByText('Ace').closest('li')).toHaveTextContent('R1 · 5658 MHz');
    // Seat 2 has no channel known: nothing is made up for it.
    expect(within(heat1).getByText('Bee').closest('li')).not.toHaveTextContent('MHz');
  });

  it('moves to Me when a pilot signs in, and off it when they sign out', async () => {
    const { rendered } = await openAs(queued(), undefined);
    expect(tab('Last race')).toHaveAttribute('aria-selected', 'true');
    await rendered.rerender({ pilot: ACE });
    await waitFor(() => expect(tab(/^Me/)).toHaveAttribute('aria-selected', 'true'));
    await rendered.rerender({ pilot: undefined });
    await waitFor(() => expect(screen.queryByRole('tab', { name: /^Me/ })).toBeNull());
    expect(tab('Last race')).toHaveAttribute('aria-selected', 'true');
    expect(screen.queryByTestId('pilot-alert')).toBeNull();
  });

  it('still has nothing on it that changes anything', async () => {
    await openAs(queued(), ACE);
    for (const name of [/^Me/, 'Last race', /Up next/, 'Results']) {
      await fireEvent.click(tab(name));
      expect(screen.queryAllByRole('button')).toEqual([]);
      expect(screen.queryAllByRole('textbox')).toEqual([]);
    }
  });
});

describe('SpectatorView: the Sign up tab', () => {
  it('is there only when a phase has sign-up, between Up next and Results', async () => {
    await open(running());
    expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull();
    cleanup();
    const { live } = await openLive(signupEvent(), {
      listPilots: vi.fn(async () => SIGNUP_PILOTS)
    });
    render(SpectatorView, { live });
    expect(screen.getAllByRole('tab').map((el) => el.textContent?.trim())).toEqual([
      'Last race',
      'Up next',
      'Sign up',
      'Results'
    ]);
  });

  it('shows the sheet and nothing else when it is picked', async () => {
    const { live } = await openLive(signupEvent(), {
      listPilots: vi.fn(async () => SIGNUP_PILOTS)
    });
    render(SpectatorView, { live });
    await fireEvent.click(tab('Sign up'));
    expect(screen.getAllByRole('region').map((el) => el.getAttribute('aria-label'))).toEqual([
      'Sign up',
      'Sign-up for Qualifying'
    ]);
    // Looking only: nothing to press.
    expect(screen.queryAllByRole('button')).toEqual([]);
  });

  it('goes back to the race when sign-up is turned off under it', async () => {
    const view = signupEvent();
    const { live } = await openLive(view, { listPilots: vi.fn(async () => SIGNUP_PILOTS) });
    render(SpectatorView, { live });
    await fireEvent.click(tab('Sign up'));
    live.view = {
      ...view,
      cursor: 2,
      phases: view.phases.map((phase) => ({ ...phase, signup: undefined }))
    };
    await waitFor(() => expect(screen.queryByRole('tab', { name: 'Sign up' })).toBeNull());
    expect(tab('Last race')).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByRole('region', { name: 'Last race' })).toBeVisible();
  });

  it('gives a signed-in pilot the buttons, and passes on what they press', async () => {
    const { live } = await openLive(signupEvent(signupSheet()), {
      listPilots: vi.fn(async () => SIGNUP_PILOTS)
    });
    const signup = {
      take: vi.fn(async () => {}),
      next: vi.fn(async () => {}),
      leave: vi.fn(async () => {})
    };
    render(SpectatorView, {
      live,
      pilot: { id: 'p-bee', callsign: 'Bee' },
      channels: [],
      buzz: vi.fn(),
      signup
    });
    await fireEvent.click(tab('Sign up'));
    await fireEvent.click(screen.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 2' }));
    expect(signup.take).toHaveBeenCalledOnce();
  });

  it('lands a pilot on the sheet when told to, and on their own tab when there is no sheet', async () => {
    const { live } = await openLive(signupEvent(signupSheet()), {
      listPilots: vi.fn(async () => SIGNUP_PILOTS)
    });
    render(SpectatorView, {
      live,
      pilot: { id: 'p-bee', callsign: 'Bee' },
      channels: [],
      buzz: vi.fn(),
      landOn: 'signup'
    });
    await waitFor(() => expect(tab('Sign up')).toHaveAttribute('aria-selected', 'true'));
    cleanup();
    const plain = await openLive(running());
    render(SpectatorView, {
      live: plain.live,
      pilot: { id: 'p-ace', callsign: 'Ace' },
      channels: [],
      buzz: vi.fn(),
      landOn: 'signup'
    });
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: /^Me/ })).toHaveAttribute('aria-selected', 'true')
    );
  });
});
