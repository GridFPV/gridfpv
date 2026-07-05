import { describe, expect, it } from 'vitest';
import {
  defineHeatCommands,
  isHeatValid,
  lineupOf,
  validateHeat,
  type HeatDefinition
} from '../src/lib/heat.js';

const def = (heat: string, ...names: string[]): HeatDefinition => ({
  heat,
  pilots: names.map((name) => ({ name }))
});

describe('heat definition', () => {
  it('flags a missing id, too few pilots, and duplicate names', () => {
    expect(validateHeat(def('', 'Ace', 'Bee'))).toContain('Heat needs an id.');
    expect(validateHeat(def('q-1', 'Ace'))).toContain('Add at least two pilots.');
    expect(validateHeat(def('q-1', 'Ace', 'Ace'))).toContain('Duplicate pilot name "Ace".');
  });

  it('accepts a two-pilot named heat', () => {
    const d = def('q-1', 'Ace', 'Bee');
    expect(validateHeat(d)).toEqual([]);
    expect(isHeatValid(d)).toBe(true);
  });

  it('cleans blank/whitespace names out of the lineup, preserving order', () => {
    const d = def('q-1', '  Ace ', '', 'Bee');
    expect(lineupOf(d)).toEqual(['Ace', 'Bee']);
  });

  it('builds one Register per pilot then a ScheduleHeat with the lineup', () => {
    const commands = defineHeatCommands(def('q-1', 'Ace', 'Bee'));
    expect(commands).toEqual([
      { Register: { adapter: 'sim', competitor: 'Ace', pilot: 'Ace' } },
      { Register: { adapter: 'sim', competitor: 'Bee', pilot: 'Bee' } },
      { ScheduleHeat: { heat: 'q-1', lineup: ['Ace', 'Bee'] } }
    ]);
  });
});
