/**
 * The built-in formats a director can pick when adding a heat or a bracket.
 *
 * A format is a named bundle of rules: how one race ends, when the heat is done, and how
 * the heat is ranked. This short list is the only place the console defines any. A format
 * editor can replace it later; the screens only read {@link BUILT_IN_FORMATS}.
 */
import type { Format } from '@gridfpv/types';

const SECOND = 1_000_000;

/** The countdown before the start tone, for every format that has one. */
const STAGING_MICROS = 5 * SECOND;

/** One entry in the format picker. */
export interface FormatChoice {
  /** Stable key for the picker's `<option>`. Not sent to the Director. */
  key: string;
  /** What the picker shows. */
  label: string;
  format: Format;
}

export const BUILT_IN_FORMATS: readonly FormatChoice[] = [
  {
    key: 'practice',
    label: 'Practice (open, no ranking, director ends it)',
    format: {
      name: 'Practice',
      // No countdown: practice starts the moment the director presses Start.
      race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
      completion: 'DirectorDecides'
    }
  },
  {
    key: 'best-3-consecutive',
    label: 'Single pilot: best 3 consecutive',
    format: {
      name: 'Best 3 consecutive',
      race: { end: 'Open', staging_micros: STAGING_MICROS, first_lap: 'HoleShot' },
      completion: 'DirectorDecides',
      ranking: { sort: [{ BestConsecutive: 3 }] }
    }
  },
  {
    key: 'first-to-3',
    label: 'First to 3 laps',
    format: {
      name: 'First to 3 laps',
      race: { end: { FirstToLaps: 3 }, staging_micros: STAGING_MICROS, first_lap: 'HoleShot' },
      completion: { SavedRaces: 1 },
      ranking: { sort: ['LastFinish'] }
    }
  },
  {
    key: 'timed-2-min',
    label: '2 minutes, most laps',
    format: {
      name: '2 minutes, most laps',
      race: {
        end: { Timed: { micros: 120 * SECOND, grace_micros: 10 * SECOND } },
        staging_micros: STAGING_MICROS,
        first_lap: 'HoleShot'
      },
      completion: { SavedRaces: 1 },
      ranking: { sort: ['Laps', 'LastFinish'] }
    }
  }
];

/**
 * How a first-to-N race stops, as the picker offers it: the director stops it (the
 * default), or it stops by itself a set wait after every pilot has finished. The wait is
 * for a track where extra crossings are likely and laps may need sorting out first.
 */
export const STOP_CHOICES: readonly { key: string; label: string; seconds: number | null }[] = [
  { key: 'manual', label: 'The director stops it', seconds: null },
  { key: '10', label: 'By itself, 10 seconds after everyone finishes', seconds: 10 },
  { key: '30', label: 'By itself, 30 seconds after everyone finishes', seconds: 30 },
  { key: '60', label: 'By itself, 60 seconds after everyone finishes', seconds: 60 },
  { key: '120', label: 'By itself, 2 minutes after everyone finishes', seconds: 120 }
];

/** Whether the stop choice applies: only a first-to-N race can stop when everyone finishes. */
export function canStopItself(format: Format | undefined): boolean {
  const end = format?.race.end;
  return typeof end === 'object' && end !== null && 'FirstToLaps' in end;
}

/**
 * A copy of `format` with the stop choice applied. A format the choice does not apply to,
 * or an unknown key, comes back unchanged.
 */
export function withStopChoice(format: Format, stopKey: string): Format {
  const seconds = STOP_CHOICES.find((choice) => choice.key === stopKey)?.seconds ?? null;
  if (seconds === null || !canStopItself(format)) return format;
  return { ...format, race: { ...format.race, stop_after_all_finish_micros: seconds * SECOND } };
}

/** The format for a picker key, or `undefined` for a key that is not in the list. */
export function formatByKey(key: string): Format | undefined {
  return BUILT_IN_FORMATS.find((choice) => choice.key === key)?.format;
}

/** Whether a heat under this format is done only when the director says so. */
export function directorEndsHeat(format: Format | undefined): boolean {
  return format?.completion === 'DirectorDecides';
}

/** Whether a heat under this format is ranked. Practice is not. */
export function isRanked(format: Format | undefined): boolean {
  return format?.ranking !== undefined;
}
