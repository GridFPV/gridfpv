/**
 * The race overlay, rendered against a stand-in session: it follows the Director's active
 * event, shows the current race with a clock on the Director's clock, uses the long form for
 * a heat of one, goes quiet when nothing is on, never touches the start tone, and puts no
 * raw id on the page.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { EventMeta, EventView, Format, LapView, OverlayLook, Pilot } from '@gridfpv/types';
import RaceOverlay from '../src/screens/RaceOverlay.svelte';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import type { Session } from '../src/lib/session.svelte.js';
import { eventView, heat, race, raceSeat } from './competition.js';
import { OVERLAY_LOOK, VIDEO_IDLE, VIDEO_OFF } from './support.js';

// The overlay must never make a sound. Stand in for the tone module so any use of it shows.
const tone = vi.hoisted(() => ({
  constructed: vi.fn(),
  update: vi.fn(),
  unlockAudio: vi.fn()
}));
vi.mock('../src/lib/startTone.js', () => ({
  StartCue: class {
    constructor() {
      tone.constructed();
    }
    update = tone.update;
    dispose = vi.fn();
  },
  unlockAudio: tone.unlockAudio
}));

const EVENT: EventMeta = { id: 'evt-77', name: 'Club Night', created_at: 0, persistent: true };
const OTHER: EventMeta = { id: 'evt-88', name: 'Sunday Whoops', created_at: 0, persistent: true };
const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];
const HEAT = 'quali-x9z1';
const S = 1_000_000;
const BEST_3: Format = {
  name: 'Best 3 consecutive',
  race: { end: 'Open', staging_micros: 5 * S, first_lap: 'HoleShot' },
  completion: 'DirectorDecides',
  ranking: { sort: [{ BestConsecutive: 3 }] }
};
const lap = (crossing: number, seconds: number): LapView => ({
  crossing,
  started_at: 0,
  ended_at: 0,
  micros: seconds * S,
  restart: false,
  timer_restarted: false
});
/** The Director's clock right now, as a snapshot carries it. */
const directorNow = (): number => Date.now() * 1000;

type OverlaySession = LiveSession &
  Pick<Session, 'readActiveEvent' | 'getOverlayLook' | 'overlayLogoUrl'>;

/**
 * Open the overlay. `views` maps an event id to the snapshot it answers with; the returned
 * `show` swaps one and tells the overlay the event changed, as the change feed would.
 */
function open(
  views: Record<string, EventView>,
  opts: { active?: EventMeta | null; theme?: 'dark' | 'light'; look?: OverlayLook } = {}
) {
  const feeds = new Map<string, (cursor: number) => void>();
  let active: EventMeta | null = opts.active === undefined ? EVENT : opts.active;
  const audio = vi.fn();
  vi.stubGlobal('AudioContext', audio);
  const session: OverlaySession = {
    readActiveEvent: vi.fn(async () => active),
    getOverlayLook: vi.fn(async () => opts.look ?? OVERLAY_LOOK),
    overlayLogoUrl: (side: 'left' | 'right') => `http://d.local/overlay/logo/${side}`,
    getEventState: vi.fn(async (id: string) => views[id] ?? eventView()),
    subscribeChanges: vi.fn((id: string, heard: (cursor: number) => void) => {
      feeds.set(id, heard);
      return () => feeds.delete(id);
    }),
    sendCommand: vi.fn(async () => ({ cursor: 1 })),
    createBracket: vi.fn(async () => ({ cursor: 1 })),
    createMains: vi.fn(async () => ({ cursor: 1 })),
    createRoundRobin: vi.fn(async () => ({ cursor: 1 })),
    postPass: vi.fn(async () => ({ cursor: 1 })),
    getVideoSettings: vi.fn(async () => VIDEO_OFF),
    getVideoStatus: vi.fn(async () => VIDEO_IDLE),
    listPilots: vi.fn(async () => PILOTS),
    listFormats: vi.fn(async () => []),
    listTimers: vi.fn(async () => []),
    listClasses: vi.fn(async () => []),
    getRaceSignal: vi.fn(async (_id: string, raceId: string) => ({ race: raceId, seats: [] }))
  };
  render(RaceOverlay, { session, theme: opts.theme, activePollMs: 20 });
  return {
    session,
    audio,
    show(id: string, view: EventView) {
      views[id] = view;
      feeds.get(id)?.(view.cursor);
    },
    setActive(event: EventMeta | null) {
      active = event;
    },
    listening: () => [...feeds.keys()]
  };
}

/** The clock, once a race is on the panel. The panel is there before that, waiting. */
async function raceClock(): Promise<HTMLElement> {
  await screen.findByTestId('overlay-heat');
  return screen.getByRole('timer');
}

const quali = (over = {}) => heat(HEAT, 'Qualifying Heat 1', ['p-1a', 'p-2b'], over);
const row = (name: string) => screen.getByRole('row', { name });
const stat = (el: HTMLElement, name: string) => el.querySelector(`[data-stat="${name}"]`);

/** Every raw handle in these fixtures. None may be on the page. */
function expectNoRawIds() {
  const text = document.body.textContent ?? '';
  for (const raw of [HEAT, '#1', 'bench', 'seat-1', 'p-1a', 'p-2b', EVENT.id, OTHER.id]) {
    expect(text).not.toContain(raw);
  }
}

afterEach(() => {
  vi.unstubAllGlobals();
  vi.clearAllMocks();
});

describe('RaceOverlay', () => {
  it('while staging it shows the heat, the lineup and a countdown to the tone', async () => {
    const now = directorNow();
    const staging = race(HEAT, 1, 'Staging', {
      tone_at: now + 5 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    open({
      [EVENT.id]: eventView({
        now,
        phases: [quali()],
        races: [staging],
        current_race: staging.race
      })
    });
    // The title is the run's name: the event, the heat, and which race of the heat.
    expect(await screen.findByTestId('overlay-heat')).toHaveTextContent(
      'Club Night Qualifying Heat 1 race 1'
    );
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('STAGING');
    const clock = screen.getByRole('timer');
    expect(clock).toHaveAttribute('data-phase', 'staging');
    expect(clock.textContent).toMatch(/^0:0[0-5]\.\d$/);
    // It is really counting: the reading moves.
    const first = clock.textContent;
    await waitFor(() => expect(clock.textContent).not.toBe(first));
    await waitFor(() => expect(row('Ace')).toBeVisible());
    expect(stat(row('Ace'), 'laps')).toHaveTextContent('0');
    expect(stat(row('Bee'), 'last')).toHaveTextContent('—');
    expectNoRawIds();
  });

  it('while running it counts up, and the rows follow each new snapshot', async () => {
    const now = directorNow();
    const running = (aceLaps: LapView[], cursor: number) => {
      const current = race(HEAT, 1, 'Running', {
        started_at: now - 30 * S,
        end: { FirstToLaps: 3 },
        seats: [
          raceSeat(0, 'p-1a', {
            finish: 1,
            laps: aceLaps,
            best_lap_micros:
              aceLaps.length > 0 ? Math.min(...aceLaps.map((l) => l.micros)) : undefined
          }),
          raceSeat(1, 'p-2b', { finish: 2 })
        ]
      });
      return eventView({
        cursor,
        now,
        phases: [quali()],
        races: [current],
        current_race: current.race
      });
    };
    const { show } = open({ [EVENT.id]: running([lap(3, 9.5)], 5) });
    await waitFor(() => expect(row('Ace')).toBeVisible());
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('LIVE');
    expect(screen.getByRole('timer')).toHaveAttribute('data-phase', 'racing');
    expect(screen.getByRole('timer').textContent).toMatch(/^0:3\d\.\d$/);
    // A race with no time limit counts up, so there is no second clock under it.
    expect(screen.queryByTestId('overlay-elapsed')).toBeNull();
    expect(stat(row('Ace'), 'place')).toHaveTextContent('1');
    expect(stat(row('Ace'), 'laps')).toHaveTextContent('1');
    expect(stat(row('Ace'), 'last')).toHaveTextContent('0:09.500');
    expect(stat(row('Bee'), 'laps')).toHaveTextContent('0');
    // Ace has a place and Bee has none yet, so Ace is first and Bee takes the next number.
    expect(stat(row('Bee'), 'place')).toHaveTextContent('2');
    const rows = within(screen.getByRole('table', { name: 'Pilots in this heat' })).getAllByRole(
      'row'
    );
    expect(rows.slice(1).map((r) => r.getAttribute('aria-label'))).toEqual(['Ace', 'Bee']);

    show(EVENT.id, running([lap(3, 9.5), lap(5, 8.25)], 6));
    await waitFor(() => expect(stat(row('Ace'), 'laps')).toHaveTextContent('2'));
    expect(stat(row('Ace'), 'last')).toHaveTextContent('0:08.250');
    expect(stat(row('Ace'), 'best')).toHaveTextContent('0:08.250');
    expectNoRawIds();
  });

  it('a timed race shows the time left', async () => {
    const now = directorNow();
    const timed = race(HEAT, 1, 'Running', {
      started_at: now - 20 * S,
      end: { Timed: { micros: 120 * S, grace_micros: 10 * S } },
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    open({
      [EVENT.id]: eventView({ now, phases: [quali()], races: [timed], current_race: timed.race })
    });
    const clock = await raceClock();
    expect(clock.textContent).toMatch(/^1:(39|40)\.\d$/);
    // Under the countdown, small: the time since the start.
    expect(screen.getByTestId('overlay-elapsed').textContent).toMatch(/^0:(19|20)\.\d$/);
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('LIVE');
  });

  it('a timed race past its time reads as time up, in the red', async () => {
    const now = directorNow();
    const over = race(HEAT, 1, 'Running', {
      started_at: now - 125 * S,
      end: { Timed: { micros: 120 * S, grace_micros: 10 * S } },
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    open({
      [EVENT.id]: eventView({ now, phases: [quali()], races: [over], current_race: over.race })
    });
    const clock = await raceClock();
    expect(clock).toHaveAttribute('data-phase', 'overtime');
    expect(clock.textContent).toMatch(/^-0:0[45]\.\d$/);
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('TIME UP');
  });

  it('a stopped race stands still on its length', async () => {
    const now = directorNow();
    const stopped = race(HEAT, 1, 'Stopped', {
      started_at: now - 90 * S,
      stopped_at: now - 28.5 * S,
      seats: [raceSeat(0, 'p-1a', { finish: 1 }), raceSeat(1, 'p-2b', { finish: 2 })]
    });
    open({
      [EVENT.id]: eventView({
        now,
        phases: [quali()],
        races: [stopped],
        current_race: stopped.race
      })
    });
    const clock = await raceClock();
    expect(clock).toHaveTextContent('1:01.5');
    expect(clock).toHaveAttribute('data-phase', 'done');
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('FINISHED');
  });

  it('a heat ranked on best 3 consecutive shows the best 3-lap run in place of the best lap', async () => {
    const now = directorNow();
    const running = race(HEAT, 1, 'Running', {
      started_at: now - 60 * S,
      seats: [
        raceSeat(0, 'p-1a', {
          laps: [lap(1, 12), lap(2, 9), lap(3, 8), lap(4, 8.5)],
          best_lap_micros: 8 * S
        }),
        raceSeat(1, 'p-2b', { laps: [lap(5, 10)], best_lap_micros: 10 * S })
      ]
    });
    open({
      [EVENT.id]: eventView({
        now,
        phases: [quali({ format: BEST_3 })],
        races: [running],
        current_race: running.race
      })
    });
    await waitFor(() => expect(row('Ace')).toBeVisible());
    expect(screen.getByRole('columnheader', { name: 'Best 3' })).toBeVisible();
    expect(stat(row('Ace'), 'best-run')).toHaveTextContent('25.500');
    // Too few laps for a run: a dash, not a best lap passed off as one.
    expect(stat(row('Bee'), 'best-run')).toHaveTextContent('—');
    expect(stat(row('Ace'), 'best')).toBeNull();
  });

  it('a heat of one uses the long form: big numbers and the last few laps', async () => {
    const now = directorNow();
    const laps = [12, 11, 10, 9, 8, 9.5, 10.5, 11.5].map((seconds, index) =>
      lap(index + 1, seconds)
    );
    const earlier = race(HEAT, 1, 'Saved', {
      started_at: now - 900 * S,
      stopped_at: now - 800 * S,
      seats: [raceSeat(0, 'p-1a', { laps: [lap(1, 8), lap(2, 8), lap(3, 8)] })]
    });
    const running = race(HEAT, 2, 'Running', {
      started_at: now - 100 * S,
      seats: [raceSeat(0, 'p-1a', { laps, best_lap_micros: 8 * S })]
    });
    open({
      [EVENT.id]: eventView({
        now,
        phases: [heat(HEAT, 'Ace Time Trial', ['p-1a'], { format: BEST_3 })],
        races: [earlier, running],
        current_race: running.race
      })
    });
    const solo = await screen.findByTestId('overlay-solo');
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.getByTestId('overlay-heat')).toHaveTextContent(
      'Club Night Ace Time Trial race 2'
    );
    expect(solo).toHaveTextContent('Ace');
    const panel = screen.getByTestId('overlay-panel');
    expect(stat(panel, 'laps')).toHaveTextContent('8');
    // 9 + 8 + 9.5: the best three in a row of this race.
    expect(panel).toHaveTextContent('Best 3 this race');
    expect(stat(panel, 'best-run')).toHaveTextContent('0:26.500');
    // The last six laps, numbered as flown, with the best run lit.
    const listed = within(screen.getByRole('list', { name: 'Recent laps' })).getAllByRole(
      'listitem'
    );
    expect(listed.map((li) => li.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'L3 0:10.000',
      'L4 0:09.000',
      'L5 0:08.000',
      'L6 0:09.500',
      'L7 0:10.500',
      'L8 0:11.500'
    ]);
    expect(listed.map((li) => li.classList.contains('lit'))).toEqual([
      false,
      true,
      true,
      true,
      false,
      false
    ]);
    expectNoRawIds();
  });

  it('an open seat shows its seat name', async () => {
    const now = directorNow();
    const running = race(HEAT, 1, 'Running', {
      started_at: now - 5 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1)]
    });
    open({
      [EVENT.id]: eventView({
        now,
        phases: [heat(HEAT, 'Open Practice', ['p-1a', 'Open'])],
        races: [running],
        current_race: running.race
      })
    });
    await waitFor(() => expect(row('Seat 2')).toBeVisible());
    expectNoRawIds();
  });

  it('keeps the saved result up when the race is saved while it watches', async () => {
    const now = directorNow();
    const seats = [raceSeat(0, 'p-1a', { finish: 1 }), raceSeat(1, 'p-2b', { finish: 2 })];
    // Stopped long ago, so only having seen it decided keeps the result up.
    const times = { started_at: now - 900 * S, stopped_at: now - 800 * S };
    const stopped = race(HEAT, 1, 'Stopped', { ...times, seats });
    const { show } = open({
      [EVENT.id]: eventView({
        cursor: 5,
        now,
        phases: [quali()],
        races: [stopped],
        current_race: stopped.race
      })
    });
    await raceClock();
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('FINISHED');
    const saved = race(HEAT, 1, 'Saved', { ...times, seats });
    show(EVENT.id, eventView({ cursor: 6, now, phases: [quali()], races: [saved] }));
    await waitFor(() =>
      expect(screen.getByRole('region', { name: 'Race overlay' })).toHaveAttribute(
        'data-state',
        'Saved'
      )
    );
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('FINISHED');
    expect(row('Ace')).toBeVisible();
  });

  it('waits with no race on: the event name, a clock at zero and READY', async () => {
    const now = directorNow();
    // Saved long ago, and this overlay never saw it happen.
    const old = race(HEAT, 1, 'Saved', {
      started_at: now - 900 * S,
      stopped_at: now - 800 * S,
      seats: [raceSeat(0, 'p-1a', { finish: 1 })]
    });
    open({ [EVENT.id]: eventView({ now, phases: [quali()], races: [old] }) });
    expect(await screen.findByTestId('overlay-idle')).toHaveTextContent('Club Night');
    expect(screen.getByRole('timer')).toHaveTextContent('0:00.0');
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('READY');
    expectNoRawIds();
  });

  it('with no active event it says it is waiting, and reads no event', async () => {
    const { session } = open({}, { active: null });
    await waitFor(() => expect(session.readActiveEvent).toHaveBeenCalled());
    expect(screen.getByTestId('overlay-idle')).toHaveTextContent('waiting for a race…');
    expect(session.getEventState).not.toHaveBeenCalled();
  });

  it('sits where the look says, with its logos, and follows a change to the look', async () => {
    const now = directorNow();
    const running = race(HEAT, 1, 'Running', {
      started_at: now - 5 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    open(
      {
        [EVENT.id]: eventView({
          now,
          phases: [quali()],
          races: [running],
          current_race: running.race
        })
      },
      {
        look: {
          placement: { corner: 'BottomRight', x: 40, y: 10, scale: 150, opacity: 30 },
          logo_left: true,
          logo_right: false
        }
      }
    );
    const panel = screen.getByTestId('overlay-panel');
    await waitFor(() => expect(panel).toHaveAttribute('data-corner', 'BottomRight'));
    expect(panel.getAttribute('style')).toContain('right: 40px');
    expect(panel.getAttribute('style')).toContain('scale(1.5)');
    expect(panel.getAttribute('style')).toContain('--panel-alpha: 0.3');
    // The left logo takes the place of the GridFPV name; there is no right one.
    const logos = panel.querySelectorAll('img');
    expect(Array.from(logos).map((img) => img.getAttribute('src'))).toEqual([
      'http://d.local/overlay/logo/left'
    ]);
    expect(panel).not.toHaveTextContent('GridFPV');
  });

  it('follows the Director to another event, and stops listening to the first', async () => {
    const now = directorNow();
    const first = race(HEAT, 1, 'Running', {
      started_at: now - 5 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const second = race('finals-k2', 1, 'Running', {
      started_at: now - 5 * S,
      seats: [raceSeat(0, 'p-2b'), raceSeat(1, 'p-1a')]
    });
    const { setActive, listening, session } = open({
      [EVENT.id]: eventView({
        now,
        phases: [quali()],
        races: [first],
        current_race: first.race
      }),
      [OTHER.id]: eventView({
        now,
        phases: [heat('finals-k2', 'Whoop Final', ['p-2b', 'p-1a'])],
        races: [second],
        current_race: second.race
      })
    });
    expect(await screen.findByTestId('overlay-heat')).toHaveTextContent('Qualifying Heat 1');
    expect(listening()).toEqual([EVENT.id]);

    setActive(OTHER);
    await waitFor(() =>
      expect(screen.getByTestId('overlay-heat')).toHaveTextContent('Whoop Final')
    );
    expect(listening()).toEqual([OTHER.id]);
    expect(session.getEventState).toHaveBeenCalledWith(OTHER.id);
    expectNoRawIds();
    expect(document.body.textContent).not.toContain('finals-k2');
  });

  it('keeps what it shows when the Director does not answer the active-event read', async () => {
    const now = directorNow();
    const running = race(HEAT, 1, 'Running', {
      started_at: now - 5 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const { session } = open({
      [EVENT.id]: eventView({
        now,
        phases: [quali()],
        races: [running],
        current_race: running.race
      })
    });
    expect(await screen.findByTestId('overlay-heat')).toHaveTextContent('Qualifying Heat 1');
    const read = vi.mocked(session.readActiveEvent);
    const before = read.mock.calls.length;
    read.mockRejectedValue(new Error('fetch failed'));
    await waitFor(() => expect(read.mock.calls.length).toBeGreaterThan(before + 1));
    expect(screen.getByTestId('overlay-heat')).toHaveTextContent('Qualifying Heat 1');
  });

  it('only reads: no command, no pass, no sound, and it reads the timers once', async () => {
    const now = directorNow();
    const staging = race(HEAT, 1, 'Staging', {
      tone_at: now + 1 * S,
      seats: [raceSeat(0, 'p-1a'), raceSeat(1, 'p-2b')]
    });
    const { session, audio } = open({
      [EVENT.id]: eventView({
        now,
        phases: [quali()],
        races: [staging],
        current_race: staging.race
      })
    });
    await screen.findByTestId('overlay-heat');
    // Long enough for several polls of the active event.
    await waitFor(() =>
      expect(vi.mocked(session.readActiveEvent).mock.calls.length).toBeGreaterThan(3)
    );
    expect(tone.constructed).not.toHaveBeenCalled();
    expect(tone.update).not.toHaveBeenCalled();
    expect(tone.unlockAudio).not.toHaveBeenCalled();
    expect(audio).not.toHaveBeenCalled();
    expect(session.sendCommand).not.toHaveBeenCalled();
    expect(session.postPass).not.toHaveBeenCalled();
    expect(session.listTimers).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('button')).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('carries the contrast it was asked for', async () => {
    open({}, { theme: 'light' });
    expect(screen.getByRole('region', { name: 'Race overlay' })).toHaveAttribute(
      'data-theme',
      'light'
    );
  });
});
