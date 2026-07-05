/**
 * Shared audit-entry rendering for the RD console — the #337 recomposition helpers, extracted
 * from Marshaling so the event-wide **Audit page** and Marshaling's **Recent rulings** strip
 * render an entry the *same* way (and never drift, the friendly-names rule in CLAUDE.md).
 *
 * The server bakes raw LOG OFFSETS into some summaries ("Lap thrown out (ref 14)") because it
 * cannot resolve anything friendlier, and it names no competitor structurally for the
 * lap-/target-addressed rulings. The client can do better: a `ProtestResolved`/`RulingReversed`
 * targets another audit entry (whose own `competitor` is structured — chase the chain), and a
 * lap-addressed ruling targets a pass the lap list still carries (where a lap list is in hand).
 * So the rendered line is re-composed from the **structured** fields — resolved callsign first,
 * the target offset only as a trailing "· #N" detail — instead of printing the server's raw-ref
 * string. Every helper takes plain data, no Svelte reactivity: the screens own the reactive
 * assembly of the inputs (the entry list, the name resolver) and call these per render.
 */
import type { AuditEntry, AuditKind, CompetitorRef } from '@gridfpv/types';

/**
 * The subset of {@link AuditEntry} these helpers read. The event-wide `EventAuditEntry` rows are
 * the same fields plus `heat` (flattened on the wire), so both entry shapes satisfy this.
 */
export type AuditEntryLike = Pick<AuditEntry, 'kind' | 'at' | 'at_ref' | 'competitor' | 'summary'>;

/**
 * The inputs an audit line resolves against. `byRef` indexes the trail by each entry's global
 * `at_ref` so a target-addressed ruling (protest resolution, reversal) chases the entry it acted
 * on; `competitorName` is the screen's shared resolver ({@link createCompetitorNameResolver});
 * `competitorForPassRef` resolves a pass offset to the competitor whose lap it bounds — only
 * Marshaling (which holds the heat's lap list) can supply it, the event-wide page omits it and
 * those lines simply render without a name (never with a raw ref).
 */
export interface AuditRenderInputs {
  byRef: Map<number, AuditEntryLike>;
  competitorName: (ref: CompetitorRef) => string;
  competitorForPassRef?: (target: number) => CompetitorRef | undefined;
}

/** The compact kind label Marshaling's strip badges an entry with (one word per kind). */
export function auditKindLabel(kind: AuditKind): string {
  switch (kind) {
    case 'Voided':
      return 'Voided';
    case 'Inserted':
      return 'Inserted';
    case 'Adjusted':
      return 'Re-timed';
    case 'Split':
      return 'Split';
    case 'PenaltyApplied':
      return 'Penalty';
    case 'LapThrownOut':
      return 'Thrown out';
    case 'ProtestFiled':
      return 'Protest filed';
    case 'ProtestResolved':
      return 'Protest resolved';
    case 'RulingReversed':
      return 'Reversed';
    case 'HeatVoided':
      return 'Heat voided';
    case 'Pass':
      return 'Detection';
    default:
      return kind;
  }
}

/**
 * The **badge** text for an entry — the user-facing "what happened" the Audit page filters by.
 * Finer-grained than {@link auditKindLabel} for `PenaltyApplied`, which covers three distinct
 * rulings the RD thinks of separately (DQ / time penalty / points); the split is derived from the
 * server-baked summary prefix ("DQ applied…" / "+2.000s penalty" / "-4 points"), the one place
 * that distinction reaches the wire.
 */
export function auditBadge(entry: Pick<AuditEntryLike, 'kind' | 'summary'>): string {
  switch (entry.kind) {
    case 'PenaltyApplied':
      if (entry.summary.startsWith('DQ')) return 'DQ';
      if (entry.summary.includes('points')) return 'Points';
      return 'Time penalty';
    case 'Inserted':
      return 'Lap added';
    case 'Adjusted':
      return 'Lap re-timed';
    case 'Split':
      return 'Lap split';
    case 'Voided':
      return 'Detection voided';
    case 'LapThrownOut':
      return 'Lap thrown out';
    case 'HeatVoided':
      return 'Heat voided';
    case 'ProtestFiled':
      return 'Protest filed';
    case 'ProtestResolved':
      return 'Protest resolved';
    case 'RulingReversed':
      return 'Ruling reversed';
    case 'Pass':
      return 'Detection';
    default:
      return entry.kind;
  }
}

/** A local wall-clock time for an entry's `recorded_at` (µs since the Unix epoch), or ''. */
export function auditTime(at: number | null): string {
  if (at == null) return '';
  // `at` is microseconds since the Unix epoch (server recorded_at).
  return new Date(at / 1000).toLocaleTimeString();
}

/** The target log offset a server summary interpolates ("(ref 42)"), if any. */
export function summaryTargetRef(summary: string): number | undefined {
  const m = /\(ref (\d+)\)/.exec(summary);
  return m ? Number(m[1]) : undefined;
}

/** The summary with the raw "(ref N)" clause removed (the offset renders as a trailing detail). */
export function stripRefClause(summary: string): string {
  return summary.replace(/\s*\(ref \d+\)/, '');
}

/**
 * The competitor an audit entry concerns: its own structured ref when the action named one, else
 * chased through the entry's target — a `ProtestResolved` / `RulingReversed` targets another
 * audit entry (follow the chain), a lap-addressed ruling targets a pass in the lap list (when the
 * caller supplied `competitorForPassRef`). `undefined` when nothing resolves (a heat-void, or a
 * voided pass with no lap list in hand) — the line then renders without a name rather than with a
 * raw ref.
 */
export function auditCompetitor(
  entry: AuditEntryLike,
  inputs: AuditRenderInputs,
  depth = 0
): CompetitorRef | undefined {
  if (entry.competitor != null) return entry.competitor;
  const target = summaryTargetRef(entry.summary);
  if (target === undefined) return undefined;
  const chained = inputs.byRef.get(target);
  if (chained && chained !== entry && depth < 4) return auditCompetitor(chained, inputs, depth + 1);
  return inputs.competitorForPassRef?.(target);
}

/**
 * The displayed audit line: the **resolved callsign** (from the structured `competitor` ref, or
 * chased through the entry's target) joined to the summary with any server-baked raw "(ref N)"
 * stripped; the target offset renders only as a trailing "· #N" detail. A baked raw-id string
 * couldn't be re-resolved, so the composition happens here, client-side (#337).
 */
export function auditSummaryLine(entry: AuditEntryLike, inputs: AuditRenderInputs): string {
  const ref = auditCompetitor(entry, inputs);
  const target = summaryTargetRef(entry.summary);
  const text = stripRefClause(entry.summary);
  const line = ref != null ? `${inputs.competitorName(ref)} · ${text}` : text;
  return target !== undefined ? `${line} · #${target}` : line;
}

/**
 * The summary line **without** the competitor name — for surfaces that render the pilot as its
 * own element (the Audit page's clickable pilot chip) and must not repeat it in the text.
 */
export function auditActionText(entry: AuditEntryLike): string {
  const target = summaryTargetRef(entry.summary);
  const text = stripRefClause(entry.summary);
  return target !== undefined ? `${text} · #${target}` : text;
}

/**
 * A ruling/protest picker option: "‹what› — ‹callsign› · #‹offset›". The action + callsign is the
 * primary label; the entry's own log offset (what the reverse/resolve command targets) is only a
 * trailing detail, never the label itself (#337).
 */
export function rulingOptionLabel(entry: AuditEntryLike, inputs: AuditRenderInputs): string {
  const ref = auditCompetitor(entry, inputs);
  const text = stripRefClause(entry.summary);
  const who = ref != null ? ` — ${inputs.competitorName(ref)}` : '';
  return `${text}${who} · #${entry.at_ref}`;
}
