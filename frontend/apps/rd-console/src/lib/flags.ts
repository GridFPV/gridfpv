/**
 * Flag SVG lookup for {@link CountrySelect} / the pilot directory (issue #74).
 *
 * We render **SVG** flags, never emoji: Windows has no flag-emoji font, so the unicode regional
 * indicators fall back to bare letters ("US") there — unreadable on the very console the RD runs on
 * a Windows laptop. `country-flag-icons/string/3x2` ships each flag as an inline SVG **string**
 * keyed by ISO 3166-1 alpha-2 code, which we resolve here and the UI drops in with `{@html}`.
 *
 * The strings are static, self-contained `<svg>…</svg>` markup from the dependency (no external
 * fetch, no `<img>`), so they theme/scale with the surrounding box and carry no scripting.
 */
import * as Flags from 'country-flag-icons/string/3x2';

/** The code → SVG-string map the dependency exports (a plain record once the namespace is read). */
const FLAGS = Flags as unknown as Record<string, string | undefined>;

/**
 * The inline SVG markup for a country `code` (ISO 3166-1 alpha-2, case-insensitive), or `undefined`
 * if there is no flag for it. The returned string is a complete `<svg>` element the caller injects
 * with `{@html}`.
 */
export function flagSvg(code: string | undefined): string | undefined {
  if (!code) return undefined;
  return FLAGS[code.toUpperCase()];
}

/** Whether a flag SVG exists for the given alpha-2 `code`. */
export function hasFlag(code: string | undefined): boolean {
  return flagSvg(code) !== undefined;
}
