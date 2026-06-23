/**
 * Format identity + friendly-name + per-format field shaping (Rounds form redesign).
 *
 * The engine's `FormatRegistry` keys (`open_practice`, `double_elim`, …) are the wire values stored
 * on a {@link RoundDef.format}; they are not human-readable. This module is the single source of
 * truth that maps each key to a friendly label and decides **which fields the Rounds create/edit
 * form shows for that format**, so the form is field-driven rather than a wall of every control.
 *
 * Reused by:
 *  - the Rounds create/edit form (field order + dynamic per-format sections),
 *  - the Rounds list + Heats area (which show the friendly name in place of the raw key).
 *
 * Kept framework-pure (no Svelte) so it unit-tests directly and every surface shares one map.
 */

/** The casual open-practice format key — a channel-seated, scoring-free practice run. */
export const OPEN_PRACTICE = 'open_practice';

/**
 * The **qualifying** format keys — `timed_qual` and `round_robin`. For these the cross-round
 * ranking metric *is* the win condition (the qualifying metric is derived from the win condition,
 * not a separate stored param — Rounds form redesign), so the win-condition dropdown offers only
 * the qualifying-applicable conditions (Best lap, Best N consecutive, Timed — Most Laps) and the
 * separate metric field is gone.
 */
export const QUALIFYING_FORMATS: readonly string[] = ['timed_qual', 'round_robin'];

/** Whether a format key is a qualifying format (its win condition drives the qualifying metric). */
export function isQualifyingFormat(format: string | undefined | null): boolean {
  return !!format && QUALIFYING_FORMATS.includes(format);
}

/**
 * The friendly, human-readable label for each known format key (the engine's
 * `FormatRegistry::standard` names). The form selector, the Rounds list, and the Heats area all
 * render this in place of the raw key while the key stays the stored wire value.
 *
 * An unknown key (a format the engine added after this map) falls back to a de-slugged title-case
 * of the key via {@link formatLabel}, so the UI never shows a bare `snake_case` token.
 */
export const FORMAT_LABELS: Readonly<Record<string, string>> = {
  open_practice: 'Open Practice',
  timed_qual: 'Qualifying',
  round_robin: 'Round Robin',
  zippyq: 'ZippyQ',
  single_elim: 'Single Elimination',
  double_elim: 'Double Elimination',
  multi_main: 'Multi-Main'
};

/**
 * The friendly label for a format key: the {@link FORMAT_LABELS} entry, else a de-slugged
 * title-case of the key (`some_new_format` → `Some New Format`) so an unmapped key is still
 * readable. A blank/undefined key yields an empty string.
 */
export function formatLabel(format: string | undefined | null): string {
  if (!format) return '';
  const known = FORMAT_LABELS[format];
  if (known) return known;
  return format
    .split(/[_-]+/)
    .filter((w) => w.length > 0)
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ');
}

/**
 * Which fields a given format's section shows in the Rounds form (Rounds form redesign).
 *
 * The form always shows **Label** then **Format**; the remaining fields are driven by this shape so
 * only the controls a format actually uses appear. Mirrors the backend's actual usage:
 *
 *  - **Open Practice** — a channel-seated, scoring-free run: the active-channels picker + an optional
 *    time limit. No eligible class, no win condition, no seeding, no channel mode.
 *  - **Bracket formats** (single/double elim, multi-main) — seeded from a prior round's ranking:
 *    eligible class + seeding (incl. the `FromRanking` source/top-N) + channel mode + win condition.
 *  - **Roster-seeded formats** (qualifying, round-robin, ZippyQ) — eligible class + win condition +
 *    seeding + channel mode.
 *
 * Every non-open-practice format also surfaces its declared **params** as proper labeled fields (the
 * caller renders them from the format's `FormatSchema`), so a format's knobs (`rounds`, `heat_size`,
 * `metric`, `bracket_reset`, `main_size`) appear inline in its section.
 */
export interface FormatFields {
  /** The eligible-class single-select dropdown (every format but open practice). */
  eligibleClass: boolean;
  /** The win-condition control (every format but open practice). */
  winCondition: boolean;
  /** The seeding control — From roster / From ranking (every format but open practice). */
  seeding: boolean;
  /** The Static / Per-heat channel-mode control (every format but open practice). */
  channelMode: boolean;
  /** The active-channels picker (open practice only). */
  activeChannels: boolean;
  /** The optional practice time-limit field (open practice only). */
  timeLimit: boolean;
  /** Whether to surface the format's declared params as labeled fields (non-open-practice). */
  params: boolean;
}

/** Whether a format key is the open-practice format. */
export function isOpenPracticeFormat(format: string | undefined | null): boolean {
  return format === OPEN_PRACTICE;
}

/**
 * The set of fields the Rounds form shows for `format`. Open practice swaps the class/win/seeding/
 * channel-mode block for the active-channels picker + time limit; every other format shows the full
 * roster/bracket block plus its declared params.
 */
export function fieldsForFormat(format: string | undefined | null): FormatFields {
  if (isOpenPracticeFormat(format)) {
    return {
      eligibleClass: false,
      winCondition: false,
      seeding: false,
      channelMode: false,
      activeChannels: true,
      timeLimit: true,
      params: false
    };
  }
  return {
    eligibleClass: true,
    winCondition: true,
    seeding: true,
    channelMode: true,
    activeChannels: false,
    timeLimit: false,
    params: true
  };
}
