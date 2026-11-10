/**
 * The one place that answers "which seats can a heat use?".
 *
 * A heat is timed by one **seat source**. A source is either a timer from the Director's
 * timer directory (`GET /timers`), with the seats that timer reports, or the **Bench**: a
 * made-up source with eight seats and no timer behind it. The Bench is how a heat is run
 * before a timer is added; its crossings are logged by hand on the Run view.
 *
 * It is also where a seat and a timer get the names people see ("Seat 2", "Gate"). The
 * shared resolver in `lib/names.ts` calls {@link seatLabel} and {@link timerName}, and the
 * sources here are built from the same two functions, so a picker and a finished heat can
 * never name the same seat differently.
 */
import type { AdapterId, PresetSeat, SeatRef, TimerView } from '@gridfpv/types';

/** The handle every bench seat carries where a timer's id would be. Never shown. */
export const BENCH_ADAPTER: AdapterId = 'bench';
/** The name people see for the bench. */
export const BENCH_NAME = 'Bench';
/** Shown for a timer that is not in the timer directory (it was removed). */
export const UNKNOWN_TIMER = 'Unknown timer';
/** How many seats the bench has. */
export const BENCH_SEAT_COUNT = 8;

/** One seat: its wire handle and the name people see. */
export interface Seat {
  adapter: AdapterId;
  seat: SeatRef;
  label: string;
}

/** Something a heat can be timed by: a timer, or the bench. */
export interface SeatSource {
  /** The wire handle lineups carry. An option value; never shown. */
  id: AdapterId;
  /** The name people see: the timer's name, or "Bench". */
  name: string;
  /** True for the bench, which is not a timer. */
  bench: boolean;
  /** Whether the Director is connected to it right now. The bench always is. */
  connected: boolean;
  /** Its seats, in order. */
  seats: Seat[];
  /** Why it cannot be chosen, when it cannot: it has reported no seats. */
  unavailable?: string;
}

const BENCH_SEATS: readonly Seat[] = Array.from({ length: BENCH_SEAT_COUNT }, (_, index) => ({
  adapter: BENCH_ADAPTER,
  seat: `seat-${index + 1}`,
  label: `Seat ${index + 1}`
}));

/** Whether a seat belongs to the bench, so its crossings are logged by hand. */
export function isBench(adapter: AdapterId): boolean {
  return adapter === BENCH_ADAPTER;
}

/** The name of whatever times a seat: the timer's name, "Bench", or "Unknown timer". */
export function timerName(timers: readonly TimerView[], adapter: AdapterId): string {
  if (isBench(adapter)) return BENCH_NAME;
  return timers.find((view) => view.timer.id === adapter)?.timer.name ?? UNKNOWN_TIMER;
}

/**
 * The seat's number, counted from 1, or `undefined` when nothing known says.
 *
 * - A bench seat is one of the eight.
 * - A timer's seat is numbered by its place in the seats the timer reported.
 * - A timer that has reported nothing since the Director started (it is switched off, say)
 *   still has heats that name its seats. An OFT timer's seat handle is its seat's index
 *   counted from 0, so the number is read from the handle.
 */
function seatNumber(
  timers: readonly TimerView[],
  adapter: AdapterId,
  seat: SeatRef
): number | undefined {
  if (isBench(adapter)) {
    const index = BENCH_SEATS.findIndex((known) => known.seat === seat);
    return index < 0 ? undefined : index + 1;
  }
  const view = timers.find((candidate) => candidate.timer.id === adapter);
  if (!view) return undefined;
  // An OFT seat handle is the seat's own number, counted from 0. Use it first: a seat's
  // name must not change when another seat is switched off and the list gets shorter.
  if (view.timer.kind === 'Oft' && /^\d+$/.test(seat)) return Number(seat) + 1;
  const index = view.status.seats.indexOf(seat);
  return index >= 0 ? index + 1 : undefined;
}

/**
 * The name to show for a seat: "Seat 2". A seat nothing knows (its timer was removed) is
 * named by its place in the lineup (`position`, counted from 0), so a raw seat handle never
 * reaches the screen.
 */
export function seatLabel(
  timers: readonly TimerView[],
  adapter: AdapterId,
  seat: SeatRef,
  position?: number
): string {
  const number =
    seatNumber(timers, adapter, seat) ?? (position === undefined ? undefined : position + 1);
  return number === undefined ? 'Seat' : `Seat ${number}`;
}

/** The bench as a seat source. */
export function benchSource(): SeatSource {
  return {
    id: BENCH_ADAPTER,
    name: BENCH_NAME,
    bench: true,
    connected: true,
    seats: [...BENCH_SEATS]
  };
}

/**
 * Every seat source, in the order they are offered: each timer in the directory (connected
 * or not), then the bench. A timer that has reported no seats is listed with the reason it
 * cannot be chosen.
 */
export function seatSources(timers: readonly TimerView[]): SeatSource[] {
  const fromTimers = timers.map(({ timer, status }): SeatSource => {
    const seats = status.seats.map((seat) => ({
      adapter: timer.id,
      seat,
      label: seatLabel(timers, timer.id, seat)
    }));
    const source: SeatSource = {
      id: timer.id,
      name: timer.name,
      bench: false,
      connected: status.connected,
      seats
    };
    if (seats.length === 0) {
      source.unavailable = status.connected ? 'no seats reported yet' : 'not connected';
    }
    return source;
  });
  return [...fromTimers, benchSource()];
}

/** Whether a heat can be put on this source. */
export function canChoose(source: SeatSource): boolean {
  return source.unavailable === undefined;
}

/** The source a new heat starts on: the first connected timer with seats, else the bench. */
export function defaultSource(sources: readonly SeatSource[]): SeatSource {
  return (
    sources.find((source) => !source.bench && source.connected && canChoose(source)) ??
    sources.find((source) => source.bench) ??
    benchSource()
  );
}

/**
 * The source in use: the one the director picked, while it can still be chosen, else the
 * default. The list is read again every couple of seconds, so a picked timer can go away.
 */
export function chosenSource(
  sources: readonly SeatSource[],
  picked: AdapterId | undefined
): SeatSource {
  const match = sources.find((source) => source.id === picked);
  return match && canChoose(match) ? match : defaultSource(sources);
}

/** What a source is called in the "Timed by" list, with the reason when it cannot be chosen. */
export function sourceOptionLabel(source: SeatSource): string {
  if (source.unavailable) return `${source.name} (${source.unavailable})`;
  if (source.bench) return `${source.name} (no timer: crossings are logged by hand)`;
  const count = source.seats.length;
  const seats = `${count} ${count === 1 ? 'seat' : 'seats'}`;
  return source.connected
    ? `${source.name} (${seats})`
    : `${source.name} (${seats}, not connected)`;
}

/** The first `count` seats of a source, in the shape the bracket and mains presets take. */
export function presetSeats(source: SeatSource, count: number): PresetSeat[] {
  return source.seats.slice(0, count).map(({ adapter, seat }) => ({ adapter, seat }));
}
