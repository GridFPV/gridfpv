/**
 * Per-section collapse persistence for the data-heavy event stages.
 *
 * The Classes & Roster and Rounds & Heats stages get dense (20 pilots across classes; many rounds,
 * each with a heats list), so the RD collapses sections to manage the density. A collapsed/expanded
 * choice should *stick* across navigation and reload, so it's persisted in `localStorage` keyed per
 * stable section id — namespaced by the event so two events don't clobber each other:
 *
 *   gf.collapse.<eventId>.<sectionId>
 *
 * `collapseStore(eventId, sectionId, defaultOpen)` returns a tiny `{ open }` rune-state object: read
 * `store.open` in markup and `bind:open={store.open}` into a {@link Collapsible}; every write mirrors
 * to `localStorage`. A missing/blocked storage degrades to in-memory (private mode, SSR, quota).
 */

const PREFIX = 'gf.collapse';

function keyFor(eventId: string, sectionId: string): string {
  return `${PREFIX}.${eventId}.${sectionId}`;
}

/** Read a persisted open state; `undefined` when unset (so the caller's default wins). */
function read(key: string): boolean | undefined {
  try {
    const raw = globalThis.localStorage?.getItem(key);
    if (raw === '1') return true;
    if (raw === '0') return false;
    return undefined;
  } catch {
    return undefined;
  }
}

function write(key: string, open: boolean): void {
  try {
    globalThis.localStorage?.setItem(key, open ? '1' : '0');
  } catch {
    /* private mode / quota / no storage — degrade to in-memory only */
  }
}

/** A reactive, persisted open flag for one collapsible section. */
export interface CollapseStore {
  /** The section's open state. Reading is reactive; assigning persists it. */
  open: boolean;
}

/**
 * A rune-backed `{ open }` store for the section `sectionId` under `eventId`. The initial value is
 * the persisted choice if there is one, else `defaultOpen` (so finished sections can default closed
 * and active/incomplete ones open). Reads are reactive; every assignment writes through to storage.
 */
export function collapseStore(
  eventId: string,
  sectionId: string,
  defaultOpen = true
): CollapseStore {
  const key = keyFor(eventId, sectionId);
  let open = $state(read(key) ?? defaultOpen);
  return {
    get open() {
      return open;
    },
    set open(v: boolean) {
      open = v;
      write(key, v);
    }
  };
}
