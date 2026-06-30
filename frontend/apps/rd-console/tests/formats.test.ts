import { describe, expect, it } from 'vitest';
import {
  fieldsForFormat,
  FORMAT_LABELS,
  formatLabel,
  isDeterministicFormat,
  isOpenPracticeFormat,
  OPEN_PRACTICE
} from '../src/lib/formats.js';

describe('formats — friendly-name mapping (Rounds form redesign item 1)', () => {
  it('maps each known format key to its friendly label', () => {
    expect(formatLabel('open_practice')).toBe('Open Practice');
    expect(formatLabel('head_to_head')).toBe('Head-to-Head');
    // `timed_qual` is the stable wire key; only its friendly label was renamed (#218).
    expect(formatLabel('timed_qual')).toBe('Time Trials');
    // ZippyQ is shelved (not offered) but a persisted `zippyq` round still resolves a friendly name.
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

  it('treats every format but open practice as deterministic (#216 generate-all eligibility)', () => {
    // Deterministic: generate-all in one action.
    for (const det of ['timed_qual', 'head_to_head']) {
      expect(isDeterministicFormat(det)).toBe(true);
    }
    // Open practice is the lone dynamic format — single-step.
    expect(isDeterministicFormat('open_practice')).toBe(false);
    // A blank/nullish format is not deterministic.
    expect(isDeterministicFormat(undefined)).toBe(false);
    expect(isDeterministicFormat('')).toBe(false);
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

  it('a roster-seeded racing format shows the full class/win/seeding block', () => {
    for (const fmt of ['timed_qual', 'head_to_head', 'zippyq']) {
      const f = fieldsForFormat(fmt);
      expect(f.eligibleClass).toBe(true);
      expect(f.winCondition).toBe(true);
      expect(f.seeding).toBe(true);
      expect(f.channelMode).toBe(true);
      expect(f.params).toBe(true);
      expect(f.activeChannels).toBe(false);
      expect(f.timeLimit).toBe(false);
    }
  });
});
