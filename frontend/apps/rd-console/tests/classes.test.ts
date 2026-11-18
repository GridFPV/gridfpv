import { describe, expect, it } from 'vitest';
import type { Class } from '@gridfpv/types';
import {
  buildCreateRequest,
  buildUpdateRequest,
  emptyForm,
  formFromClass,
  isBuiltin,
  isHidden,
  partitionHidden,
  orgProblem,
  orgTone,
  ORG_MAX,
  type ClassFormValues
} from '../src/lib/classes.js';

/** A class of the director's own with every field filled: the baseline for the edit tests. */
const FULL: Class = {
  id: 'c1',
  name: 'House Spec',
  org: 'Tri-County FPV',
  reference: 'https://example.com/spec',
  description: 'The house spec class.'
};

/** A locked, fixed-id built-in (the server flags these with `builtin: true`). */
const BUILTIN: Class = {
  id: 'mgp-open',
  name: 'Open Class',
  org: 'MultiGP',
  reference: 'https://www.multigp.com/class-specifications/',
  description: 'Open/unlimited 5–6" class.',
  builtin: true
};

describe('buildCreateRequest', () => {
  it('sends the name and the filled optionals, tidied; blank optionals are left out', () => {
    const v = emptyForm();
    v.name = '  Spec  ';
    v.org = '  MultiGP ';
    v.reference = 'mg-spec';
    const req = buildCreateRequest(v);
    expect(req).toEqual({ name: 'Spec', org: 'MultiGP', reference: 'mg-spec' });
    expect(req).not.toHaveProperty('description');
  });

  it('a class with no org typed is sent with none, and never a made-up one', () => {
    const v = emptyForm();
    v.name = 'House';
    v.org = '   ';
    expect(buildCreateRequest(v)).toEqual({ name: 'House' });
  });
});

describe('buildUpdateRequest — clear-via-null', () => {
  function valuesFrom(c: Class): ClassFormValues {
    return formFromClass(c);
  }

  it('omits every field when nothing changed', () => {
    expect(buildUpdateRequest(FULL, valuesFrom(FULL))).toEqual({});
  });

  it('emits null for org, reference and description when the user clears them', () => {
    const v = valuesFrom(FULL);
    v.org = ' ';
    v.reference = '';
    v.description = '';
    const req = buildUpdateRequest(FULL, v);
    expect(req.org).toBeNull();
    expect(req.reference).toBeNull();
    expect(req.description).toBeNull();
    expect(req).not.toHaveProperty('name');
  });

  it('sets a changed name and leaves others untouched', () => {
    const v = valuesFrom(FULL);
    v.name = 'Pro House';
    expect(buildUpdateRequest(FULL, v)).toEqual({ name: 'Pro House' });
  });

  it('sets a changed org, and an org on a class that had none', () => {
    const v = valuesFrom(FULL);
    v.org = 'MultiGP';
    expect(buildUpdateRequest(FULL, v)).toEqual({ org: 'MultiGP' });
    const { org: _none, ...bare } = FULL;
    const w = valuesFrom(bare);
    expect(w.org).toBe('');
    w.org = 'My Club';
    expect(buildUpdateRequest(bare, w)).toEqual({ org: 'My Club' });
  });

  it('never clears the name when blanked', () => {
    const v = valuesFrom(FULL);
    v.name = '   ';
    expect(buildUpdateRequest(FULL, v)).not.toHaveProperty('name');
  });
});

describe('isBuiltin + org tags', () => {
  it('flags only built-in classes', () => {
    expect(isBuiltin(BUILTIN)).toBe(true);
    expect(isBuiltin(FULL)).toBe(false);
    expect(isBuiltin({ ...FULL, builtin: false })).toBe(false);
  });

  it("gives each org that comes with Grid its own tag colour, and the director's own violet", () => {
    expect(orgTone('MultiGP')).toBe('accent');
    expect(orgTone('Five33')).toBe('success');
    expect(orgTone('Freedom Spec')).toBe('info');
    expect(orgTone('Street League')).toBe('warn');
    expect(orgTone('UDL')).toBe('danger');
    // Typed by a director: the same org is the same colour however it is spelled.
    expect(orgTone(' multigp ')).toBe('accent');
    expect(orgTone('udl')).toBe('danger');
    expect(orgTone('Tri-County FPV')).toBe('violet');
    expect(orgTone('')).toBe('violet');
  });

  it('takes an org up to the limit, and says so past it', () => {
    expect(orgProblem('')).toBeUndefined();
    expect(orgProblem('x'.repeat(ORG_MAX))).toBeUndefined();
    expect(orgProblem(`  ${'x'.repeat(ORG_MAX)}  `)).toBeUndefined();
    expect(orgProblem('x'.repeat(ORG_MAX + 1))).toBe('An org name can have 24 characters at most.');
  });
});

describe('isHidden + partitionHidden (hide/archive classes)', () => {
  it('flags only hidden classes', () => {
    expect(isHidden({ ...FULL, hidden: true })).toBe(true);
    expect(isHidden(FULL)).toBe(false);
    expect(isHidden({ ...FULL, hidden: false })).toBe(false);
    // A built-in can be hidden too (hiding is a visibility preference, not an edit).
    expect(isHidden({ ...BUILTIN, hidden: true })).toBe(true);
  });

  it('partitions visible vs hidden, preserving input order in each group', () => {
    const list: Class[] = [
      { ...FULL, id: 'a', hidden: false },
      { ...FULL, id: 'b', hidden: true },
      { ...FULL, id: 'c', hidden: false },
      { ...BUILTIN, id: 'mgp-open', hidden: true }
    ];
    const { visible, hidden } = partitionHidden(list);
    expect(visible.map((c) => c.id)).toEqual(['a', 'c']);
    expect(hidden.map((c) => c.id)).toEqual(['b', 'mgp-open']);
  });
});
