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

describe('controlTrouble', () => {
  const gate = { name: 'Gate', address: '192.168.1.50:7270' };

  it('says nothing when Grid may change the timer', async () => {
    const { controlTrouble } = await import('../src/lib/timers.js');
    // A timer with no control at all, one nobody controls, and one Grid controls.
    expect(controlTrouble(gate, { connected: true })).toBeUndefined();
    expect(
      controlTrouble(gate, { connected: true, control: { holder: 'Nobody' } })
    ).toBeUndefined();
    expect(
      controlTrouble(gate, {
        connected: true,
        control: { holder: 'Grid' },
        control_name: 'GridFPV race-laptop'
      })
    ).toBeUndefined();
  });

  it('names the program that has control, the name Grid is listed under, and the timer’s page', async () => {
    const { controlTrouble } = await import('../src/lib/timers.js');
    expect(
      controlTrouble(gate, {
        connected: true,
        control: { holder: 'Other', name: 'RotorHazard pi-timer' },
        control_name: 'GridFPV race-laptop'
      })
    ).toEqual({
      trouble: '“RotorHazard pi-timer” has control of Gate.',
      detail:
        "Grid still shows its settings and takes its laps, but cannot change it. To change it from here, open the timer's own page and give control to “GridFPV race-laptop”.",
      page: 'http://192.168.1.50:7270/'
    });
  });

  it('still makes sense with no name for the program or for Grid', async () => {
    const { controlTrouble } = await import('../src/lib/timers.js');
    const said = controlTrouble(gate, {
      connected: true,
      control: { holder: 'Other', name: '  ' }
    });
    expect(said?.trouble).toBe('Another program has control of Gate.');
    expect(said?.detail).toContain('give control to Grid.');
  });

  it('leaves a timer that is not connected to the bigger warning', async () => {
    const { controlTrouble } = await import('../src/lib/timers.js');
    expect(
      controlTrouble(gate, {
        connected: false,
        control: { holder: 'Other', name: 'RotorHazard' }
      })
    ).toBeUndefined();
  });
});
