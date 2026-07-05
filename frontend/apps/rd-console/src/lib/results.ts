/**
 * Results-screen export helpers (#56).
 *
 * The export is a JSON download of the current views with **friendly names** baked in — competitor
 * refs resolved to callsigns and class/round ids to their labels (CLAUDE.md: never leak a raw id) —
 * while preserving the raw ref alongside so it stays traceable.
 */
import type {
  ClassStanding,
  CompetitorRef,
  HeatResult,
  Placement,
  RankEntry
} from '@gridfpv/types';

/** Serialize a value to pretty JSON; the bigint replacer is a defensive no-op now
 * that wire numerics are plain `number`s. */
export function toExportJson(value: unknown): string {
  return JSON.stringify(value, (_k, v) => (typeof v === 'bigint' ? Number(v) : v), 2);
}

/** A standings/ranking row with its competitor ref resolved to a friendly name (raw ref kept). */
type WithName<T> = Omit<T, 'competitor'> & { competitor: string; competitor_ref: CompetitorRef };

/**
 * A scored heat's placement with the competitor resolved to its friendly name. The raw
 * source-local ref stays alongside as `competitor_ref` (the same traceability convention as the
 * standings rows), replacing the wire's `CompetitorKey`.
 */
type PlacementWithName = Omit<Placement, 'competitor'> & {
  competitor: string;
  competitor_ref: CompetitorRef;
};

/** A {@link HeatResult} with every placement's competitor resolved (raw ref kept alongside). */
export type HeatResultExport = Omit<HeatResult, 'places'> & { places: PlacementWithName[] };

/** The friendly, human-usable results payload (whichever views are present). */
export interface ResultsExport {
  class_standings?: { class: string; standings: WithName<ClassStanding>[] };
  round_ranking?: { round: string; ranking: WithName<RankEntry>[] };
  /** The legacy event-level projection — competitor refs resolved like every other view (#341). */
  standings?: WithName<RankEntry>[];
  heatResult?: HeatResultExport;
}

/** The inputs the Results screen passes to {@link buildResultsExport}. */
export interface ResultsExportInput {
  /** Resolve a competitor ref to its friendly display name (the shared resolver). */
  resolveCompetitor: (ref: CompetitorRef) => string;
  className?: string;
  classStandings?: ClassStanding[];
  roundLabel?: string;
  roundRanking?: RankEntry[];
  standings?: RankEntry[];
  heatResult?: HeatResult;
}

/**
 * Build the friendly export payload: each competitor ref is resolved to its callsign (with the raw
 * ref preserved as `competitor_ref`), and the class / round are labelled. Only the views that are
 * present (the current view + any event projection) are included.
 */
export function buildResultsExport(input: ResultsExportInput): ResultsExport {
  const withName = <T extends { competitor: CompetitorRef }>(rows: T[]): WithName<T>[] =>
    rows.map((r) => ({
      ...r,
      competitor: input.resolveCompetitor(r.competitor),
      competitor_ref: r.competitor
    }));

  const out: ResultsExport = {};
  if (input.classStandings) {
    out.class_standings = {
      class: input.className ?? '—',
      standings: withName(input.classStandings)
    };
  }
  if (input.roundRanking) {
    out.round_ranking = { round: input.roundLabel ?? '—', ranking: withName(input.roundRanking) };
  }
  // The legacy event-level projections resolve too (#341): the standings rows through the same
  // `withName`, and the heat result's placements — whose wire competitor is a `CompetitorKey`
  // (`{ adapter, competitor }`) — through the resolver on the key's ref, raw ref kept alongside.
  if (input.standings) out.standings = withName(input.standings);
  if (input.heatResult) {
    out.heatResult = {
      ...input.heatResult,
      places: input.heatResult.places.map((p) => ({
        ...p,
        competitor: input.resolveCompetitor(p.competitor.competitor),
        competitor_ref: p.competitor.competitor
      }))
    };
  }
  return out;
}

/**
 * Trigger a browser download of `json` as `filename`. No-op outside a DOM (tests call
 * `toExportJson` directly). Returns whether a download was initiated.
 */
export function downloadJson(filename: string, json: string): boolean {
  if (typeof document === 'undefined' || typeof URL?.createObjectURL !== 'function') return false;
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
  return true;
}
