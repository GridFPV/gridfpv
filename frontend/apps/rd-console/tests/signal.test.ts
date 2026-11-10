/** The live signal store: the ring, thinning for the screen, crossings, range and the clock. */
import { describe, expect, it } from 'vitest';
import { crossingSpans, SignalRing, thin, TimerClock, verticalRange } from '../src/lib/signal.js';

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
