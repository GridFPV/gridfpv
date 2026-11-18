/**
 * The spectator's helpers: which race to show, what is coming up, and when to ask for
 * names again.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { Class, Pilot, TimerView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import {
  missingNames,
  currentRaceView,
  publicSource,
  raceWord,
  upcomingHeats
} from '../src/lib/spectator.js';
import { eventView, group, heat, placed, race, raceSeat, timerView } from './competition.js';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

const pilot = (id: string, callsign: string): Pilot => ({ id, callsign, vtx_types: [] });
const PILOTS = [pilot('p-ace', 'Ace'), pilot('p-bee', 'Bee'), pilot('p-cat', 'Cat')];
const klass = (id: string, name: string): Class => ({ id, name, source: 'Custom', builtin: false });

describe('the current race', () => {
  const of = (view: ReturnType<typeof eventView> | undefined) =>
    currentRaceView(view, buildNames(view?.phases ?? [], PILOTS));
  const threeHeats = (over: Parameters<typeof eventView>[0] = {}) =>
    eventView({
      phases: [
        heat('h1', 'Heat 1', ['p-ace']),
        heat('h2', 'Heat 2', ['p-bee']),
        heat('h3', 'Heat 3', ['p-cat'])
      ],
      queue: ['h1', 'h2', 'h3'],
      ...over
    });

  it('is nothing with no snapshot, or with nothing raced and nothing lined up', () => {
    expect(of(undefined)).toBeUndefined();
    expect(of(eventView())).toBeUndefined();
  });

  it('is the race that is on, in every state it can be on in, whatever else is so', () => {
    for (const state of ['Staging', 'Running', 'Stopped'] as const) {
      const on = race('h1', 2, state);
      const view = threeHeats({
        races: [race('h1', 1, 'Saved'), on],
        current_race: on.race,
        on_timer: 'h1'
      });
      expect(of(view), state).toEqual({ kind: 'race', race: on });
    }
  });

  it('is the heat on the timer, with who is in it, before it has been flown', () => {
    // The director put heat 2 on the timer, out of the run order.
    const found = of(threeHeats({ on_timer: 'h2' }));
    expect(found?.kind).toBe('next');
    if (found?.kind !== 'next') return;
    expect(found.heat.id).toBe('h2');
    expect(found.heat.name).toBe('Heat 2');
    expect(found.heat.seats.map((seat) => [seat.seatName, seat.who])).toEqual([['Seat 1', 'Bee']]);
  });

  it('is the result of the heat on the timer once it is flown, however long ago', () => {
    const saved = race('h1', 1, 'Saved', { stopped_at: 1 });
    const view = threeHeats({ races: [saved], on_timer: 'h1', queue: ['h2', 'h3'], now: 9e12 });
    expect(of(view)).toEqual({ kind: 'result', race: saved });
  });

  it('moves to the next heat only when the director puts it on the timer', () => {
    const saved = race('h1', 1, 'Saved', { stopped_at: 1 });
    const flown = threeHeats({ races: [saved], queue: ['h2', 'h3'] });
    expect(of({ ...flown, on_timer: 'h1' })?.kind).toBe('result');
    const moved = of({ ...flown, on_timer: 'h2' });
    expect(moved?.kind === 'next' && moved.heat.id).toBe('h2');
  });

  it('for a heat raced more than once, shows its latest saved race', () => {
    const first = race('h1', 1, 'Saved');
    const thrown = race('h1', 2, 'Discarded');
    const third = race('h1', 3, 'Saved');
    const view = threeHeats({ races: [first, thrown, third], on_timer: 'h1' });
    expect(of(view)).toEqual({ kind: 'result', race: third });
    // Only a thrown-out race behind it: the heat is still to be flown.
    const again = of(threeHeats({ races: [race('h1', 1, 'Discarded')], on_timer: 'h1' }));
    expect(again?.kind === 'next' && again.heat.id).toBe('h1');
  });

  it('shows the heat on the timer, not a newer result from another heat', () => {
    const other = race('h3', 1, 'Saved');
    const found = of(threeHeats({ races: [other], on_timer: 'h2', queue: ['h1', 'h2'] }));
    expect(found?.kind === 'next' && found.heat.id).toBe('h2');
  });

  describe('before the director has put anything on the timer', () => {
    it('is the heat they would run first', () => {
      const found = of(threeHeats());
      expect(found?.kind === 'next' && found.heat.id).toBe('h1');
    });

    it('with nothing lined up, is the last result, and never a race that was thrown out', () => {
      const saved = race('h1', 1, 'Saved');
      expect(of(eventView({ races: [saved, race('h1', 2, 'Discarded')] }))).toEqual({
        kind: 'result',
        race: saved
      });
      expect(of(eventView({ races: [race('h1', 1, 'Discarded')] }))).toBeUndefined();
    });
  });

  it('a heat on the timer that is no longer in the event is not shown', () => {
    const found = of(threeHeats({ on_timer: 'gone' }));
    expect(found?.kind === 'next' && found.heat.id).toBe('h1');
    // Nor is a phase that holds heats, should one ever be named.
    const view = eventView({ phases: [group('g', 'Group')], on_timer: 'g' });
    expect(of(view)).toBeUndefined();
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

  it('says which phase a heat is in when its name does not', () => {
    const view = eventView({
      phases: [
        group('sport', 'Sport'),
        heat('sport/a', 'A-Main', ['p-ace'], { parent: 'sport' }),
        group('whoop', 'Whoop'),
        heat('whoop/a', 'A-Main', ['p-bee'], { parent: 'whoop' }),
        heat('whoop/1', 'Whoop Heat 1', ['p-cat'], { parent: 'whoop' })
      ],
      queue: ['sport/a', 'whoop/a', 'whoop/1']
    });
    expect(upcomingHeats(view, names(view)).map((entry) => [entry.name, entry.within])).toEqual([
      ['A-Main', 'Sport'],
      ['A-Main', 'Whoop'],
      ['Whoop Heat 1', undefined]
    ]);
  });

  it('puts the heat on the timer first, whatever the run order says', () => {
    const view = eventView({
      phases: [
        heat('h1', 'Heat 1', ['p-ace']),
        heat('h2', 'Heat 2', ['p-bee']),
        heat('h3', 'Heat 3', ['p-cat'])
      ],
      queue: ['h1', 'h2', 'h3'],
      on_timer: 'h3'
    });
    expect(upcomingHeats(view, names(view)).map((entry) => entry.name)).toEqual([
      'Heat 3',
      'Heat 1',
      'Heat 2'
    ]);
    // A heat on the timer that is not coming up (it is flown) changes nothing.
    const flown = { ...view, queue: ['h1', 'h2'], on_timer: 'h3' };
    expect(upcomingHeats(flown, names(view)).map((entry) => entry.name)).toEqual([
      'Heat 1',
      'Heat 2'
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
    await source.listChannels();
    expect(asked.map((call) => call.url)).toEqual([
      'http://grid.local:8081/public/event',
      'http://grid.local:8081/public/event/state',
      'http://grid.local:8081/public/event/pilots',
      'http://grid.local:8081/public/event/classes',
      'http://grid.local:8081/public/event/timers',
      'http://grid.local:8081/public/event/channels'
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

  it("a pilot's own sign-up goes to the pilot routes, and never names the pilot", async () => {
    const asked: { url: string; method: string; body?: string }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        asked.push({
          url: String(input),
          method: init?.method ?? 'GET',
          body: init?.body as string | undefined
        });
        return {
          ok: true,
          status: 200,
          json: async () => ({ cursor: 9 }),
          text: async () => '{"cursor":9}'
        } as Response;
      })
    );
    const source = publicSource('http://grid.local:8081');
    const seat = { adapter: 'gate', seat: '0' };
    await source.takeSpot('quali/heat-1', seat);
    await source.takeNextSpot('quali', seat);
    await source.leaveSpot('quali/heat-1');
    await source.takeSeat('practice#1', seat);
    await source.dropSeat('practice#1');
    expect(asked.map((call) => [call.method, call.url, JSON.parse(call.body ?? 'null')])).toEqual([
      ['POST', 'http://grid.local:8081/public/pilot/signup/take', { heat: 'quali/heat-1', seat }],
      ['POST', 'http://grid.local:8081/public/pilot/signup/next', { phase: 'quali', seat }],
      ['POST', 'http://grid.local:8081/public/pilot/signup/leave', { heat: 'quali/heat-1' }],
      [
        'POST',
        'http://grid.local:8081/public/pilot/checkin/take',
        { race: 'practice#1', adapter: 'gate', seat: '0' }
      ],
      ['POST', 'http://grid.local:8081/public/pilot/checkin/drop', { race: 'practice#1' }]
    ]);
    for (const call of asked) expect(call.body).not.toContain('pilot');
  });

  it('at a kiosk, the pass rides in its header and the code goes to the kiosk route', async () => {
    const asked: { url: string; method: string; body?: string; kiosk?: string }[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
        asked.push({
          url: String(input),
          method: init?.method ?? 'GET',
          body: init?.body as string | undefined,
          kiosk: (init?.headers as Record<string, string>)['X-GridFPV-Kiosk']
        });
        const body = { pilot: { id: 'p-1', callsign: 'Ace' }, pass: 'pass-1', idle_seconds: 120 };
        return {
          ok: true,
          status: 200,
          json: async () => body,
          text: async () => JSON.stringify(body)
        } as Response;
      })
    );
    const source = publicSource('http://grid.local:8081');
    const seat = { adapter: 'gate', seat: '0' };
    expect((await source.kioskIn('ABC234')).pass).toBe('pass-1');
    await source.takeSpot('quali/heat-1', seat, 'pass-1');
    await source.takeNextSpot('quali', seat, 'pass-1');
    await source.leaveSpot('quali/heat-1', 'pass-1');
    await source.kioskDone('pass-1');
    await source.kioskSetUp('ABCD2345');
    // A pilot on their own phone sends no pass at all.
    await source.takeSpot('quali/heat-1', seat);
    expect(
      asked.map((call) => [call.method, call.url.replace('http://grid.local:8081', ''), call.kiosk])
    ).toEqual([
      ['POST', '/public/pilot/kiosk', undefined],
      ['POST', '/public/pilot/signup/take', 'pass-1'],
      ['POST', '/public/pilot/signup/next', 'pass-1'],
      ['POST', '/public/pilot/signup/leave', 'pass-1'],
      ['DELETE', '/public/pilot/kiosk', 'pass-1'],
      ['POST', '/public/pilot/kiosk/device', undefined],
      ['POST', '/public/pilot/signup/take', undefined]
    ]);
    expect(JSON.parse(asked[0].body ?? 'null')).toEqual({ code: 'ABC234' });
    expect(JSON.parse(asked[5].body ?? 'null')).toEqual({ code: 'ABCD2345' });
    // The pass is never in an address or a body.
    for (const call of asked) {
      expect(call.url).not.toContain('pass-1');
      expect(call.body ?? '').not.toContain('pass-1');
    }
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
