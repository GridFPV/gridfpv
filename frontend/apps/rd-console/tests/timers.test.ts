/** The words for a timer's status (`lib/timers.ts`). */
import { describe, expect, it } from 'vitest';
import { clockSentence, notAddedYet, seatCount, seatCountSentence } from '../src/lib/timers.js';

describe('clockSentence', () => {
  it('says so when the clock has not been measured', () => {
    expect(clockSentence(undefined)).toBe('Clock not measured yet');
  });

  it('gives the error in milliseconds', () => {
    expect(clockSentence(200)).toBe('Clock known to within 0.2 ms');
    expect(clockSentence(1_540)).toBe('Clock known to within 1.5 ms');
    expect(clockSentence(25_400)).toBe('Clock known to within 25 ms');
  });

  it('never claims a perfect clock', () => {
    expect(clockSentence(0)).toBe('Clock known to within 0.1 ms');
    expect(clockSentence(12)).toBe('Clock known to within 0.1 ms');
  });
});

describe('seatCountSentence', () => {
  it('counts the seats the timer reports', () => {
    expect(seatCountSentence({ connected: true, seats: ['0', '1', '2', '3'] })).toBe('4 seats');
    expect(seatCountSentence({ connected: true, seats: ['0'] })).toBe('1 seat');
    expect(seatCountSentence({ connected: false, seats: [] })).toBe('No seats reported yet');
  });
});

describe('seatCount', () => {
  it('says one seat or many', () => {
    expect(seatCount(1)).toBe('1 seat');
    expect(seatCount(4)).toBe('4 seats');
    expect(seatCount(0)).toBe('0 seats');
  });
});

describe('notAddedYet', () => {
  it('keeps the found timers that are not added, in the order given', () => {
    const found = [
      { name: 'Gate', address: '10.0.0.5:7270', seats: 4, added: true },
      { name: 'Far Gate', address: '10.0.0.9:7270', added: false },
      { name: 'Bench', address: '10.0.0.7:7270', seats: 1, added: false }
    ];
    expect(notAddedYet(found).map((timer) => timer.name)).toEqual(['Far Gate', 'Bench']);
    expect(notAddedYet([])).toEqual([]);
  });
});
