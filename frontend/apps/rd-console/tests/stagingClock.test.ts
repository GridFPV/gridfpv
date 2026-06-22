/**
 * Unit tests for the staging-countdown **formatter** (heat-lifecycle Slice 3). The reactive
 * `useStagingClock` rune helper is exercised through the LiveRaceControl render test (over-time
 * state included); here we pin the pure `formatStaging` mapping — crucially the over-time (negative)
 * branch that the console renders red and the RD reads as "the field isn't ready".
 */
import { describe, expect, it } from 'vitest';
import { formatStaging } from '../src/lib/stagingClock.svelte.js';

describe('formatStaging', () => {
  it('formats a positive remainder as M:SS', () => {
    expect(formatStaging(300_000)).toBe('5:00'); // 5:00 default window
    expect(formatStaging(65_000)).toBe('1:05');
    expect(formatStaging(7_000)).toBe('0:07');
    expect(formatStaging(0)).toBe('0:00');
  });

  it('floors to whole seconds (a stage clock, not a stopwatch)', () => {
    expect(formatStaging(7_999)).toBe('0:07');
    expect(formatStaging(60_500)).toBe('1:00');
  });

  it('prefixes a minus sign once over-time (the red, negative reading)', () => {
    expect(formatStaging(-1_000)).toBe('−0:01');
    expect(formatStaging(-14_000)).toBe('−0:14');
    expect(formatStaging(-65_000)).toBe('−1:05');
  });
});
