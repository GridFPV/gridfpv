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

/**
 * Enter and exit levels as the director is editing them: the timer's, or one seat's own. The
 * fields of the mode that is not in use are kept, so switching back loses nothing typed.
 */
export interface LevelsDraft {
  mode: ThresholdMode;
  /** Follow the noise floor: enter this many dB above the floor. */
  aboveFloorDb: FieldValue;
  /** Follow the noise floor: exit this many dB below enter. */
  belowEnterDb: FieldValue;
  /** Fixed levels: the enter level in dB. */
  enterDb: FieldValue;
  /** Fixed levels: the exit level in dB. */
  exitDb: FieldValue;
}

/** One seat in the draft. */
export interface DraftSeat {
  /** The seat's wire handle. Never shown. */
  seat: SeatRef;
  /** The frequency in MHz. */
  mhz: FieldValue;
  enabled: boolean;
  /** The seat's own levels. Absent means the seat uses the timer's. */
  levels?: LevelsDraft;
}

/** The settings as the director is editing them. */
export interface TuneDraft {
  seats: DraftSeat[];
  seatsPerReceiver: FieldValue;
  /** The timer's levels: what every seat with none of its own uses. */
  levels: LevelsDraft;
  /** The minimum lap time in seconds. */
  minLapSeconds: FieldValue;
}

/** Which mode a threshold is in. */
export function thresholdMode(threshold: TimerThreshold): ThresholdMode {
  return 'Auto' in threshold ? 'Auto' : 'Manual';
}

/**
 * Levels that say what a threshold says. The fields of the mode that is not in use start
 * empty: the timer holds no values for them.
 */
export function levelsFrom(threshold: TimerThreshold): LevelsDraft {
  const levels: LevelsDraft = {
    mode: thresholdMode(threshold),
    aboveFloorDb: '',
    belowEnterDb: '',
    enterDb: '',
    exitDb: ''
  };
  if ('Auto' in threshold) {
    levels.aboveFloorDb = tenthsToDb(threshold.Auto.snr_enter);
    levels.belowEnterDb = tenthsToDb(threshold.Auto.hysteresis);
  } else {
    levels.enterDb = tenthsToDb(threshold.Manual.enter);
    levels.exitDb = tenthsToDb(threshold.Manual.exit);
  }
  return levels;
}

/**
 * A draft that says what the timer's settings say. A seat has levels in the draft only when
 * it has its own on the timer.
 */
export function draftFrom(config: TimerConfig): TuneDraft {
  return {
    seats: config.seats.map((seat) => ({
      seat: seat.seat,
      mhz: seat.freq_mhz,
      enabled: seat.enabled,
      ...(seat.threshold ? { levels: levelsFrom(seat.threshold) } : {})
    })),
    seatsPerReceiver: config.seats_per_node,
    levels: levelsFrom(config.threshold),
    minLapSeconds: msToSeconds(config.min_lap_ms)
  };
}

/** The settings a draft would send, or a sentence saying why it cannot be sent yet. */
export type DraftResult = { ok: true; config: TimerConfig } | { ok: false; problem: string };

const problem = (sentence: string): DraftResult => ({ ok: false, problem: sentence });

/**
 * The threshold a set of levels would send, or a sentence saying what is missing. `who` is
 * the name people see for the seat whose own levels these are ("Seat 2"); leave it out for
 * the timer's levels.
 */
function thresholdFrom(
  levels: LevelsDraft,
  who?: string
): { ok: true; threshold: TimerThreshold } | { ok: false; problem: string } {
  const missing = (sentence: string) => ({ ok: false as const, problem: sentence });
  if (levels.mode === 'Auto') {
    const snrEnter = dbToTenths(levels.aboveFloorDb);
    const hysteresis = dbToTenths(levels.belowEnterDb);
    if (snrEnter === undefined) {
      return missing(
        who
          ? `Say how far above the noise floor ${who} enters.`
          : 'Say how far above the noise floor to enter.'
      );
    }
    if (hysteresis === undefined) {
      return missing(
        who ? `Say how far below enter ${who} exits.` : 'Say how far below enter to exit.'
      );
    }
    return { ok: true, threshold: { Auto: { snr_enter: snrEnter, hysteresis } } };
  }
  const enter = dbToTenths(levels.enterDb);
  const exit = dbToTenths(levels.exitDb);
  if (enter === undefined) {
    return missing(who ? `${who} needs an enter level.` : 'Fixed levels need an enter level.');
  }
  if (exit === undefined) {
    return missing(who ? `${who} needs an exit level.` : 'Fixed levels need an exit level.');
  }
  if (exit >= enter) {
    return missing(
      who
        ? `On ${who}, the exit level must be below the enter level.`
        : 'The exit level must be below the enter level.'
    );
  }
  return { ok: true, threshold: { Manual: { enter, exit } } };
}

/**
 * Turn a draft into the settings to send. `seatName` gives the name people see for a seat
 * ("Seat 2"), so a sentence about a seat never carries its wire handle.
 *
 * The timer replaces the whole of its settings each time, so every seat with its own levels
 * carries them, and a seat with none carries no `threshold` at all. `seatLevels` says whether
 * the timer takes levels per seat; when it does not, none is ever sent.
 *
 * Only what cannot be sent at all is caught here (an empty field, a level order that makes no
 * crossing). Whether the timer accepts a value is the timer's call, and its answer is shown
 * as it is.
 */
export function draftConfig(
  draft: TuneDraft,
  seatName: (seat: SeatRef, position: number) => string,
  seatLevels = true
): DraftResult {
  const seats: TimerSeatConfig[] = [];
  for (const [position, seat] of draft.seats.entries()) {
    const name = seatName(seat.seat, position);
    const mhz = fieldNumber(seat.mhz);
    if (mhz === undefined || !Number.isInteger(mhz) || mhz <= 0) {
      return problem(`${name} needs a frequency in whole MHz.`);
    }
    const sent: TimerSeatConfig = { seat: seat.seat, freq_mhz: mhz, enabled: seat.enabled };
    if (seatLevels && seat.levels) {
      const own = thresholdFrom(seat.levels, name);
      if (!own.ok) return problem(own.problem);
      sent.threshold = own.threshold;
    }
    seats.push(sent);
  }

  const perReceiver = fieldNumber(draft.seatsPerReceiver);
  if (perReceiver === undefined || !Number.isInteger(perReceiver) || perReceiver < 1) {
    return problem('Seats per receiver must be a whole number, 1 or more.');
  }

  const timerWide = thresholdFrom(draft.levels);
  if (!timerWide.ok) return problem(timerWide.problem);

  const minLap = secondsToMs(draft.minLapSeconds);
  if (minLap === undefined || minLap < 0) {
    return problem('The minimum lap time must be 0 seconds or more.');
  }

  return {
    ok: true,
    config: {
      seats,
      seats_per_node: perReceiver,
      threshold: timerWide.threshold,
      min_lap_ms: minLap
    }
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
        seat.enabled === other.enabled &&
        (seat.threshold && other.threshold
          ? sameThreshold(seat.threshold, other.threshold)
          : !seat.threshold && !other.threshold)
      );
    })
  );
}

/**
 * Whether the draft differs from what the timer has. A draft that cannot be sent yet differs:
 * the director has changed something and it is not on the timer. `seatLevels` is as for
 * {@link draftConfig}.
 */
export function isDirty(draft: TuneDraft, config: TimerConfig, seatLevels = true): boolean {
  const result = draftConfig(draft, () => 'Seat', seatLevels);
  return !result.ok || !sameConfig(result.config, config);
}

/** An enter and an exit level, in tenths of a dB. */
export interface EnterExit {
  enter: number;
  exit: number;
}

/**
 * The fixed levels to start from when the director switches the timer's levels from following
 * the noise floor: the levels the timer is using right now. Fixed levels are one setting for
 * every seat that uses them, while each seat follows its own floor, so the seat with the
 * highest enter level is taken; that level is above the noise on every seat. Only seats that
 * are on and use the timer's levels count: a seat with its own says nothing about the timer's.
 * `undefined` when the timer has reported no levels for such a seat.
 */
export function levelsInUse(
  levels: readonly SeatLevels[],
  config: TimerConfig
): EnterExit | undefined {
  const counted = new Set(
    config.seats.filter((seat) => seat.enabled && !seat.threshold).map((seat) => seat.seat)
  );
  let highest: SeatLevels | undefined;
  for (const seat of levels) {
    if (!counted.has(seat.seat)) continue;
    if (!highest || seat.enter > highest.enter) highest = seat;
  }
  return highest ? { enter: highest.enter, exit: highest.exit } : undefined;
}

/**
 * Levels with their mode changed. Going to fixed levels, empty enter and exit fields are
 * filled from the levels in use right now (`inUse`); numbers the director already typed are
 * kept.
 */
export function withMode(
  levels: LevelsDraft,
  mode: ThresholdMode,
  inUse: EnterExit | undefined
): LevelsDraft {
  if (mode === levels.mode) return levels;
  const next: LevelsDraft = { ...levels, mode };
  if (mode === 'Manual' && inUse) {
    if (fieldNumber(next.enterDb) === undefined) next.enterDb = tenthsToDb(inUse.enter);
    if (fieldNumber(next.exitDb) === undefined) next.exitDb = tenthsToDb(inUse.exit);
  }
  return next;
}

/**
 * The levels a seat starts from when the director gives it its own. It starts the way the
 * timer's levels are set in the draft, so nothing changes until a number does. Its fixed enter
 * and exit are the levels the seat is using right now (`inUse`, from the live feed), so going
 * to fixed levels starts from what is on the trace. Without live levels the timer's fixed
 * levels in the draft stand in.
 */
export function ownLevels(timer: LevelsDraft, inUse: EnterExit | undefined): LevelsDraft {
  if (!inUse) return { ...timer };
  return { ...timer, enterDb: tenthsToDb(inUse.enter), exitDb: tenthsToDb(inUse.exit) };
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
