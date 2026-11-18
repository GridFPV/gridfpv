/**
 * Which phases are unfolded on the Phases page, to show the phases inside them.
 *
 * Kept outside the page so the choice survives opening a phase and coming back. It is only
 * how one console looks: it is not saved, and a reload folds everything again.
 */
import { SvelteSet } from 'svelte/reactivity';
import type { PhaseId } from '@gridfpv/types';

export const unfolded = new SvelteSet<PhaseId>();

/** Fold or unfold one phase. */
export function toggleFold(phase: PhaseId): void {
  if (unfolded.has(phase)) unfolded.delete(phase);
  else unfolded.add(phase);
}
