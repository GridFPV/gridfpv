import { describe, expect, it } from 'vitest';
import { formatClock, formatMicros, medalFor } from '../src/format.js';

describe('formatClock', () => {
  it('formats milliseconds as M:SS.mmm', () => {
    expect(formatClock(0)).toBe('0:00.000');
    expect(formatClock(83_456)).toBe('1:23.456');
    expect(formatClock(605_007)).toBe('10:05.007');
  });
  it('renders negatives sign-prefixed (a countdown inside its grace window)', () => {
    expect(formatClock(-100)).toBe('-0:00.100');
    expect(formatClock(-3_250)).toBe('-0:03.250');
    expect(formatClock(-83_456)).toBe('-1:23.456');
  });
});

describe('formatMicros', () => {
  it('renders sub-minute durations as S.mmm seconds', () => {
    expect(formatMicros(41_250_000)).toBe('41.250');
    expect(formatMicros(1_500_000)).toBe('1.500');
  });
  it('rolls over to M:SS.mmm at or above a minute', () => {
    expect(formatMicros(83_456_000)).toBe('1:23.456');
  });
  it('renders null/undefined as an em dash', () => {
    expect(formatMicros(null)).toBe('—');
    expect(formatMicros(undefined)).toBe('—');
  });
});

describe('medalFor', () => {
  it('maps the podium and nothing below it', () => {
    expect(medalFor(1)).toBe('gold');
    expect(medalFor(2)).toBe('silver');
    expect(medalFor(3)).toBe('bronze');
    expect(medalFor(4)).toBeNull();
  });
});
