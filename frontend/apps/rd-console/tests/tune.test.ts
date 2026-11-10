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
  nextSeat,
  sameConfig,
  secondsToMs,
  tenthsToDb,
  unitLabel,
  withMode
} from '../src/lib/tune.js';

const AUTO: TimerConfig = {
  seats: [
    { seat: '0', freq_mhz: 5658, enabled: true },
    { seat: '1', freq_mhz: 5695, enabled: false }
  ],
  seats_per_node: 4,
  threshold: { Auto: { snr_enter: 150, hysteresis: 50 } },
  min_lap_ms: 2500
};
const MANUAL: TimerConfig = { ...AUTO, threshold: { Manual: { enter: -700, exit: -765 } } };
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
      seatsPerReceiver: 4,
      mode: 'Auto',
      aboveFloorDb: 15,
      belowEnterDb: 5,
      enterDb: '',
      exitDb: '',
      minLapSeconds: 2.5
    });
    const manual = draftFrom(MANUAL);
    expect(manual.mode).toBe('Manual');
    expect(manual.enterDb).toBe(-70);
    expect(manual.exitDb).toBe(-76.5);
    expect(manual.aboveFloorDb).toBe('');
  });

  it('turns back into the very same settings', () => {
    for (const config of [AUTO, MANUAL]) {
      expect(draftConfig(draftFrom(config), seatName)).toEqual({ ok: true, config });
    }
  });

  it('carries every edit into the settings it would send', () => {
    const draft = draftFrom(AUTO);
    draft.seats[1].enabled = true;
    draft.seats[1].mhz = '5800';
    draft.seatsPerReceiver = 2;
    draft.aboveFloorDb = '12.5';
    draft.belowEnterDb = 4;
    draft.minLapSeconds = '3.2';
    expect(draftConfig(draft, seatName)).toEqual({
      ok: true,
      config: {
        seats: [
          { seat: '0', freq_mhz: 5658, enabled: true },
          { seat: '1', freq_mhz: 5800, enabled: true }
        ],
        seats_per_node: 2,
        threshold: { Auto: { snr_enter: 125, hysteresis: 40 } },
        min_lap_ms: 3200
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
    expect(problemOf((d) => (d.seatsPerReceiver = 0))).toBe(
      'Seats per receiver must be a whole number, 1 or more.'
    );
    expect(problemOf((d) => (d.aboveFloorDb = ''))).toBe(
      'Say how far above the noise floor to enter.'
    );
    expect(problemOf((d) => (d.belowEnterDb = ''))).toBe('Say how far below enter to exit.');
    expect(problemOf((d) => (d.enterDb = ''), MANUAL)).toBe('Fixed levels need an enter level.');
    expect(problemOf((d) => (d.exitDb = ''), MANUAL)).toBe('Fixed levels need an exit level.');
    expect(problemOf((d) => (d.exitDb = -70), MANUAL)).toBe(
      'The exit level must be below the enter level.'
    );
    expect(problemOf((d) => (d.minLapSeconds = -1))).toBe(
      'The minimum lap time must be 0 seconds or more.'
    );
    expect(problemOf((d) => (d.minLapSeconds = ''))).toBe(
      'The minimum lap time must be 0 seconds or more.'
    );
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
    draft.aboveFloorDb = '';
    expect(isDirty(draft, AUTO)).toBe(true);
  });

  it('is dirty with a seat added, a mode changed, or a number changed', () => {
    expect(isDirty(addSeat(draftFrom(AUTO)), AUTO)).toBe(true);
    expect(isDirty(draftFrom(MANUAL), AUTO)).toBe(true);
    expect(isDirty({ ...draftFrom(AUTO), minLapSeconds: 2.6 }, AUTO)).toBe(true);
    expect(isDirty({ ...draftFrom(AUTO), seatsPerReceiver: 8 }, AUTO)).toBe(true);
  });

  it('sameConfig compares every part', () => {
    expect(sameConfig(AUTO, structuredClone(AUTO))).toBe(true);
    expect(sameConfig(AUTO, MANUAL)).toBe(false);
    expect(sameConfig(MANUAL, AUTO)).toBe(false);
    expect(sameConfig(AUTO, { ...AUTO, seats: AUTO.seats.slice(0, 1) })).toBe(false);
    expect(
      sameConfig(AUTO, { ...AUTO, threshold: { Auto: { snr_enter: 150, hysteresis: 60 } } })
    ).toBe(false);
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

  it('fills the fixed levels from the levels in use when going to fixed', () => {
    const next = withMode(draftFrom(AUTO), 'Manual', { enter: -770, exit: -820 });
    expect(next.mode).toBe('Manual');
    expect(next.enterDb).toBe(-77);
    expect(next.exitDb).toBe(-82);
    expect(draftConfig(next, seatName)).toMatchObject({
      ok: true,
      config: { threshold: { Manual: { enter: -770, exit: -820 } } }
    });
  });

  it('keeps numbers the director already typed, and what was there on the way back', () => {
    const typed = { ...draftFrom(AUTO), enterDb: -60, exitDb: '' };
    const next = withMode(typed, 'Manual', { enter: -770, exit: -820 });
    expect(next.enterDb).toBe(-60);
    expect(next.exitDb).toBe(-82);
    const back = withMode(next, 'Auto', { enter: -770, exit: -820 });
    expect(back.mode).toBe('Auto');
    expect(back.aboveFloorDb).toBe(15);
    expect(back.enterDb).toBe(-60);
  });

  it('leaves the fields empty when the timer has reported no levels', () => {
    const next = withMode(draftFrom(AUTO), 'Manual', undefined);
    expect(next.enterDb).toBe('');
    expect(draftConfig(next, seatName)).toEqual({
      ok: false,
      problem: 'Fixed levels need an enter level.'
    });
  });

  it('is the same draft when the mode does not change', () => {
    const draft = draftFrom(AUTO);
    expect(withMode(draft, 'Auto', undefined)).toBe(draft);
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
