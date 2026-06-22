import { describe, expect, it } from 'vitest';
import type { Class } from '@gridfpv/types';
import {
  buildCreateRequest,
  buildUpdateRequest,
  emptyForm,
  formFromClass,
  isBuiltin,
  sourceTone,
  type ClassFormValues
} from '../src/lib/classes.js';

/** A custom class with every field populated — the baseline for the edit/diff tests. */
const FULL: Class = {
  id: 'c1',
  name: 'House Spec',
  source: 'Custom',
  reference: 'https://example.com/spec',
  description: 'The house spec class.'
};

/** A locked, fixed-id built-in (the server flags these with `builtin: true`). */
const BUILTIN: Class = {
  id: 'mgp-open',
  name: 'Open Class',
  source: 'MultiGP',
  reference: 'https://www.multigp.com/class-specifications/',
  description: 'Open/unlimited 5–6" class.',
  builtin: true
};

describe('buildCreateRequest (create = Custom only)', () => {
  it('always sends source=Custom + the name + filled optionals (blank optionals omitted)', () => {
    const v = emptyForm();
    v.name = '  Spec  ';
    v.reference = 'mg-spec';
    const req = buildCreateRequest(v);
    expect(req).toEqual({ name: 'Spec', source: 'Custom', reference: 'mg-spec' });
    expect(req).not.toHaveProperty('description');
  });

  it('defaults to a Custom class on a blank add form', () => {
    const v = emptyForm();
    v.name = 'House';
    expect(buildCreateRequest(v)).toEqual({ name: 'House', source: 'Custom' });
  });
});

describe('buildUpdateRequest — clear-via-null (no source field)', () => {
  function valuesFrom(c: Class): ClassFormValues {
    return formFromClass(c);
  }

  it('omits every field when nothing changed', () => {
    expect(buildUpdateRequest(FULL, valuesFrom(FULL))).toEqual({});
  });

  it('emits null for reference and description when the user clears them', () => {
    const v = valuesFrom(FULL);
    v.reference = '';
    v.description = '';
    const req = buildUpdateRequest(FULL, v);
    expect(req.reference).toBeNull();
    expect(req.description).toBeNull();
    expect(req).not.toHaveProperty('name');
  });

  it('sets a changed name and leaves others untouched', () => {
    const v = valuesFrom(FULL);
    v.name = 'Pro House';
    expect(buildUpdateRequest(FULL, v)).toEqual({ name: 'Pro House' });
  });

  it('never clears the name when blanked', () => {
    const v = valuesFrom(FULL);
    v.name = '   ';
    expect(buildUpdateRequest(FULL, v)).not.toHaveProperty('name');
  });

  it('never emits a source (the create/edit forms no longer pick a source)', () => {
    const v = valuesFrom(FULL);
    v.name = 'Renamed';
    expect(buildUpdateRequest(FULL, v)).not.toHaveProperty('source');
  });
});

describe('isBuiltin + source badges', () => {
  it('flags only built-in classes', () => {
    expect(isBuiltin(BUILTIN)).toBe(true);
    expect(isBuiltin(FULL)).toBe(false);
    expect(isBuiltin({ ...FULL, builtin: false })).toBe(false);
  });

  it('gives each org source a distinct badge tone, Custom its own violet, Other neutral', () => {
    expect(sourceTone('MultiGP')).toBe('accent');
    expect(sourceTone('Five33')).toBe('success');
    expect(sourceTone('FreedomSpec')).toBe('info');
    expect(sourceTone('StreetLeague')).toBe('warn');
    expect(sourceTone('UDL')).toBe('danger');
    expect(sourceTone('Custom')).toBe('violet');
    expect(sourceTone('Other')).toBe('neutral');
  });
});
