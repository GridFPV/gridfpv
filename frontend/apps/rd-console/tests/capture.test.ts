/** A seat's recorded signal and the passes a pair of levels makes of it (`lib/capture.ts`). */
import { describe, expect, it } from 'vitest';
import {
  RECORDING_CAPACITY,
  Recording,
  detectPasses,
  levelGap,
  moveLevel
} from '../src/lib/capture.js';

/** A recording of `levels`, one sample each millisecond from `from`. */
const MS = 1000;
const recorded = (levels: number[], from = 0): Recording => {
  const recording = new Recording();
  recording.push(from, MS, levels);
  return recording;
};
const samples = (recording: Recording, since = -Infinity, until = Infinity) => {
  const seen: [number, number][] = [];
  recording.each(since, until, (time, level) => seen.push([time, level]));
  return seen;
};

describe('Recording', () => {
  it('starts empty: no samples, no times, no span and no range', () => {
    const recording = new Recording();
    expect(recording.length).toBe(0);
    expect(recording.full).toBe(false);
    expect(recording.from).toBeUndefined();
    expect(recording.to).toBeUndefined();
    expect(recording.spanMicros).toBe(0);
    expect(recording.range()).toBeUndefined();
    expect(samples(recording)).toEqual([]);
  });

  it('holds five minutes at 1 kHz unless told otherwise', () => {
    expect(RECORDING_CAPACITY).toBe(300_000);
    expect(new Recording().capacity).toBe(RECORDING_CAPACITY);
    expect(new Recording(10).capacity).toBe(10);
  });

  it('times each sample of a chunk from the chunk start and the sample period', () => {
    const recording = new Recording();
    recording.push(5_000_000, 250, [-900, -880, -700]);
    expect(samples(recording)).toEqual([
      [5_000_000, -900],
      [5_000_250, -880],
      [5_000_500, -700]
    ]);
    expect(recording.length).toBe(3);
    expect(recording.from).toBe(5_000_000);
    expect(recording.to).toBe(5_000_500);
    expect(recording.spanMicros).toBe(500);
  });

  it('adds chunk after chunk in the order they arrive, keeping a gap between them', () => {
    const recording = recorded([1, 2], 0);
    recording.push(10 * MS, MS, [3]);
    expect(samples(recording)).toEqual([
      [0, 1],
      [MS, 2],
      [10 * MS, 3]
    ]);
    expect(recording.spanMicros).toBe(10 * MS);
  });

  it('has no span with one sample', () => {
    expect(recorded([7]).spanMicros).toBe(0);
  });

  it('ignores an empty chunk', () => {
    const recording = recorded([]);
    expect(recording.length).toBe(0);
    expect(recording.from).toBeUndefined();
  });

  it('drops a chunk from before the newest sample whole: one recording is on one clock', () => {
    const recording = recorded([1, 2, 3], 100 * MS);
    recording.push(0, MS, [9, 9, 9]);
    recording.push(101 * MS, MS, [9, 9, 9]);
    expect(recording.length).toBe(3);
    expect(recording.from).toBe(100 * MS);
    expect(recording.to).toBe(102 * MS);
  });

  it('stops at its capacity, dropping the samples past it', () => {
    const recording = new Recording(4);
    recording.push(0, MS, [1, 2, 3]);
    expect(recording.full).toBe(false);
    recording.push(3 * MS, MS, [4, 5, 6]);
    expect(recording.full).toBe(true);
    expect(recording.length).toBe(4);
    expect(recording.to).toBe(3 * MS);
    recording.push(10 * MS, MS, [7]);
    expect(recording.length).toBe(4);
    expect(samples(recording).map(([, level]) => level)).toEqual([1, 2, 3, 4]);
  });

  it('visits the samples at or after `since` and before `until`, oldest first', () => {
    const recording = recorded([10, 11, 12, 13, 14]);
    expect(samples(recording, MS, 3 * MS)).toEqual([
      [MS, 11],
      [2 * MS, 12]
    ]);
    expect(samples(recording, 3 * MS, 3 * MS)).toEqual([]);
    expect(samples(recording, 100 * MS, 200 * MS)).toEqual([]);
    expect(samples(recording, -5 * MS, 1)).toEqual([[0, 10]]);
  });

  it('gives the lowest and highest level recorded', () => {
    expect(recorded([-880, -910, -640, -700]).range()).toEqual({ lo: -910, hi: -640 });
    expect(recorded([-500]).range()).toEqual({ lo: -500, hi: -500 });
  });
});

describe('detectPasses', () => {
  const ENTER = 50;
  const EXIT = 30;

  it('finds nothing in an empty recording, or one that never reaches the enter level', () => {
    expect(detectPasses(new Recording(), ENTER, EXIT)).toEqual([]);
    expect(detectPasses(recorded([10, 49, 35, 49, 10]), ENTER, EXIT)).toEqual([]);
  });

  it('opens a pass at the enter level and closes it at the first sample below exit', () => {
    expect(detectPasses(recorded([10, 10, 50, 80, 60, 10, 10]), ENTER, EXIT)).toEqual([
      { start: 2 * MS, end: 5 * MS, at: 3 * MS, peak: 80, open: false }
    ]);
  });

  it('takes the lap time at the middle of the stretch at the peak level', () => {
    const [pass] = detectPasses(recorded([10, 60, 80, 80, 80, 60, 10]), ENTER, EXIT);
    expect(pass.peak).toBe(80);
    expect(pass.at).toBe(3 * MS);
    // A later, higher sample moves the peak; an equal one only stretches it.
    const [late] = detectPasses(recorded([60, 80, 70, 90, 90, 10]), ENTER, EXIT);
    expect(late.peak).toBe(90);
    expect(late.at).toBe(3.5 * MS);
  });

  it('counts a sample exactly at a level as reaching it', () => {
    // At enter: the pass opens. At exit: it has not gone below, so it stays open.
    expect(detectPasses(recorded([ENTER, EXIT, EXIT, 29]), ENTER, EXIT)).toEqual([
      { start: 0, end: 3 * MS, at: 0, peak: ENTER, open: false }
    ]);
  });

  it('stays in one pass between exit and enter: only going below exit ends it', () => {
    const passes = detectPasses(recorded([60, 40, 35, 60, 40, 10]), ENTER, EXIT);
    expect(passes).toHaveLength(1);
    expect(passes[0]).toMatchObject({ start: 0, end: 5 * MS, open: false });
  });

  it('needs the signal back at the enter level to open the next pass', () => {
    // After the first pass closes, 40 is above exit but below enter: no new pass.
    expect(detectPasses(recorded([60, 10, 40, 40, 10]), ENTER, EXIT)).toHaveLength(1);
  });

  it('finds each pass of a recording, in order', () => {
    const passes = detectPasses(recorded([10, 60, 10, 10, 70, 75, 10, 10]), ENTER, EXIT);
    expect(passes).toEqual([
      { start: MS, end: 2 * MS, at: MS, peak: 60, open: false },
      { start: 4 * MS, end: 6 * MS, at: 5 * MS, peak: 75, open: false }
    ]);
  });

  it('shows a bounce as two passes with no exit hold, so it can be tuned away', () => {
    const bounce = recorded([60, 10, 60, 10, 10, 10]);
    expect(detectPasses(bounce, ENTER, EXIT)).toEqual([
      { start: 0, end: MS, at: 0, peak: 60, open: false },
      { start: 2 * MS, end: 3 * MS, at: 2 * MS, peak: 60, open: false }
    ]);
  });

  it('does not split a pass at a dip below exit shorter than the hold', () => {
    const bounce = recorded([60, 10, 60, 10, 10, 10]);
    const passes = detectPasses(bounce, ENTER, EXIT, 2 * MS);
    expect(passes).toHaveLength(1);
    // It closes where the signal first went below exit for good, not when the hold ran out.
    expect(passes[0]).toMatchObject({ start: 0, end: 3 * MS, peak: 60, open: false });
  });

  it('closes a pass once the signal has stayed below exit for exactly the hold', () => {
    // Below from 1 ms; the hold of 2 ms has run at the 3 ms sample.
    const exact = detectPasses(recorded([60, 10, 10, 10, 60]), ENTER, EXIT, 2 * MS);
    expect(exact.map((pass) => [pass.start, pass.end, pass.open])).toEqual([
      [0, MS, false],
      [4 * MS, 4 * MS, true]
    ]);
    // One sample short of the hold, the signal coming back keeps it the same pass.
    const short = detectPasses(recorded([60, 10, 10, 60, 10, 10, 10, 10]), ENTER, EXIT, 3 * MS);
    expect(short.map((pass) => [pass.start, pass.end])).toEqual([[0, 4 * MS]]);
  });

  it('marks a pass still above exit when the recording ends as open, ending at the newest sample', () => {
    expect(detectPasses(recorded([10, 60, 70]), ENTER, EXIT)).toEqual([
      { start: MS, end: 2 * MS, at: 2 * MS, peak: 70, open: true }
    ]);
    expect(detectPasses(recorded([10, 10, 55]), ENTER, EXIT)).toEqual([
      { start: 2 * MS, end: 2 * MS, at: 2 * MS, peak: 55, open: true }
    ]);
  });

  it('marks a pass as open when the recording ends before the exit hold has run out', () => {
    // Below exit for one sample only, with a hold of three: the timer has not closed it yet.
    expect(detectPasses(recorded([10, 60, 70, 10]), ENTER, EXIT, 3 * MS)).toEqual([
      { start: MS, end: 3 * MS, at: 2 * MS, peak: 70, open: true }
    ]);
  });

  it('works on levels below zero, as tenths of a dB are', () => {
    const passes = detectPasses(recorded([-900, -650, -600, -640, -900], 7_000_000), -700, -760);
    expect(passes).toEqual([
      {
        start: 7_000_000 + MS,
        end: 7_000_000 + 4 * MS,
        at: 7_000_000 + 2 * MS,
        peak: -600,
        open: false
      }
    ]);
  });

  it('reads every sample, so a crossing one sample long is found', () => {
    const levels = Array.from({ length: 2000 }, () => 10);
    levels[1234] = 90;
    expect(detectPasses(recorded(levels), ENTER, EXIT)).toEqual([
      { start: 1234 * MS, end: 1235 * MS, at: 1234 * MS, peak: 90, open: false }
    ]);
  });
});

describe('moveLevel', () => {
  it('leaves the other level alone while the two keep their room', () => {
    expect(moveLevel('enter', -650, -700, -760)).toEqual({ enter: -650, exit: -760 });
    expect(moveLevel('exit', -800, -700, -760)).toEqual({ enter: -700, exit: -800 });
    // Exactly the gap apart is still room enough.
    expect(moveLevel('enter', -759, -700, -760)).toEqual({ enter: -759, exit: -760 });
    expect(moveLevel('exit', -701, -700, -760)).toEqual({ enter: -700, exit: -701 });
  });

  it('pushes exit down ahead of enter when enter is moved onto it or past it', () => {
    expect(moveLevel('enter', -760, -700, -760)).toEqual({ enter: -760, exit: -761 });
    expect(moveLevel('enter', -900, -700, -760)).toEqual({ enter: -900, exit: -901 });
  });

  it('pushes enter up ahead of exit when exit is moved onto it or past it', () => {
    expect(moveLevel('exit', -700, -700, -760)).toEqual({ enter: -699, exit: -700 });
    expect(moveLevel('exit', -100, -700, -760)).toEqual({ enter: -99, exit: -100 });
  });

  it('always puts the moved level where it was moved to', () => {
    for (const value of [-2000, -760, -730, -700, 0, 500]) {
      expect(moveLevel('enter', value, -700, -760).enter).toBe(value);
      expect(moveLevel('exit', value, -700, -760).exit).toBe(value);
    }
  });

  it('keeps the gap it is given, pushing the other level only as far as that needs', () => {
    expect(moveLevel('enter', -757, -700, -760, 5)).toEqual({ enter: -757, exit: -762 });
    expect(moveLevel('enter', -755, -700, -760, 5)).toEqual({ enter: -755, exit: -760 });
    expect(moveLevel('exit', -703, -700, -760, 5)).toEqual({ enter: -698, exit: -703 });
    expect(moveLevel('exit', -705, -700, -760, 5)).toEqual({ enter: -700, exit: -705 });
  });

  it('never leaves exit less than the gap below enter, wherever a level is moved', () => {
    for (const gap of [1, 5]) {
      for (const which of ['enter', 'exit'] as const) {
        for (let value = -780; value <= -680; value += 1) {
          const moved = moveLevel(which, value, -700, -760, gap);
          expect(moved.enter - moved.exit).toBeGreaterThanOrEqual(gap);
        }
      }
    }
  });
});

describe('levelGap', () => {
  it('is half a dB for a timer that reports dB or dBm, and one count for one that counts', () => {
    expect(levelGap('TenthsDb')).toBe(5);
    expect(levelGap('TenthsDbm')).toBe(5);
    expect(levelGap('Counts')).toBe(1);
  });
});
