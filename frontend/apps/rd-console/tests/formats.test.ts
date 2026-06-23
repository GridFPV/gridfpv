import { describe, expect, it } from 'vitest';
import {
  fieldsForFormat,
  FORMAT_LABELS,
  formatLabel,
  isOpenPracticeFormat,
  OPEN_PRACTICE
} from '../src/lib/formats.js';

describe('formats — friendly-name mapping (Rounds form redesign item 1)', () => {
  it('maps each known format key to its friendly label', () => {
    expect(formatLabel('double_elim')).toBe('Double Elimination');
    expect(formatLabel('open_practice')).toBe('Open Practice');
    expect(formatLabel('single_elim')).toBe('Single Elimination');
    expect(formatLabel('timed_qual')).toBe('Qualifying');
    expect(formatLabel('round_robin')).toBe('Round Robin');
    expect(formatLabel('multi_main')).toBe('Multi-Main');
    expect(formatLabel('zippyq')).toBe('ZippyQ');
  });

  it('covers every label-map entry', () => {
    for (const [key, label] of Object.entries(FORMAT_LABELS)) {
      expect(formatLabel(key)).toBe(label);
    }
  });

  it('de-slugs an unmapped key to title case rather than showing the raw token', () => {
    expect(formatLabel('some_new_format')).toBe('Some New Format');
    expect(formatLabel('triple-elim')).toBe('Triple Elim');
  });

  it('yields an empty string for a blank / nullish key', () => {
    expect(formatLabel('')).toBe('');
    expect(formatLabel(undefined)).toBe('');
    expect(formatLabel(null)).toBe('');
  });

  it('recognises the open-practice format key', () => {
    expect(OPEN_PRACTICE).toBe('open_practice');
    expect(isOpenPracticeFormat('open_practice')).toBe(true);
    expect(isOpenPracticeFormat('timed_qual')).toBe(false);
    expect(isOpenPracticeFormat(undefined)).toBe(false);
  });
});

describe('formats — dynamic field set per format (Rounds form redesign item 2)', () => {
  it('open practice shows channels + time limit and hides class / win / seeding / channel mode', () => {
    const f = fieldsForFormat('open_practice');
    expect(f.activeChannels).toBe(true);
    expect(f.timeLimit).toBe(true);
    expect(f.eligibleClass).toBe(false);
    expect(f.winCondition).toBe(false);
    expect(f.seeding).toBe(false);
    expect(f.channelMode).toBe(false);
    expect(f.params).toBe(false);
  });

  it('a bracket format shows class + win + seeding + channel mode + params (not channels)', () => {
    for (const bracket of ['single_elim', 'double_elim', 'multi_main']) {
      const f = fieldsForFormat(bracket);
      expect(f.eligibleClass).toBe(true);
      expect(f.winCondition).toBe(true);
      expect(f.seeding).toBe(true);
      expect(f.channelMode).toBe(true);
      expect(f.params).toBe(true);
      expect(f.activeChannels).toBe(false);
      expect(f.timeLimit).toBe(false);
    }
  });

  it('a roster-seeded qual format shows the full class/win/seeding block too', () => {
    for (const qual of ['timed_qual', 'round_robin', 'zippyq']) {
      const f = fieldsForFormat(qual);
      expect(f.eligibleClass).toBe(true);
      expect(f.winCondition).toBe(true);
      expect(f.seeding).toBe(true);
      expect(f.activeChannels).toBe(false);
    }
  });
});
