/**
 * The replay overlay: one finished race, moved through its recording by whoever opened the
 * page, through the handle it puts on the window.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { EventView, LapView, Pilot } from '@gridfpv/types';
import ReplayOverlay from '../src/screens/ReplayOverlay.svelte';
import { eventView, heat, race, raceSeat } from './competition.js';

const S = 1_000_000;
const START = 1_000 * S;
const HEAT = 'track-2-a1b2';
const RACE = `${HEAT}#1`;
const EVENT = 'evt-77';
const PILOTS: Pilot[] = [{ id: 'p-1a', callsign: 'Ace', vtx_types: [] }];

const lap = (crossing: number, from: number, to: number): LapView => ({
  crossing,
  started_at: from * S,
  ended_at: to * S,
  micros: (to - from) * S,
  restart: false,
  timer_restarted: false
});

/** One pilot: hole shot 1 s in, laps ending 11 s and 20 s in. Filmed from 5 s before the start. */
const view = (): EventView =>
  eventView({
    phases: [heat(HEAT, 'Track 2', ['p-1a'])],
    races: [
      race(HEAT, 1, 'Saved', {
        tone_at: START,
        started_at: START,
        stopped_at: START + 25 * S,
        video: { file: 'a.mkv', first_frame_at: START - 5 * S, clean: true, overlay: 'Missing' },
        seats: [
          raceSeat(0, 'p-1a', {
            adapter: 'gate-1',
            start_at: 0,
            hole_shot: { crossing: 10, at: 1 * S },
            laps: [lap(11, 1, 11), lap(12, 11, 20)],
            finish: 1,
            best_lap_micros: 9 * S
          })
        ]
      })
    ]
  });

interface Handle {
  ready(): Promise<string>;
  seek(ms: number): Promise<string>;
}
const handle = () => (window as unknown as { gridfpvReplay?: Handle }).gridfpvReplay;

function open(over: { view?: EventView; race?: string; fail?: string } = {}) {
  const session = {
    getEventState: vi.fn(async () => {
      if (over.fail) throw new Error(over.fail);
      return over.view ?? view();
    }),
    listPilots: vi.fn(async () => PILOTS),
    listTimers: vi.fn(async () => []),
    listClasses: vi.fn(async () => [])
  };
  render(ReplayOverlay, { session, event: EVENT, race: over.race ?? RACE });
  return session;
}

const stat = (name: string) => document.querySelector(`[data-stat="${name}"]`)?.textContent;

afterEach(() => {
  cleanup();
});

describe('ReplayOverlay', () => {
  it('puts a handle on the window, and is ready once the race is drawn', async () => {
    const session = open();
    await waitFor(() => expect(handle()).toBeDefined());
    expect(await handle()!.ready()).toBe('');
    expect(session.getEventState).toHaveBeenCalledWith(EVENT);
    // The first frame is 5 s before the start: the countdown.
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('Starting');
    expect(screen.getByTestId('overlay-heat')).toHaveTextContent('Track 2');
    expect(screen.getByRole('timer')).toHaveTextContent('0:05.000');
  });

  it('shows the race as it stood at each moment of the recording', async () => {
    open();
    await waitFor(() => expect(handle()).toBeDefined());
    await handle()!.ready();
    // 4 s into the file: a second to go.
    await handle()!.seek(4000);
    expect(screen.getByRole('timer')).toHaveTextContent('0:01.000');
    // 17 s into the file is 12 s into the race: one lap flown.
    await handle()!.seek(17_000);
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('Live');
    expect(screen.getByRole('timer')).toHaveTextContent('0:12.000');
    expect(stat('laps')).toBe('1');
    expect(stat('last')).toBe('10.000');
    expect(screen.getByTestId('overlay-solo')).toHaveAccessibleName('Ace');
    // 31 s into the file: the race stopped 25 s in, and the clock stands there.
    await handle()!.seek(31_000);
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('Finished');
    expect(screen.getByRole('timer')).toHaveTextContent('0:25.000');
    expect(stat('laps')).toBe('2');
    // And back: any moment can be asked for in any order.
    await handle()!.seek(0);
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('Starting');
    expect(stat('laps')).toBe('0');
  });

  it('answers a seek with a key that changes only when the picture does', async () => {
    open();
    await waitFor(() => expect(handle()).toBeDefined());
    await handle()!.ready();
    const a = await handle()!.seek(17_000);
    const b = await handle()!.seek(17_033);
    expect(b).not.toBe(a);
    // After the stop nothing moves: the same key, so no new picture is needed.
    const c = await handle()!.seek(31_000);
    expect(await handle()!.seek(31_033)).toBe(c);
    expect(await handle()!.seek(17_000)).toBe(a);
  });

  it('reads the event once, however far it is moved', async () => {
    const session = open();
    await waitFor(() => expect(handle()).toBeDefined());
    await handle()!.ready();
    for (const ms of [0, 5000, 10_000, 20_000]) await handle()!.seek(ms);
    expect(session.getEventState).toHaveBeenCalledTimes(1);
  });

  it('says why when the race cannot be shown, and draws nothing', async () => {
    open({ race: `${HEAT}#9` });
    await waitFor(() => expect(handle()).toBeDefined());
    expect(await handle()!.ready()).toBe('This race is not in the event any more.');
    expect(screen.queryByRole('region', { name: 'Race overlay' })).toBeNull();
    cleanup();

    open({ view: eventView({ races: [race(HEAT, 1, 'Discarded')] }) });
    await waitFor(() => expect(handle()).toBeDefined());
    expect(await handle()!.ready()).toBe('This race never started.');
    cleanup();

    open({ fail: 'The Director did not answer.' });
    await waitFor(() => expect(handle()).toBeDefined());
    expect(await handle()!.ready()).toBe('The Director did not answer.');
  });

  it('puts no raw id on the page, and takes its handle away when it goes', async () => {
    open();
    await waitFor(() => expect(handle()).toBeDefined());
    await handle()!.ready();
    await handle()!.seek(17_000);
    const text = document.body.textContent ?? '';
    for (const raw of [HEAT, '#1', 'gate-1', 'seat-1', 'p-1a', EVENT]) {
      expect(text).not.toContain(raw);
    }
    cleanup();
    expect(handle()).toBeUndefined();
  });
});
