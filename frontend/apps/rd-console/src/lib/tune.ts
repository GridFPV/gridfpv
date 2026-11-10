/**
 * The Tune page's numbers and its draft. Pure, so it can be tested alone.
 *
 * A timer's settings are edited as a **draft**. Nothing reaches the timer until the director
 * presses Apply, and then the whole of the settings goes at once. This module turns the
 * timer's settings into a draft, a draft back into settings (or a sentence saying what is
 * missing), and says whether the two differ.
 *
 * The wire holds thresholds in tenths of a dB and the minimum lap in milliseconds. People
 * read dB with one decimal and seconds with one decimal, so the draft holds those.
 */
import type {
  SeatLevels,
  SeatRef,
  SignalUnit,
  TimerConfig,
  TimerSeatConfig,
  TimerThreshold
} from '@gridfpv/types';
import { BANDS } from './channels.js';

/** What a form field holds: a number, or text (empty while nothing is typed). */
export type FieldValue = number | string;

/** How many seats a timer can have. They are numbered 0 to 7 on the wire and 1 to 8 on screen. */
export const MAX_SEATS = 8;

/** Shown where a number is not known yet. */
export const NO_VALUE = '—';

/** The number a field holds, or `undefined` when it is empty or not a number. */
export function fieldNumber(value: FieldValue | null | undefined): number | undefined {
  if (typeof value === 'number') return Number.isFinite(value) ? value : undefined;
  if (typeof value !== 'string' || value.trim() === '') return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

/** The unit's short name: "dB", "dBm", or nothing for plain counts. */
export function unitLabel(unit: SignalUnit): string {
  if (unit === 'TenthsDbm') return 'dBm';
  if (unit === 'TenthsDb') return 'dB';
  return '';
}

/**
 * A signal level in words: "-77.3 dB", or "-77.3 dBm" when the timer is calibrated. Counts
 * are shown as they are. A level that is not known is a dash.
 */
export function formatLevel(value: number | undefined, unit: SignalUnit): string {
  if (value === undefined) return NO_VALUE;
  if (unit === 'Counts') return String(value);
  return `${(value / 10).toFixed(1)} ${unitLabel(unit)}`;
}

/** A difference between two levels, from tenths of a dB: "15.0 dB". */
export function formatDb(tenths: number): string {
  return `${(tenths / 10).toFixed(1)} dB`;
}

/** Tenths of a dB as the dB a field shows: -773 is -77.3. */
export function tenthsToDb(tenths: number): number {
  return tenths / 10;
}

/** The dB a field holds as tenths of a dB, or `undefined` when the field has no number. */
export function dbToTenths(value: FieldValue | null | undefined): number | undefined {
  const db = fieldNumber(value);
  return db === undefined ? undefined : Math.round(db * 10);
}

/** Milliseconds as the seconds a field shows: 2500 is 2.5. */
export function msToSeconds(ms: number): number {
  return ms / 1000;
}

/** The seconds a field holds as milliseconds, to the nearest tenth of a second. */
export function secondsToMs(value: FieldValue | null | undefined): number | undefined {
  const seconds = fieldNumber(value);
  return seconds === undefined ? undefined : Math.round(seconds * 10) * 100;
}

// ── The draft ──────────────────────────────────────────────────────────────────

/** How the timer sets its levels: following the noise floor, or fixed. */
export type ThresholdMode = 'Auto' | 'Manual';

/** One seat in the draft. */
export interface DraftSeat {
  /** The seat's wire handle. Never shown. */
  seat: SeatRef;
  /** The frequency in MHz. */
  mhz: FieldValue;
  enabled: boolean;
}

/** The settings as the director is editing them. */
export interface TuneDraft {
  seats: DraftSeat[];
  seatsPerReceiver: FieldValue;
  mode: ThresholdMode;
  /** Follow the noise floor: enter this many dB above the floor. */
  aboveFloorDb: FieldValue;
  /** Follow the noise floor: exit this many dB below enter. */
  belowEnterDb: FieldValue;
  /** Fixed levels: the enter level in dB. */
  enterDb: FieldValue;
  /** Fixed levels: the exit level in dB. */
  exitDb: FieldValue;
  /** The minimum lap time in seconds. */
  minLapSeconds: FieldValue;
}

/** Which mode a threshold is in. */
export function thresholdMode(threshold: TimerThreshold): ThresholdMode {
  return 'Auto' in threshold ? 'Auto' : 'Manual';
}

/**
 * A draft that says what the timer's settings say. The fields of the mode that is not in use
 * start empty: the timer holds no values for them.
 */
export function draftFrom(config: TimerConfig): TuneDraft {
  const { threshold } = config;
  const draft: TuneDraft = {
    seats: config.seats.map((seat) => ({
      seat: seat.seat,
      mhz: seat.freq_mhz,
      enabled: seat.enabled
    })),
    seatsPerReceiver: config.seats_per_node,
    mode: thresholdMode(threshold),
    aboveFloorDb: '',
    belowEnterDb: '',
    enterDb: '',
    exitDb: '',
    minLapSeconds: msToSeconds(config.min_lap_ms)
  };
  if ('Auto' in threshold) {
    draft.aboveFloorDb = tenthsToDb(threshold.Auto.snr_enter);
    draft.belowEnterDb = tenthsToDb(threshold.Auto.hysteresis);
  } else {
    draft.enterDb = tenthsToDb(threshold.Manual.enter);
    draft.exitDb = tenthsToDb(threshold.Manual.exit);
  }
  return draft;
}

/** The settings a draft would send, or a sentence saying why it cannot be sent yet. */
export type DraftResult = { ok: true; config: TimerConfig } | { ok: false; problem: string };

const problem = (sentence: string): DraftResult => ({ ok: false, problem: sentence });

/**
 * Turn a draft into the settings to send. `seatName` gives the name people see for a seat
 * ("Seat 2"), so a sentence about a seat never carries its wire handle.
 *
 * Only what cannot be sent at all is caught here (an empty field, a level order that makes no
 * crossing). Whether the timer accepts a value is the timer's call, and its answer is shown
 * as it is.
 */
export function draftConfig(
  draft: TuneDraft,
  seatName: (seat: SeatRef, position: number) => string
): DraftResult {
  const seats: TimerSeatConfig[] = [];
  for (const [position, seat] of draft.seats.entries()) {
    const mhz = fieldNumber(seat.mhz);
    if (mhz === undefined || !Number.isInteger(mhz) || mhz <= 0) {
      return problem(`${seatName(seat.seat, position)} needs a frequency in whole MHz.`);
    }
    seats.push({ seat: seat.seat, freq_mhz: mhz, enabled: seat.enabled });
  }

  const perReceiver = fieldNumber(draft.seatsPerReceiver);
  if (perReceiver === undefined || !Number.isInteger(perReceiver) || perReceiver < 1) {
    return problem('Seats per receiver must be a whole number, 1 or more.');
  }

  let threshold: TimerThreshold;
  if (draft.mode === 'Auto') {
    const snrEnter = dbToTenths(draft.aboveFloorDb);
    const hysteresis = dbToTenths(draft.belowEnterDb);
    if (snrEnter === undefined) return problem('Say how far above the noise floor to enter.');
    if (hysteresis === undefined) return problem('Say how far below enter to exit.');
    threshold = { Auto: { snr_enter: snrEnter, hysteresis } };
  } else {
    const enter = dbToTenths(draft.enterDb);
    const exit = dbToTenths(draft.exitDb);
    if (enter === undefined) return problem('Fixed levels need an enter level.');
    if (exit === undefined) return problem('Fixed levels need an exit level.');
    if (exit >= enter) return problem('The exit level must be below the enter level.');
    threshold = { Manual: { enter, exit } };
  }

  const minLap = secondsToMs(draft.minLapSeconds);
  if (minLap === undefined || minLap < 0) {
    return problem('The minimum lap time must be 0 seconds or more.');
  }

  return {
    ok: true,
    config: { seats, seats_per_node: perReceiver, threshold, min_lap_ms: minLap }
  };
}

function sameThreshold(a: TimerThreshold, b: TimerThreshold): boolean {
  if ('Auto' in a) {
    return (
      'Auto' in b &&
      a.Auto.snr_enter === b.Auto.snr_enter &&
      a.Auto.hysteresis === b.Auto.hysteresis
    );
  }
  return 'Manual' in b && a.Manual.enter === b.Manual.enter && a.Manual.exit === b.Manual.exit;
}

/** Whether two sets of settings say the same thing. */
export function sameConfig(a: TimerConfig, b: TimerConfig): boolean {
  return (
    a.seats_per_node === b.seats_per_node &&
    a.min_lap_ms === b.min_lap_ms &&
    sameThreshold(a.threshold, b.threshold) &&
    a.seats.length === b.seats.length &&
    a.seats.every((seat, index) => {
      const other = b.seats[index];
      return (
        seat.seat === other.seat &&
        seat.freq_mhz === other.freq_mhz &&
        seat.enabled === other.enabled
      );
    })
  );
}

/**
 * Whether the draft differs from what the timer has. A draft that cannot be sent yet differs:
 * the director has changed something and it is not on the timer.
 */
export function isDirty(draft: TuneDraft, config: TimerConfig): boolean {
  const result = draftConfig(draft, () => 'Seat');
  return !result.ok || !sameConfig(result.config, config);
}

/**
 * The fixed levels to start from when the director switches from following the noise floor:
 * the levels the timer is using right now. Fixed levels are one setting for the whole timer,
 * while each seat follows its own floor, so the seat with the highest enter level is taken;
 * that level is above the noise on every seat. Only seats that are on count. `undefined` when
 * the timer has reported no levels.
 */
export function levelsInUse(
  levels: readonly SeatLevels[],
  config: TimerConfig
): { enter: number; exit: number } | undefined {
  const on = new Set(config.seats.filter((seat) => seat.enabled).map((seat) => seat.seat));
  let highest: SeatLevels | undefined;
  for (const seat of levels) {
    if (!on.has(seat.seat)) continue;
    if (!highest || seat.enter > highest.enter) highest = seat;
  }
  return highest ? { enter: highest.enter, exit: highest.exit } : undefined;
}

/**
 * The draft with its threshold mode changed. Going to fixed levels, empty enter and exit
 * fields are filled from the levels the timer is using right now (`inUse`); numbers the
 * director already typed are kept.
 */
export function withMode(
  draft: TuneDraft,
  mode: ThresholdMode,
  inUse: { enter: number; exit: number } | undefined
): TuneDraft {
  if (mode === draft.mode) return draft;
  const next: TuneDraft = { ...draft, mode };
  if (mode === 'Manual' && inUse) {
    if (fieldNumber(next.enterDb) === undefined) next.enterDb = tenthsToDb(inUse.enter);
    if (fieldNumber(next.exitDb) === undefined) next.exitDb = tenthsToDb(inUse.exit);
  }
  return next;
}

/**
 * The handle of the next seat to add: the lowest seat number, 0 to 7, the draft does not
 * have. `undefined` when the draft already has every seat.
 */
export function nextSeat(draft: TuneDraft): SeatRef | undefined {
  const taken = new Set(draft.seats.map((seat) => seat.seat));
  for (let number = 0; number < MAX_SEATS; number += 1) {
    if (!taken.has(String(number))) return String(number);
  }
  return undefined;
}

/**
 * The draft with one more seat, turned on, on the first Raceband channel no seat in the draft
 * is on. The same draft when there is no seat left to add. Seats stay in number order.
 */
export function addSeat(draft: TuneDraft): TuneDraft {
  const seat = nextSeat(draft);
  if (seat === undefined) return draft;
  const inUse = new Set(draft.seats.map((existing) => fieldNumber(existing.mhz)));
  const raceband = BANDS[0].mhz;
  const mhz = raceband.find((candidate) => !inUse.has(candidate)) ?? raceband[0];
  const seats = [...draft.seats, { seat, mhz, enabled: true }].sort(
    (a, b) => Number(a.seat) - Number(b.seat)
  );
  return { ...draft, seats };
}
