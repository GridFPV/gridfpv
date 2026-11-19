/**
 * A pilot's sign-in code (docs/primitives.md §11.3): how it is written out for a person,
 * what is wrong with one as typed, and the link a QR code holds. Pure.
 */

/** How many letters and digits a code has. The Director decides; this only saves a trip. */
export const CODE_LENGTH = 6;
/** What a code is written in: no `0`, `O`, `1` or `I`, which are easy to mistake. */
const CODE_CHARS = /^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]+$/;

/** A code as it is shown: in two halves, so it can be read out and typed. `ABC 234`. */
export function spellCode(code: string): string {
  const half = Math.ceil(code.length / 2);
  return `${code.slice(0, half)} ${code.slice(half)}`.trim();
}

/** A code as typed, tidied the way the Director tidies it: capitals, no spaces or dashes. */
export function tidyCode(typed: string): string {
  return typed.replace(/[\s-]/g, '').toUpperCase();
}

/**
 * What is wrong with a code as typed, in words, or `undefined` when it can be sent. The
 * Director checks it for real; this catches a slip before it counts as a wrong try.
 */
export function codeProblem(typed: string): string | undefined {
  const code = tidyCode(typed);
  if (code === '') return 'Type your code.';
  if (code.length < CODE_LENGTH) return `A code has ${CODE_LENGTH} letters and numbers.`;
  if (code.length > CODE_LENGTH) return `A code has only ${CODE_LENGTH} letters and numbers.`;
  if (!CODE_CHARS.test(code)) {
    return 'A code has capital letters and numbers, and never 0, O, 1 or I.';
  }
  return undefined;
}

/**
 * The code in a page address, when a pilot got here by scanning their QR code:
 * `#/join/ABC234`. `undefined` for any other address.
 */
export function codeInHash(hash: string): string | undefined {
  const found = /^#\/join\/([^/?#]+)$/.exec(hash);
  if (!found) return undefined;
  let code: string;
  try {
    code = decodeURIComponent(found[1]);
  } catch {
    return undefined;
  }
  return codeProblem(code) === undefined ? tidyCode(code) : undefined;
}

/**
 * How many cards a printed page of sign-in codes may hold. Six is three across and two
 * down, which is as many as fit a page with a QR code big enough to scan.
 */
export const PER_PAGE_CHOICES = [4, 6] as const;
export type PerPage = (typeof PER_PAGE_CHOICES)[number];

/** Split `rows` into pages of `perPage`, in order. No rows is no pages. */
export function sheetPages<T>(rows: readonly T[], perPage: PerPage): T[][] {
  const pages: T[][] = [];
  for (let at = 0; at < rows.length; at += perPage) pages.push(rows.slice(at, at + perPage));
  return pages;
}
