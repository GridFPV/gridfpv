/**
 * Setup-wizard config model (#52).
 *
 * The wizard walks the RD through event → class(es) → track → format + win condition
 * (clients.html §1, roadmap v0.4). It produces a single typed {@link EventConfig}.
 *
 * ── GAP: no event/class/track setup command on the contract yet ─────────────────
 * `Command` (protocol.html §5) today exposes only `ScheduleHeat` and `Register` among
 * the setup-shaped variants — there is NO `ConfigureEvent` / `DefineClass` / `SetTrack`
 * / `SetFormat` command. So the wizard's event/class/track/format choices are held in
 * **app state** (this config object), and the one part it *can* drive on the wire is
 * scheduling heats: each planned heat becomes a `ScheduleHeat` command carrying its
 * lineup. When #45 (and the server's setup surface) grows configuration commands, the
 * wizard emits them here and this config becomes their payload rather than a local hold.
 *
 * `WinCondition` is taken verbatim from `@gridfpv/types` (it is a real wire type the
 * heat/format scoring already uses), so the format step is type-accurate today even
 * though the event/class/track steps are local-only.
 */

import type {
  ClassId,
  Command,
  CompetitorRef,
  EventId,
  HeatId,
  WinCondition
} from '@gridfpv/types';

/** The race format a class runs under (roadmap v0.4: timed-qual / single-elim / zippyq). */
export type RaceFormat = 'timed-qual' | 'single-elim' | 'zippyq';

/** Human labels for the formats, for the wizard's format step. */
export const FORMAT_LABELS: Record<RaceFormat, string> = {
  'timed-qual': 'Timed qualifying',
  'single-elim': 'Single elimination',
  zippyq: 'ZippyQ (rolling)'
};

/** One class within the event being configured. */
export interface ClassConfig {
  /** Event-scoped class id (a stable handle the RD names). */
  id: ClassId;
  /** Human class name (e.g. "Open", "Spec"). */
  name: string;
  /** The format this class runs. */
  format: RaceFormat;
  /** How a heat in this class is won. */
  winCondition: WinCondition;
}

/** The whole event configuration the wizard produces. */
export interface EventConfig {
  /** Event id (a stable handle the RD names). */
  eventId: EventId;
  /** Human event name. */
  eventName: string;
  /** Track / venue name (free text until a track model exists on the wire). */
  track: string;
  /** The classes configured for the event (at least one). */
  classes: ClassConfig[];
}

/** A reasonable default win condition per format, for the wizard's initial state. */
export function defaultWinCondition(format: RaceFormat): WinCondition {
  switch (format) {
    case 'timed-qual':
      // A 2-minute timed window (µs).
      return { Timed: { window_micros: 120_000_000 } };
    case 'single-elim':
      // First to 3 laps decides a bracket heat.
      return { FirstToLaps: { n: 3 } };
    case 'zippyq':
      // ZippyQ ranks on best 3 consecutive laps.
      return { BestConsecutive: { n: 3 } };
  }
}

/** An empty config to seed the wizard. */
export function emptyConfig(): EventConfig {
  return { eventId: '', eventName: '', track: '', classes: [] };
}

/** Build a default class config for `format`. */
export function defaultClass(id: ClassId, name: string, format: RaceFormat): ClassConfig {
  return { id, name, format, winCondition: defaultWinCondition(format) };
}

/**
 * Validate the config for completeness — the wizard's "you can finish" gate. Returns
 * the list of human problems (empty ⇒ ready).
 */
export function validateConfig(config: EventConfig): string[] {
  const problems: string[] = [];
  if (!config.eventName.trim()) problems.push('Event needs a name.');
  if (!config.eventId.trim()) problems.push('Event needs an id.');
  if (!config.track.trim()) problems.push('Track needs a name.');
  if (config.classes.length === 0) problems.push('Add at least one class.');
  config.classes.forEach((c, i) => {
    if (!c.id.trim()) problems.push(`Class ${i + 1} needs an id.`);
    if (!c.name.trim()) problems.push(`Class ${i + 1} needs a name.`);
  });
  return problems;
}

/** Is the config complete enough to finish the wizard? */
export function isConfigComplete(config: EventConfig): boolean {
  return validateConfig(config).length === 0;
}

/**
 * Build the one setup command the contract supports today: `ScheduleHeat`, creating a
 * heat with its lineup (protocol.html §5). The wizard uses this to lay down the first
 * heat(s) once the field is known; the rest of the config is held locally (see the
 * module note on the gap).
 */
export function scheduleHeatCommand(heat: HeatId, lineup: CompetitorRef[]): Command {
  return { ScheduleHeat: { heat, lineup } };
}
