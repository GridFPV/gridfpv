/**
 * Where the race overlay sits (`lib/overlayPlacement.ts`): the style a placement gives the
 * panel, and what is refused before it is sent.
 */
import { describe, expect, it } from 'vitest';
import type { OverlayPlacement } from '@gridfpv/types';
import {
  DEFAULT_PLACEMENT,
  logoUrls,
  placementProblem,
  placementStyle,
  samePlacement
} from '../src/lib/overlayPlacement.js';
import { OVERLAY_LOOK } from './support.js';

const at = (over: Partial<OverlayPlacement>): OverlayPlacement => ({
  ...DEFAULT_PLACEMENT,
  ...over
});

describe('placementStyle', () => {
  it('pins the panel to its corner, scaled about that corner, with the glass as solid as asked', () => {
    expect(placementStyle(DEFAULT_PLACEMENT)).toBe(
      'top: 24px; left: 24px; transform: scale(1); transform-origin: top left; --panel-alpha: 0.9;'
    );
    expect(
      placementStyle(at({ corner: 'BottomRight', x: 40, y: 10, scale: 150, opacity: 0 }))
    ).toBe(
      'bottom: 10px; right: 40px; transform: scale(1.5); transform-origin: bottom right; --panel-alpha: 0;'
    );
  });

  it('in a bottom corner the head stays put and the laps hang below it', () => {
    const bottom = at({ corner: 'BottomLeft', x: 30, y: 20, scale: 50 });
    // The head is 200 px tall unscaled, so 100 px at half size: its top edge is pinned.
    expect(placementStyle(bottom, { headPx: 200, hangs: true })).toBe(
      'top: calc(100vh - 20px - 100px); left: 30px; transform: scale(0.5); transform-origin: top left; --panel-alpha: 0.9;'
    );
    // Nothing hanging, or nothing measured yet: pinned by the bottom edge.
    expect(placementStyle(bottom, { headPx: 200, hangs: false })).toContain('bottom: 20px;');
    expect(placementStyle(bottom, { headPx: 0, hangs: true })).toContain('bottom: 20px;');
    // A top corner never needs it.
    expect(placementStyle(at({ corner: 'TopRight' }), { headPx: 200, hangs: true })).toContain(
      'top: 24px; right: 24px;'
    );
  });
});

describe('placementProblem', () => {
  it('passes what the Director takes and names what it would refuse', () => {
    expect(placementProblem(DEFAULT_PLACEMENT)).toBeUndefined();
    expect(placementProblem(at({ x: 0, y: 4000, scale: 25, opacity: 0 }))).toBeUndefined();
    expect(placementProblem(at({ x: -1 }))).toMatch(/whole pixels, 0 to 4000/);
    expect(placementProblem(at({ y: 1.5 }))).toMatch(/whole pixels/);
    expect(placementProblem(at({ scale: 301 }))).toBe('The size is 25 to 300 percent.');
    expect(placementProblem(at({ scale: Number.NaN }))).toBe('The size is 25 to 300 percent.');
    expect(placementProblem(at({ opacity: 101 }))).toBe(
      'The background is 0 to 100 percent solid.'
    );
  });
});

describe('samePlacement and logoUrls', () => {
  it('compares every field', () => {
    expect(samePlacement(DEFAULT_PLACEMENT, at({}))).toBe(true);
    expect(samePlacement(DEFAULT_PLACEMENT, at({ opacity: 89 }))).toBe(false);
  });

  it('gives an address only for a side that has a logo', () => {
    const url = (side: string) => `/overlay/logo/${side}`;
    expect(logoUrls(undefined, url)).toEqual({});
    expect(logoUrls(OVERLAY_LOOK, url)).toEqual({});
    expect(logoUrls({ ...OVERLAY_LOOK, logo_right: true }, url)).toEqual({
      right: '/overlay/logo/right'
    });
    expect(logoUrls({ ...OVERLAY_LOOK, logo_left: true, logo_right: true }, url, 3)).toEqual({
      left: '/overlay/logo/left?v=3',
      right: '/overlay/logo/right?v=3'
    });
  });
});
