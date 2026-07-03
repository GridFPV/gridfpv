/**
 * The cross-screen **Audit prefilter seam**: how another screen jumps to the Audit tab already
 * filtered to a heat or a pilot (Marshaling's "View full audit →" on the marshaled heat, a
 * Results row's per-pilot audit link).
 *
 * Navigation itself stays the shell's job (`setTab` writes the `#/event/audit` hash, route.ts) —
 * but a hash carries no payload, and threading a filter through the shell's route state would
 * grow the URL scheme for what is a one-shot handoff. So the prefilter is a tiny module-level
 * `$state` mailbox instead: {@link openAudit} deposits it and switches the tab; the Audit screen
 * {@link consumeAuditPrefilter consumes and clears} it on mount. One-shot by design — a later
 * manual visit to the Audit tab (or a refresh, which drops the in-memory mailbox) starts
 * unfiltered, exactly like every other tab.
 */
import type { CompetitorRef, HeatId } from '@gridfpv/types';

import type { WorkspaceTab } from './route.js';

/** What the Audit page should be pre-filtered to on arrival. Both fields optional. */
export interface AuditPrefilter {
  /** Pre-select this heat in the Audit page's heat filter. */
  heat?: HeatId;
  /** Pre-select this pilot (competitor ref) in the Audit page's pilot filter. */
  pilot?: CompetitorRef;
}

// The one-shot mailbox. Module-level `$state` so the depositing screen and the consuming screen
// share it without threading it through the shell; exported only via the functions below (a
// module cannot export reassigned `$state` directly).
let pending = $state<AuditPrefilter | undefined>(undefined);

/**
 * Jump to the Audit tab pre-filtered: deposit `prefilter`, then switch the tab. `setTab` is the
 * shell's tab navigation (App.svelte threads it in via the screen's callback prop, the same way
 * sibling screens receive `ongolive` etc.).
 */
export function openAudit(setTab: (tab: WorkspaceTab) => void, prefilter: AuditPrefilter): void {
  pending = prefilter;
  setTab('audit');
}

/**
 * Take the pending prefilter, clearing it — the Audit page calls this once on mount. Returns
 * `undefined` when nothing was deposited (a direct visit to the tab).
 */
export function consumeAuditPrefilter(): AuditPrefilter | undefined {
  const prefilter = pending;
  pending = undefined;
  return prefilter;
}
