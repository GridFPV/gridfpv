import { describe, expect, it } from 'vitest';
import { toExportJson } from '../src/lib/results.js';

describe('toExportJson', () => {
  it('serializes typed projection data with bigints as numbers', () => {
    const json = toExportJson({ at: 1_000_000 });
    expect(JSON.parse(json)).toEqual({ at: 1_000_000 });
  });
});
