import { describe, expect, it } from 'vitest';
import {
  defaultWinConditionKindFor,
  fieldsForFormat,
  FORMAT_LABELS,
  formatLabel,
  HEAD_TO_HEAD,
  isDeterministicFormat,
  isOpenPracticeFormat,
  OPEN_PRACTICE,
  TIMED_QUAL,
  WIN_CONDITION_KINDS,
  WIN_CONDITION_LABELS,
  winConditionKindsFor
} from '../src/lib/formats.js';

describe('formats — friendly-name mapping (Rounds form redesign item 1)', () => {
  it('maps each known format key to its friendly label', () => {
    expect(formatLabel('open_practice')).toBe('Practice');
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

describe('formats — the format↔win-condition taxonomy (#472)', () => {
  it('puts "Timed — Most Laps" in the head-to-head bucket, not the time-trial one', () => {
    // The decision (Ryan, 2026-08-27): pilots flying simultaneously and competing on lap count in
    // one window are racing each other; "time trial" means the solo/async run against the clock.
    expect(winConditionKindsFor(HEAD_TO_HEAD)).toContain('Timed');
    expect(winConditionKindsFor(TIMED_QUAL)).not.toContain('Timed');
  });

  it('head-to-head offers the two racing conditions and no time-trial metric', () => {
    expect(winConditionKindsFor(HEAD_TO_HEAD)).toEqual(['Timed', 'FirstToLaps']);
  });

  it('a time trial offers Best-of-N alone — its single ranking metric', () => {
    expect(winConditionKindsFor(TIMED_QUAL)).toEqual(['BestOfN']);
  });

  it('an unrecognised format (a structure round reached by editing) still offers all three', () => {
    expect(winConditionKindsFor('single_elim')).toEqual(WIN_CONDITION_KINDS);
    expect(winConditionKindsFor(undefined)).toEqual(WIN_CONDITION_KINDS);
  });

  it('defaults a fresh round to a kind its own family offers', () => {
    // Not a `Timed` the taxonomy would have to snap away the moment the form opened.
    expect(defaultWinConditionKindFor(TIMED_QUAL)).toBe('BestOfN');
    expect(defaultWinConditionKindFor(HEAD_TO_HEAD)).toBe('Timed');
    for (const fmt of [HEAD_TO_HEAD, TIMED_QUAL, 'single_elim']) {
      expect(winConditionKindsFor(fmt)).toContain(defaultWinConditionKindFor(fmt));
    }
  });

  it('labels every kind, so the picker never renders a bare discriminator', () => {
    for (const kind of WIN_CONDITION_KINDS) {
      expect(WIN_CONDITION_LABELS[kind]).toBeTruthy();
    }
    expect(WIN_CONDITION_LABELS.Timed).toBe('Timed — Most Laps');
  });
});
