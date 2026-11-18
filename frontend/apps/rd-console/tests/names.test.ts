/**
 * The one resolver from ids to names (`lib/names.ts`). A raw id reaching the screen is a
 * bug, so the fallbacks are pinned here too: an id nobody knows becomes plain words.
 */
import { describe, expect, it } from 'vitest';
import type { PhaseView, Pilot } from '@gridfpv/types';
import { buildNames, ordinal, placeOf, UNKNOWN_PHASE, UNKNOWN_PILOT } from '../src/lib/names.js';
import { BENCH_ADAPTER, UNKNOWN_TIMER } from '../src/lib/seats.js';
import { heat, group, timerView } from './competition.js';

const PILOTS: Pilot[] = [
  { id: 'p-1a', callsign: 'Ace', vtx_types: [] },
  { id: 'p-2b', callsign: 'Bee', vtx_types: [] }
];

const PHASES: PhaseView[] = [
  group('finals', 'Finals'),
  group('finals/winners-1', 'Winners Round 1', 'finals'),
  heat('finals/winners-1/heat-2', 'Winners Round 1 Heat 2', ['p-1a', 'p-2b'], {
    parent: 'finals/winners-1'
  })
];

describe('ordinal', () => {
  it('ends each number the way it is said', () => {
    expect([1, 2, 3, 4, 11, 12, 13, 21, 22, 23, 101, 111].map(ordinal)).toEqual([
      '1st',
      '2nd',
      '3rd',
      '4th',
      '11th',
      '12th',
      '13th',
      '21st',
      '22nd',
      '23rd',
      '101st',
      '111th'
    ]);
  });
});

describe('placeOf', () => {
  it('calls first place the winner and the rest by their place', () => {
    expect(placeOf(1, 'Heat 1')).toBe('Winner of Heat 1');
    expect(placeOf(2, 'Heat 1')).toBe('2nd of Heat 1');
    expect(placeOf(5, 'Heat 1')).toBe('5th of Heat 1');
  });
});

describe('buildNames', () => {
  const names = buildNames(PHASES, PILOTS);

  it('names a phase by its own label, which already reads in full', () => {
    expect(names.phase('finals')).toBe('Finals');
    expect(names.phase('finals/winners-1/heat-2')).toBe('Winners Round 1 Heat 2');
  });

  it('names a pilot by callsign', () => {
    expect(names.pilot('p-2b')).toBe('Bee');
  });

  it('never falls back to the raw id', () => {
    expect(names.phase('finals/gone')).toBe(UNKNOWN_PHASE);
    expect(names.pilot('p-zz')).toBe(UNKNOWN_PILOT);
  });

  it('says who is on a seat', () => {
    expect(names.who({ Pilot: 'p-1a' })).toBe('Ace');
    expect(names.who('Open')).toBe('Open seat');
    expect(names.who('Nobody')).toBe('Empty');
  });

  it('says where a seat that is still waiting will be filled from', () => {
    const phase = 'finals/winners-1/heat-2';
    expect(names.who({ Waiting: { phase, position: 1 } })).toBe('Winner of Winners Round 1 Heat 2');
    expect(names.who({ Waiting: { phase, position: 2 } })).toBe('2nd of Winners Round 1 Heat 2');
    expect(names.who({ Waiting: { phase, position: 4 } })).toBe('4th of Winners Round 1 Heat 2');
    expect(names.who({ Waiting: { phase: 'nope', position: 1 } })).toBe(
      `Winner of ${UNKNOWN_PHASE}`
    );
  });
});

describe('the phases that hold a phase', () => {
  const names = buildNames(
    [
      group('sf', 'Sport Finals'),
      group('sf/s', 'Semi-finals', 'sf'),
      heat('sf/s/1', 'Semi-final 1', [], { parent: 'sf/s' }),
      group('p', 'Practice'),
      heat('p/3', 'Practice Heat 3', [], { parent: 'p' }),
      group('b', 'Whoop Bracket'),
      group('b/w1', 'Winners Round 1', 'b'),
      heat('b/w1/1', 'Winners Round 1 Heat 1', [], { parent: 'b/w1' }),
      group('b/f', 'Final', 'b'),
      heat('b/f/1', 'Final', [], { parent: 'b/f' }),
      heat('solo', 'Solo', []),
      heat('lost', 'Lost', [], { parent: 'gone' })
    ],
    []
  );

  it('are a path, outermost first', () => {
    expect(names.within('sf/s/1')).toBe('Sport Finals › Semi-finals');
    expect(names.within('sf/s')).toBe('Sport Finals');
  });

  it('leave out a phase the name already starts with', () => {
    expect(names.within('p/3')).toBeUndefined();
    // The round is in the heat's name; the bracket is not.
    expect(names.within('b/w1/1')).toBe('Whoop Bracket');
    expect(names.within('b/f/1')).toBe('Whoop Bracket');
  });

  it('are nothing for a phase at the top, one whose parent is gone, or one that is gone', () => {
    expect(names.within('solo')).toBeUndefined();
    expect(names.within('sf')).toBeUndefined();
    expect(names.within('lost')).toBeUndefined();
    expect(names.within('never-was')).toBeUndefined();
  });

  it('never shows a raw id', () => {
    for (const id of ['sf/s/1', 'b/w1/1', 'b/f/1']) {
      expect(names.within(id)).not.toMatch(/\//);
    }
  });
});

describe('buildNames: seats and timers', () => {
  const TIMERS = [
    timerView('gate-x1y2', 'Gate', 4),
    // Switched off since the Director started: not connected, and no seats reported.
    timerView('far-gate-9zz0', 'Far Gate', 0, { connected: false, problem: 'No answer.' }),
    // Was connected, then dropped: the seats it reported are still known.
    timerView('loop-77aa', 'Loop', 2, { connected: false, problem: 'The connection dropped.' })
  ];
  const names = buildNames(PHASES, PILOTS, TIMERS);

  it('names a timer by the name its director gave it, connected or not', () => {
    expect(names.timer('gate-x1y2')).toBe('Gate');
    expect(names.timer('far-gate-9zz0')).toBe('Far Gate');
    expect(names.timer('loop-77aa')).toBe('Loop');
  });

  it('calls the bench "Bench" and a timer nobody knows "Unknown timer"', () => {
    expect(names.timer(BENCH_ADAPTER)).toBe('Bench');
    expect(names.timer('gone-0000')).toBe(UNKNOWN_TIMER);
    expect(UNKNOWN_TIMER).toBe('Unknown timer');
  });

  it("numbers a timer's seat by its place in the seats the timer reported", () => {
    expect(names.seat('gate-x1y2', '0')).toBe('Seat 1');
    expect(names.seat('gate-x1y2', '3')).toBe('Seat 4');
    // The timer's own order wins over the place in a lineup.
    expect(names.seat('gate-x1y2', '2', 0)).toBe('Seat 3');
  });

  it('still numbers the seats of a timer that is not connected', () => {
    expect(names.seat('loop-77aa', '1')).toBe('Seat 2');
    // Nothing reported yet: the seat's own number says which it is.
    expect(names.seat('far-gate-9zz0', '2')).toBe('Seat 3');
  });

  it('numbers the bench seats', () => {
    expect(names.seat(BENCH_ADAPTER, 'seat-1')).toBe('Seat 1');
    expect(names.seat(BENCH_ADAPTER, 'seat-8')).toBe('Seat 8');
  });

  it('never shows a raw seat handle or timer id', () => {
    // A removed timer: named by the place in the lineup when there is one.
    expect(names.seat('gone-0000', 'gate-b', 1)).toBe('Seat 2');
    expect(names.seat('gone-0000', 'gate-b')).toBe('Seat');
    expect(names.seat(BENCH_ADAPTER, 'seat-99')).toBe('Seat');
  });

  it('works with no timer list at all, as before the first read', () => {
    const bare = buildNames(PHASES, PILOTS);
    expect(bare.seat(BENCH_ADAPTER, 'seat-2')).toBe('Seat 2');
    expect(bare.timer('gate-x1y2')).toBe(UNKNOWN_TIMER);
  });
});

describe("a seat's channel in its name", () => {
  const timers = [
    {
      timer: { id: 'gate', name: 'Gate', kind: 'Oft' as const, address: '' },
      status: { connected: true, seats: ['0', '1', '2'] }
    }
  ];
  const names = buildNames(
    [],
    [],
    timers,
    [],
    [
      { adapter: 'gate', seat: '0', freq_mhz: 5658 },
      { adapter: 'gate', seat: '1', freq_mhz: 5700 }
    ]
  );

  it('goes after the seat: the band and channel, or the frequency off the usual bands', () => {
    expect(names.seat('gate', '0')).toBe('Seat 1 · R1');
    expect(names.seat('gate', '1')).toBe('Seat 2 · 5700 MHz');
  });

  it('is left off a seat the timer has not spoken for', () => {
    expect(names.seat('gate', '2')).toBe('Seat 3');
    expect(names.seat('bench', 'seat-1', 0)).toBe('Seat 1');
    expect(names.seat('other', '0', 4)).toBe('Seat 5');
    expect(buildNames([], [], timers).seat('gate', '0')).toBe('Seat 1');
  });

  it('seatPlain never has it, and channel gives it in full', () => {
    expect(names.seatPlain('gate', '0')).toBe('Seat 1');
    expect(names.channel('gate', '0')).toBe('R1 · 5658 MHz');
    expect(names.channel('gate', '1')).toBe('5700 MHz');
    expect(names.channel('gate', '2')).toBeUndefined();
    expect(names.channel('other', '0')).toBeUndefined();
    expect(names.channel('bench', 'seat-1')).toBeUndefined();
  });

  it('a seat on another timer with the same number keeps its own channel', () => {
    const two = buildNames(
      [],
      [],
      [],
      [],
      [
        { adapter: 'a', seat: '0', freq_mhz: 5658 },
        { adapter: 'b', seat: '0', freq_mhz: 5695 }
      ]
    );
    expect(two.channel('a', '0')).toBe('R1 · 5658 MHz');
    expect(two.channel('b', '0')).toBe('R2 · 5695 MHz');
    expect(two.seat('b', '0', 0)).toBe('Seat 1 · R2');
  });
});

describe('seats of a heat that is on more than one timer', () => {
  const one = (id: string, name: string) => timerView(id, name, 1);
  const timers = [one('g1', 'Gate 1'), one('g2', 'Gate 2'), timerView('big', 'Big', 4)];
  const mixed = {
    ...heat('h1', 'Heat 1', []),
    lineup: [
      { adapter: 'g1', seat: '0', who: 'Nobody' as const },
      { adapter: 'g2', seat: '0', who: 'Nobody' as const }
    ]
  };

  it('are named by their timer, so no two seats share a name', () => {
    const names = buildNames(
      [mixed],
      [],
      timers,
      [],
      [
        { adapter: 'g1', seat: '0', freq_mhz: 5658 },
        { adapter: 'g2', seat: '0', freq_mhz: 5695 }
      ]
    );
    expect(names.seatPlain('g1', '0', 0)).toBe('Gate 1');
    expect(names.seatPlain('g2', '0', 1)).toBe('Gate 2');
    // The channel still goes on the end.
    expect(names.seat('g1', '0', 0)).toBe('Gate 1 · R1');
    expect(names.seat('g2', '0', 1)).toBe('Gate 2 · R2');
  });

  it('a timer with several seats keeps the seat number after its name', () => {
    const both = {
      ...heat('h2', 'Heat 2', []),
      lineup: [
        { adapter: 'g1', seat: '0', who: 'Nobody' as const },
        { adapter: 'big', seat: '2', who: 'Nobody' as const }
      ]
    };
    const names = buildNames([both], [], timers);
    expect(names.seatPlain('big', '2', 1)).toBe('Big Seat 3');
    expect(names.seatPlain('g1', '0', 0)).toBe('Gate 1');
  });

  it('a heat on one timer is named as before, and so is the bench', () => {
    const plain = {
      ...heat('h3', 'Heat 3', []),
      lineup: [
        { adapter: 'big', seat: '0', who: 'Nobody' as const },
        { adapter: 'big', seat: '1', who: 'Nobody' as const }
      ]
    };
    const names = buildNames([plain], [], timers);
    expect(names.seat('big', '0', 0)).toBe('Seat 1');
    expect(names.seat('big', '1', 1)).toBe('Seat 2');
    // The bench beside a timer in one heat is still the bench's seat.
    const withBench = {
      ...heat('h4', 'Heat 4', []),
      lineup: [
        { adapter: BENCH_ADAPTER, seat: 'seat-2', who: 'Nobody' as const },
        { adapter: 'g1', seat: '0', who: 'Nobody' as const }
      ]
    };
    const more = buildNames([withBench], [], timers);
    expect(more.seat(BENCH_ADAPTER, 'seat-2', 0)).toBe('Seat 2');
    expect(more.seat('g1', '0', 1)).toBe('Gate 1');
  });

  it('a timer that has been removed falls back to the place in the lineup', () => {
    const gone = {
      ...heat('h5', 'Heat 5', []),
      lineup: [
        { adapter: 'g1', seat: '0', who: 'Nobody' as const },
        { adapter: 'removed', seat: '0', who: 'Nobody' as const }
      ]
    };
    const names = buildNames([gone], [], timers);
    expect(names.seat('removed', '0', 1)).toBe('Seat 2');
  });

  it('a sign-up phase says which timers share its heats, even for a seat nobody is on yet', () => {
    const phase = {
      ...group('practice', 'Practice'),
      signup: {
        rules: {
          seats: [
            { adapter: 'g1', seat: '0' },
            { adapter: 'g2', seat: '0' }
          ],
          open_heats: 1,
          rest_heats: 0,
          open: true
        },
        heats: [],
        pilots: []
      }
    };
    // The heat has one pilot, on gate 2. Its lineup alone names one timer.
    const lonely = {
      ...heat('practice/heat-1', 'Practice Heat 1', []),
      lineup: [{ adapter: 'g2', seat: '0', who: 'Nobody' as const }]
    };
    const names = buildNames([phase, lonely], [], timers);
    expect(names.seat('g2', '0', 0)).toBe('Gate 2');
    expect(names.seat('g1', '0', 0)).toBe('Gate 1');
  });
});
