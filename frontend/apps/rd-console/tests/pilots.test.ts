import { describe, expect, it } from 'vitest';
import type { Pilot } from '@gridfpv/types';
import {
  buildCreateRequest,
  buildUpdateRequest,
  cleanAttributes,
  emptyForm,
  formFromPilot,
  type PilotFormValues
} from '../src/lib/pilots.js';

/** A pilot with every field populated — the baseline for the edit/diff tests. */
const FULL: Pilot = {
  id: 'p1',
  callsign: 'Ace',
  name: 'Alice',
  phonetic: 'AYSS',
  team: 'Red',
  color: '#FF0000',
  country: 'US',
  vtx_type: 'DJI',
  multigp_id: 'mg-1',
  velocidrone_id: 'vd-1',
  attributes: { bib: '7' }
};

describe('buildCreateRequest', () => {
  it('sends only the callsign + filled optionals (blank optionals omitted)', () => {
    const v = emptyForm();
    v.callsign = '  Ace  ';
    v.name = 'Alice';
    v.country = 'us'; // lower-case input → uppercased
    const req = buildCreateRequest(v);
    expect(req).toEqual({ callsign: 'Ace', name: 'Alice', country: 'US', attributes: {} });
    // Untouched optionals are absent (not empty strings).
    expect(req).not.toHaveProperty('team');
    expect(req).not.toHaveProperty('color');
    expect(req).not.toHaveProperty('vtx_type');
  });

  it('carries the cleaned attributes bag', () => {
    const v = emptyForm();
    v.callsign = 'Ace';
    v.attributes = { ' license ': 'FAA-9', '': 'dropped' };
    const req = buildCreateRequest(v);
    expect(req.attributes).toEqual({ license: 'FAA-9' });
  });
});

describe('buildUpdateRequest — clear-via-null', () => {
  function valuesFrom(p: Pilot): PilotFormValues {
    return formFromPilot(p);
  }

  it('omits every field when nothing changed', () => {
    const req = buildUpdateRequest(FULL, valuesFrom(FULL));
    expect(req).toEqual({});
  });

  it('emits null for color and country when the user clears them', () => {
    const v = valuesFrom(FULL);
    v.color = '';
    v.country = '';
    const req = buildUpdateRequest(FULL, v);
    expect(req.color).toBeNull();
    expect(req.country).toBeNull();
    // Unchanged fields stay omitted.
    expect(req).not.toHaveProperty('name');
    expect(req).not.toHaveProperty('team');
  });

  it('sets a changed value and leaves others untouched', () => {
    const v = valuesFrom(FULL);
    v.name = 'Alicia';
    const req = buildUpdateRequest(FULL, v);
    expect(req).toEqual({ name: 'Alicia' });
  });

  it('clears the VTX type with null when set to None', () => {
    const v = valuesFrom(FULL);
    v.vtx = '';
    const req = buildUpdateRequest(FULL, v);
    expect(req.vtx_type).toBeNull();
  });

  it('uppercases a changed country code and only sends it when it differs', () => {
    const v = valuesFrom(FULL);
    v.country = 'gb';
    expect(buildUpdateRequest(FULL, v).country).toBe('GB');
    // Same code in different case is NOT a change.
    const same = valuesFrom(FULL);
    same.country = 'us';
    expect(buildUpdateRequest(FULL, same)).not.toHaveProperty('country');
  });

  it('sends the full attributes map only when it differs', () => {
    const unchanged = valuesFrom(FULL);
    expect(buildUpdateRequest(FULL, unchanged)).not.toHaveProperty('attributes');

    const added = valuesFrom(FULL);
    added.attributes = { bib: '7', sponsor: 'Acme' };
    expect(buildUpdateRequest(FULL, added).attributes).toEqual({ bib: '7', sponsor: 'Acme' });

    const cleared = valuesFrom(FULL);
    cleared.attributes = {};
    expect(buildUpdateRequest(FULL, cleared).attributes).toEqual({});
  });

  it('replaces the callsign when changed and never clears it when blanked', () => {
    const changed = valuesFrom(FULL);
    changed.callsign = 'Neo';
    expect(buildUpdateRequest(FULL, changed).callsign).toBe('Neo');

    const blanked = valuesFrom(FULL);
    blanked.callsign = '   ';
    expect(buildUpdateRequest(FULL, blanked)).not.toHaveProperty('callsign');
  });
});

describe('cleanAttributes', () => {
  it('trims keys and drops blank ones', () => {
    expect(cleanAttributes({ ' a ': '1', '': '2', b: '3' })).toEqual({ a: '1', b: '3' });
  });
});
