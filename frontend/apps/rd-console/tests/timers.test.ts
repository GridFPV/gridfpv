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

describe('the pass window on the Run page', () => {
  const on = { connected: true, pass_window: true };
  const none = { connected: true, pass_window: false };

  it('says nothing when the event sets none, whatever the timer can do', async () => {
    const { passWindowTrouble } = await import('../src/lib/timers.js');
    for (const window of [undefined, 0]) {
      expect(passWindowTrouble('Gate', none, window)).toBeUndefined();
      expect(passWindowTrouble('Gate', on, window)).toBeUndefined();
    }
  });

  it('says a timer with none does not have the event’s window in force, in seconds', async () => {
    const { passWindowTrouble } = await import('../src/lib/timers.js');
    expect(passWindowTrouble('Gate', none, 1_500_000)).toEqual({
      trouble: 'Gate has no pass window.',
      detail:
        "The event's pass window of 1.5 s is not in force on it: every crossing it reports counts by itself."
    });
    expect(passWindowTrouble('Gate', on, 1_500_000)).toBeUndefined();
  });

  it("passes on the Director's reason when the window could not be set, even with none on the event", async () => {
    const { passWindowTrouble } = await import('../src/lib/timers.js');
    const failed = { ...on, pass_window_problem: 'Could not reach Gate.' };
    for (const window of [undefined, 400_000]) {
      expect(passWindowTrouble('Gate', failed, window)).toEqual({
        trouble: 'The pass window could not be set on Gate.',
        detail: 'Could not reach Gate.'
      });
    }
  });

  it('leaves a timer that is not connected to the bigger warning', async () => {
    const { passWindowTrouble } = await import('../src/lib/timers.js');
    expect(
      passWindowTrouble('Gate', { connected: false, pass_window: false }, 400_000)
    ).toBeUndefined();
  });
});
