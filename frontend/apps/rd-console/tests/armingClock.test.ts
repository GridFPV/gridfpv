/**
 * Unit tests for the start-tone-countdown **formatter** (RD-only tone countdown while Armed). The
 * reactive `useArmingClock` rune helper + its RD-only gating are exercised through the
 * LiveRaceControl render test; here we pin the pure `formatArming` mapping — the short-hold tenths
 * readout the RD reads as "tone in S.s", and the rare `M:SS` fallback for a minute-plus hold.
 */
import { describe, expect, it } from 'vitest';
import { formatArming } from '../src/lib/armingClock.svelte.js';

describe('formatArming', () => {
  it('formats a short (sub-minute) remainder as tenths of a second', () => {
    expect(formatArming(3_200)).toBe('3.2s');
    expect(formatArming(400)).toBe('0.4s');
    expect(formatArming(0)).toBe('0.0s');
    expect(formatArming(5_000)).toBe('5.0s');
  });

  it('floors to whole tenths (a stage clock, not a round-to-nearest stopwatch)', () => {
    expect(formatArming(3_299)).toBe('3.2s');
    expect(formatArming(199)).toBe('0.1s');
  });

  it('clamps a negative remainder at zero', () => {
    expect(formatArming(-1_000)).toBe('0.0s');
  });

  it('falls back to M:SS for a minute-or-more remainder', () => {
    expect(formatArming(60_000)).toBe('1:00');
    expect(formatArming(65_000)).toBe('1:05');
  });
});
