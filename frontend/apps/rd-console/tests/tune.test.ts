/** The Tune page's numbers and its draft: formatting, draft ↔ settings, dirty, mode, seats. */
import { describe, expect, it } from 'vitest';
import type { SeatLevels, TimerConfig } from '@gridfpv/types';
import {
  addSeat,
  dbToTenths,
  draftConfig,
  draftFrom,
  fieldNumber,
  formatDb,
  formatLevel,
  isDirty,
  levelsInUse,
  msToSeconds,
  levelsFrom,
  nextSeat,
  ownLevels,
  sameConfig,
  secondsToMs,
  tenthsToDb,
  unitLabel,
  withMode,
  type DraftSeat
} from '../src/lib/tune.js';

const AUTO: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5695, enabled: false }
  ],
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } },
  set_on_timer: { min_lap_ms: 2500 }
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

  it('writes a difference in dB', () => {
    expect(formatDb(150)).toBe('15.0 dB');
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
    expect(tenthsToDb(-773)).toBe(-77.3);
    expect(dbToTenths(-77.3)).toBe(-773);
    expect(dbToTenths('15')).toBe(150);
    expect(dbToTenths(0.1 + 0.2)).toBe(3);
    expect(dbToTenths('')).toBeUndefined();
  });

  it('converts seconds to milliseconds, to one decimal of a second', () => {
    expect(msToSeconds(2500)).toBe(2.5);
    expect(secondsToMs(2.5)).toBe(2500);
    expect(secondsToMs('3')).toBe(3000);
    expect(secondsToMs(1.26)).toBe(1300);
    expect(secondsToMs('')).toBeUndefined();
  });
});

describe('the draft', () => {
  it('says what the settings say, in dB and seconds', () => {
    expect(draftFrom(AUTO)).toEqual({
      seats: [
        { seat: '0', mhz: 5658, enabled: true },
        { seat: '1', mhz: 5695, enabled: false }
      ],
      levels: { mode: 'Auto', aboveFloorDb: 15, belowEnterDb: 5, enterDb: '', exitDb: '' }
    });
    const manual = draftFrom(MANUAL).levels;
    expect(manual.mode).toBe('Manual');
    expect(manual.enterDb).toBe(-70);
    expect(manual.exitDb).toBe(-76.5);
    expect(manual.aboveFloorDb).toBe('');
  });

  it("holds a seat's own levels only for a seat that has them", () => {
    const draft = draftFrom(OWN);
    expect('levels' in draft.seats[0]).toBe(false);
    expect(draft.seats[1].levels).toEqual({
      mode: 'Manual',
      aboveFloorDb: '',
      belowEnterDb: '',
      enterDb: -60,
      exitDb: -66
    });
    expect(levelsFrom({ Auto: { snr_enter: 120, hysteresis: 45 } })).toEqual({
      mode: 'Auto',
      aboveFloorDb: 12,
      belowEnterDb: 4.5,
      enterDb: '',
      exitDb: ''
    });
  });

  it('turns back into the very same settings', () => {
    for (const config of [AUTO, MANUAL, OWN]) {
      // All but what is set on the timer itself: that is shown, and never sent.
      const { set_on_timer: _shown, ...sent } = config;
      expect(draftConfig(draftFrom(config), seatName)).toEqual({ ok: true, config: sent });
    }
  });

  it('carries every edit into the settings it would send', () => {
    const draft = draftFrom(AUTO);
    draft.seats[1].enabled = true;
    draft.seats[1].mhz = '5800';
    draft.levels.aboveFloorDb = '12.5';
    draft.levels.belowEnterDb = 4;
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
      const draft = draftFrom(from);
      change(draft);
      const result = draftConfig(draft, seatName);
      return result.ok ? undefined : result.problem;
    };
    expect(problemOf((d) => (d.seats[1].mhz = ''))).toBe('Seat 2 needs a frequency in whole MHz.');
    expect(problemOf((d) => (d.seats[0].mhz = 5658.5))).toBe(
      'Seat 1 needs a frequency in whole MHz.'
    );
    expect(problemOf((d) => (d.levels.aboveFloorDb = ''))).toBe(
      'Say how far above the noise floor to enter.'
    );
    expect(problemOf((d) => (d.levels.belowEnterDb = ''))).toBe('Say how far below enter to exit.');
    expect(problemOf((d) => (d.levels.enterDb = ''), MANUAL)).toBe(
      'Fixed levels need an enter level.'
    );
    expect(problemOf((d) => (d.levels.exitDb = ''), MANUAL)).toBe(
      'Fixed levels need an exit level.'
    );
    expect(problemOf((d) => (d.levels.exitDb = -70), MANUAL)).toBe(
      'The exit level must be below the enter level.'
    );
  });
});

describe("a seat's own levels in the draft", () => {
  it('sends a threshold only for a seat with its own, and no key at all for the others', () => {
    const draft = draftFrom(AUTO);
    draft.seats[1].levels = {
      mode: 'Auto',
      aboveFloorDb: 12,
      belowEnterDb: '4.5',
      enterDb: '',
      exitDb: ''
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
    const draft = draftFrom(OWN);
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
    const draft = draftFrom(OWN);
    draft.seats[1].levels = undefined;
    const result = draftConfig(draft, seatName);
    if (!result.ok) throw new Error(result.problem);
    expect('threshold' in result.config.seats[1]).toBe(false);
  });

  it('never sends one to a timer that does not take them', () => {
    const result = draftConfig(draftFrom(OWN), seatName, false);
    if (!result.ok) throw new Error(result.problem);
    expect(result.config.seats.some((seat) => 'threshold' in seat)).toBe(false);
    // A hole in levels that will not be sent is not a problem either.
    const holed = draftFrom(OWN);
    holed.seats[1].levels!.enterDb = '';
    expect(draftConfig(holed, seatName, false).ok).toBe(true);
  });

  it('says what is missing, naming the seat by its name', () => {
    const problemOf = (change: (levels: NonNullable<DraftSeat['levels']>) => void) => {
      const draft = draftFrom(OWN);
      change(draft.seats[1].levels!);
      const result = draftConfig(draft, seatName);
      return result.ok ? undefined : result.problem;
    };
    expect(problemOf((own) => (own.enterDb = ''))).toBe('Seat 2 needs an enter level.');
    expect(problemOf((own) => (own.exitDb = ''))).toBe('Seat 2 needs an exit level.');
    expect(problemOf((own) => (own.exitDb = -60))).toBe(
      'On Seat 2, the exit level must be below the enter level.'
    );
    expect(problemOf((own) => (own.mode = 'Auto'))).toBe(
      'Say how far above the noise floor Seat 2 enters.'
    );
    expect(
      problemOf((own) => {
        own.mode = 'Auto';
        own.aboveFloorDb = 12;
      })
    ).toBe('Say how far below enter Seat 2 exits.');
  });

  it("starts a seat's own levels the way the timer's are set, with the seat's levels right now as its fixed levels", () => {
    const timer = draftFrom(AUTO).levels;
    const own = ownLevels(timer, { enter: -771, exit: -821 });
    expect(own).toEqual({
      mode: 'Auto',
      aboveFloorDb: 15,
      belowEnterDb: 5,
      enterDb: -77.1,
      exitDb: -82.1
    });
    expect(own).not.toBe(timer);
    // Going to fixed levels keeps what was filled in.
    expect(withMode(own, 'Manual', undefined)).toMatchObject({ enterDb: -77.1, exitDb: -82.1 });
    // With no live levels the timer's fixed levels in the draft stand in.
    expect(ownLevels(draftFrom(MANUAL).levels, undefined)).toEqual(draftFrom(MANUAL).levels);
    expect(ownLevels(draftFrom(MANUAL).levels, { enter: -650, exit: -700 })).toMatchObject({
      mode: 'Manual',
      enterDb: -65,
      exitDb: -70
    });
  });
});

describe('dirty', () => {
  it('is clean straight from the timer', () => {
    expect(isDirty(draftFrom(AUTO), AUTO)).toBe(false);
    expect(isDirty(draftFrom(MANUAL), MANUAL)).toBe(false);
  });

  it('is dirty after any edit, and clean again when the edit is undone', () => {
    const draft = draftFrom(AUTO);
    draft.seats[0].mhz = 5800;
    expect(isDirty(draft, AUTO)).toBe(true);
    draft.seats[0].mhz = '5658';
    expect(isDirty(draft, AUTO)).toBe(false);
    draft.seats[1].enabled = true;
    expect(isDirty(draft, AUTO)).toBe(true);
  });

  it('is dirty when the draft cannot be sent yet', () => {
    const draft = draftFrom(AUTO);
    draft.levels.aboveFloorDb = '';
    expect(isDirty(draft, AUTO)).toBe(true);
  });

  it("sees a seat's own levels given, changed and taken away, and is clean again after a discard", () => {
    const draft = draftFrom(AUTO);
    draft.seats[0].levels = ownLevels(draft.levels, undefined);
    // Same numbers as the timer's, but the seat now carries its own: that is a change.
    expect(isDirty(draft, AUTO)).toBe(true);
    draft.seats[0].levels = undefined;
    expect(isDirty(draft, AUTO)).toBe(false);

    const own = draftFrom(OWN);
    expect(isDirty(own, OWN)).toBe(false);
    own.seats[1].levels!.enterDb = -58;
    expect(isDirty(own, OWN)).toBe(true);
    own.seats[1].levels!.enterDb = '';
    expect(isDirty(own, OWN)).toBe(true);
    own.seats[1].levels = undefined;
    expect(isDirty(own, OWN)).toBe(true);
    // Discard is a new draft from what the timer has.
    expect(draftFrom(OWN).seats[1].levels?.enterDb).toBe(-60);
    expect(isDirty(draftFrom(OWN), OWN)).toBe(false);
  });

  it("does not count a seat's levels on a timer that does not take them", () => {
    const draft = draftFrom(AUTO);
    draft.seats[0].levels = ownLevels(draft.levels, undefined);
    expect(isDirty(draft, AUTO, false)).toBe(false);
  });

  it('is dirty with a seat added, a mode changed, or a number changed', () => {
    expect(isDirty(addSeat(draftFrom(AUTO)), AUTO)).toBe(true);
    expect(isDirty(draftFrom(MANUAL), AUTO)).toBe(true);
    expect(isDirty(draftFrom(OWN), AUTO)).toBe(true);
    // What is set on the timer itself is not the draft's to change.
    expect(isDirty(draftFrom(AUTO), { ...AUTO, set_on_timer: { min_lap_ms: 0 } })).toBe(false);
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
});

describe('switching how the levels are set', () => {
  const LEVELS: SeatLevels[] = [
    { seat: '0', level: -880, floor: -920, enter: -770, exit: -820, crossing: false },
    // Seat 1 has the higher enter level, but it is off, so it does not count.
    { seat: '1', level: -700, floor: -800, enter: -650, exit: -700, crossing: false }
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
    const draft = draftFrom(AUTO);
    const next = withMode(draft.levels, 'Manual', { enter: -770, exit: -820 });
    expect(next.mode).toBe('Manual');
    expect(next.enterDb).toBe(-77);
    expect(next.exitDb).toBe(-82);
    expect(draftConfig({ ...draft, levels: next }, seatName)).toMatchObject({
      ok: true,
      config: { threshold: { Manual: { enter: -770, exit: -820 } } }
    });
  });

  it('keeps numbers the director already typed, and what was there on the way back', () => {
    const typed = { ...draftFrom(AUTO).levels, enterDb: -60, exitDb: '' };
    const next = withMode(typed, 'Manual', { enter: -770, exit: -820 });
    expect(next.enterDb).toBe(-60);
    expect(next.exitDb).toBe(-82);
    const back = withMode(next, 'Auto', { enter: -770, exit: -820 });
    expect(back.mode).toBe('Auto');
    expect(back.aboveFloorDb).toBe(15);
    expect(back.enterDb).toBe(-60);
  });

  it('leaves the fields empty when the timer has reported no levels', () => {
    const draft = draftFrom(AUTO);
    const next = withMode(draft.levels, 'Manual', undefined);
    expect(next.enterDb).toBe('');
    expect(draftConfig({ ...draft, levels: next }, seatName)).toEqual({
      ok: false,
      problem: 'Fixed levels need an enter level.'
    });
  });

  it('is the same draft when the mode does not change', () => {
    const { levels } = draftFrom(AUTO);
    expect(withMode(levels, 'Auto', undefined)).toBe(levels);
  });
});

describe('adding a seat', () => {
  it('adds the lowest seat number not in use, on, on a free Raceband channel', () => {
    const draft = draftFrom(AUTO);
    expect(nextSeat(draft)).toBe('2');
    const next = addSeat(draft);
    expect(next.seats).toHaveLength(3);
    expect(next.seats[2]).toEqual({ seat: '2', mhz: 5732, enabled: true });
  });

  it('fills a gap and keeps the seats in number order', () => {
    const draft = draftFrom({
      ...AUTO,
      seats: [
        { seat: '0', freq_mhz: 5658, enabled: true },
        { seat: '2', freq_mhz: 5732, enabled: true }
      ]
    });
    expect(addSeat(draft).seats.map((seat) => seat.seat)).toEqual(['0', '1', '2']);
    expect(addSeat(draft).seats[1].mhz).toBe(5695);
  });

  it('starts an empty timer at seat 0 on R1', () => {
    const next = addSeat(draftFrom({ ...AUTO, seats: [] }));
    expect(next.seats).toEqual([{ seat: '0', mhz: 5658, enabled: true }]);
  });

  it('stops at eight seats', () => {
    let draft = draftFrom({ ...AUTO, seats: [] });
    for (let i = 0; i < 8; i += 1) draft = addSeat(draft);
    expect(draft.seats.map((seat) => seat.seat)).toEqual(['0', '1', '2', '3', '4', '5', '6', '7']);
    expect(nextSeat(draft)).toBeUndefined();
    expect(addSeat(draft)).toBe(draft);
  });
});

describe('the exit hold', () => {
  const name = () => 'Seat';

  it('is in the draft and sent back only for a timer that has it', () => {
    expect(draftFrom(AUTO).exitHoldMs).toBeUndefined();
    const without = draftConfig(draftFrom(AUTO), name);
    expect(without.ok && 'exit_hold_ms' in without.config).toBe(false);

    const has: TimerConfig = { ...AUTO, exit_hold_ms: 30 };
    const draft = draftFrom(has);
    expect(draft.exitHoldMs).toBe(30);
    const sent = draftConfig(draft, name);
    expect(sent.ok && sent.config.exit_hold_ms).toBe(30);
  });

  it('makes the draft dirty when it changes, and must be a whole number', () => {
    const has: TimerConfig = { ...AUTO, exit_hold_ms: 30 };
    const draft = draftFrom(has);
    expect(isDirty(draft, has)).toBe(false);
    draft.exitHoldMs = 80;
    expect(isDirty(draft, has)).toBe(true);
    draft.exitHoldMs = '';
    expect(draftConfig(draft, name)).toEqual({
      ok: false,
      problem: 'The exit hold must be a whole number of milliseconds, 0 or more.'
    });
  });
});
