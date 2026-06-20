/**
 * Registration command builder (#53).
 *
 * Registration binds a *source-local* competitor handle (a node seat, a sim name —
 * `CompetitorRef`, only meaningful relative to its `AdapterId`) to an event-scoped
 * `PilotId`. The adapter never does this binding itself (architecture.html §9); the
 * RD does it here via the `Register` command.
 *
 * The competitors the RD can bind are the ones the timing source has *seen* — surfaced
 * by the live projection (`LiveRaceState.active_pilots`, which are `CompetitorRef`s).
 * This module is the pure builder; the screen lists seen refs and emits one `Register`
 * per binding.
 */

import type { AdapterId, Command, CompetitorRef, PilotId } from '@gridfpv/types';

/** Bind a source-local competitor (on `adapter`) to an event-scoped `pilot`. */
export function registerCommand(
  adapter: AdapterId,
  competitor: CompetitorRef,
  pilot: PilotId
): Command {
  return { Register: { adapter, competitor, pilot } };
}
