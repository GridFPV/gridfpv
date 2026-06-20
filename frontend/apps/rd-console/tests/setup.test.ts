import { describe, expect, it } from 'vitest';
import {
  defaultClass,
  defaultWinCondition,
  emptyConfig,
  isConfigComplete,
  scheduleHeatCommand,
  validateConfig,
  type EventConfig
} from '../src/lib/setup.js';

describe('setup config', () => {
  it('seeds an empty config that is not yet complete', () => {
    const c = emptyConfig();
    expect(isConfigComplete(c)).toBe(false);
    expect(validateConfig(c).length).toBeGreaterThan(0);
  });

  it('picks a sensible default win condition per format', () => {
    expect(defaultWinCondition('timed-qual')).toEqual({ Timed: { window_micros: 120_000_000n } });
    expect(defaultWinCondition('single-elim')).toEqual({ FirstToLaps: { n: 3 } });
    expect(defaultWinCondition('zippyq')).toEqual({ BestConsecutive: { n: 3 } });
  });

  it('validates a complete config as ready', () => {
    const c: EventConfig = {
      eventId: 'spring',
      eventName: 'Spring Cup',
      track: 'Main field',
      classes: [defaultClass('open', 'Open', 'timed-qual')]
    };
    expect(validateConfig(c)).toEqual([]);
    expect(isConfigComplete(c)).toBe(true);
  });

  it('flags each missing piece', () => {
    const problems = validateConfig({ eventId: '', eventName: '', track: '', classes: [] });
    expect(problems).toContain('Event needs a name.');
    expect(problems).toContain('Add at least one class.');
  });

  it('builds the one supported setup command: ScheduleHeat with a lineup', () => {
    expect(scheduleHeatCommand('heat-1', ['ALICE', 'BOB'])).toEqual({
      ScheduleHeat: { heat: 'heat-1', lineup: ['ALICE', 'BOB'] }
    });
  });
});
