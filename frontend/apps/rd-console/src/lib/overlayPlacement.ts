/**
 * Where the race overlay sits on the video: the corner its panel hangs from, pixels in from
 * that corner's edges, its size as a percentage, and how solid its background is.
 *
 * There is one placement for the whole Director (`GET /overlay/look`). The live page in
 * OBS, the replay page, and so every overlay the Director redraws onto a recording all use
 * it, so they agree.
 */
import type { OverlayCorner, OverlayLook, OverlayPlacement } from '@gridfpv/types';

export const CORNERS: readonly OverlayCorner[] = [
  'TopLeft',
  'TopRight',
  'BottomLeft',
  'BottomRight'
];
export const CORNER_LABELS: Record<OverlayCorner, string> = {
  TopLeft: 'Top left',
  TopRight: 'Top right',
  BottomLeft: 'Bottom left',
  BottomRight: 'Bottom right'
};

export const DEFAULT_PLACEMENT: OverlayPlacement = {
  corner: 'TopLeft',
  x: 24,
  y: 24,
  scale: 100,
  opacity: 90
};
export const OFFSET_MAX = 4000;
export const SCALE_MIN = 25;
export const SCALE_MAX = 300;
export const OPACITY_MIN = 0;
export const OPACITY_MAX = 100;

const SIDES: Record<OverlayCorner, ['top' | 'bottom', 'left' | 'right']> = {
  TopLeft: ['top', 'left'],
  TopRight: ['top', 'right'],
  BottomLeft: ['bottom', 'left'],
  BottomRight: ['bottom', 'right']
};

/**
 * The panel's inline style. The scale sizes the panel's *head* (title, clock, pilot, stats).
 * The head is what sits in the corner, and the lap list hangs below it and grows downward
 * whichever corner was chosen, so a clock in a bottom corner never creeps up as laps come
 * in. For a bottom corner that means pinning the head's top edge at `100vh - y - head`,
 * where `head` is the head's unscaled height. Until it is measured (or with nothing below
 * it, which comes to the same place) the head is pinned by its bottom edge instead.
 * `--panel-alpha` is the background's opacity, which the panel's glass reads.
 */
export function placementStyle(
  p: OverlayPlacement,
  measure: { headPx: number; hangs: boolean } = { headPx: 0, hangs: false }
): string {
  const [v, h] = SIDES[p.corner];
  const s = p.scale / 100;
  const alpha = ` --panel-alpha: ${p.opacity / 100};`;
  if (v === 'bottom' && measure.hangs && measure.headPx > 0)
    return `top: calc(100vh - ${p.y}px - ${Math.round(measure.headPx * s * 100) / 100}px); ${h}: ${p.x}px; transform: scale(${s}); transform-origin: top ${h};${alpha}`;
  return `${v}: ${p.y}px; ${h}: ${p.x}px; transform: scale(${s}); transform-origin: ${v} ${h};${alpha}`;
}

export function samePlacement(a: OverlayPlacement, b: OverlayPlacement): boolean {
  return (
    a.corner === b.corner &&
    a.x === b.x &&
    a.y === b.y &&
    a.scale === b.scale &&
    a.opacity === b.opacity
  );
}

/**
 * Where each logo is served from, for the sides that have one. `stamp` is added to the
 * address so a logo that was just replaced is fetched again.
 */
export function logoUrls(
  look: OverlayLook | undefined,
  urlOf: (side: 'left' | 'right') => string,
  stamp?: number
): { left?: string; right?: string } {
  const at = (side: 'left' | 'right') =>
    stamp === undefined ? urlOf(side) : `${urlOf(side)}?v=${stamp}`;
  return {
    ...(look?.logo_left ? { left: at('left') } : {}),
    ...(look?.logo_right ? { right: at('right') } : {})
  };
}

/** Why a placement cannot be saved, or `undefined` when it can. */
export function placementProblem(p: OverlayPlacement): string | undefined {
  const whole = (n: number) => Number.isInteger(n);
  if (![p.x, p.y].every((n) => whole(n) && n >= 0 && n <= OFFSET_MAX))
    return `The distances from the edges are whole pixels, 0 to ${OFFSET_MAX}.`;
  if (!whole(p.scale) || p.scale < SCALE_MIN || p.scale > SCALE_MAX)
    return `The size is ${SCALE_MIN} to ${SCALE_MAX} percent.`;
  if (!whole(p.opacity) || p.opacity < OPACITY_MIN || p.opacity > OPACITY_MAX)
    return 'The background is 0 to 100 percent solid.';
  return undefined;
}
