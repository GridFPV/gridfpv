/**
 * Formats as the console reads and edits them (`lib/formats.ts`): the words for a format,
 * the draft a form binds to and its two conversions, and the name for a copy.
 */
import { describe, expect, it } from 'vitest';
import type { Format, Measure } from '@gridfpv/types';
import {
  RANK_CHOICES,
  blankDraft,
  completionWords,
  copyName,
  directorEndsHeat,
  draftFromFormat,
  durationWords,
  formatFromDraft,
  formatSummary,
  isRanked,
  rankingWords,
  raceWords,
  startWords,
  type FormatDraft
} from '../src/lib/formats.js';

const S = 1_000_000;

const PRACTICE: Format = {
  name: 'Practice',
  race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
  completion: 'DirectorDecides'
};
const BEST_3: Format = {
  name: 'Best 3 in a row',
  race: { end: 'Open', staging_micros: 5 * S, first_lap: 'HoleShot' },
  completion: 'DirectorDecides',
  ranking: { sort: [{ BestConsecutive: 3 }] }
};
const FIRST_TO_3: Format = {
  name: 'First to 3 laps',
  race: { end: { FirstToLaps: 3 }, staging_micros: 5 * S, first_lap: 'HoleShot' },
  completion: { SavedRaces: 1 },
  ranking: { sort: ['LastFinish'] }
};
const TIMED: Format = {
  name: '2 minutes, most laps',
  race: {
    end: { Timed: { micros: 120 * S, grace_micros: 10 * S } },
    staging_micros: 5 * S,
    first_lap: 'HoleShot'
  },
  completion: { SavedRaces: 3 },
  ranking: { sort: ['Laps', 'LastFinish'] }
};
const CHASE: Format = {
  name: 'Chase the Ace',
  race: {
    end: { FirstToLaps: 5 },
    staging_micros: 3.5 * S,
    first_lap: 'FromTone',
    stop_after_all_finish_micros: 10 * S,
    max_micros: 180 * S,
    min_lap_micros: 2.5 * S
  },
  completion: { AnyPilotReaches: { measure: 'Wins', at_least: 2 } },
  ranking: { sort: ['Wins', 'LastFinish'] }
};
const POINTS: Format = {
  name: 'Points over three',
  race: { end: { FirstToLaps: 3 }, staging_micros: 5 * S, first_lap: 'HoleShot' },
  completion: { SavedRaces: 3 },
  ranking: { sort: ['Points', 'BestFinish'] },
  points: [10, 8, 6, 4]
};

/** A format that differs from `base` only in its race rules. */
const withRace = (base: Format, race: Partial<Format['race']>): Format => ({
  ...base,
  race: { ...base.race, ...race }
});
const ranked = (...sort: Measure[]): Format => ({ ...PRACTICE, ranking: { sort } });

/** A blank draft with a name and the given changes. */
const draft = (over: Partial<FormatDraft> = {}): FormatDraft => ({
  ...blankDraft(),
  name: 'Test',
  ...over
});
const problemOf = (over: Partial<FormatDraft>): string | undefined => {
  const result = formatFromDraft(draft(over));
  return result.ok ? undefined : result.problem;
};
const formatOf = (over: Partial<FormatDraft>): Format => {
  const result = formatFromDraft(draft(over));
  if (!result.ok) throw new Error(result.problem);
  return result.format;
};

describe('what the Run page asks of a format', () => {
  it('directorEndsHeat is true only when the heat is done when the director says so', () => {
    expect(directorEndsHeat(PRACTICE)).toBe(true);
    expect(directorEndsHeat(FIRST_TO_3)).toBe(false);
    expect(directorEndsHeat(CHASE)).toBe(false);
    expect(directorEndsHeat(undefined)).toBe(false);
  });

  it('isRanked is true for a format with a ranking rule; practice and no format are not', () => {
    expect(isRanked(BEST_3)).toBe(true);
    expect(isRanked(PRACTICE)).toBe(false);
    expect(isRanked(undefined)).toBe(false);
  });
});

describe('durationWords', () => {
  it('writes whole seconds of a minute or more as minutes and seconds', () => {
    expect(durationWords(60 * S)).toBe('1:00');
    expect(durationWords(90 * S)).toBe('1:30');
    expect(durationWords(120 * S)).toBe('2:00');
    expect(durationWords(125 * S)).toBe('2:05');
    expect(durationWords(600 * S)).toBe('10:00');
  });

  it('writes anything under a minute in seconds, to one decimal at most', () => {
    expect(durationWords(45 * S)).toBe('45 s');
    expect(durationWords(59 * S)).toBe('59 s');
    expect(durationWords(0.5 * S)).toBe('0.5 s');
    expect(durationWords(2.26 * S)).toBe('2.3 s');
    expect(durationWords(0)).toBe('0 s');
  });

  it('keeps a part of a second on a time over a minute by staying in seconds', () => {
    expect(durationWords(90.5 * S)).toBe('90.5 s');
  });
});

describe('raceWords', () => {
  it('an open race runs until stopped, with its time limit when it has one', () => {
    expect(raceWords(PRACTICE)).toBe('Until stopped');
    expect(raceWords(withRace(PRACTICE, { max_micros: 300 * S }))).toBe(
      'Until stopped, 5:00 at most'
    );
  });

  it('a first-to race names its laps, one lap in the singular', () => {
    expect(raceWords(FIRST_TO_3)).toBe('First to 3 laps');
    expect(raceWords(withRace(FIRST_TO_3, { end: { FirstToLaps: 1 } }))).toBe('First to 1 lap');
  });

  it('a first-to race says when it stops by itself and its time limit', () => {
    expect(raceWords(withRace(FIRST_TO_3, { stop_after_all_finish_micros: 10 * S }))).toBe(
      'First to 3 laps, stops 10 s after the last finish'
    );
    expect(raceWords(withRace(FIRST_TO_3, { max_micros: 180 * S }))).toBe(
      'First to 3 laps, 3:00 at most'
    );
    expect(raceWords(CHASE)).toBe(
      'First to 5 laps, stops 10 s after the last finish, 3:00 at most'
    );
  });

  it('a timed race gives its length and the time to finish the lap, or says it stops', () => {
    expect(raceWords(TIMED)).toBe('2:00, then 10 s to finish the lap');
    expect(
      raceWords(withRace(TIMED, { end: { Timed: { micros: 45 * S, grace_micros: 0 } } }))
    ).toBe('45 s, then it stops');
  });
});

describe('rankingWords', () => {
  it('says a format with no ranking rule, or an empty one, is not ranked', () => {
    expect(rankingWords(PRACTICE)).toBe('Not ranked');
    expect(rankingWords(ranked())).toBe('Not ranked');
  });

  it('names every measure', () => {
    expect(rankingWords(ranked('Wins'))).toBe('Ranked by most wins');
    expect(rankingWords(ranked('Points'))).toBe('Ranked by most points');
    expect(rankingWords(ranked('Laps'))).toBe('Ranked by most laps');
    expect(rankingWords(ranked('BestLap'))).toBe('Ranked by fastest lap');
    expect(rankingWords(ranked('BestFinish'))).toBe('Ranked by best finish');
    expect(rankingWords(ranked('LastFinish'))).toBe('Ranked by finishing order');
    expect(rankingWords(ranked({ BestConsecutive: 3 }))).toBe('Ranked by fastest 3 laps in a row');
  });

  it('joins the measures in the order they are sorted by', () => {
    expect(rankingWords(TIMED)).toBe('Ranked by most laps, then finishing order');
    expect(rankingWords(ranked('Points', 'BestFinish', 'BestLap'))).toBe(
      'Ranked by most points, then best finish, then fastest lap'
    );
  });
});

describe('completionWords', () => {
  it('says the director calls it done', () => {
    expect(completionWords(PRACTICE)).toBe('Until you call it done');
  });

  it('counts the races, one race in words', () => {
    expect(completionWords(FIRST_TO_3)).toBe('One race');
    expect(completionWords(TIMED)).toBe('3 races');
  });

  it('counts the wins, one win in the singular', () => {
    expect(completionWords(CHASE)).toBe('Until a pilot has 2 wins');
    expect(
      completionWords({
        ...CHASE,
        completion: { AnyPilotReaches: { measure: 'Wins', at_least: 1 } }
      })
    ).toBe('Until a pilot has 1 win');
  });

  it('names any other measure a pilot must reach', () => {
    expect(
      completionWords({
        ...CHASE,
        completion: { AnyPilotReaches: { measure: 'Points', at_least: 20 } }
      })
    ).toBe('Until a pilot reaches 20 (most points)');
  });
});

describe('startWords', () => {
  it('gives the countdown, or says the race starts at once', () => {
    expect(startWords(FIRST_TO_3)).toBe('5 s countdown');
    expect(startWords(PRACTICE)).toBe('Starts at once');
  });

  it('adds what starts lap 1 only when it is the tone', () => {
    expect(startWords(CHASE)).toBe('3.5 s countdown, lap 1 timed from the tone');
    expect(startWords(withRace(PRACTICE, { first_lap: 'FromTone' }))).toBe(
      'Starts at once, lap 1 timed from the tone'
    );
  });
});

describe('formatSummary', () => {
  it('is the race, the ranking and the heat in one line', () => {
    expect(formatSummary(PRACTICE)).toBe('Until stopped · Not ranked · Until you call it done');
    expect(formatSummary(TIMED)).toBe(
      '2:00, then 10 s to finish the lap · Ranked by most laps, then finishing order · 3 races'
    );
    expect(formatSummary(FIRST_TO_3)).toBe(
      'First to 3 laps · Ranked by finishing order · One race'
    );
  });
});

describe('a blank draft', () => {
  it('is an open race with a 5 second countdown, ended by the director, with no name', () => {
    expect(blankDraft()).toEqual({
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
    });
  });

  it('is a new draft each time, so editing one does not change the next', () => {
    const first = blankDraft();
    first.name = 'Edited';
    expect(blankDraft().name).toBe('');
  });

  it('cannot be saved until it has a name, and then saves what it says', () => {
    expect(formatFromDraft(blankDraft())).toEqual({
      ok: false,
      problem: 'Give the format a name.'
    });
    expect(formatFromDraft({ ...blankDraft(), name: 'Best 3 in a row' })).toEqual({
      ok: true,
      format: BEST_3
    });
  });
});

describe('draftFromFormat', () => {
  it('reads an open, unranked format', () => {
    expect(draftFromFormat(PRACTICE)).toEqual({
      ...blankDraft(),
      name: 'Practice',
      end: 'Open',
      stagingSeconds: 0,
      done: 'Director',
      rank: 'none'
    });
  });

  it('reads a first-to race, its count of races and its ranking', () => {
    expect(draftFromFormat(FIRST_TO_3)).toEqual({
      ...blankDraft(),
      name: 'First to 3 laps',
      end: 'Laps',
      laps: 3,
      done: 'Races',
      doneCount: 1,
      rank: 'finish'
    });
  });

  it('reads a timed race in seconds', () => {
    const made = draftFromFormat(TIMED);
    expect(made.end).toBe('Timed');
    expect(made.timedSeconds).toBe(120);
    expect(made.graceSeconds).toBe(10);
    expect(made.done).toBe('Races');
    expect(made.doneCount).toBe(3);
    expect(made.rank).toBe('laps');
  });

  it('reads every optional race rule, and leaves its field empty when the format has none', () => {
    expect(draftFromFormat(CHASE)).toMatchObject({
      end: 'Laps',
      laps: 5,
      stagingSeconds: 3.5,
      fromTone: true,
      autoStopSeconds: 10,
      limitSeconds: 180,
      minLapSeconds: 2.5,
      done: 'Wins',
      doneCount: 2,
      rank: 'wins'
    });
    expect(draftFromFormat(FIRST_TO_3)).toMatchObject({
      fromTone: false,
      autoStopSeconds: '',
      limitSeconds: '',
      minLapSeconds: ''
    });
  });

  it('keeps a wait or a minimum lap of 0 as 0, not as empty', () => {
    const zero = draftFromFormat(
      withRace(FIRST_TO_3, { stop_after_all_finish_micros: 0, min_lap_micros: 0 })
    );
    expect(zero.autoStopSeconds).toBe(0);
    expect(zero.minLapSeconds).toBe(0);
  });

  it('reads the N of laps in a row', () => {
    expect(draftFromFormat(ranked({ BestConsecutive: 2 }))).toMatchObject({
      rank: 'consecutive',
      consecutive: 2
    });
  });

  it('reads each ranking the editor offers as that choice', () => {
    expect(draftFromFormat(ranked('LastFinish')).rank).toBe('finish');
    expect(draftFromFormat(ranked('Laps', 'LastFinish')).rank).toBe('laps');
    expect(draftFromFormat(ranked('BestLap')).rank).toBe('best-lap');
    expect(draftFromFormat(ranked('Wins', 'LastFinish')).rank).toBe('wins');
    expect(draftFromFormat(ranked('Points', 'BestFinish')).rank).toBe('points');
  });

  it('reads the points table as a list', () => {
    expect(draftFromFormat(POINTS).points).toBe('10, 8, 6, 4');
    expect(draftFromFormat(FIRST_TO_3).points).toBe('');
  });

  it('keeps a ranking rule the editor did not make as a custom one', () => {
    for (const sort of [
      ['Laps'],
      ['LastFinish', 'Laps'],
      [{ BestConsecutive: 3 }, 'BestLap'],
      []
    ] satisfies Measure[][]) {
      const made = draftFromFormat(ranked(...sort));
      expect(made.rank).toBe('custom');
      expect(made.customSort).toEqual(sort);
    }
    expect(draftFromFormat(FIRST_TO_3).customSort).toBeUndefined();
  });
});

describe('a draft turns back into the very same format', () => {
  const formats: Format[] = [
    PRACTICE,
    BEST_3,
    FIRST_TO_3,
    TIMED,
    CHASE,
    POINTS,
    withRace(PRACTICE, { max_micros: 300 * S }),
    withRace(TIMED, { end: { Timed: { micros: 90 * S, grace_micros: 0 } } }),
    withRace(FIRST_TO_3, { stop_after_all_finish_micros: 0, min_lap_micros: 0 }),
    ranked('BestLap'),
    ranked({ BestConsecutive: 1 }),
    ranked('Laps'),
    ranked({ BestConsecutive: 3 }, 'BestLap'),
    { ...ranked(), name: 'Empty rule' }
  ];
  for (const format of formats) {
    it(`${format.name}: ${formatSummary(format)}`, () => {
      expect(formatFromDraft(draftFromFormat(format))).toEqual({ ok: true, format });
    });
  }

  it('keeps the points table of a custom ranking rule that sorts by points', () => {
    // A rule the editor did not make: points, then fastest lap.
    const format: Format = { ...ranked('Points', 'BestLap'), points: [10, 8, 6] };
    expect(draftFromFormat(format).rank).toBe('custom');
    expect(formatFromDraft(draftFromFormat(format))).toEqual({ ok: true, format });
  });

  it('calls the fastest run of one lap what it is: the fastest lap', () => {
    expect(rankingWords(ranked({ BestConsecutive: 1 }))).toBe('Ranked by fastest lap');
    expect(rankingWords(ranked({ BestConsecutive: 3 }))).toBe('Ranked by fastest 3 laps in a row');
  });
});

describe('formatFromDraft', () => {
  it('needs a name, and saves it without the spaces around it', () => {
    expect(problemOf({ name: '' })).toBe('Give the format a name.');
    expect(problemOf({ name: '   ' })).toBe('Give the format a name.');
    expect(formatOf({ name: '  Sprint  ' }).name).toBe('Sprint');
  });

  it('asks for the name first when more than one thing is missing', () => {
    expect(problemOf({ name: '', stagingSeconds: '' })).toBe('Give the format a name.');
  });

  it('needs a countdown of 0 seconds or more, typed or picked', () => {
    for (const stagingSeconds of ['', '  ', 'abc', -1, '-0.5', Number.NaN, Infinity]) {
      expect(problemOf({ stagingSeconds })).toBe('The countdown must be 0 seconds or more.');
    }
    expect(formatOf({ stagingSeconds: 0 }).race.staging_micros).toBe(0);
    expect(formatOf({ stagingSeconds: '3' }).race.staging_micros).toBe(3 * S);
    expect(formatOf({ stagingSeconds: ' 0.5 ' }).race.staging_micros).toBe(500_000);
  });

  it('times lap 1 from the first crossing unless the draft says from the tone', () => {
    expect(formatOf({}).race.first_lap).toBe('HoleShot');
    expect(formatOf({ fromTone: true }).race.first_lap).toBe('FromTone');
  });

  it('saves whole microseconds for a number of seconds that floats badly', () => {
    expect(formatOf({ stagingSeconds: 0.1 + 0.2 }).race.staging_micros).toBe(300_000);
  });

  describe('a first-to race', () => {
    it('needs a whole number of laps, 1 or more', () => {
      for (const laps of ['', 0, -3, 2.5, 'three']) {
        expect(problemOf({ end: 'Laps', laps })).toBe('Say how many laps win the race: 1 or more.');
      }
      expect(formatOf({ end: 'Laps', laps: 1 }).race.end).toEqual({ FirstToLaps: 1 });
      expect(formatOf({ end: 'Laps', laps: '4' }).race.end).toEqual({ FirstToLaps: 4 });
    });

    it('stops by itself only when a wait is given, and 0 is a wait', () => {
      expect('stop_after_all_finish_micros' in formatOf({ end: 'Laps' }).race).toBe(false);
      expect(
        'stop_after_all_finish_micros' in formatOf({ end: 'Laps', autoStopSeconds: '  ' }).race
      ).toBe(false);
      expect(formatOf({ end: 'Laps', autoStopSeconds: 0 }).race.stop_after_all_finish_micros).toBe(
        0
      );
      expect(
        formatOf({ end: 'Laps', autoStopSeconds: '7.5' }).race.stop_after_all_finish_micros
      ).toBe(7.5 * S);
    });

    it('refuses a wait that is not 0 seconds or more', () => {
      for (const autoStopSeconds of [-1, 'soon']) {
        expect(problemOf({ end: 'Laps', autoStopSeconds })).toBe(
          'The wait before the race stops by itself must be 0 seconds or more.'
        );
      }
    });
  });

  describe('a timed race', () => {
    it('needs a length of more than 0 seconds', () => {
      for (const timedSeconds of ['', 0, -60, 'abc']) {
        expect(problemOf({ end: 'Timed', timedSeconds })).toBe(
          'A timed race needs a length in seconds, more than 0.'
        );
      }
    });

    it('needs a time to finish the lap of 0 seconds or more', () => {
      for (const graceSeconds of ['', -1, 'abc']) {
        expect(problemOf({ end: 'Timed', graceSeconds })).toBe(
          'The time to finish the lap must be 0 seconds or more.'
        );
      }
      expect(formatOf({ end: 'Timed', timedSeconds: '90', graceSeconds: 0 }).race.end).toEqual({
        Timed: { micros: 90 * S, grace_micros: 0 }
      });
    });

    it('has no time limit and no wait, whatever was typed in those fields', () => {
      const race = formatOf({
        end: 'Timed',
        limitSeconds: 'not a number',
        autoStopSeconds: -5
      }).race;
      expect('max_micros' in race).toBe(false);
      expect('stop_after_all_finish_micros' in race).toBe(false);
    });
  });

  describe('fields that do not apply', () => {
    it('an open race ignores the laps, the length and the wait', () => {
      expect(
        formatOf({ end: 'Open', laps: '', timedSeconds: '', graceSeconds: '', autoStopSeconds: -1 })
          .race
      ).toEqual({ end: 'Open', staging_micros: 5 * S, first_lap: 'HoleShot' });
    });

    it('a first-to race ignores the timed fields', () => {
      expect(formatOf({ end: 'Laps', timedSeconds: '', graceSeconds: 'abc' }).race.end).toEqual({
        FirstToLaps: 3
      });
    });

    it('a heat the director ends ignores the count', () => {
      expect(formatOf({ done: 'Director', doneCount: '' }).completion).toBe('DirectorDecides');
    });

    it('a ranking that is not laps in a row ignores the N', () => {
      expect(formatOf({ rank: 'laps', consecutive: '' }).ranking).toEqual({
        sort: ['Laps', 'LastFinish']
      });
    });

    it('a ranking that is not on points saves no points table', () => {
      expect('points' in formatOf({ rank: 'finish', points: '10, 8, 6' })).toBe(false);
      expect('points' in formatOf({ rank: 'finish', points: 'rubbish' })).toBe(false);
    });
  });

  describe('the time limit', () => {
    it('is left out when the field is empty', () => {
      expect('max_micros' in formatOf({ end: 'Open' }).race).toBe(false);
      expect('max_micros' in formatOf({ end: 'Laps', limitSeconds: ' ' }).race).toBe(false);
    });

    it('applies to an open race and a first-to race', () => {
      expect(formatOf({ end: 'Open', limitSeconds: 300 }).race.max_micros).toBe(300 * S);
      expect(formatOf({ end: 'Laps', limitSeconds: '180' }).race.max_micros).toBe(180 * S);
    });

    it('must be more than 0 seconds', () => {
      for (const limitSeconds of [0, -10, 'abc']) {
        expect(problemOf({ limitSeconds })).toBe(
          'The time limit must be more than 0 seconds. Leave it empty for no limit.'
        );
      }
    });
  });

  describe("the format's own minimum lap", () => {
    it("is left out when the field is empty, so the event's is used", () => {
      expect('min_lap_micros' in formatOf({}).race).toBe(false);
    });

    it('is saved for any kind of race, and 0 is a minimum lap', () => {
      expect(formatOf({ end: 'Open', minLapSeconds: 2.5 }).race.min_lap_micros).toBe(2.5 * S);
      expect(formatOf({ end: 'Laps', minLapSeconds: '4' }).race.min_lap_micros).toBe(4 * S);
      expect(formatOf({ end: 'Timed', minLapSeconds: 0 }).race.min_lap_micros).toBe(0);
    });

    it('must be 0 seconds or more', () => {
      for (const minLapSeconds of [-1, 'abc']) {
        expect(problemOf({ minLapSeconds })).toBe(
          "The minimum lap must be 0 seconds or more. Leave it empty to use the event's."
        );
      }
    });
  });

  describe('when the heat is done', () => {
    it('counts races', () => {
      expect(formatOf({ done: 'Races', doneCount: 1 }).completion).toEqual({ SavedRaces: 1 });
      expect(formatOf({ done: 'Races', doneCount: '3' }).completion).toEqual({ SavedRaces: 3 });
    });

    it('counts wins', () => {
      expect(formatOf({ done: 'Wins', doneCount: 2 }).completion).toEqual({
        AnyPilotReaches: { measure: 'Wins', at_least: 2 }
      });
    });

    it('needs a whole count of 1 or more, and says which count', () => {
      for (const doneCount of ['', 0, 1.5, -2, 'two']) {
        expect(problemOf({ done: 'Races', doneCount })).toBe(
          'Say how many races the heat has: 1 or more.'
        );
        expect(problemOf({ done: 'Wins', doneCount })).toBe(
          'Say how many wins end the heat: 1 or more.'
        );
      }
    });
  });

  describe('the ranking', () => {
    it('saves no ranking for a heat that is not ranked', () => {
      expect('ranking' in formatOf({ rank: 'none' })).toBe(false);
    });

    it('saves the measures of each choice', () => {
      expect(formatOf({ rank: 'finish' }).ranking).toEqual({ sort: ['LastFinish'] });
      expect(formatOf({ rank: 'laps' }).ranking).toEqual({ sort: ['Laps', 'LastFinish'] });
      expect(formatOf({ rank: 'best-lap' }).ranking).toEqual({ sort: ['BestLap'] });
      expect(formatOf({ rank: 'wins' }).ranking).toEqual({ sort: ['Wins', 'LastFinish'] });
      expect(formatOf({ rank: 'consecutive', consecutive: '2' }).ranking).toEqual({
        sort: [{ BestConsecutive: 2 }]
      });
    });

    it('needs a whole N of 1 or more for laps in a row', () => {
      for (const consecutive of ['', 0, 2.5, -1]) {
        expect(problemOf({ rank: 'consecutive', consecutive })).toBe(
          'Say how many laps in a row are timed: 1 or more.'
        );
      }
      expect(formatOf({ rank: 'consecutive', consecutive: 1 }).ranking).toEqual({
        sort: [{ BestConsecutive: 1 }]
      });
    });

    it('saves a custom rule as it was, and no ranking when there is no rule to keep', () => {
      expect(formatOf({ rank: 'custom', customSort: ['BestLap', 'Laps'] }).ranking).toEqual({
        sort: ['BestLap', 'Laps']
      });
      expect('ranking' in formatOf({ rank: 'custom' })).toBe(false);
    });

    it('offers every choice it can read back', () => {
      expect(RANK_CHOICES.map((choice) => choice.key)).toEqual([
        'none',
        'finish',
        'laps',
        'best-lap',
        'consecutive',
        'wins',
        'points'
      ]);
      for (const { key } of RANK_CHOICES) {
        const saved = formatOf({ rank: key, points: '3, 2, 1' });
        expect(draftFromFormat(saved).rank).toBe(key);
      }
    });
  });

  describe('points', () => {
    it('reads the list, split by commas or spaces', () => {
      expect(formatOf({ rank: 'points', points: '10, 8, 6, 4' })).toMatchObject({
        ranking: { sort: ['Points', 'BestFinish'] },
        points: [10, 8, 6, 4]
      });
      expect(formatOf({ rank: 'points', points: ' 5 3,1 ,, 0 ' }).points).toEqual([5, 3, 1, 0]);
      expect(formatOf({ rank: 'points', points: '1' }).points).toEqual([1]);
    });

    it('needs at least one place, each a whole number of 0 or more', () => {
      for (const points of ['', '  ', ', ,', '10, x, 6', '10, 7.5', '10, -1']) {
        expect(problemOf({ rank: 'points', points })).toBe(
          'List the points for each place, first place first, like 10, 8, 6, 4.'
        );
      }
    });
  });
});

describe('copyName', () => {
  it('adds "copy" when no format has that name yet', () => {
    expect(copyName('Sprint', [])).toBe('Sprint copy');
    expect(copyName('Sprint', ['Sprint', 'Other copy'])).toBe('Sprint copy');
  });

  it('counts up from 2 past the copies there already are', () => {
    expect(copyName('Sprint', ['Sprint', 'Sprint copy'])).toBe('Sprint copy 2');
    expect(copyName('Sprint', ['Sprint copy', 'Sprint copy 2', 'Sprint copy 3'])).toBe(
      'Sprint copy 4'
    );
  });

  it('takes the first free name, so a gap is filled', () => {
    expect(copyName('Sprint', ['Sprint copy 2'])).toBe('Sprint copy');
    expect(copyName('Sprint', ['Sprint copy', 'Sprint copy 3'])).toBe('Sprint copy 2');
  });

  it('treats names that differ only in letter case or outer spaces as taken', () => {
    expect(copyName('Sprint', ['SPRINT COPY'])).toBe('Sprint copy 2');
    expect(copyName('Sprint', ['  sprint copy  ', 'Sprint Copy 2'])).toBe('Sprint copy 3');
  });
});
