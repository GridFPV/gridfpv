/**
 * Format identity + friendly-name + per-format field shaping (Rounds form redesign).
 *
 * The engine's `FormatRegistry` keys (`open_practice`, `head_to_head`, `timed_qual`, …) are the wire values stored
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
 * The **Head-to-Head** racing format key (D17) — the atomic racing round: a field split into heats of
 * `group_size`, each won by the round's win condition, then ranked by `scoring` (Placement, or Points
 * from a per-position table authored in the round form). It is the building block tournament
 * structures compose; as a directly-addable round it is one of the three round types the Add-round
 * picker offers.
 */
export const HEAD_TO_HEAD = 'head_to_head';

/** Whether a format key is the Head-to-Head racing format. */
export function isHeadToHeadFormat(format: string | undefined | null): boolean {
  return format === HEAD_TO_HEAD;
}

/**
 * The **Time Trials** (qualifying) format key. The wire key is the persistence-stable `timed_qual`
 * (only the friendly label was renamed Qualifying → Time Trials, #218 / D11); a time-trial round is
 * ranked by its win-condition metric — **Best of N laps** (N = 1 being the best single lap).
 *
 * *Not* "Timed — Most Laps": that was reclassified head-to-head-only in #472. Pilots flying
 * simultaneously and competing on lap count in one window are racing each other; "time trial"
 * means the solo/async run against the clock.
 */
export const TIMED_QUAL = 'timed_qual';

/** Whether a format key is the Time Trials (time-trial / qualifying) format. */
export function isTimedQualFormat(format: string | undefined | null): boolean {
  return format === TIMED_QUAL;
}

/**
 * The **qualifying** format keys — currently just `timed_qual`. For these the cross-round
 * ranking metric *is* the win condition (the qualifying metric is derived from the win condition,
 * not a separate stored param — Rounds form redesign), so the win-condition dropdown offers only
 * the qualifying-applicable condition (Best of N laps) and the separate metric field is gone.
 */
export const QUALIFYING_FORMATS: readonly string[] = ['timed_qual'];

/** Whether a format key is a qualifying format (its win condition drives the qualifying metric). */
export function isQualifyingFormat(format: string | undefined | null): boolean {
  return !!format && QUALIFYING_FORMATS.includes(format);
}

/**
 * The win-condition kinds the Rounds form authors — the discriminator, not the wire shape.
 *
 * `BestOfN` is the converged time-trial metric: N = 1 is the best single lap (serialised as
 * `BestLap`), N > 1 the best N consecutive laps (`BestConsecutive`). `Timed` is most-laps-in-a-
 * window; `FirstToLaps` is the race to a lap target.
 */
export type WinConditionKind = 'Timed' | 'FirstToLaps' | 'BestOfN';

/** Every win-condition kind, in picker order — the offering for a format with no family rule. */
export const WIN_CONDITION_KINDS: readonly WinConditionKind[] = ['Timed', 'FirstToLaps', 'BestOfN'];

/**
 * Which win-condition kinds `format` may be raced under — **the format↔win-condition taxonomy**,
 * and the single source the Rounds form's win-condition picker groups by (#472).
 *
 * - **Head-to-Head** → `Timed` | `FirstToLaps`. Both are ways to *decide a race between pilots on
 *   track together*: most laps inside one shared window, or first to a lap target. `BestOfN` is a
 *   time-trial metric, not how you decide a race.
 * - **Qualifying / Time Trials** → `BestOfN` only. A time trial is a solo/async run against the
 *   clock, ranked by your best result. **`Timed` — Most Laps was moved out of this bucket in
 *   #472**: pilots flying simultaneously and competing on lap count in one window are racing each
 *   other, which is head-to-head. (Qualifying a field *by* total laps is a round-level seeding
 *   concern, not a claim about what kind of format this is.)
 * - Anything else (a tournament structure round reached by editing) → all three, unchanged.
 *
 * Returns kinds in {@link WIN_CONDITION_KINDS} order so the picker's option order is stable.
 */
export function winConditionKindsFor(
  format: string | undefined | null
): readonly WinConditionKind[] {
  if (isHeadToHeadFormat(format)) return ['Timed', 'FirstToLaps'];
  if (isQualifyingFormat(format)) return ['BestOfN'];
  return WIN_CONDITION_KINDS;
}

/**
 * The win-condition kind a fresh round of `format` opens on: the first kind the format offers
 * ({@link winConditionKindsFor}). So Head-to-Head defaults to `Timed` (unchanged) and a Time Trial
 * defaults to `BestOfN` rather than to a `Timed` its family no longer offers.
 */
export function defaultWinConditionKindFor(format: string | undefined | null): WinConditionKind {
  return winConditionKindsFor(format)[0];
}

/** The picker's human-readable label for a win-condition kind. */
export const WIN_CONDITION_LABELS: Readonly<Record<WinConditionKind, string>> = {
  Timed: 'Timed — Most Laps',
  FirstToLaps: 'First to N laps',
  BestOfN: 'Best of N laps'
};

/**
 * The three **round types** the Add-round picker offers (D17 taxonomy): Practice ({@link OPEN_PRACTICE}),
 * Time Trial (`timed_qual`), and Head-to-Head ({@link HEAD_TO_HEAD}). Everything an RD flies directly is
 * one of these. Tournament *structures* (round-robin, single/double elim, multi-main) are **not** round
 * types — they are composed from Head-to-Head via the tournament builder, so they never appear in the
 * Add-round menu (only via editing an existing structure round, or the builder).
 */
export const ROUND_TYPE_FORMATS: readonly string[] = [OPEN_PRACTICE, 'timed_qual', HEAD_TO_HEAD];

/** Whether a format key is one of the three directly-addable round types (vs a tournament structure). */
export function isRoundTypeFormat(format: string | undefined | null): boolean {
  return !!format && ROUND_TYPE_FORMATS.includes(format);
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
  // The `open_practice` wire key is unchanged (persistence-stable); only the friendly label was
  // shortened "Open Practice" → "Practice".
  open_practice: 'Practice',
  // The `timed_qual` wire key is unchanged (persistence-stable); only the friendly label was
  // renamed Qualifying → Time Trials (#218 / decisions D11).
  timed_qual: 'Time Trials',
  // NOTE: ZippyQ shelved (#218) — the generator stays registered (so persisted `zippyq` rounds
  // still load) but the format is excluded from the offered set, so a new round can't select it.
  // Its label is kept here so any persisted `zippyq` round still renders a friendly name.
  zippyq: 'ZippyQ',
  head_to_head: 'Head-to-Head'
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
 * Whether a format's heats are **deterministic** — its whole set of heats is a pure function of the
 * field (and, across rounds, the prior heats' results), so the round can be filled in **one action**
 * ("Generate heats", #216). Every format *except* Open Practice is deterministic: Time Trials and
 * Head-to-Head draw a fixed schedule from the roster/ranking.
 *
 * Open Practice is the lone **dynamic** format — its channel-seated practice heats are created on
 * demand, with no fixed set — so it single-steps ("Add next heat") instead. Defined as "not open
 * practice" (with a guard for a missing format) so a future deterministic format is included by
 * default rather than silently excluded.
 */
export function isDeterministicFormat(format: string | undefined | null): boolean {
  return !!format && !isOpenPracticeFormat(format);
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
