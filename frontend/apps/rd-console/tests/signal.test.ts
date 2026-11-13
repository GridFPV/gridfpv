/** The live signal store: the ring, thinning for the screen, crossings, range and the clock. */
import { describe, expect, it } from 'vitest';
import {
  clampWindow,
  crossingSpans,
  MIN_WINDOW_MICROS,
  nearestMark,
  panWindow,
  RaceTrace,
  SignalRing,
  thin,
  timeAtX,
  TimerClock,
  verticalRange,
  wholeWindow,
  xAtTime,
  zoomWindow
} from '../src/lib/signal.js';

function samples(ring: SignalRing): [number, number][] {
  const out: [number, number][] = [];
  ring.each(-Infinity, Infinity, (time, level) => out.push([time, level]));
  return out;
}

describe('SignalRing', () => {
  it('keeps each sample with its time on the timer clock', () => {
    const ring = new SignalRing(8);
    expect(ring.length).toBe(0);
    expect(ring.latest).toBeUndefined();
    ring.push(1000, 250, [-900, -890, -880]);
    expect(samples(ring)).toEqual([
      [1000, -900],
      [1250, -890],
      [1500, -880]
    ]);
    expect(ring.length).toBe(3);
    expect(ring.latest).toBe(1500);
  });

  it('drops the oldest samples once it is full, and keeps the rest unchanged', () => {
    const ring = new SignalRing(4);
    ring.push(0, 10, [1, 2, 3]);
    ring.push(30, 10, [4, 5, 6]);
    expect(ring.length).toBe(4);
    expect(samples(ring)).toEqual([
      [20, 3],
      [30, 4],
      [40, 5],
      [50, 6]
    ]);
    expect(ring.latest).toBe(50);
  });

  it('takes a chunk longer than itself, keeping the newest part', () => {
    const ring = new SignalRing(3);
    ring.push(0, 1, [1, 2, 3, 4, 5]);
    expect(samples(ring)).toEqual([
      [2, 3],
      [3, 4],
      [4, 5]
    ]);
  });

  it('visits only the samples in the asked window, oldest first', () => {
    const ring = new SignalRing(8);
    ring.push(0, 10, [1, 2, 3, 4, 5]);
    const seen: number[] = [];
    ring.each(10, 40, (_time, level) => seen.push(level));
    expect(seen).toEqual([2, 3, 4]);
  });

  it('forgets everything on clear', () => {
    const ring = new SignalRing(4);
    ring.push(0, 1, [1, 2]);
    ring.clear();
    expect(ring.length).toBe(0);
    expect(samples(ring)).toEqual([]);
  });
});

describe('thin', () => {
  it('keeps the lowest and highest sample of each column, so a peak is never lost', () => {
    const ring = new SignalRing(64);
    // Ten samples over 0..100; one spike in the second half.
    ring.push(0, 10, [-900, -905, -895, -900, -902, -898, -500, -900, -901, -899]);
    const thinned = thin(ring, 0, 100, 2);
    expect([...thinned.min]).toEqual([-905, -901]);
    expect([...thinned.max]).toEqual([-895, -500]);
    expect(thinned.lo).toBe(-905);
    expect(thinned.hi).toBe(-500);
  });

  it('leaves a column with no sample empty', () => {
    const ring = new SignalRing(8);
    ring.push(0, 10, [5]);
    ring.push(90, 10, [7]);
    const thinned = thin(ring, 0, 100, 4);
    expect(thinned.max[0]).toBe(5);
    expect(Number.isNaN(thinned.max[1])).toBe(true);
    expect(Number.isNaN(thinned.min[2])).toBe(true);
    expect(thinned.max[3]).toBe(7);
  });

  it('reads only the window, and says so when it is empty', () => {
    const ring = new SignalRing(8);
    ring.push(0, 10, [1, 2, 3]);
    const thinned = thin(ring, 1000, 2000, 4);
    expect(thinned.lo).toBeUndefined();
    expect(thinned.hi).toBeUndefined();
    expect([...thinned.max].every(Number.isNaN)).toBe(true);
  });

  it('does not change the ring', () => {
    const ring = new SignalRing(8);
    ring.push(0, 10, [3, 1, 2]);
    const before = samples(ring);
    thin(ring, 0, 30, 1);
    expect(samples(ring)).toEqual(before);
  });

  it('copes with no columns', () => {
    const ring = new SignalRing(8);
    ring.push(0, 10, [1]);
    expect(thin(ring, 0, 10, 0).lo).toBeUndefined();
  });
});

describe('crossingSpans', () => {
  const ENTER = -700;
  const EXIT = -760;

  it('opens at enter and closes when the signal falls below exit', () => {
    const ring = new SignalRing(64);
    //             0     10    20    30    40    50    60    70
    ring.push(0, 10, [-900, -720, -690, -650, -740, -770, -900, -900]);
    // Opens at 20 (first sample at or above enter). -740 is between the levels: still in.
    // Closes at 50 (first sample below exit).
    expect(crossingSpans(ring, 0, 100, ENTER, EXIT)).toEqual([{ from: 20, to: 50 }]);
  });

  it('finds each crossing', () => {
    const ring = new SignalRing(64);
    ring.push(0, 10, [-900, -600, -900, -900, -700, -800]);
    expect(crossingSpans(ring, 0, 100, ENTER, EXIT)).toEqual([
      { from: 10, to: 20 },
      { from: 40, to: 50 }
    ]);
  });

  it('ends a crossing that is still open at the newest sample', () => {
    const ring = new SignalRing(64);
    ring.push(0, 10, [-900, -600, -610, -620]);
    expect(crossingSpans(ring, 0, 100, ENTER, EXIT)).toEqual([{ from: 10, to: 30 }]);
  });

  it('finds none in a quiet signal', () => {
    const ring = new SignalRing(64);
    ring.push(0, 10, [-900, -880, -910]);
    expect(crossingSpans(ring, 0, 100, ENTER, EXIT)).toEqual([]);
  });
});

describe('verticalRange', () => {
  it('is fixed before any signal arrives', () => {
    expect(verticalRange('TenthsDb', undefined, undefined)).toEqual({ min: -1000, max: -300 });
    expect(verticalRange('TenthsDbm', undefined, undefined, [-700])).toEqual({
      min: -1000,
      max: -300
    });
    expect(verticalRange('Counts', undefined, undefined)).toEqual({ min: 0, max: 250 });
  });

  it('follows the signal and the lines, snapped outward to 5 dB', () => {
    // Signal -93.0..-61.0 dB, enter at -55.0: the range must hold all three with room.
    const range = verticalRange('TenthsDb', -930, -610, [-550, -600, undefined]);
    expect(range).toEqual({ min: -1000, max: -500 });
    expect(Math.abs(range.min % 50)).toBe(0);
    expect(Math.abs(range.max % 50)).toBe(0);
  });

  it('is never less than 20 dB tall', () => {
    const range = verticalRange('TenthsDb', -902, -898);
    expect(range.max - range.min).toBeGreaterThanOrEqual(200);
    expect(range.min).toBeLessThanOrEqual(-902);
    expect(range.max).toBeGreaterThanOrEqual(-898);
  });

  it('does not twitch for a small change in the signal', () => {
    expect(verticalRange('TenthsDb', -920, -630)).toEqual(verticalRange('TenthsDb', -915, -635));
  });

  it('uses steps of 10 for counts', () => {
    expect(verticalRange('Counts', 42, 118)).toEqual({ min: 30, max: 130 });
  });
});

describe('TimerClock', () => {
  it('has no time before any sample', () => {
    expect(new TimerClock().now(1000)).toBeUndefined();
  });

  it('runs on between chunks at the local clock rate', () => {
    const clock = new TimerClock();
    clock.observe(5_000_000, 100); // timer 5.000 s seen at local 100 ms
    expect(clock.now(100)).toBe(5_000_000);
    expect(clock.now(350)).toBe(5_250_000);
  });

  it('keeps the least delayed chunk, so a late one does not pull the time back', () => {
    const clock = new TimerClock();
    clock.observe(5_000_000, 100);
    clock.observe(5_050_000, 180); // arrived 30 ms late
    expect(clock.now(200)).toBe(5_100_000);
    clock.observe(5_200_000, 290); // arrived 10 ms early against the lead held
    expect(clock.now(300)).toBe(5_210_000);
  });

  it('starts again when the timer restarts and its clock goes back', () => {
    const clock = new TimerClock();
    clock.observe(900_000_000, 1000);
    clock.observe(40_000, 2000);
    expect(clock.now(2000)).toBe(40_000);
  });
});

describe('RaceTrace', () => {
  const chunks = [
    { from: 1_000_000, period_micros: 1000, levels: [-900, -850, -600] },
    // A gap in the signal, then more.
    { from: 2_000_000, period_micros: 1000, levels: [-700, -880] }
  ];

  it('joins the chunks into one trace on the timer clock', () => {
    const trace = new RaceTrace(chunks);
    expect(trace.length).toBe(5);
    expect(trace.first).toBe(1_000_000);
    expect(trace.last).toBe(2_001_000);
    const seen: [number, number][] = [];
    trace.each(-Infinity, Infinity, (time, level) => seen.push([time, level]));
    expect(seen).toEqual([
      [1_000_000, -900],
      [1_001_000, -850],
      [1_002_000, -600],
      [2_000_000, -700],
      [2_001_000, -880]
    ]);
  });

  it('visits only the samples in the stretch asked for', () => {
    const trace = new RaceTrace(chunks);
    const seen: number[] = [];
    trace.each(1_001_000, 2_001_000, (time) => seen.push(time));
    expect(seen).toEqual([1_001_000, 1_002_000, 2_000_000]);
  });

  it('puts chunks in time order even when they arrive out of order', () => {
    const trace = new RaceTrace([chunks[1], chunks[0]]);
    expect(trace.first).toBe(1_000_000);
    expect(trace.last).toBe(2_001_000);
  });

  it('is empty for a seat with no signal', () => {
    const trace = new RaceTrace([]);
    expect(trace.length).toBe(0);
    expect(trace.first).toBeUndefined();
    expect(wholeWindow(trace)).toBeUndefined();
  });

  it('thins like the live ring: a peak inside a column is kept', () => {
    const trace = new RaceTrace([{ from: 0, period_micros: 10, levels: [1, 9, 2, 3, 4, 0] }]);
    const thinned = thin(trace, 0, 60, 2);
    expect([...thinned.max]).toEqual([9, 4]);
    expect([...thinned.min]).toEqual([1, 0]);
  });
});

describe('the race trace window', () => {
  const bounds = { from: 0, to: 60_000_000 };

  it('covers the samples and the marks, with a little room', () => {
    const trace = new RaceTrace([{ from: 10_000_000, period_micros: 1000, levels: [1, 2] }]);
    const whole = wholeWindow(trace, [{ at: 5_000_000 }, { at: 30_000_000 }]);
    expect(whole).toBeDefined();
    expect(whole!.from).toBeLessThan(5_000_000);
    expect(whole!.to).toBeGreaterThan(30_000_000);
  });

  it('zooms in around the anchor, which stays where it is on screen', () => {
    const zoomed = zoomWindow(bounds, bounds, 0.5, 15_000_000);
    expect(zoomed.to - zoomed.from).toBe(30_000_000);
    // The anchor was a quarter of the way across, and still is.
    expect((15_000_000 - zoomed.from) / (zoomed.to - zoomed.from)).toBeCloseTo(0.25);
  });

  it('never zooms out past the whole race or in past the shortest stretch', () => {
    expect(zoomWindow(bounds, bounds, 4)).toEqual(bounds);
    let window = bounds;
    for (let i = 0; i < 40; i += 1) window = zoomWindow(window, bounds, 0.5);
    expect(window.to - window.from).toBe(MIN_WINDOW_MICROS);
  });

  it('pans without changing length and stops at the ends', () => {
    const window = { from: 10_000_000, to: 20_000_000 };
    expect(panWindow(window, bounds, 5_000_000)).toEqual({ from: 15_000_000, to: 25_000_000 });
    expect(panWindow(window, bounds, -50_000_000)).toEqual({ from: 0, to: 10_000_000 });
    expect(panWindow(window, bounds, 90_000_000)).toEqual({ from: 50_000_000, to: 60_000_000 });
  });

  it('clamps a window that is longer than the whole race to the whole race', () => {
    expect(clampWindow({ from: -5, to: 90_000_000 }, bounds)).toEqual(bounds);
  });

  it('turns a pixel into a whole-microsecond time and back', () => {
    const window = { from: 1_000_000, to: 2_000_000 };
    expect(timeAtX(window, 1000, 250)).toBe(1_250_000);
    expect(timeAtX(window, 3, 1)).toBe(1_333_333);
    expect(xAtTime(window, 1000, 1_250_000)).toBe(250);
    expect(timeAtX(window, 0, 10)).toBe(1_000_000);
  });

  it('finds the nearest mark within reach, or none', () => {
    const marks = [{ at: 100 }, { at: 200 }, { at: 260 }];
    expect(nearestMark(marks, 210, 30)).toBe(marks[1]);
    expect(nearestMark(marks, 240, 30)).toBe(marks[2]);
    expect(nearestMark(marks, 150, 30)).toBeUndefined();
  });
});
