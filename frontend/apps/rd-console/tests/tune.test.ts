/** The Seats page's numbers and its draft: formatting, draft ↔ settings, dirty, mode, seats. */
import { describe, expect, it } from 'vitest';
import type { SeatLevels, TimerConfig } from '@gridfpv/types';
import {
  addSeat,
  draftConfig,
  draftFrom,
  fieldNumber,
  formatGap,
  fromShown,
  formatLevel,
  isDirty,
  levelsInUse,
  msToSeconds,
  levelsFrom,
  nextSeat,
  ownLevels,
  sameConfig,
  secondsToMs,
  toShown,
  unitLabel,
  unitScale,
  withMode,
  type DraftSeat
} from '../src/lib/tune.js';

const AUTO: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5695, enabled: false }
  ],
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
};
const MANUAL: TimerConfig = { ...AUTO, threshold: { Manual: { enter: -700, exit: -765 } } };
/** A timer that takes levels per seat, with seat 2 on its own fixed levels. */
const OWN: TimerConfig = {
  ...AUTO,
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5695, enabled: true, threshold: { Manual: { enter: -600, exit: -660 } } }
  ]
};
const seatName = (_seat: string, position: number) => `Seat ${position + 1}`;

describe('levels in words', () => {
  it('shows tenths of a dB as dB with one decimal', () => {
    expect(formatLevel(-773, 'TenthsDb')).toBe('-77.3 dB');
    expect(formatLevel(-900, 'TenthsDb')).toBe('-90.0 dB');
    expect(formatLevel(5, 'TenthsDb')).toBe('0.5 dB');
  });

  it('says dBm for a calibrated timer, and leaves counts as they are', () => {
    expect(formatLevel(-773, 'TenthsDbm')).toBe('-77.3 dBm');
    expect(formatLevel(142, 'Counts')).toBe('142');
    expect(unitLabel('TenthsDb')).toBe('dB');
    expect(unitLabel('TenthsDbm')).toBe('dBm');
    expect(unitLabel('Counts')).toBe('');
  });

  it('shows a dash for a level that is not known', () => {
    expect(formatLevel(undefined, 'TenthsDb')).toBe('—');
  });

  it('writes a difference in dB, also between two dBm levels', () => {
    expect(formatGap(150, 'TenthsDb')).toBe('15.0 dB');
    expect(formatGap(150, 'TenthsDbm')).toBe('15.0 dB');
  });

  it('shows millivolts whole, never divided by ten', () => {
    // A receiver that reads a signal-strength pin (OFT spec v0.8): 850 is 850 mV.
    expect(unitLabel('Millivolts')).toBe('mV');
    expect(unitScale('Millivolts')).toBe(1);
    expect(formatLevel(850, 'Millivolts')).toBe('850 mV');
    expect(formatGap(40, 'Millivolts')).toBe('40 mV');
    expect(formatGap(12, 'Counts')).toBe('12');
  });
});

describe('field numbers', () => {
  it('reads a number or a typed number, and nothing from an empty or odd field', () => {
    expect(fieldNumber(12.5)).toBe(12.5);
    expect(fieldNumber(' -77.3 ')).toBe(-77.3);
    expect(fieldNumber('')).toBeUndefined();
    expect(fieldNumber('  ')).toBeUndefined();
    expect(fieldNumber('abc')).toBeUndefined();
    expect(fieldNumber(null)).toBeUndefined();
    expect(fieldNumber(undefined)).toBeUndefined();
    expect(fieldNumber(Number.NaN)).toBeUndefined();
  });

  it('converts dB to tenths and back without drift', () => {
    expect(toShown(-773, 'TenthsDb')).toBe(-77.3);
    expect(fromShown(-77.3, 'TenthsDb')).toBe(-773);
    expect(fromShown('15', 'TenthsDb')).toBe(150);
    expect(fromShown(0.1 + 0.2, 'TenthsDb')).toBe(3);
    expect(fromShown('', 'TenthsDb')).toBeUndefined();
  });

  it('leaves millivolts and counts as they are, to the whole number', () => {
    expect(toShown(850, 'Millivolts')).toBe(850);
    expect(fromShown('850', 'Millivolts')).toBe(850);
    expect(fromShown(850.4, 'Millivolts')).toBe(850);
    expect(fromShown(120, 'Counts')).toBe(120);
  });

  it('converts seconds to milliseconds, to one decimal of a second', () => {
    expect(msToSeconds(2500)).toBe(2.5);
    expect(secondsToMs(2.5)).toBe(2500);
    expect(secondsToMs('3')).toBe(3000);
    expect(secondsToMs(1.26)).toBe(1300);
    expect(secondsToMs('')).toBeUndefined();
  });
});

describe('a timer that measures millivolts', () => {
  const MV: TimerConfig = {
    seats: [
      { seat: '0', freq_mhz: 5658, enabled: true, threshold: { Manual: { enter: 850, exit: 810 } } }
    ],
    threshold: { Auto: { snr_enter: 150, hysteresis: 50 } }
  };

  it('drafts its settings as the same whole numbers, and sends them back unchanged', () => {
    const draft = draftFrom(MV, 'Millivolts');
    expect(draft.unit).toBe('Millivolts');
    // 150 above the floor is 150 mV here. Read as dB it would have shown 15.
    expect(draft.levels).toMatchObject({ unit: 'Millivolts', aboveFloor: 150, belowEnter: 50 });
    expect(draft.seats[0].levels).toMatchObject({ enter: 850, exit: 810 });
    expect(draftConfig(draft, seatName)).toEqual({ ok: true, config: MV });
    expect(isDirty(draft, MV)).toBe(false);
  });

  it('sends a typed level as millivolts, not ten times it', () => {
    const draft = draftFrom(MV, 'Millivolts');
    draft.seats[0].levels!.enter = '900';
    const result = draftConfig(draft, seatName);
    expect(result.ok && result.config.seats[0].threshold).toEqual({
      Manual: { enter: 900, exit: 810 }
    });
  });

  it('fills fixed levels from the levels in use, in millivolts', () => {
    const draft = draftFrom(MV, 'Millivolts');
    expect(withMode(draft.levels, 'Manual', { enter: 846, exit: 800 })).toMatchObject({
      enter: 846,
      exit: 800
    });
    expect(ownLevels(draft.levels, { enter: 846, exit: 800 })).toMatchObject({
      unit: 'Millivolts',
      enter: 846,
      exit: 800
    });
  });
});

describe('the draft', () => {
  it('says what the settings say, in dB and seconds', () => {
    expect(draftFrom(AUTO, 'TenthsDb')).toEqual({
      unit: 'TenthsDb',
      seats: [
        { seat: '0', mhz: 5658, enabled: true },
        { seat: '1', mhz: 5695, enabled: false }
      ],
      levels: { mode: 'Auto', unit: 'TenthsDb', aboveFloor: 15, belowEnter: 5, enter: '', exit: '' }
    });
    const manual = draftFrom(MANUAL, 'TenthsDb').levels;
    expect(manual.mode).toBe('Manual');
    expect(manual.enter).toBe(-70);
    expect(manual.exit).toBe(-76.5);
    expect(manual.aboveFloor).toBe('');
  });

  it("holds a seat's own levels only for a seat that has them", () => {
    const draft = draftFrom(OWN, 'TenthsDb');
    expect('levels' in draft.seats[0]).toBe(false);
    expect(draft.seats[1].levels).toEqual({
      mode: 'Manual',
      unit: 'TenthsDb',
      aboveFloor: '',
      belowEnter: '',
      enter: -60,
      exit: -66
    });
    expect(levelsFrom({ Auto: { snr_enter: 120, hysteresis: 45 } }, 'TenthsDb')).toEqual({
      mode: 'Auto',
      unit: 'TenthsDb',
      aboveFloor: 12,
      belowEnter: 4.5,
      enter: '',
      exit: ''
    });
  });

  it('turns back into the very same settings', () => {
    const everything: TimerConfig = { ...OWN, exit_hold_ms: 30, lap_cooldown_ms: 2500 };
    for (const config of [AUTO, MANUAL, OWN, everything]) {
      expect(draftConfig(draftFrom(config, 'TenthsDb'), seatName)).toEqual({ ok: true, config });
    }
  });

  it('carries every edit into the settings it would send', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    draft.seats[1].enabled = true;
    draft.seats[1].mhz = '5800';
    draft.levels.aboveFloor = '12.5';
    draft.levels.belowEnter = 4;
    expect(draftConfig(draft, seatName)).toEqual({
      ok: true,
      config: {
        seats: [
          { seat: '0', freq_mhz: 5658, enabled: true },
          { seat: '1', freq_mhz: 5800, enabled: true }
        ],
        threshold: { Auto: { snr_enter: 125, hysteresis: 40 } }
      }
    });
  });

  it('says what is missing in a sentence, naming a seat by its name', () => {
    const problemOf = (change: (draft: ReturnType<typeof draftFrom>) => void, from = AUTO) => {
      const draft = draftFrom(from, 'TenthsDb');
      change(draft);
      const result = draftConfig(draft, seatName);
      return result.ok ? undefined : result.problem;
    };
    expect(problemOf((d) => (d.seats[1].mhz = ''))).toBe('Seat 2 needs a frequency in whole MHz.');
    expect(problemOf((d) => (d.seats[0].mhz = 5658.5))).toBe(
      'Seat 1 needs a frequency in whole MHz.'
    );
    expect(problemOf((d) => (d.levels.aboveFloor = ''))).toBe(
      'Say how far above the noise floor to enter.'
    );
    expect(problemOf((d) => (d.levels.belowEnter = ''))).toBe('Say how far below enter to exit.');
    expect(problemOf((d) => (d.levels.enter = ''), MANUAL)).toBe(
      'Fixed levels need an enter level.'
    );
    expect(problemOf((d) => (d.levels.exit = ''), MANUAL)).toBe('Fixed levels need an exit level.');
    expect(problemOf((d) => (d.levels.exit = -70), MANUAL)).toBe(
      'The exit level must be below the enter level.'
    );
  });
});

describe("a seat's own levels in the draft", () => {
  it('sends a threshold only for a seat with its own, and no key at all for the others', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    draft.seats[1].levels = {
      mode: 'Auto',
      unit: 'TenthsDb',
      aboveFloor: 12,
      belowEnter: '4.5',
      enter: '',
      exit: ''
    };
    const result = draftConfig(draft, seatName);
    if (!result.ok) throw new Error(result.problem);
    expect(result.config.seats[1].threshold).toEqual({ Auto: { snr_enter: 120, hysteresis: 45 } });
    // Absent, not null and not undefined: the body must not carry the key.
    expect('threshold' in result.config.seats[0]).toBe(false);
    expect(JSON.parse(JSON.stringify(result.config)).seats[0]).toEqual({
      seat: '0',
      freq_mhz: 5658,
      enabled: true
    });
    // The timer's own levels are untouched.
    expect(result.config.threshold).toEqual(AUTO.threshold);
  });

  it('sends every seat that has its own each time, whichever one was edited', () => {
    const draft = draftFrom(OWN, 'TenthsDb');
    draft.seats[0].mhz = 5800;
    expect(draftConfig(draft, seatName)).toMatchObject({
      ok: true,
      config: {
        seats: [
          { seat: '0', freq_mhz: 5800 },
          { seat: '1', threshold: { Manual: { enter: -600, exit: -660 } } }
        ]
      }
    });
  });

  it('puts a seat back on the timer by dropping its levels', () => {
    const draft = draftFrom(OWN, 'TenthsDb');
    draft.seats[1].levels = undefined;
    const result = draftConfig(draft, seatName);
    if (!result.ok) throw new Error(result.problem);
    expect('threshold' in result.config.seats[1]).toBe(false);
  });

  it('never sends one to a timer that does not take them', () => {
    const result = draftConfig(draftFrom(OWN, 'TenthsDb'), seatName, false);
    if (!result.ok) throw new Error(result.problem);
    expect(result.config.seats.some((seat) => 'threshold' in seat)).toBe(false);
    // A hole in levels that will not be sent is not a problem either.
    const holed = draftFrom(OWN, 'TenthsDb');
    holed.seats[1].levels!.enter = '';
    expect(draftConfig(holed, seatName, false).ok).toBe(true);
  });

  it('says what is missing, naming the seat by its name', () => {
    const problemOf = (change: (levels: NonNullable<DraftSeat['levels']>) => void) => {
      const draft = draftFrom(OWN, 'TenthsDb');
      change(draft.seats[1].levels!);
      const result = draftConfig(draft, seatName);
      return result.ok ? undefined : result.problem;
    };
    expect(problemOf((own) => (own.enter = ''))).toBe('Seat 2 needs an enter level.');
    expect(problemOf((own) => (own.exit = ''))).toBe('Seat 2 needs an exit level.');
    expect(problemOf((own) => (own.exit = -60))).toBe(
      'On Seat 2, the exit level must be below the enter level.'
    );
    expect(problemOf((own) => (own.mode = 'Auto'))).toBe(
      'Say how far above the noise floor Seat 2 enters.'
    );
    expect(
      problemOf((own) => {
        own.mode = 'Auto';
        own.aboveFloor = 12;
      })
    ).toBe('Say how far below enter Seat 2 exits.');
  });

  it("starts a seat's own levels the way the timer's are set, with the seat's levels right now as its fixed levels", () => {
    const timer = draftFrom(AUTO, 'TenthsDb').levels;
    const own = ownLevels(timer, { enter: -771, exit: -821 });
    expect(own).toEqual({
      mode: 'Auto',
      unit: 'TenthsDb',
      aboveFloor: 15,
      belowEnter: 5,
      enter: -77.1,
      exit: -82.1
    });
    expect(own).not.toBe(timer);
    // Going to fixed levels keeps what was filled in.
    expect(withMode(own, 'Manual', undefined)).toMatchObject({ enter: -77.1, exit: -82.1 });
    // With no live levels the timer's fixed levels in the draft stand in.
    expect(ownLevels(draftFrom(MANUAL, 'TenthsDb').levels, undefined)).toEqual(
      draftFrom(MANUAL, 'TenthsDb').levels
    );
    expect(
      ownLevels(draftFrom(MANUAL, 'TenthsDb').levels, { enter: -650, exit: -700 })
    ).toMatchObject({
      mode: 'Manual',
      enter: -65,
      exit: -70
    });
  });
});

describe('dirty', () => {
  it('is clean straight from the timer', () => {
    expect(isDirty(draftFrom(AUTO, 'TenthsDb'), AUTO)).toBe(false);
    expect(isDirty(draftFrom(MANUAL, 'TenthsDb'), MANUAL)).toBe(false);
  });

  it('is dirty after any edit, and clean again when the edit is undone', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    draft.seats[0].mhz = 5800;
    expect(isDirty(draft, AUTO)).toBe(true);
    draft.seats[0].mhz = '5658';
    expect(isDirty(draft, AUTO)).toBe(false);
    draft.seats[1].enabled = true;
    expect(isDirty(draft, AUTO)).toBe(true);
  });

  it('is dirty when the draft cannot be sent yet', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    draft.levels.aboveFloor = '';
    expect(isDirty(draft, AUTO)).toBe(true);
  });

  it("sees a seat's own levels given, changed and taken away, and is clean again after a discard", () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    draft.seats[0].levels = ownLevels(draft.levels, undefined);
    // Same numbers as the timer's, but the seat now carries its own: that is a change.
    expect(isDirty(draft, AUTO)).toBe(true);
    draft.seats[0].levels = undefined;
    expect(isDirty(draft, AUTO)).toBe(false);

    const own = draftFrom(OWN, 'TenthsDb');
    expect(isDirty(own, OWN)).toBe(false);
    own.seats[1].levels!.enter = -58;
    expect(isDirty(own, OWN)).toBe(true);
    own.seats[1].levels!.enter = '';
    expect(isDirty(own, OWN)).toBe(true);
    own.seats[1].levels = undefined;
    expect(isDirty(own, OWN)).toBe(true);
    // Discard is a new draft from what the timer has.
    expect(draftFrom(OWN, 'TenthsDb').seats[1].levels?.enter).toBe(-60);
    expect(isDirty(draftFrom(OWN, 'TenthsDb'), OWN)).toBe(false);
  });

  it("does not count a seat's levels on a timer that does not take them", () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    draft.seats[0].levels = ownLevels(draft.levels, undefined);
    expect(isDirty(draft, AUTO, false)).toBe(false);
  });

  it('is dirty with a seat added, a mode changed, or a number changed', () => {
    expect(isDirty(addSeat(draftFrom(AUTO, 'TenthsDb')), AUTO)).toBe(true);
    expect(isDirty(draftFrom(MANUAL, 'TenthsDb'), AUTO)).toBe(true);
    expect(isDirty(draftFrom(OWN, 'TenthsDb'), AUTO)).toBe(true);
  });

  it('sameConfig compares every part', () => {
    expect(sameConfig(AUTO, structuredClone(AUTO))).toBe(true);
    expect(sameConfig(AUTO, MANUAL)).toBe(false);
    expect(sameConfig(MANUAL, AUTO)).toBe(false);
    expect(sameConfig(AUTO, { ...AUTO, seats: AUTO.seats.slice(0, 1) })).toBe(false);
    expect(
      sameConfig(AUTO, { ...AUTO, threshold: { Auto: { snr_enter: 150, hysteresis: 60 } } })
    ).toBe(false);
    // A seat's own threshold: there or not, and what it says.
    expect(sameConfig(OWN, structuredClone(OWN))).toBe(true);
    const bothOn: TimerConfig = {
      ...OWN,
      seats: OWN.seats.map(({ threshold: _, ...seat }) => seat)
    };
    expect(sameConfig(OWN, bothOn)).toBe(false);
    expect(sameConfig(bothOn, OWN)).toBe(false);
    const moved = structuredClone(OWN);
    moved.seats[1].threshold = { Manual: { enter: -600, exit: -670 } };
    expect(sameConfig(OWN, moved)).toBe(false);
  });

  it('sameConfig compares each seat: which seat, its frequency, and whether it is on', () => {
    const changed = (change: (seat: TimerConfig['seats'][number]) => void): TimerConfig => {
      const config = structuredClone(AUTO);
      change(config.seats[1]);
      return config;
    };
    expect(
      sameConfig(
        AUTO,
        changed((seat) => (seat.freq_mhz = 5732))
      )
    ).toBe(false);
    expect(
      sameConfig(
        AUTO,
        changed((seat) => (seat.enabled = true))
      )
    ).toBe(false);
    expect(
      sameConfig(
        AUTO,
        changed((seat) => (seat.seat = '2'))
      )
    ).toBe(false);
    expect(sameConfig(AUTO, { ...AUTO, seats: [...AUTO.seats].reverse() })).toBe(false);
  });

  it('sameConfig compares the lap cooldown and the exit hold, there or not', () => {
    const cooled: TimerConfig = { ...AUTO, lap_cooldown_ms: 2500 };
    expect(sameConfig(cooled, { ...AUTO, lap_cooldown_ms: 2500 })).toBe(true);
    expect(sameConfig(cooled, { ...AUTO, lap_cooldown_ms: 3000 })).toBe(false);
    expect(sameConfig(cooled, AUTO)).toBe(false);
    expect(sameConfig(AUTO, cooled)).toBe(false);
    // A cooldown of 0 is a setting; it is not the same as a timer that has none.
    expect(sameConfig({ ...AUTO, lap_cooldown_ms: 0 }, AUTO)).toBe(false);

    const held: TimerConfig = { ...AUTO, exit_hold_ms: 30 };
    expect(sameConfig(held, { ...AUTO, exit_hold_ms: 30 })).toBe(true);
    expect(sameConfig(held, { ...AUTO, exit_hold_ms: 40 })).toBe(false);
    expect(sameConfig(held, AUTO)).toBe(false);
    // One is not mistaken for the other.
    expect(sameConfig({ ...AUTO, exit_hold_ms: 30 }, { ...AUTO, lap_cooldown_ms: 30 })).toBe(false);
  });
});

describe('switching how the levels are set', () => {
  const LEVELS: SeatLevels[] = [
    {
      seat: '0',
      unit: 'TenthsDb',
      level: -880,
      floor: -920,
      enter: -770,
      exit: -820,
      crossing: false
    },
    // Seat 1 has the higher enter level, but it is off, so it does not count.
    {
      seat: '1',
      unit: 'TenthsDb',
      level: -700,
      floor: -800,
      enter: -650,
      exit: -700,
      crossing: false
    }
  ];

  it('takes the levels in use from the seats that are on, the highest enter winning', () => {
    expect(levelsInUse(LEVELS, AUTO)).toEqual({ enter: -770, exit: -820 });
    const bothOn: TimerConfig = {
      ...AUTO,
      seats: AUTO.seats.map((seat) => ({ ...seat, enabled: true }))
    };
    expect(levelsInUse(LEVELS, bothOn)).toEqual({ enter: -650, exit: -700 });
    expect(levelsInUse([], AUTO)).toBeUndefined();
  });

  it("leaves out a seat with its own levels: it says nothing about the timer's", () => {
    // Seat 2 is on and has the higher enter level, but those are its own.
    expect(levelsInUse(LEVELS, OWN)).toEqual({ enter: -770, exit: -820 });
    const allOwn: TimerConfig = {
      ...OWN,
      seats: OWN.seats.map((seat) => ({ ...seat, threshold: OWN.seats[1].threshold }))
    };
    expect(levelsInUse(LEVELS, allOwn)).toBeUndefined();
  });

  it('fills the fixed levels from the levels in use when going to fixed', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    const next = withMode(draft.levels, 'Manual', { enter: -770, exit: -820 });
    expect(next.mode).toBe('Manual');
    expect(next.enter).toBe(-77);
    expect(next.exit).toBe(-82);
    expect(draftConfig({ ...draft, levels: next }, seatName)).toMatchObject({
      ok: true,
      config: { threshold: { Manual: { enter: -770, exit: -820 } } }
    });
  });

  it('keeps numbers the director already typed, and what was there on the way back', () => {
    const typed = { ...draftFrom(AUTO, 'TenthsDb').levels, enter: -60, exit: '' };
    const next = withMode(typed, 'Manual', { enter: -770, exit: -820 });
    expect(next.enter).toBe(-60);
    expect(next.exit).toBe(-82);
    const back = withMode(next, 'Auto', { enter: -770, exit: -820 });
    expect(back.mode).toBe('Auto');
    expect(back.aboveFloor).toBe(15);
    expect(back.enter).toBe(-60);
  });

  it('leaves the fields empty when the timer has reported no levels', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    const next = withMode(draft.levels, 'Manual', undefined);
    expect(next.enter).toBe('');
    expect(draftConfig({ ...draft, levels: next }, seatName)).toEqual({
      ok: false,
      problem: 'Fixed levels need an enter level.'
    });
  });

  it('is the same draft when the mode does not change', () => {
    const { levels } = draftFrom(AUTO, 'TenthsDb');
    expect(withMode(levels, 'Auto', undefined)).toBe(levels);
  });
});

describe('adding a seat', () => {
  it('adds the lowest seat number not in use, on, on a free Raceband channel', () => {
    const draft = draftFrom(AUTO, 'TenthsDb');
    expect(nextSeat(draft)).toBe('2');
    const next = addSeat(draft);
    expect(next.seats).toHaveLength(3);
    expect(next.seats[2]).toEqual({ seat: '2', mhz: 5732, enabled: true });
  });

  it('fills a gap and keeps the seats in number order', () => {
    const draft = draftFrom(
      {
        ...AUTO,
        seats: [
          { seat: '0', freq_mhz: 5658, enabled: true },
          { seat: '2', freq_mhz: 5732, enabled: true }
        ]
      },
      'TenthsDb'
    );
    expect(addSeat(draft).seats.map((seat) => seat.seat)).toEqual(['0', '1', '2']);
    expect(addSeat(draft).seats[1].mhz).toBe(5695);
  });

  it('starts an empty timer at seat 0 on R1', () => {
    const next = addSeat(draftFrom({ ...AUTO, seats: [] }, 'TenthsDb'));
    expect(next.seats).toEqual([{ seat: '0', mhz: 5658, enabled: true }]);
  });

  it('stops at eight seats', () => {
    let draft = draftFrom({ ...AUTO, seats: [] }, 'TenthsDb');
    for (let i = 0; i < 8; i += 1) draft = addSeat(draft);
    expect(draft.seats.map((seat) => seat.seat)).toEqual(['0', '1', '2', '3', '4', '5', '6', '7']);
    expect(nextSeat(draft)).toBeUndefined();
    expect(addSeat(draft)).toBe(draft);
  });
});

describe('the exit hold', () => {
  const name = () => 'Seat';

  it('is in the draft and sent back only for a timer that has it', () => {
    expect(draftFrom(AUTO, 'TenthsDb').exitHoldMs).toBeUndefined();
    const without = draftConfig(draftFrom(AUTO, 'TenthsDb'), name);
    expect(without.ok && 'exit_hold_ms' in without.config).toBe(false);

    const has: TimerConfig = { ...AUTO, exit_hold_ms: 30 };
    const draft = draftFrom(has, 'TenthsDb');
    expect(draft.exitHoldMs).toBe(30);
    const sent = draftConfig(draft, name);
    expect(sent.ok && sent.config.exit_hold_ms).toBe(30);
  });

  it('makes the draft dirty when it changes, and must be a whole number', () => {
    const has: TimerConfig = { ...AUTO, exit_hold_ms: 30 };
    const draft = draftFrom(has, 'TenthsDb');
    expect(isDirty(draft, has)).toBe(false);
    draft.exitHoldMs = 80;
    expect(isDirty(draft, has)).toBe(true);
    draft.exitHoldMs = '';
    expect(draftConfig(draft, name)).toEqual({
      ok: false,
      problem: 'The exit hold must be a whole number of milliseconds, 0 or more.'
    });
    for (const exitHoldMs of [12.5, -1, 'abc']) {
      expect(draftConfig({ ...draft, exitHoldMs }, name)).toEqual({
        ok: false,
        problem: 'The exit hold must be a whole number of milliseconds, 0 or more.'
      });
    }
    const zero = draftConfig({ ...draft, exitHoldMs: '0' }, name);
    expect(zero.ok && zero.config.exit_hold_ms).toBe(0);
  });
});

describe('the lap cooldown', () => {
  const name = () => 'Seat';
  const has: TimerConfig = { ...AUTO, lap_cooldown_ms: 2500 };
  const PROBLEM = 'The lap cooldown must be a number of seconds, 0 or more.';

  it('is in the draft, in seconds, only for a timer that has it', () => {
    expect('lapCooldownS' in draftFrom(AUTO, 'TenthsDb')).toBe(false);
    expect(draftFrom(has, 'TenthsDb').lapCooldownS).toBe(2.5);
    // A cooldown of 0 is still a setting the timer has.
    expect(draftFrom({ ...AUTO, lap_cooldown_ms: 0 }, 'TenthsDb').lapCooldownS).toBe(0);
  });

  it('is sent back in milliseconds only to a timer that has it', () => {
    const without = draftConfig(draftFrom(AUTO, 'TenthsDb'), name);
    if (!without.ok) throw new Error(without.problem);
    expect('lap_cooldown_ms' in without.config).toBe(false);

    const sent = draftConfig(draftFrom(has, 'TenthsDb'), name);
    if (!sent.ok) throw new Error(sent.problem);
    expect(sent.config.lap_cooldown_ms).toBe(2500);
  });

  it('carries an edit, typed or picked, to the nearest tenth of a second', () => {
    const sentFor = (lapCooldownS: number | string) => {
      const result = draftConfig({ ...draftFrom(has, 'TenthsDb'), lapCooldownS }, name);
      if (!result.ok) throw new Error(result.problem);
      return result.config.lap_cooldown_ms;
    };
    expect(sentFor(3)).toBe(3000);
    expect(sentFor('1.5')).toBe(1500);
    expect(sentFor(1.26)).toBe(1300);
    expect(sentFor(0)).toBe(0);
  });

  it('must be a number of seconds, 0 or more', () => {
    for (const lapCooldownS of ['', '  ', 'abc', -1, '-2.5']) {
      expect(draftConfig({ ...draftFrom(has, 'TenthsDb'), lapCooldownS }, name)).toEqual({
        ok: false,
        problem: PROBLEM
      });
    }
  });

  it('makes the draft dirty when it changes, and clean again when it is put back', () => {
    const draft = draftFrom(has, 'TenthsDb');
    expect(isDirty(draft, has)).toBe(false);
    draft.lapCooldownS = 3;
    expect(isDirty(draft, has)).toBe(true);
    draft.lapCooldownS = '2.5';
    expect(isDirty(draft, has)).toBe(false);
    draft.lapCooldownS = '';
    expect(isDirty(draft, has)).toBe(true);
  });

  it('is its own setting: changing it leaves the exit hold alone, and the other way round', () => {
    const both: TimerConfig = { ...AUTO, exit_hold_ms: 30, lap_cooldown_ms: 2500 };
    const draft = draftFrom(both, 'TenthsDb');
    draft.lapCooldownS = 4;
    expect(draftConfig(draft, name)).toMatchObject({
      ok: true,
      config: { exit_hold_ms: 30, lap_cooldown_ms: 4000 }
    });
    const other = draftFrom(both, 'TenthsDb');
    other.exitHoldMs = 50;
    expect(draftConfig(other, name)).toMatchObject({
      ok: true,
      config: { exit_hold_ms: 50, lap_cooldown_ms: 2500 }
    });
  });
});
