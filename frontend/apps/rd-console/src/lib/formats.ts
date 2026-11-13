/**
 * Formats, as the console reads and edits them.
 *
 * A format is a named bundle of rules: how one race ends, when the heat is done, and how
 * the heat is ranked (`docs/primitives.md` §5.6). The saved formats live on the Director
 * (`GET /formats`). This module holds what the screens need around them, all pure:
 *
 * - the words for a format ({@link formatSummary}), so a list can say what each one does;
 * - a **draft** a form binds to, and the two conversions between a draft and a format;
 * - the two questions the Run page asks of a format.
 *
 * The wire holds times in microseconds. People type seconds, so the draft holds those.
 */
import type { Format, Measure } from '@gridfpv/types';

const SECOND = 1_000_000;

/** What a form field holds: a number, or text (empty while nothing is typed). */
export type FieldValue = number | string;

/** The number a field holds, or `undefined` when it is empty or not a number. */
function fieldNumber(value: FieldValue): number | undefined {
  if (typeof value === 'number') return Number.isFinite(value) ? value : undefined;
  if (value.trim() === '') return undefined;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : undefined;
}

const isEmpty = (value: FieldValue): boolean => typeof value === 'string' && value.trim() === '';

/** Whether a heat under this format is done only when the director says so. */
export function directorEndsHeat(format: Format | undefined): boolean {
  return format?.completion === 'DirectorDecides';
}

/** Whether a heat under this format is ranked. Practice is not. */
export function isRanked(format: Format | undefined): boolean {
  return format?.ranking !== undefined;
}

// ── How a heat is ranked ─────────────────────────────────────────────────────────

/** The ways to rank a heat that the editor offers. `custom` is a rule it did not make. */
export type RankKey =
  | 'none'
  | 'finish'
  | 'laps'
  | 'best-lap'
  | 'consecutive'
  | 'wins'
  | 'points'
  | 'custom';

/** The ranking choices, in the order the editor lists them. */
export const RANK_CHOICES: readonly { key: Exclude<RankKey, 'custom'>; label: string }[] = [
  { key: 'none', label: 'Not ranked (practice)' },
  { key: 'finish', label: 'Finishing order of the race' },
  { key: 'laps', label: 'Most laps, then finishing order' },
  { key: 'best-lap', label: 'Fastest single lap' },
  { key: 'consecutive', label: 'Fastest laps in a row' },
  { key: 'wins', label: 'Most wins, then finishing order' },
  { key: 'points', label: 'Most points over the races' }
];

const sameSort = (a: readonly Measure[], b: readonly Measure[]): boolean =>
  JSON.stringify(a) === JSON.stringify(b);

/** The measures each choice sorts by. `consecutive` takes its N from the draft. */
function sortFor(rank: RankKey, consecutive: number): Measure[] | undefined {
  switch (rank) {
    case 'finish':
      return ['LastFinish'];
    case 'laps':
      return ['Laps', 'LastFinish'];
    case 'best-lap':
      return ['BestLap'];
    case 'consecutive':
      return [{ BestConsecutive: consecutive }];
    case 'wins':
      return ['Wins', 'LastFinish'];
    case 'points':
      return ['Points', 'BestFinish'];
    default:
      return undefined;
  }
}

/** Which choice a ranking rule is, and its N when it is "laps in a row". */
function rankOf(sort: readonly Measure[] | undefined): { rank: RankKey; consecutive: number } {
  if (sort === undefined) return { rank: 'none', consecutive: 3 };
  const first = sort[0];
  if (
    sort.length === 1 &&
    typeof first === 'object' &&
    first !== null &&
    'BestConsecutive' in first
  ) {
    return { rank: 'consecutive', consecutive: first.BestConsecutive };
  }
  for (const choice of RANK_CHOICES) {
    const made = sortFor(choice.key, 3);
    if (made && sameSort(made, sort)) return { rank: choice.key, consecutive: 3 };
  }
  return { rank: 'custom', consecutive: 3 };
}

// ── The words for a format ───────────────────────────────────────────────────────

/** A length of time in words: "2:00", "1:30", "45 s", "0.5 s". */
export function durationWords(micros: number): string {
  const seconds = micros / SECOND;
  if (seconds >= 60 && Number.isInteger(seconds)) {
    return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, '0')}`;
  }
  return `${Number(seconds.toFixed(1))} s`;
}

const measureWords = (measure: Measure): string => {
  if (typeof measure === 'object') {
    return measure.BestConsecutive === 1
      ? 'fastest lap'
      : `fastest ${measure.BestConsecutive} laps in a row`;
  }
  switch (measure) {
    case 'Wins':
      return 'most wins';
    case 'Points':
      return 'most points';
    case 'Laps':
      return 'most laps';
    case 'BestLap':
      return 'fastest lap';
    case 'BestFinish':
      return 'best finish';
    case 'LastFinish':
      return 'finishing order';
  }
};

/** How one race ends: "First to 3 laps", "2:00, then 10 s to finish the lap", "Until stopped". */
export function raceWords(format: Format): string {
  const { end, max_micros: limit, stop_after_all_finish_micros: wait } = format.race;
  const limitWords = limit === undefined ? '' : `, ${durationWords(limit)} at most`;
  if (end === 'Open') return `Until stopped${limitWords}`;
  if ('FirstToLaps' in end) {
    const laps = end.FirstToLaps;
    const stops = wait === undefined ? '' : `, stops ${durationWords(wait)} after the last finish`;
    return `First to ${laps} ${laps === 1 ? 'lap' : 'laps'}${stops}${limitWords}`;
  }
  const grace = end.Timed.grace_micros;
  return grace > 0
    ? `${durationWords(end.Timed.micros)}, then ${durationWords(grace)} to finish the lap`
    : `${durationWords(end.Timed.micros)}, then it stops`;
}

/** How a heat is ranked: "Ranked by most laps, then finishing order", or "Not ranked". */
export function rankingWords(format: Format): string {
  if (!format.ranking || format.ranking.sort.length === 0) return 'Not ranked';
  return `Ranked by ${format.ranking.sort.map(measureWords).join(', then ')}`;
}

/** When a heat is done: "One race", "3 races", "Until a pilot has 2 wins", "Until you call it done". */
export function completionWords(format: Format): string {
  const rule = format.completion;
  if (rule === 'DirectorDecides') return 'Until you call it done';
  if ('SavedRaces' in rule) {
    return rule.SavedRaces === 1 ? 'One race' : `${rule.SavedRaces} races`;
  }
  const { measure, at_least: count } = rule.AnyPilotReaches;
  if (measure === 'Wins') return `Until a pilot has ${count} ${count === 1 ? 'win' : 'wins'}`;
  return `Until a pilot reaches ${count} (${measureWords(measure)})`;
}

/** How a race starts: "5 s countdown", or "Starts at once", with what starts lap 1. */
export function startWords(format: Format): string {
  const { staging_micros: staging, first_lap: firstLap } = format.race;
  const countdown = staging > 0 ? `${durationWords(staging)} countdown` : 'Starts at once';
  return firstLap === 'FromTone' ? `${countdown}, lap 1 timed from the tone` : countdown;
}

/** A format in one line, for a list or a picker: the race, the ranking, the heat. */
export function formatSummary(format: Format): string {
  return [raceWords(format), rankingWords(format), completionWords(format)].join(' · ');
}

// ── The draft ────────────────────────────────────────────────────────────────────

/** How one race ends, as the editor offers it. */
export type EndKey = 'Open' | 'Laps' | 'Timed';
/** When a heat is done, as the editor offers it. */
export type DoneKey = 'Director' | 'Races' | 'Wins';

/** A format as the director is editing it. Fields that do not apply keep what was typed. */
export interface FormatDraft {
  name: string;
  end: EndKey;
  /** First to this many laps. */
  laps: FieldValue;
  /** A timed race's length, in seconds. */
  timedSeconds: FieldValue;
  /** After the time is up, this long to finish the lap in progress, in seconds. */
  graceSeconds: FieldValue;
  /** The countdown before the start tone, in seconds. 0 starts at once. */
  stagingSeconds: FieldValue;
  /** Lap 1 timed from the start tone, not from the first crossing. */
  fromTone: boolean;
  /** First to N: stop by itself this long after everyone finishes. Empty: the director stops it. */
  autoStopSeconds: FieldValue;
  /** A race that is not timed: the longest it may run, in seconds. Empty: no limit. */
  limitSeconds: FieldValue;
  done: DoneKey;
  /** How many races, or how many wins, end the heat. */
  doneCount: FieldValue;
  rank: RankKey;
  /** The N of "fastest N laps in a row". */
  consecutive: FieldValue;
  /** Points by finishing position, first place first: "10, 8, 6, 4". */
  points: string;
  /** This format's own minimum lap, in seconds. Empty: the event's. */
  minLapSeconds: FieldValue;
  /** A ranking rule the editor did not make, kept so saving does not lose it. */
  customSort?: Measure[];
}

const seconds = (micros: number): number => micros / SECOND;

/** A draft for a new format: an open race, a 5 second countdown, ended by the director. */
export function blankDraft(): FormatDraft {
  return {
    name: '',
    end: 'Open',
    laps: 3,
    timedSeconds: 120,
    graceSeconds: 10,
    stagingSeconds: 5,
    fromTone: false,
    autoStopSeconds: '',
    limitSeconds: '',
    done: 'Director',
    doneCount: 1,
    rank: 'consecutive',
    consecutive: 3,
    points: '',
    minLapSeconds: ''
  };
}

/** A draft that says what a format says. */
export function draftFromFormat(format: Format): FormatDraft {
  const draft = blankDraft();
  draft.name = format.name;
  const { end } = format.race;
  if (end === 'Open') draft.end = 'Open';
  else if ('FirstToLaps' in end) {
    draft.end = 'Laps';
    draft.laps = end.FirstToLaps;
  } else {
    draft.end = 'Timed';
    draft.timedSeconds = seconds(end.Timed.micros);
    draft.graceSeconds = seconds(end.Timed.grace_micros);
  }
  draft.stagingSeconds = seconds(format.race.staging_micros);
  draft.fromTone = format.race.first_lap === 'FromTone';
  const wait = format.race.stop_after_all_finish_micros;
  draft.autoStopSeconds = wait === undefined ? '' : seconds(wait);
  const limit = format.race.max_micros;
  draft.limitSeconds = limit === undefined ? '' : seconds(limit);
  const minLap = format.race.min_lap_micros;
  draft.minLapSeconds = minLap === undefined ? '' : seconds(minLap);

  const rule = format.completion;
  if (rule === 'DirectorDecides') draft.done = 'Director';
  else if ('SavedRaces' in rule) {
    draft.done = 'Races';
    draft.doneCount = rule.SavedRaces;
  } else {
    // The editor offers wins; any other measure is kept as a count of wins would be shown.
    draft.done = 'Wins';
    draft.doneCount = rule.AnyPilotReaches.at_least;
  }

  const { rank, consecutive } = rankOf(format.ranking?.sort);
  draft.rank = rank;
  draft.consecutive = consecutive;
  if (rank === 'custom') draft.customSort = format.ranking?.sort;
  draft.points = (format.points ?? []).join(', ');
  return draft;
}

/** The format a draft would save, or a sentence saying what is missing. */
export type FormatResult = { ok: true; format: Format } | { ok: false; problem: string };

const problem = (sentence: string): FormatResult => ({ ok: false, problem: sentence });

/** A whole number of 1 or more from a field, or `undefined`. */
function count(value: FieldValue): number | undefined {
  const n = fieldNumber(value);
  return n !== undefined && Number.isInteger(n) && n >= 1 ? n : undefined;
}

/** Seconds from a field as microseconds, or `undefined` when it is not 0 or more. */
function micros(value: FieldValue): number | undefined {
  const n = fieldNumber(value);
  return n !== undefined && n >= 0 ? Math.round(n * SECOND) : undefined;
}

/**
 * Turn a draft into the format to save. Only what cannot be a format at all is caught here
 * (an empty name, a count that is not a number). Whether the Director takes it (a name
 * already in use) is the Director's call.
 */
export function formatFromDraft(draft: FormatDraft): FormatResult {
  const name = draft.name.trim();
  if (!name) return problem('Give the format a name.');

  const staging = micros(draft.stagingSeconds);
  if (staging === undefined) return problem('The countdown must be 0 seconds or more.');

  const race: Format['race'] = {
    end: 'Open',
    staging_micros: staging,
    first_lap: draft.fromTone ? 'FromTone' : 'HoleShot'
  };
  if (draft.end === 'Laps') {
    const laps = count(draft.laps);
    if (laps === undefined) return problem('Say how many laps win the race: 1 or more.');
    race.end = { FirstToLaps: laps };
    if (!isEmpty(draft.autoStopSeconds)) {
      const wait = micros(draft.autoStopSeconds);
      if (wait === undefined) {
        return problem('The wait before the race stops by itself must be 0 seconds or more.');
      }
      race.stop_after_all_finish_micros = wait;
    }
  } else if (draft.end === 'Timed') {
    const length = micros(draft.timedSeconds);
    if (length === undefined || length === 0) {
      return problem('A timed race needs a length in seconds, more than 0.');
    }
    const grace = micros(draft.graceSeconds);
    if (grace === undefined)
      return problem('The time to finish the lap must be 0 seconds or more.');
    race.end = { Timed: { micros: length, grace_micros: grace } };
  }
  if (draft.end !== 'Timed' && !isEmpty(draft.limitSeconds)) {
    const limit = micros(draft.limitSeconds);
    if (limit === undefined || limit === 0) {
      return problem('The time limit must be more than 0 seconds. Leave it empty for no limit.');
    }
    race.max_micros = limit;
  }
  if (!isEmpty(draft.minLapSeconds)) {
    const minLap = micros(draft.minLapSeconds);
    if (minLap === undefined) {
      return problem(
        "The minimum lap must be 0 seconds or more. Leave it empty to use the event's."
      );
    }
    race.min_lap_micros = minLap;
  }

  let completion: Format['completion'] = 'DirectorDecides';
  if (draft.done !== 'Director') {
    const n = count(draft.doneCount);
    if (n === undefined) {
      return problem(
        draft.done === 'Races'
          ? 'Say how many races the heat has: 1 or more.'
          : 'Say how many wins end the heat: 1 or more.'
      );
    }
    completion =
      draft.done === 'Races'
        ? { SavedRaces: n }
        : { AnyPilotReaches: { measure: 'Wins', at_least: n } };
  }

  const format: Format = { name, race, completion };
  if (draft.rank === 'custom') {
    if (draft.customSort) format.ranking = { sort: draft.customSort };
  } else if (draft.rank !== 'none') {
    const n = count(draft.consecutive);
    if (draft.rank === 'consecutive' && n === undefined) {
      return problem('Say how many laps in a row are timed: 1 or more.');
    }
    const sort = sortFor(draft.rank, n ?? 3);
    if (sort) format.ranking = { sort };
  }
  // A rule the editor did not make may sort on points too; its table must not be lost.
  const scored = draft.rank === 'points' || format.ranking?.sort.includes('Points') === true;
  if (scored && (draft.rank === 'points' || draft.points.trim() !== '')) {
    const points = draft.points
      .split(/[\s,]+/)
      .filter((part) => part !== '')
      .map(Number);
    if (points.length === 0 || points.some((p) => !Number.isInteger(p) || p < 0)) {
      return problem('List the points for each place, first place first, like 10, 8, 6, 4.');
    }
    format.points = points;
  }
  return { ok: true, format };
}

/** A name for a copy of a format that no saved format has yet: "‹name› copy", "… copy 2". */
export function copyName(name: string, taken: readonly string[]): string {
  const used = new Set(taken.map((other) => other.trim().toLowerCase()));
  for (let n = 1; ; n += 1) {
    const candidate = n === 1 ? `${name} copy` : `${name} copy ${n}`;
    if (!used.has(candidate.toLowerCase())) return candidate;
  }
}
