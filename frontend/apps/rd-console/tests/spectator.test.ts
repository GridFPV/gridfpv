/**
 * The spectator's helpers: which race to show, what is coming up, and when to ask for
 * names again.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Class, Pilot, TimerView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import {
  missingNames,
  nowShowing,
  publicSource,
  raceWord,
  upcomingHeats
} from '../src/lib/spectator.js';
import { eventView, heat, placed, race, raceSeat, timerView } from './competition.js';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const pilot = (id: string, callsign: string): Pilot => ({ id, callsign, vtx_types: [] });
const PILOTS = [pilot('p-ace', 'Ace'), pilot('p-bee', 'Bee'), pilot('p-cat', 'Cat')];
const klass = (id: string, name: string): Class => ({ id, name, source: 'Custom', builtin: false });

describe('the race on top', () => {
  it('is nothing when there is no snapshot or no race', () => {
    expect(nowShowing(undefined)).toBeUndefined();
    expect(nowShowing(eventView())).toBeUndefined();
  });

  it('is the race that is on, in every state it can be on in', () => {
    for (const state of ['Staging', 'Running', 'Stopped'] as const) {
      const on = race('h1', 2, state);
      const view = eventView({ races: [race('h1', 1, 'Saved'), on], current_race: on.race });
      expect(nowShowing(view), state).toEqual({ race: on, live: true });
    }
  });

  it('is the last saved race when none is on, however long ago it was', () => {
    const saved = race('h1', 1, 'Saved', { stopped_at: 1 });
    const view = eventView({ races: [saved], now: 99_000_000_000 });
    expect(nowShowing(view)).toEqual({ race: saved, live: false });
  });

  it('never shows a race that was thrown out', () => {
    const view = eventView({ races: [race('h1', 1, 'Discarded')] });
    expect(nowShowing(view)).toBeUndefined();
    // An earlier saved race of the same heat is shown in its place.
    const saved = race('h1', 1, 'Saved');
    const again = eventView({ races: [saved, race('h1', 2, 'Discarded')] });
    expect(nowShowing(again)).toEqual({ race: saved, live: false });
  });

  it('shows the heat raced last, not an older heat with a newer number', () => {
    const older = race('h1', 3, 'Saved');
    const newer = race('h2', 1, 'Saved');
    expect(nowShowing(eventView({ races: [older, newer] }))?.race).toBe(newer);
  });
});

describe('the heats coming up', () => {
  const names = (view: ReturnType<typeof eventView>) => buildNames(view.phases, PILOTS);

  it('is empty with no snapshot or an empty queue', () => {
    expect(upcomingHeats(undefined, buildNames([], []))).toEqual([]);
    const view = eventView({ phases: [heat('h1', 'Heat 1', ['p-ace'])] });
    expect(upcomingHeats(view, names(view))).toEqual([]);
  });

  it("follows the director's order, with each heat's pilots by seat", () => {
    const view = eventView({
      phases: [
        heat('h1', 'Heat 1', ['p-ace', 'p-bee']),
        heat('h2', 'Heat 2', ['p-cat', 'Open']),
        heat('h3', 'Heat 3', ['p-bee'])
      ],
      queue: ['h3', 'h1', 'h2']
    });
    const found = upcomingHeats(view, names(view));
    expect(found.map((entry) => entry.name)).toEqual(['Heat 3', 'Heat 1', 'Heat 2']);
    expect(found[2].seats.map((seat) => [seat.seatName, seat.who])).toEqual([
      ['Seat 1', 'Cat'],
      ['Seat 2', 'Open seat']
    ]);
  });

  it('leaves out the heat that is on now', () => {
    const on = race('h1', 1, 'Running');
    const view = eventView({
      phases: [heat('h1', 'Heat 1', ['p-ace']), heat('h2', 'Heat 2', ['p-bee'])],
      queue: ['h1', 'h2'],
      races: [on],
      current_race: on.race
    });
    expect(upcomingHeats(view, names(view)).map((entry) => entry.name)).toEqual(['Heat 2']);
  });

  it('lists a heat that waits on another one after the ready ones, with where its pilots come from', () => {
    const view = eventView({
      phases: [
        heat('h1', 'Semi 1', ['p-ace', 'p-bee']),
        heat('final', 'Final', [{ Waiting: { phase: 'h1', position: 1 } }, 'Nobody'], {
          waiting: true
        })
      ],
      queue: ['h1']
    });
    const found = upcomingHeats(view, names(view));
    expect(found.map((entry) => entry.name)).toEqual(['Semi 1', 'Final']);
    expect(found[1].seats.map((seat) => seat.who)).toEqual(['Winner of Semi 1', 'Empty']);
  });

  it('leaves out heats that are done or locked, and a queue entry that is not a phase', () => {
    const view = eventView({
      phases: [
        heat('h1', 'Heat 1', ['p-ace'], { waiting: true, complete: true }),
        heat('h2', 'Heat 2', ['p-bee'], { waiting: true, locked: true }),
        heat('h3', 'Heat 3', ['p-cat'])
      ],
      queue: ['gone', 'h3']
    });
    expect(upcomingHeats(view, names(view)).map((entry) => entry.name)).toEqual(['Heat 3']);
  });

  it('does not list a heat twice when it is both queued and waiting', () => {
    const view = eventView({
      phases: [heat('h1', 'Heat 1', ['p-ace'], { waiting: true })],
      queue: ['h1']
    });
    expect(upcomingHeats(view, names(view))).toHaveLength(1);
  });
});

describe('asking for names again', () => {
  const GATE: TimerView = timerView('gate', 'Gate', 2);

  it('is not needed with no snapshot, or when every name is known', () => {
    expect(missingNames(undefined, [], [], [])).toEqual({
      pilots: false,
      classes: false,
      timers: false
    });
    const view = eventView({
      rosters: [{ class: 'c-open', pilots: ['p-ace'], counted_phases: [], standings: [] }],
      phases: [heat('h1', 'Heat 1', ['p-ace'])],
      races: [race('h1', 1, 'Saved', { seats: [raceSeat(0, 'p-ace')] })]
    });
    expect(missingNames(view, PILOTS, [klass('c-open', 'Open')], [])).toEqual({
      pilots: false,
      classes: false,
      timers: false
    });
  });

  it('is needed for a pilot the page has no name for, wherever they appear', () => {
    const known = [pilot('p-ace', 'Ace')];
    const inRoster = eventView({
      rosters: [{ class: 'c', pilots: ['p-new'], counted_phases: [], standings: [] }]
    });
    const inField = eventView({ phases: [heat('h1', 'Heat 1', ['p-new'])] });
    const inRanking = eventView({
      phases: [heat('h1', 'Heat 1', [], { ranking: [placed('p-new', 1)] })]
    });
    const inRace = eventView({
      races: [race('h1', 1, 'Running', { seats: [raceSeat(0, 'p-new')] })]
    });
    for (const view of [inRoster, inField, inRanking, inRace]) {
      expect(missingNames(view, known, [klass('c', 'Open')], []).pilots).toBe(true);
    }
  });

  it('does not count an open or empty seat as a missing pilot', () => {
    const view = eventView({
      phases: [heat('h1', 'Heat 1', ['Open', 'Nobody'])],
      races: [race('h1', 1, 'Running', { seats: [raceSeat(0)] })]
    });
    expect(missingNames(view, [], [], []).pilots).toBe(false);
  });

  it('is needed for a class or a timer with no name, but never for the bench', () => {
    const view = eventView({
      rosters: [{ class: 'c-new', pilots: [], counted_phases: [], standings: [] }],
      phases: [
        heat('h1', 'Heat 1', ['p-ace']),
        {
          ...heat('h2', 'Heat 2', ['p-ace']),
          lineup: [{ adapter: 'gate', seat: '0', who: { Pilot: 'p-ace' } }]
        }
      ]
    });
    expect(missingNames(view, PILOTS, [], [])).toEqual({
      pilots: false,
      classes: true,
      timers: true
    });
    expect(missingNames(view, PILOTS, [klass('c-new', 'New')], [GATE])).toEqual({
      pilots: false,
      classes: false,
      timers: false
    });
    // A heat on the bench only.
    const bench = eventView({ phases: [heat('h1', 'Heat 1', ['p-ace'])] });
    expect(missingNames(bench, PILOTS, [], []).timers).toBe(false);
  });
});

describe('the word for a race', () => {
  it('says what each state means to someone watching', () => {
    const word = (state: Parameters<typeof race>[2]) => raceWord(race('h', 1, state));
    expect(word('Staging')).toBe('Starting');
    expect(word('Running')).toBe('Racing');
    expect(word('Stopped')).toBe('Finished');
    expect(word('Saved')).toBe('Result');
    expect(word('Discarded')).toBe('Thrown out');
  });
});

describe("the visitor's source", () => {
  function answering(body: unknown) {
    const asked: { url: string; method: string; auth?: string }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        asked.push({
          url: String(input),
          method: init?.method ?? 'GET',
          auth: (init?.headers as Record<string, string> | undefined)?.Authorization
        });
        return {
          ok: true,
          status: 200,
          json: async () => body,
          text: async () => JSON.stringify(body)
        } as Response;
      })
    );
    return asked;
  }

  it('reads only the public routes, and never names an event or sends a token', async () => {
    const asked = answering([]);
    const source = publicSource('http://grid.local:8081/');
    await source.getEvent();
    await source.getEventState('some-event-id');
    await source.listPilots();
    await source.listClasses();
    await source.listTimers();
    expect(asked.map((call) => call.url)).toEqual([
      'http://grid.local:8081/public/event',
      'http://grid.local:8081/public/event/state',
      'http://grid.local:8081/public/event/pilots',
      'http://grid.local:8081/public/event/classes',
      'http://grid.local:8081/public/event/timers'
    ]);
    for (const call of asked) {
      expect(call.method).toBe('GET');
      expect(call.auth).toBeUndefined();
      expect(call.url).not.toContain('some-event-id');
    }
  });

  it('listens for changes on the public feed, whatever event id it is handed', () => {
    const opened: string[] = [];
    class FakeSource {
      constructor(url: string) {
        opened.push(url);
      }
      addEventListener() {}
      close() {}
    }
    vi.stubGlobal('EventSource', FakeSource);
    const stop = publicSource('http://grid.local:8081').subscribeChanges('any-id', () => {});
    expect(opened).toEqual(['http://grid.local:8081/public/event/changes']);
    stop();
  });

  it('gives no saved formats and asks the Director for none', async () => {
    const asked = answering([]);
    expect(await publicSource('http://grid.local:8081').listFormats()).toEqual([]);
    expect(asked).toEqual([]);
  });

  it('refuses every write and every director-only read without sending anything', async () => {
    const asked = answering({});
    const source = publicSource('http://grid.local:8081');
    const tries = [
      () => source.sendCommand('e', { StartRace: { heat: 'h1' } }),
      () => source.createBracket('e', {} as never),
      () => source.postPass('e', { adapter: 'bench', seat: 'seat-1' } as never),
      () => source.getRaceSignal('e', 'h1#1'),
      () => source.getVideoSettings(),
      () => source.getVideoStatus()
    ];
    for (const attempt of tries) {
      await expect(attempt()).rejects.toThrow('Only the director can do this.');
    }
    expect(asked).toEqual([]);
  });
});
