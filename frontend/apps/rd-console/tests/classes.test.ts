import { describe, expect, it } from 'vitest';
import type { Class } from '@gridfpv/types';
import {
  MULTIGP_CLASSES,
  buildCreateRequest,
  buildUpdateRequest,
  emptyForm,
  formFromClass,
  formFromMultiGp,
  type ClassFormValues
} from '../src/lib/classes.js';

/** A class with every field populated — the baseline for the edit/diff tests. */
const FULL: Class = {
  id: 'c1',
  name: 'Open',
  source: 'MultiGP',
  reference: 'https://multigp.com/open',
  description: 'The unlimited class.'
};

describe('buildCreateRequest', () => {
  it('sends the name + source + filled optionals (blank optionals omitted)', () => {
    const v = emptyForm();
    v.name = '  Spec  ';
    v.source = 'MultiGP';
    v.reference = 'mg-spec';
    const req = buildCreateRequest(v);
    expect(req).toEqual({ name: 'Spec', source: 'MultiGP', reference: 'mg-spec' });
    // Untouched optionals are absent (not empty strings).
    expect(req).not.toHaveProperty('description');
  });

  it('defaults source to Custom on a blank add form', () => {
    const v = emptyForm();
    v.name = 'House';
    expect(buildCreateRequest(v)).toEqual({ name: 'House', source: 'Custom' });
  });
});

describe('buildUpdateRequest — clear-via-null', () => {
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
    // Unchanged fields stay omitted.
    expect(req).not.toHaveProperty('name');
    expect(req).not.toHaveProperty('source');
  });

  it('sets a changed name + source and leaves others untouched', () => {
    const v = valuesFrom(FULL);
    v.name = 'Pro Open';
    v.source = 'Custom';
    expect(buildUpdateRequest(FULL, v)).toEqual({ name: 'Pro Open', source: 'Custom' });
  });

  it('never clears the name when blanked', () => {
    const v = valuesFrom(FULL);
    v.name = '   ';
    expect(buildUpdateRequest(FULL, v)).not.toHaveProperty('name');
  });

  it('sets a changed reference value', () => {
    const v = valuesFrom(FULL);
    v.reference = 'https://multigp.com/open-2';
    expect(buildUpdateRequest(FULL, v).reference).toBe('https://multigp.com/open-2');
  });
});

describe('MultiGP quick-pick', () => {
  it('lists the seven standard MultiGP classes, each with a reference', () => {
    expect(MULTIGP_CLASSES.map((p) => p.name)).toEqual([
      'Spec',
      'Pro Spec',
      'Freedom Spec',
      'Street League',
      'Open',
      'Tiny Whoop',
      'Tiny Trainer'
    ]);
    for (const preset of MULTIGP_CLASSES) {
      expect(preset.reference).toMatch(/^https?:\/\//);
    }
  });

  it('formFromMultiGp pre-fills source=MultiGP + name + reference', () => {
    const form = formFromMultiGp(MULTIGP_CLASSES[0]);
    expect(form).toEqual({
      name: 'Spec',
      source: 'MultiGP',
      reference: MULTIGP_CLASSES[0].reference,
      description: ''
    });
    // It maps straight to a create body carrying the MultiGP provenance + reference.
    expect(buildCreateRequest(form)).toEqual({
      name: 'Spec',
      source: 'MultiGP',
      reference: MULTIGP_CLASSES[0].reference
    });
  });
});
