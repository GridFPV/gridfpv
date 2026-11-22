/**
 * The Seats page's numbers and its draft. Pure, so it can be tested alone.
 *
 * A timer's settings are edited as a **draft**. Nothing reaches the timer until the director
 * presses Apply, and then the whole of the settings goes at once. This module turns the
 * timer's settings into a draft, a draft back into settings (or a sentence saying what is
 * missing), and says whether the two differ.
 *
 * The wire holds thresholds as whole numbers in the timer's own unit: tenths of a dB on most
 * timers, millivolts on one whose receivers read a signal-strength pin. People read dB with
 * one decimal and millivolts whole, so the draft holds what people read, and remembers which
 * unit that is. Nothing here assumes dB.
 *
 * The timer's **lap cooldown** is one of its settings: after a crossing on a seat it reports
 * no other on that seat for that long, which swallows the bounce of one pass. It is not the
 * race's minimum lap. That is the event's lap rule, and Grid applies it to what the timer
 * reports.
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

/** The unit's short name: "dB", "dBm", "mV", or nothing for plain counts. */
export function unitLabel(unit: SignalUnit): string {
  if (unit === 'TenthsDbm') return 'dBm';
  if (unit === 'TenthsDb') return 'dB';
  if (unit === 'Millivolts') return 'mV';
  return '';
}

/**
 * The unit of a **difference** between two levels: the gap between two dBm levels is dB.
 * Millivolts and counts stay what they are.
 */
export function gapLabel(unit: SignalUnit): string {
  return unit === 'TenthsDbm' ? 'dB' : unitLabel(unit);
}

/**
 * How many of the unit's wire counts make one of what people read: 10 for tenths of a dB
 * (read as dB with one decimal), 1 for millivolts and plain counts.
 */
export function unitScale(unit: SignalUnit): number {
  return unit === 'TenthsDb' || unit === 'TenthsDbm' ? 10 : 1;
}

/** A wire number in words, with the label given: "-77.3 dB", "742 mV", "120". */
function inWords(value: number, unit: SignalUnit, label: string): string {
  const scale = unitScale(unit);
  const number = scale === 1 ? String(value) : (value / scale).toFixed(1);
  return label ? `${number} ${label}` : number;
}

/**
 * A signal level in words: "-77.3 dB", "-77.3 dBm" when the timer is calibrated, "742 mV"
 * for a timer that measures millivolts. Counts are shown as they are. A level that is not
 * known is a dash.
 */
export function formatLevel(value: number | undefined, unit: SignalUnit): string {
  if (value === undefined) return NO_VALUE;
  return inWords(value, unit, unitLabel(unit));
}

/** A difference between two levels, in words: "15.0 dB", "150 mV". */
export function formatGap(value: number, unit: SignalUnit): string {
  return inWords(value, unit, gapLabel(unit));
}

/** A wire level as the number a field shows: -773 tenths of a dB is -77.3; 742 mV is 742. */
export function toShown(value: number, unit: SignalUnit): number {
  return value / unitScale(unit);
}

/** The number a field holds as a wire level, or `undefined` when the field has no number. */
export function fromShown(
  value: FieldValue | null | undefined,
  unit: SignalUnit
): number | undefined {
  const shown = fieldNumber(value);
  return shown === undefined ? undefined : Math.round(shown * unitScale(unit));
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
  /**
   * The unit the four numbers below are read in. They are what people read (dB with a
   * decimal, whole millivolts), never the wire's counts.
   */
  unit: SignalUnit;
  /** Follow the noise floor: enter this far above the floor. */
  aboveFloor: FieldValue;
  /** Follow the noise floor: exit this far below enter. */
  belowEnter: FieldValue;
  /** Fixed levels: the enter level. */
  enter: FieldValue;
  /** Fixed levels: the exit level. */
  exit: FieldValue;
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
  /** The unit every level in the draft is read in: the timer's. */
  unit: SignalUnit;
  seats: DraftSeat[];
  /** The timer's levels: what every seat with none of its own uses. */
  levels: LevelsDraft;
  /**
   * How long the signal must stay below the exit level before a crossing closes, in
   * milliseconds. `undefined` on a timer that does not have the setting.
   */
  exitHoldMs?: FieldValue;
  /**
   * The lap cooldown, in seconds. `undefined` on a timer that does not have the setting.
   */
  lapCooldownS?: FieldValue;
}

/** Which mode a threshold is in. */
export function thresholdMode(threshold: TimerThreshold): ThresholdMode {
  return 'Auto' in threshold ? 'Auto' : 'Manual';
}

/**
 * Levels that say what a threshold says. The fields of the mode that is not in use start
 * empty: the timer holds no values for them.
 */
export function levelsFrom(threshold: TimerThreshold, unit: SignalUnit): LevelsDraft {
  const levels: LevelsDraft = {
    mode: thresholdMode(threshold),
    unit,
    aboveFloor: '',
    belowEnter: '',
    enter: '',
    exit: ''
  };
  if ('Auto' in threshold) {
    levels.aboveFloor = toShown(threshold.Auto.snr_enter, unit);
    levels.belowEnter = toShown(threshold.Auto.hysteresis, unit);
  } else {
    levels.enter = toShown(threshold.Manual.enter, unit);
    levels.exit = toShown(threshold.Manual.exit, unit);
  }
  return levels;
}

/**
 * A draft that says what the timer's settings say. A seat has levels in the draft only when
 * it has its own on the timer. `unit` is what the timer's levels measure: the settings are
 * bare numbers, and only the timer says what they count.
 */
export function draftFrom(config: TimerConfig, unit: SignalUnit): TuneDraft {
  return {
    unit,
    seats: config.seats.map((seat) => ({
      seat: seat.seat,
      mhz: seat.freq_mhz,
      enabled: seat.enabled,
      ...(seat.threshold ? { levels: levelsFrom(seat.threshold, unit) } : {})
    })),
    levels: levelsFrom(config.threshold, unit),
    ...(config.exit_hold_ms !== undefined ? { exitHoldMs: config.exit_hold_ms } : {}),
    ...(config.lap_cooldown_ms !== undefined
      ? { lapCooldownS: msToSeconds(config.lap_cooldown_ms) }
      : {})
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
    const snrEnter = fromShown(levels.aboveFloor, levels.unit);
    const hysteresis = fromShown(levels.belowEnter, levels.unit);
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
  const enter = fromShown(levels.enter, levels.unit);
  const exit = fromShown(levels.exit, levels.unit);
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

  const timerWide = thresholdFrom(draft.levels);
  if (!timerWide.ok) return problem(timerWide.problem);

  const config: TimerConfig = {
    seats,
    threshold: timerWide.threshold
  };
  // Sent only to a timer that has the setting. Left out, the timer would go back to its
  // default, so a timer that has it always gets a number.
  if (draft.exitHoldMs !== undefined) {
    const exitHold = fieldNumber(draft.exitHoldMs);
    if (exitHold === undefined || !Number.isInteger(exitHold) || exitHold < 0) {
      return problem('The exit hold must be a whole number of milliseconds, 0 or more.');
    }
    config.exit_hold_ms = exitHold;
  }
  if (draft.lapCooldownS !== undefined) {
    const cooldown = secondsToMs(draft.lapCooldownS);
    if (cooldown === undefined || cooldown < 0) {
      return problem('The lap cooldown must be a number of seconds, 0 or more.');
    }
    config.lap_cooldown_ms = cooldown;
  }

  return { ok: true, config };
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
    a.exit_hold_ms === b.exit_hold_ms &&
    a.lap_cooldown_ms === b.lap_cooldown_ms &&
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

/** An enter and an exit level, as the wire holds them (the timer's own unit). */
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
    if (fieldNumber(next.enter) === undefined) next.enter = toShown(inUse.enter, next.unit);
    if (fieldNumber(next.exit) === undefined) next.exit = toShown(inUse.exit, next.unit);
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
  return {
    ...timer,
    enter: toShown(inUse.enter, timer.unit),
    exit: toShown(inUse.exit, timer.unit)
  };
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
