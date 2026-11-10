/**
 * The built-in formats (`lib/formats.ts`). The Director takes these shapes as they are, so
 * each one is pinned in full: a slip here would start a different race than the picker says.
 */
import { describe, expect, it } from 'vitest';
import { BUILT_IN_FORMATS, directorEndsHeat, formatByKey, isRanked } from '../src/lib/formats.js';

const S = 1_000_000;

describe('BUILT_IN_FORMATS', () => {
  it('lists the four formats in picker order, each with its own key', () => {
    expect(BUILT_IN_FORMATS.map((choice) => choice.label)).toEqual([
      'Practice (open, no ranking, director ends it)',
      'Single pilot: best 3 consecutive',
      'First to 3 laps',
      '2 minutes, most laps'
    ]);
    const keys = BUILT_IN_FORMATS.map((choice) => choice.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it('practice is open, unranked, ended by the director, with no countdown', () => {
    expect(formatByKey('practice')).toEqual({
      name: 'Practice',
      race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
      completion: 'DirectorDecides'
    });
  });

  it('best 3 consecutive is an open race ranked on the best three laps in a row', () => {
    expect(formatByKey('best-3-consecutive')).toEqual({
      name: 'Best 3 consecutive',
      race: { end: 'Open', staging_micros: 5 * S, first_lap: 'HoleShot' },
      completion: 'DirectorDecides',
      ranking: { sort: [{ BestConsecutive: 3 }] }
    });
  });

  it('first to 3 laps is one saved race, ranked by the finish', () => {
    expect(formatByKey('first-to-3')).toEqual({
      name: 'First to 3 laps',
      race: { end: { FirstToLaps: 3 }, staging_micros: 5 * S, first_lap: 'HoleShot' },
      completion: { SavedRaces: 1 },
      ranking: { sort: ['LastFinish'] }
    });
  });

  it('2 minutes is timed with 10 seconds of grace, ranked by laps then the finish', () => {
    expect(formatByKey('timed-2-min')).toEqual({
      name: '2 minutes, most laps',
      race: {
        end: { Timed: { micros: 120 * S, grace_micros: 10 * S } },
        staging_micros: 5 * S,
        first_lap: 'HoleShot'
      },
      completion: { SavedRaces: 1 },
      ranking: { sort: ['Laps', 'LastFinish'] }
    });
  });

  it('every format but practice has a 5 second countdown', () => {
    for (const { key, format } of BUILT_IN_FORMATS) {
      expect(format.race.staging_micros).toBe(key === 'practice' ? 0 : 5 * S);
    }
  });
});

describe('format questions', () => {
  it('formatByKey answers undefined for a key that is not in the list', () => {
    expect(formatByKey('nope')).toBeUndefined();
  });

  it('directorEndsHeat is true only where the director decides', () => {
    expect(directorEndsHeat(formatByKey('practice'))).toBe(true);
    expect(directorEndsHeat(formatByKey('best-3-consecutive'))).toBe(true);
    expect(directorEndsHeat(formatByKey('first-to-3'))).toBe(false);
    expect(directorEndsHeat(undefined)).toBe(false);
  });

  it('isRanked is false for practice and for no format', () => {
    expect(isRanked(formatByKey('practice'))).toBe(false);
    expect(isRanked(formatByKey('first-to-3'))).toBe(true);
    expect(isRanked(undefined)).toBe(false);
  });
});

describe('the stop choice', () => {
  it('applies only to a first-to-N race', async () => {
    const { canStopItself } = await import('../src/lib/formats.js');
    expect(canStopItself(formatByKey('first-to-3'))).toBe(true);
    expect(canStopItself(formatByKey('timed-2-min'))).toBe(false);
    expect(canStopItself(formatByKey('practice'))).toBe(false);
    expect(canStopItself(undefined)).toBe(false);
  });

  it('leaves the stop to the director by default', async () => {
    const { STOP_CHOICES, withStopChoice } = await import('../src/lib/formats.js');
    const format = formatByKey('first-to-3')!;
    expect(STOP_CHOICES[0].key).toBe('manual');
    expect(withStopChoice(format, 'manual')).toBe(format);
    expect(withStopChoice(format, 'no-such-choice')).toBe(format);
  });

  it('sets the wait, in microseconds, without touching the built-in format', async () => {
    const { withStopChoice } = await import('../src/lib/formats.js');
    const format = formatByKey('first-to-3')!;
    const stopping = withStopChoice(format, '60');
    expect(stopping.race.stop_after_all_finish_micros).toBe(60 * S);
    expect(format.race.stop_after_all_finish_micros).toBeUndefined();
    // A timed race already ends by itself: the choice changes nothing.
    const timed = formatByKey('timed-2-min')!;
    expect(withStopChoice(timed, '60')).toBe(timed);
  });
});
