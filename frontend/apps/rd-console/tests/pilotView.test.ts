/**
 * What a signed-in pilot sees: their heats, how close they are to flying, and their races.
 */
import { describe, expect, it } from 'vitest';
import type { Pilot, SeatChannel } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import {
  isMine,
  myHeats,
  myRaces,
  nearnessWords,
  pilotAlert,
  SOON_WITHIN
} from '../src/lib/pilotView.js';
import { eventView, heat, lap, race, raceSeat } from './competition.js';

const S = 1_000_000;
const pilot = (id: string, callsign: string): Pilot => ({ id, callsign, vtx_types: [] });
const PILOTS = [pilot('p-ace', 'Ace'), pilot('p-bee', 'Bee'), pilot('p-cat', 'Cat')];
const CHANNELS: SeatChannel[] = [
  { adapter: 'gate', seat: '0', freq_mhz: 5658 },
  { adapter: 'gate', seat: '1', freq_mhz: 5700 }
];

/** Five heats queued in order. Ace is in heats 1, 3 and 5. */
function queued(over: Parameters<typeof eventView>[0] = {}) {
  const view = eventView({
    phases: [
      heat('h1', 'Heat 1', ['p-ace', 'p-bee']),
      heat('h2', 'Heat 2', ['p-bee', 'p-cat']),
      heat('h3', 'Heat 3', ['p-cat', 'p-ace']),
      heat('h4', 'Heat 4', ['p-bee']),
      heat('h5', 'Heat 5', ['p-ace'])
    ],
    queue: ['h1', 'h2', 'h3', 'h4', 'h5'],
    ...over
  });
  return { view, names: buildNames(view.phases, PILOTS) };
}

describe("a pilot's heats", () => {
  it('is empty with no snapshot, and for a pilot who is in none', () => {
    expect(myHeats(undefined, buildNames([], []), 'p-ace')).toEqual([]);
    const { view, names } = queued();
    expect(myHeats(view, names, 'p-nobody')).toEqual([]);
  });

  it('with no race on, the first heat in the queue is next and the rest count from there', () => {
    const { view, names } = queued();
    expect(
      myHeats(view, names, 'p-ace').map((h) => [h.name, h.seatName, h.heatsBefore, h.nearness])
    ).toEqual([
      ['Heat 1', 'Seat 1', 0, 'next'],
      ['Heat 3', 'Seat 2', 2, 'soon'],
      ['Heat 5', 'Seat 1', 4, 'later']
    ]);
    expect(myHeats(view, names, 'p-cat').map((h) => [h.name, h.heatsBefore, h.nearness])).toEqual([
      ['Heat 2', 1, 'soon'],
      ['Heat 3', 2, 'soon']
    ]);
  });

  it('with a race on that the pilot is in, that heat is now and the others count the race too', () => {
    const on = race('h1', 1, 'Running', {
      seats: [raceSeat(0, 'p-ace'), raceSeat(1, 'p-bee')]
    });
    const { view, names } = queued({ races: [on], current_race: on.race });
    expect(
      myHeats(view, names, 'p-ace').map((h) => [h.name, h.seatName, h.heatsBefore, h.nearness])
    ).toEqual([
      ['Heat 1', 'Seat 1', 0, 'now'],
      // Heat 2 flies in between, so one heat is before Heat 3 once this race is done... and
      // the race on now makes two.
      ['Heat 3', 'Seat 2', 2, 'soon'],
      ['Heat 5', 'Seat 1', 4, 'later']
    ]);
  });

  it('with a race on that the pilot is not in, the heat after it is one away, not next', () => {
    const on = race('h1', 1, 'Running', { seats: [raceSeat(0, 'p-ace'), raceSeat(1, 'p-bee')] });
    const { view, names } = queued({ races: [on], current_race: on.race });
    expect(myHeats(view, names, 'p-cat').map((h) => [h.name, h.heatsBefore, h.nearness])).toEqual([
      ['Heat 2', 1, 'soon'],
      ['Heat 3', 2, 'soon']
    ]);
  });

  it('counts a race that is starting or stopped as on, the same as one that is running', () => {
    for (const state of ['Staging', 'Stopped'] as const) {
      const on = race('h1', 1, state, { seats: [raceSeat(0, 'p-ace')] });
      const { view, names } = queued({ races: [on], current_race: on.race });
      expect(myHeats(view, names, 'p-ace')[0].nearness, state).toBe('now');
    }
  });

  it(`calls a heat soon up to ${SOON_WITHIN} heats away, and later beyond that`, () => {
    const { view, names } = queued({ queue: ['h2', 'h4', 'h3', 'h1', 'h5'] });
    expect(myHeats(view, names, 'p-ace').map((h) => [h.name, h.heatsBefore, h.nearness])).toEqual([
      ['Heat 3', 2, 'soon'],
      ['Heat 1', 3, 'later'],
      ['Heat 5', 4, 'later']
    ]);
  });

  it("gives each heat's channel when the timer has said it", () => {
    const view = eventView({
      phases: [
        {
          ...heat('h1', 'Heat 1', ['p-ace']),
          lineup: [
            { adapter: 'gate', seat: '0', who: { Pilot: 'p-ace' } },
            { adapter: 'gate', seat: '1', who: { Pilot: 'p-bee' } }
          ]
        }
      ],
      queue: ['h1']
    });
    const names = buildNames(view.phases, PILOTS, [], [], CHANNELS);
    const ace = myHeats(view, names, 'p-ace')[0];
    expect(ace.channel).toBe('R1 · 5658 MHz');
    // The channel is beside it in full, so the seat is named without one.
    expect(ace.seatName).toBe('Seat 1');
    expect(myHeats(view, names, 'p-bee')[0].channel).toBe('5700 MHz');
    // With nothing from the timer, there is no channel to give.
    const bare = buildNames(view.phases, PILOTS);
    expect(myHeats(view, bare, 'p-ace')[0].channel).toBeUndefined();
  });

  it('includes a heat that still waits on another, after the ready ones', () => {
    const view = eventView({
      phases: [heat('h1', 'Semi', ['p-bee']), heat('final', 'Final', ['p-ace'], { waiting: true })],
      queue: ['h1']
    });
    const names = buildNames(view.phases, PILOTS);
    expect(myHeats(view, names, 'p-ace').map((h) => [h.name, h.heatsBefore])).toEqual([
      ['Final', 1]
    ]);
  });

  it('does not count a seat the pilot is only waited for on', () => {
    const view = eventView({
      phases: [
        heat('h1', 'Semi', ['p-ace']),
        heat('final', 'Final', [{ Waiting: { phase: 'h1', position: 1 } }], { waiting: true })
      ],
      queue: ['h1']
    });
    const names = buildNames(view.phases, PILOTS);
    expect(myHeats(view, names, 'p-ace').map((h) => h.name)).toEqual(['Semi']);
  });
});

describe('the alert', () => {
  const alertFor = (id: string, over: Parameters<typeof queued>[0] = {}) => {
    const { view, names } = queued(over);
    return pilotAlert(myHeats(view, names, id));
  };

  it('says nothing to a pilot with no heat, or whose next heat is far off', () => {
    expect(pilotAlert([])).toBeUndefined();
    expect(alertFor('p-nobody')).toBeUndefined();
    expect(alertFor('p-ace', { queue: ['h2', 'h4', 'h2', 'h1'] })).toBeUndefined();
  });

  it('says you are next, with where', () => {
    const alert = alertFor('p-ace');
    expect(alert?.nearness).toBe('next');
    expect(alert?.title).toBe('You are next');
    expect(alert?.detail).toBe('Heat 1 · Seat 1');
  });

  it('says how many heats away, with the right word for one', () => {
    expect(alertFor('p-cat')?.title).toBe('You are 1 heat away');
    expect(alertFor('p-cat')?.nearness).toBe('soon');
    expect(alertFor('p-ace', { queue: ['h2', 'h4', 'h3'] })?.title).toBe('You are 2 heats away');
  });

  it('says you are flying now', () => {
    const on = race('h1', 1, 'Running', { seats: [raceSeat(0, 'p-ace')] });
    const alert = alertFor('p-ace', { races: [on], current_race: on.race });
    expect(alert?.nearness).toBe('now');
    expect(alert?.title).toBe('You are flying now');
  });

  it('puts the channel in when it is known', () => {
    const view = eventView({
      phases: [
        {
          ...heat('h1', 'Heat 1', ['p-ace']),
          lineup: [{ adapter: 'gate', seat: '0', who: { Pilot: 'p-ace' } }]
        }
      ],
      queue: ['h1']
    });
    const names = buildNames(
      view.phases,
      PILOTS,
      [
        {
          timer: { id: 'gate', name: 'Gate', kind: 'Oft', address: '' },
          status: { connected: true, seats: ['0', '1'] }
        }
      ],
      [],
      CHANNELS
    );
    expect(pilotAlert(myHeats(view, names, 'p-ace'))?.detail).toBe(
      'Heat 1 · Seat 1 · R1 · 5658 MHz'
    );
  });

  it('has a key that changes each time there is something new to say, and only then', () => {
    const next = alertFor('p-ace');
    const same = alertFor('p-ace');
    expect(same?.key).toBe(next?.key);
    const on = race('h1', 1, 'Running', { seats: [raceSeat(0, 'p-ace')] });
    const now = alertFor('p-ace', { races: [on], current_race: on.race });
    expect(now?.key).not.toBe(next?.key);
    const two = alertFor('p-ace', { queue: ['h2', 'h4', 'h3'] });
    const one = alertFor('p-ace', { queue: ['h4', 'h3'] });
    expect(two?.key).not.toBe(one?.key);
    const otherHeat = alertFor('p-ace', { queue: ['h5'] });
    expect(otherHeat?.key).not.toBe(next?.key);
  });
});

describe('how far off a heat is, in words', () => {
  it('says on now, next, or how many heats away', () => {
    const { view, names } = queued();
    expect(myHeats(view, names, 'p-ace').map(nearnessWords)).toEqual([
      'Next',
      '2 heats away',
      '4 heats away'
    ]);
    expect(nearnessWords(myHeats(view, names, 'p-cat')[0])).toBe('1 heat away');
    const on = race('h1', 1, 'Running', { seats: [raceSeat(0, 'p-ace')] });
    const racing = queued({ races: [on], current_race: on.race });
    expect(nearnessWords(myHeats(racing.view, racing.names, 'p-ace')[0])).toBe('On now');
  });
});

describe("a pilot's races", () => {
  const view = eventView({
    phases: [heat('h1', 'Heat 1', ['p-ace', 'p-bee']), heat('h2', 'Heat 2', ['p-ace'])],
    races: [
      race('h1', 1, 'Saved', {
        seats: [
          raceSeat(0, 'p-ace', {
            finish: 2,
            laps: [lap(1, 7), lap(2, 6.5), lap(3, 6.9)],
            best_lap_micros: 6.5 * S,
            total_micros: 20.4 * S
          }),
          raceSeat(1, 'p-bee', { finish: 1, laps: [lap(4, 6)] })
        ]
      }),
      race('h1', 2, 'Discarded', { seats: [raceSeat(0, 'p-ace', { laps: [lap(5, 1)] })] }),
      race('h2', 1, 'Saved', {
        seats: [
          raceSeat(0, 'p-ace', {
            finish: 1,
            false_start: true,
            laps: [lap(6, 8), lap(7, 0, { restart: true }), lap(8, 7.5)],
            best_lap_micros: 7.5 * S
          })
        ]
      }),
      race('h2', 2, 'Running', { seats: [raceSeat(0, 'p-ace', { laps: [lap(9, 5)] })] })
    ]
  });
  const names = buildNames(view.phases, PILOTS);

  it('lists the saved races the pilot flew, newest first, by name and number', () => {
    expect(myRaces(view, names, 'p-ace').map((r) => r.name)).toEqual([
      'Heat 2 · race 1',
      'Heat 1 · race 1'
    ]);
  });

  it('gives the place, every counted lap, the best lap and the total', () => {
    const first = myRaces(view, names, 'p-ace')[1];
    expect(first.place).toBe(2);
    expect(first.laps).toEqual([7 * S, 6.5 * S, 6.9 * S]);
    expect(first.bestLapMicros).toBe(6.5 * S);
    expect(first.bestLapIndex).toBe(1);
    expect(first.totalMicros).toBe(20.4 * S);
    expect(first.falseStart).toBe(false);
  });

  it('leaves a marked restart out of the laps, and carries a false start', () => {
    const second = myRaces(view, names, 'p-ace')[0];
    expect(second.laps).toEqual([8 * S, 7.5 * S]);
    expect(second.bestLapIndex).toBe(1);
    expect(second.falseStart).toBe(true);
    expect(second.totalMicros).toBeUndefined();
  });

  it('is only the races of that pilot', () => {
    expect(myRaces(view, names, 'p-bee').map((r) => r.name)).toEqual(['Heat 1 · race 1']);
    expect(myRaces(view, names, 'p-cat')).toEqual([]);
    expect(myRaces(undefined, names, 'p-ace')).toEqual([]);
  });

  it('a race with no laps has none, and no best lap to mark', () => {
    const none = eventView({
      phases: [heat('h1', 'Heat 1', ['p-ace'])],
      races: [race('h1', 1, 'Saved', { seats: [raceSeat(0, 'p-ace')] })]
    });
    const [only] = myRaces(none, buildNames(none.phases, PILOTS), 'p-ace');
    expect(only.laps).toEqual([]);
    expect(only.bestLapIndex).toBeUndefined();
    expect(only.bestLapMicros).toBeUndefined();
    expect(only.place).toBeUndefined();
  });
});

describe('whose race it is', () => {
  it("is the pilot's when they are on a seat of it", () => {
    const flown = race('h1', 1, 'Saved', { seats: [raceSeat(0, 'p-ace'), raceSeat(1)] });
    expect(isMine(flown, 'p-ace')).toBe(true);
    expect(isMine(flown, 'p-bee')).toBe(false);
    expect(isMine(flown, undefined)).toBe(false);
  });
});
