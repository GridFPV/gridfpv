/**
 * Keyboard shortcuts on the Run page, so one person can run the night with a hand on the
 * laptop. Space does the next step of the race's life (Start, then Stop, then Save) and
 * single letters mirror the buttons. Discard wants two presses within a few seconds: one
 * stray key must not throw a race away.
 *
 * The decision is pure (key + what the page can do now → an action). The page owns the
 * guards that depend on the document (typing in a field, a dialog open) through
 * {@link shortcutFree}.
 */
export type RunAction = 'start' | 'stop' | 'save' | 'discard' | 'legend';
export interface RunCaps {
  start: boolean;
  stop: boolean;
  save: boolean;
  discard: boolean;
}

/** The legend, in the order it is shown. */
export const RUN_KEYS: readonly { keys: string[]; does: string }[] = [
  { keys: ['Space'], does: 'the next step: Start, then Stop, then Save' },
  { keys: ['z'], does: 'Start' },
  { keys: ['x'], does: 'Stop, or call off a countdown' },
  { keys: ['s', 'c'], does: 'Save' },
  { keys: ['d', 'd'], does: 'Discard (press twice within 3 seconds; v works too)' },
  { keys: ['?'], does: 'show or hide this list' }
];

/** How long a first Discard press stays armed. */
export const DISCARD_ARM_MS = 3000;

/** What a key does given what the page can do now; `null` when nothing. */
export function runShortcut(key: string, caps: RunCaps): RunAction | null {
  // Caps Lock must not switch the keys off.
  switch (key.length === 1 ? key.toLowerCase() : key) {
    case ' ':
      // Never discards: the next step of a stopped race is to save it.
      return caps.start ? 'start' : caps.stop ? 'stop' : caps.save ? 'save' : null;
    case 'z':
      return caps.start ? 'start' : null;
    case 'x':
      return caps.stop ? 'stop' : null;
    case 's':
    case 'c':
      return caps.save ? 'save' : null;
    case 'd':
    case 'v':
      return caps.discard ? 'discard' : null;
    case '?':
      return 'legend';
    default:
      return null;
  }
}

interface KeyLike {
  key: string;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
  repeat: boolean;
  defaultPrevented: boolean;
}
interface DocLike {
  activeElement: Element | null;
  querySelector(selectors: string): Element | null;
}
const TYPING = new Set(['INPUT', 'TEXTAREA', 'SELECT']);

/**
 * True when a key press is the page's to act on: no modifier (Shift is fine, "?" needs
 * it), not a held key repeating, not already handled, not typed into a field, and no
 * dialog open (the native `<dialog>` the components use carries `open` while it shows).
 */
export function shortcutFree(e: KeyLike, doc: DocLike): boolean {
  if (e.ctrlKey || e.metaKey || e.altKey || e.repeat || e.defaultPrevented) return false;
  const el = doc.activeElement;
  if (el) {
    if (TYPING.has(el.tagName)) return false;
    if ((el as HTMLElement).isContentEditable) return false;
  }
  return doc.querySelector('dialog[open]') === null;
}
