/**
 * Lap callouts on the Run page: each new lap is spoken, so a pilot flying alone hears their
 * time without looking at the screen.
 *
 * The words are worked out here, purely, from the seat rows before and after a change.
 * {@link Caller} is the thin part that remembers what was already said and talks to the
 * browser's speech engine. A browser with no speech engine says nothing.
 */
import type { SeatRow } from './run.js';

/** A lap time as it is spoken: seconds to two places, "6.77". Over a minute: "1 minute 4.20". */
export function spokenLap(micros: number): string {
  // Rounded to hundredths first, so 59.996 is "1 minute 0.00" and never "60.00".
  const hundredths = Math.round(micros / 10_000);
  const minutes = Math.floor(hundredths / 6000);
  const rest = ((hundredths - minutes * 6000) / 100).toFixed(2);
  if (minutes === 0) return rest;
  return `${minutes} ${minutes === 1 ? 'minute' : 'minutes'} ${rest}`;
}

/** The key a seat is remembered by between snapshots. A wire handle: never spoken. */
const seatKey = (row: SeatRow): string => `${row.adapter}\u0000${row.seat}`;

/** How many laps each seat had, by {@link seatKey}. */
export type LapCounts = ReadonlyMap<string, number>;

export function lapCounts(rows: readonly SeatRow[]): Map<string, number> {
  return new Map(rows.map((row) => [seatKey(row), row.laps.length]));
}

/**
 * What to say for the laps that are new since `before`. One line per seat that gained a
 * lap, for its latest lap only: a seat that gained two at once (a reconnect) is not read
 * out twice. With one seat in the heat the line is just the time. With more it names the
 * pilot and the lap: "Ana, lap 3, 31.20".
 *
 * A seat that lost laps (a ruling removed one) says nothing.
 *
 * `spoken` gives how a row's pilot is said out loud (a pilot's phonetic spelling). Left
 * out, or with nothing to say for a row, the name on screen is used.
 */
export function newLapLines(
  before: LapCounts,
  rows: readonly SeatRow[],
  spoken?: (row: SeatRow) => string | undefined
): string[] {
  const alone = rows.length === 1;
  const lines: string[] = [];
  for (const row of rows) {
    const had = before.get(seatKey(row)) ?? 0;
    const lap = row.laps.at(-1);
    if (row.laps.length <= had || !lap) continue;
    const time = spokenLap(lap.micros);
    const who = spoken?.(row)?.trim() || row.who;
    lines.push(alone ? time : `${who}, lap ${row.laps.length}, ${time}`);
  }
  return lines;
}

const STORAGE_KEY = 'gridfpv.lap_callouts';

/** Whether callouts are on in this browser. On until switched off. */
export function readCalloutsOn(storage: Pick<Storage, 'getItem'> | null | undefined): boolean {
  try {
    return storage?.getItem(STORAGE_KEY) !== 'false';
  } catch {
    return true;
  }
}

export function saveCalloutsOn(
  storage: Pick<Storage, 'setItem'> | null | undefined,
  on: boolean
): void {
  try {
    storage?.setItem(STORAGE_KEY, String(on));
  } catch {
    /* storage unavailable: the choice lasts for this page only */
  }
}

/** The part of the browser's speech engine a {@link Caller} needs. */
export interface Speaker {
  speak(line: string): void;
  cancel(): void;
}

/** The browser's speech engine, or `undefined` where there is none. */
export function browserSpeaker(): Speaker | undefined {
  const scope = globalThis as {
    speechSynthesis?: SpeechSynthesis;
    SpeechSynthesisUtterance?: typeof SpeechSynthesisUtterance;
  };
  const engine = scope.speechSynthesis;
  const Utterance = scope.SpeechSynthesisUtterance;
  if (!engine || !Utterance) return undefined;
  return {
    speak(line) {
      const utterance = new Utterance(line);
      // A little quicker than the default: a lap time is short and the next lap is coming.
      utterance.rate = 1.15;
      engine.speak(utterance);
    },
    cancel: () => engine.cancel()
  };
}

/**
 * Speaks new laps of one race. Give it the race on screen and its rows each time they
 * change. It never reads out laps that were already there when it first saw a race, so
 * opening the page in the middle of a race does not recite the whole race.
 */
export class Caller {
  #speaker: Speaker | undefined;
  #race: string | undefined;
  #counts: LapCounts = new Map();

  constructor(speaker: Speaker | undefined = browserSpeaker()) {
    this.#speaker = speaker;
  }

  /**
   * `race` is the id of the race that is running, or `undefined` when none is. `on` is the
   * director's switch. Laps are tracked either way, so switching callouts on mid-race
   * starts with the next lap.
   */
  update(
    race: string | undefined,
    rows: readonly SeatRow[],
    on: boolean,
    spoken?: (row: SeatRow) => string | undefined
  ): void {
    if (race === undefined) {
      this.#race = undefined;
      this.#counts = new Map();
      return;
    }
    if (race !== this.#race) {
      // A race this caller has not seen: what it already has is not news.
      this.#race = race;
      this.#counts = lapCounts(rows);
      return;
    }
    const lines = newLapLines(this.#counts, rows, spoken);
    this.#counts = lapCounts(rows);
    if (!on) return;
    for (const line of lines) this.#speaker?.speak(line);
  }

  /** Stop talking, for a page that is going away. */
  dispose(): void {
    this.#speaker?.cancel();
  }
}
