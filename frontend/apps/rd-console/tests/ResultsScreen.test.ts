import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type {
  Class,
  ClassStandings,
  EventMeta,
  HeatResult,
  HeatSummary,
  Pilot,
  RankEntry,
  RoundDef,
  RoundMetric,
  RoundStanding
} from '@gridfpv/types';
import Results from '../src/screens/Results.svelte';
import { heatResult, standings } from './fixtures.js';
import { makeTestSession } from './support.js';

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const ACE: Pilot = { id: 'p1', callsign: 'AceOne', vtx_types: [] };
const BOLT: Pilot = { id: 'p2', callsign: 'Bolt', vtx_types: [] };

const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: ['p1', 'p2'],
  classes: ['c1']
};

const STANDINGS: ClassStandings = {
  class: 'c1',
  standings: [
    {
      competitor: 'p1',
      position: 1,
      points: 6,
      best_lap_micros: 41_250_000, // → "41.250"
      total_laps: 9,
      rounds_entered: 2
    },
    {
      competitor: 'p2',
      position: 2,
      points: 3,
      best_lap_micros: null, // → "—"
      total_laps: 4,
      rounds_entered: 2
    }
  ]
};

// A non-bracket head-to-head round (the roundRanking path), and a single-level single-elim tournament
// seeded from it. The tournament root's label carries the "‹Bracket› — ‹Level›" convention so it reads
// by its name. (A timed_qual round takes the richer Best-lap-+-metric standings path — see TT below.)
const QUAL: RoundDef = {
  id: 'r1',
  label: 'Qualifying',
  classes: ['c1'],
  format: 'head_to_head',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster',
  channel_mode: 'Static',
  staging_timer_secs: 300,
  start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
  grace_window: { Duration: { micros: 3_000_000 } },
  protest_window: 'Off'
};
const BRACKET_FINAL: RoundDef = {
  id: 'b1',
  label: 'Pro — Final',
  classes: ['c1'],
  format: 'single_elim',
  params: { heat_size: '2' },
  win_condition: { FirstToLaps: { n: 3 } },
  seeding: { FromRanking: { source_rounds: ['r1'], top_n: 2 } },
  channel_mode: 'PerHeat',
  staging_timer_secs: 300,
  start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
  grace_window: { Duration: { micros: 3_000_000 } },
  protest_window: 'Off'
};

const QUAL_HEAT: HeatSummary = {
  heat: 'r1-h1',
  lineup: ['p1', 'p2'],
  round: 'r1',
  class: 'c1',
  phase: 'Final',
  is_current: false
};
const FINAL_HEAT: HeatSummary = {
  heat: 'b1-h1',
  lineup: ['p1', 'p2'],
  round: 'b1',
  class: 'c1',
  phase: 'Final',
  is_current: false
};

/** A round-ranking mock: the bracket final ranks p1 first, the qualifier ranks p2 first. */
const rankingImpl = vi.fn(
  async (_b: string, _e: string, roundId: string): Promise<RankEntry[]> =>
    roundId === 'b1'
      ? [
          { competitor: 'p1', position: 1 },
          { competitor: 'p2', position: 2 }
        ]
      : [
          { competitor: 'p2', position: 1 },
          { competitor: 'p1', position: 2 }
        ]
);

describe('Results — per-class standings (race redesign Slice 5/6b)', () => {
  it('renders a class standings table with callsign, points, best lap (µs→s.mmm), laps, and rounds', async () => {
    const { session } = makeTestSession({
      event: EVENT,
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => []),
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });

    const table = (await screen.findByLabelText(/Open standings/i)) as HTMLElement;
    // Pilot 1's row: resolved callsign, points, best lap formatted µs → "41.250", laps, rounds.
    const aceCell = within(table).getByText('AceOne');
    const aceRow = aceCell.closest('tr') as HTMLElement;
    expect(within(aceRow).getByText('6')).toBeInTheDocument();
    expect(within(aceRow).getByText('41.250')).toBeInTheDocument();
    expect(within(aceRow).getByText('9')).toBeInTheDocument();
    // Pilot 2 has no best lap → renders a dash.
    const boltRow = within(table).getByText('Bolt').closest('tr') as HTMLElement;
    expect(within(boltRow).getByText('—')).toBeInTheDocument();
  });

  it('shows an empty state for a class with no scored rounds yet', async () => {
    const { session } = makeTestSession({
      event: EVENT,
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => []),
      classStandingsImpl: vi.fn(async () => ({ class: 'c1', standings: [] }))
    });
    render(Results, { session });
    await waitFor(() => expect(screen.getByText(/Nothing scored/i)).toBeInTheDocument());
  });

  it('shows a no-results message when the event has no rounds and selects no classes', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, classes: [] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [])
    });
    render(Results, { session });
    expect(await screen.findByText(/No results yet/i)).toBeInTheDocument();
  });
});

describe('Results — phase-aware views (round / per-class selector)', () => {
  it('lists each round, then per-class — and defaults to the latest scored round', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL, BRACKET_FINAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [QUAL_HEAT, FINAL_HEAT]),
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });

    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    const options = within(select).getAllByRole('option') as HTMLOptionElement[];
    // Order: each round (by label), immediately followed by that round's SCORED heats (the per-heat
    // results view, #56 / #77), then the class. Each heat reads by its FRIENDLY name.
    expect(options.map((o) => o.textContent?.trim())).toEqual([
      'Qualifying',
      'Qualifying Heat 1',
      'Pro — Final',
      'Pro — Final Heat 1',
      'Open'
    ]);
    expect(options.map((o) => o.value)).toEqual([
      'round:r1',
      'heat:r1-h1',
      'round:b1',
      'heat:b1-h1',
      'class:c1'
    ]);
    // A round's heats sit in an optgroup titled for the round, so the grouping is visible too.
    expect(
      Array.from(select.querySelectorAll('optgroup')).map((g) => g.getAttribute('label'))
    ).toEqual(['Qualifying heats', 'Pro — Final heats']);
    // Both rounds have scored heats, so the default selection is the latest scored round.
    await waitFor(() => expect(select.value).toBe('round:b1'));
  });

  it('defaults to the latest scored round when no bracket has run', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [QUAL_HEAT]),
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:r1'));
    // The round's ranking renders as a standings table with callsigns resolved (p2 first).
    const table = (await screen.findByLabelText(/Qualifying standings/i)) as HTMLElement;
    const rows = within(table).getAllByRole('row').slice(1); // drop the header row
    expect(within(rows[0]).getByText('Bolt')).toBeInTheDocument();
    expect(within(rows[1]).getByText('AceOne')).toBeInTheDocument();
    // The raw refs never leak.
    expect(within(table).queryByText('p1')).not.toBeInTheDocument();
    expect(within(table).queryByText('p2')).not.toBeInTheDocument();
  });

  it('defaults to per-class standings when nothing has run', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => []), // no heats → nothing has run
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('class:c1'));
  });

  it('selecting a round shows its roundRanking as a standings table (callsigns resolved)', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL, BRACKET_FINAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [QUAL_HEAT, FINAL_HEAT]),
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:b1'));

    await fireEvent.change(select, { target: { value: 'round:r1' } });
    const table = (await screen.findByLabelText(/Qualifying standings/i)) as HTMLElement;
    expect(within(table).getByText('Bolt')).toBeInTheDocument();
    expect(within(table).getByText('AceOne')).toBeInTheDocument();
  });

  it('each ROUND-standings row offers an audit jump pre-filtered to that pilot', async () => {
    // The per-pilot "audit" affordance (the defensible-results cross-link): clicking it hands the
    // pilot's competitor ref to the auditFilter seam, which the shell wires to the Audit tab.
    const onviewaudit = vi.fn();
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [QUAL_HEAT]),
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session, onviewaudit });

    const table = (await screen.findByLabelText(/Qualifying standings/i)) as HTMLElement;
    // The affordance is labelled by the resolved callsign — never the raw pilot id.
    await fireEvent.click(within(table).getByRole('button', { name: 'View audit for Bolt' }));
    expect(onviewaudit).toHaveBeenCalledWith({ pilot: 'p2' });
  });
});

describe('Results — time-trial round standings (Best lap + win-condition metric)', () => {
  // A timed_qual round + its scored heat. The round view takes the richer Best-lap-+-metric standings.
  const TT: RoundDef = { ...QUAL, id: 'tt1', label: 'Time Trials', format: 'timed_qual' };
  const TT_HEAT: HeatSummary = {
    heat: 'tt1-h1',
    lineup: ['p1', 'p2'],
    round: 'tt1',
    class: 'c1',
    phase: 'Final',
    is_current: false
  };
  /** A standings mock for a given win-condition metric (p1 has a value, p2 a no-show → nulls). */
  const standingsImpl = (p1Metric: RoundMetric, p2Metric: RoundMetric) =>
    vi.fn(
      async (): Promise<RoundStanding[]> => [
        { competitor: 'p1', position: 1, best_lap_micros: 41_250_000, laps: 9, metric: p1Metric },
        { competitor: 'p2', position: 2, best_lap_micros: null, laps: 4, metric: p2Metric }
      ]
    );

  it('renders a "Best lap" column and a "Best N consec" metric column for BestConsecutive', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [TT] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [TT_HEAT]),
      roundStandingsImpl: standingsImpl(
        { BestConsecutive: { n: 3, micros: 12_345_000 } },
        { BestConsecutive: { n: 3, micros: null } }
      ),
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:tt1'));

    const table = (await screen.findByLabelText(/Time Trials standings/i)) as HTMLElement;
    // Headers: Pos | Pilot | Best lap | Best 3 consec.
    const heads = within(table)
      .getAllByRole('columnheader')
      .map((h) => h.textContent?.trim());
    expect(heads).toEqual(['Pos', 'Pilot', 'Best lap', 'Best 3 consec']);
    // p1 row: callsign, best lap formatted (µs → "41.250"), best-3-consec formatted ("12.345").
    const aceRow = within(table).getByText('AceOne').closest('tr') as HTMLElement;
    expect(within(aceRow).getByText('41.250')).toBeInTheDocument();
    expect(within(aceRow).getByText('12.345')).toBeInTheDocument();
    // p2 (no value) → dashes for both the best lap and the metric.
    const boltRow = within(table).getByText('Bolt').closest('tr') as HTMLElement;
    expect(within(boltRow).getAllByText('—')).toHaveLength(2);
    // Raw refs never leak.
    expect(within(table).queryByText('p1')).not.toBeInTheDocument();
    expect(within(table).queryByText('p2')).not.toBeInTheDocument();
  });

  it('renders a "Laps" metric column for MostLaps, with the lap counts', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [TT] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [TT_HEAT]),
      roundStandingsImpl: standingsImpl({ MostLaps: { laps: 9 } }, { MostLaps: { laps: 4 } }),
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:tt1'));

    const table = (await screen.findByLabelText(/Time Trials standings/i)) as HTMLElement;
    const heads = within(table)
      .getAllByRole('columnheader')
      .map((h) => h.textContent?.trim());
    expect(heads).toEqual(['Pos', 'Pilot', 'Best lap', 'Laps']);
    const aceRow = within(table).getByText('AceOne').closest('tr') as HTMLElement;
    expect(within(aceRow).getByText('41.250')).toBeInTheDocument(); // best lap
    expect(within(aceRow).getByText('9')).toBeInTheDocument(); // laps metric
    const boltRow = within(table).getByText('Bolt').closest('tr') as HTMLElement;
    expect(within(boltRow).getByText('4')).toBeInTheDocument();
  });

  it('omits the extra metric column for a BestLap round (Best lap already covers it)', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [TT] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [TT_HEAT]),
      roundStandingsImpl: standingsImpl(
        { BestLap: { micros: 41_250_000 } },
        { BestLap: { micros: null } }
      ),
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:tt1'));

    const table = (await screen.findByLabelText(/Time Trials standings/i)) as HTMLElement;
    const heads = within(table)
      .getAllByRole('columnheader')
      .map((h) => h.textContent?.trim());
    expect(heads).toEqual(['Pos', 'Pilot', 'Best lap']);
  });
});

describe('Results — durable per-heat name resolution (the raw node-0 fix)', () => {
  it('resolves a FINISHED node-seeded heat’s seats from the heat-window bindings, never raw', async () => {
    // A node-seeded heat: the ranking rows carry the raw `node-0` seat ref, the heat is FINISHED,
    // and the global live stream is on a DIFFERENT heat (so its progress can't resolve it — the
    // regression). The durable bind (`node-0 → p1`) lives in the heat's own `?projection=live`
    // fold, which `session.ensureHeatBindings` pulls + caches for the screen's resolver.
    const NODE_HEAT: HeatSummary = {
      heat: 'r1-h1',
      lineup: ['node-0'],
      round: 'r1',
      class: 'c1',
      frequencies: [],
      phase: 'Final',
      is_current: false
    };
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      live: { current_heat: 'other-heat', phase: 'Running' },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [NODE_HEAT]),
      roundRankingImpl: vi.fn(async () => [{ competitor: 'node-0', position: 1 }]),
      classStandingsImpl: vi.fn(async () => STANDINGS),
      heatFetches: {
        'r1-h1': {
          live: {
            current_heat: 'r1-h1',
            phase: 'Final',
            progress: [{ competitor: 'node-0', pilot: 'p1', laps_completed: 3 }]
          }
        }
      }
    });
    render(Results, { session });

    const table = (await screen.findByLabelText(/Qualifying standings/i)) as HTMLElement;
    // The seat resolves to the bound pilot's callsign — never the raw `node-0` (CLAUDE.md).
    await waitFor(() => expect(within(table).getByText('AceOne')).toBeInTheDocument());
    expect(within(table).queryByText('node-0')).not.toBeInTheDocument();
    expect(within(table).queryByText('Node 1')).not.toBeInTheDocument();
  });
});

describe('Results — fetch races + heats-read failure (#340)', () => {
  it('latest wins: a SLOWER earlier round fetch cannot overwrite the newer view', async () => {
    // r1's ranking read hangs (resolved manually below); b1's resolves immediately. Start on r1,
    // flip to b1, then let r1's stale response land — the b1 table must stand.
    let resolveQual!: (rows: RankEntry[]) => void;
    const racingRankingImpl = vi.fn(
      (_b: string, _e: string, roundId: string): Promise<RankEntry[]> =>
        roundId === 'r1'
          ? new Promise<RankEntry[]>((res) => (resolveQual = res))
          : Promise.resolve([
              { competitor: 'p1', position: 1 },
              { competitor: 'p2', position: 2 }
            ])
    );
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL, BRACKET_FINAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      // Only r1 has scored → the default view is round:r1 (its fetch is the slow one).
      listHeatsImpl: vi.fn(async () => [QUAL_HEAT, { ...FINAL_HEAT, phase: 'Scheduled' as const }]),
      roundRankingImpl: racingRankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });

    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:r1'));
    // Flip to the bracket final while r1's read is still in flight.
    await fireEvent.change(select, { target: { value: 'round:b1' } });
    const table = (await screen.findByLabelText(/Pro — Final standings/i)) as HTMLElement;
    await waitFor(() => expect(within(table).getByText('AceOne')).toBeInTheDocument());

    // The STALE r1 response lands late (Bolt first) — without the latest-wins guard it replaced
    // the rendered rows, leaving Qualifying's order under the "Pro — Final" header.
    resolveQual([
      { competitor: 'p2', position: 1 },
      { competitor: 'p1', position: 2 }
    ]);
    await waitFor(() => {
      const rows = within(screen.getByLabelText(/Pro — Final standings/i))
        .getAllByRole('row')
        .slice(1);
      expect(within(rows[0]).getByText('AceOne')).toBeInTheDocument();
      expect(within(rows[1]).getByText('Bolt')).toBeInTheDocument();
    });
  });

  it('keeps the last good heats list on a failed re-read, with a visible retry (#340)', async () => {
    // First read succeeds; every later read fails until `heal` flips. The old code swallowed the
    // failure into `heats = []`, silently blanking the phase default + channel labels.
    let heal = false;
    let calls = 0;
    const listHeatsImpl = vi.fn(async () => {
      calls += 1;
      if (calls > 1 && !heal) throw new Error('GET /events/e1/heats failed: HTTP 500');
      return [QUAL_HEAT];
    });
    const { session, pushLive } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl,
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:r1'));

    // A stream tick re-reads the heats list — this read FAILS.
    pushLive({ current_heat: 'r1-h1', phase: 'Unofficial' });
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/Couldn't load the heats list/);
    // The last good list stands: the round view (derived off the heats) keeps rendering.
    expect(select.value).toBe('round:r1');
    expect(screen.getByLabelText(/Qualifying standings/i)).toBeInTheDocument();

    // Retry with the read healthy → the error state clears.
    heal = true;
    await fireEvent.click(within(alert).getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('alert')).toBeNull());
  });
});

describe('Results — event-level projections (kept from #56)', () => {
  it('renders a ranking from typed fixtures', () => {
    render(Results, { heatResult, standings });
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
  });

  it('shows an empty state when nothing is scored yet and no session', () => {
    render(Results, {});
    expect(screen.getByText(/No results yet/i)).toBeInTheDocument();
  });

  it('offers an export action', () => {
    render(Results, { heatResult });
    expect(screen.getByRole('button', { name: 'Export JSON' })).toBeInTheDocument();
  });
});

// ── Per-heat results (#56 / #77) ────────────────────────────────────────────────────────────────
// The engine models penalties fully — a DQ is ranked after every non-DQ competitor and a voided
// heat is nullified — but until this view NO console file consumed `Placement.disqualified` or
// `HeatResult.voided`, so the ordering changed with no visible cause. These lock the display.

/** A clean two-pilot heat: no DQ, not voided. Best-lap metric → the metric column is suppressed. */
const CLEAN_HEAT_RESULT: HeatResult = {
  places: [
    {
      competitor: { adapter: 'rh-1', competitor: 'p1' },
      position: 1,
      laps: 3,
      metric: { BestLapMicros: 41_250_000 },
      best_lap_micros: 41_250_000
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'p2' },
      position: 2,
      laps: 3,
      metric: { BestLapMicros: 42_100_000 },
      best_lap_micros: 42_100_000
    }
  ]
};

/**
 * A heat where the DQ is the ONLY explanation for the order: AceOne flew more laps (5 v 4) and a
 * faster best lap, and is still placed second. Without the reason on screen this reads as a bug.
 */
const DQ_HEAT_RESULT: HeatResult = {
  places: [
    {
      competitor: { adapter: 'rh-1', competitor: 'p2' },
      position: 1,
      laps: 4,
      metric: { BestConsecutiveMicros: 45_000_000 },
      best_lap_micros: 41_000_000
    },
    {
      competitor: { adapter: 'rh-1', competitor: 'p1' },
      position: 2,
      laps: 5,
      metric: { BestConsecutiveMicros: 44_000_000 },
      best_lap_micros: 39_000_000,
      disqualified: true
    }
  ]
};

/** Build a session whose `r1-h1` heat-scope result read serves `result`, and select that heat. */
async function renderHeatView(result: HeatResult): Promise<HTMLElement> {
  const { session } = makeTestSession({
    event: { ...EVENT, rounds: [QUAL] },
    listClassesImpl: vi.fn(async () => [OPEN]),
    listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
    listHeatsImpl: vi.fn(async () => [QUAL_HEAT]),
    roundRankingImpl: rankingImpl,
    classStandingsImpl: vi.fn(async () => STANDINGS),
    heatFetches: { 'r1-h1': { result } }
  });
  render(Results, { session });
  const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
  await waitFor(() => expect(select.value).toBe('round:r1'));
  await fireEvent.change(select, { target: { value: 'heat:r1-h1' } });
  // The table is labelled by the heat's FRIENDLY name, never its raw `r1-h1` id.
  return (await screen.findByLabelText('Qualifying Heat 1 results')) as HTMLElement;
}

describe('Results — per-heat results view (#56 / #77)', () => {
  it('renders a clean heat unchanged: placements by callsign, laps and best lap, no DQ or void marking', async () => {
    const table = await renderHeatView(CLEAN_HEAT_RESULT);

    const aceRow = within(table).getByText('AceOne').closest('tr') as HTMLElement;
    expect(within(aceRow).getByText('1')).toBeInTheDocument(); // position
    expect(within(aceRow).getByText('3')).toBeInTheDocument(); // laps
    expect(within(aceRow).getByText('41.250')).toBeInTheDocument(); // best lap, µs → S.mmm
    expect(within(table).getByText('Bolt')).toBeInTheDocument();

    // A clean heat carries no penalty furniture at all.
    expect(within(table).queryByText('DQ')).toBeNull();
    expect(screen.queryByText(/Disqualified/i)).toBeNull();
    expect(screen.queryByText(/voided/i)).toBeNull();
    // The Best-lap metric is already its own column, so no duplicate metric column is added.
    const headers = within(table)
      .getAllByRole('columnheader')
      .map((h) => h.textContent?.trim());
    expect(headers).toEqual(['Pos', 'Pilot', 'Laps', 'Best lap']);
    // The raw competitor refs never reach the screen (CLAUDE.md).
    expect(within(table).queryByText('p1')).toBeNull();
    expect(within(table).queryByText('p2')).toBeNull();
  });

  it('shows a disqualified pilot by callsign WITH a visible reason, not just a worse position', async () => {
    const table = await renderHeatView(DQ_HEAT_RESULT);

    // The DQ'd pilot reads by CALLSIGN, never the raw ref.
    const aceRow = within(table).getByText('AceOne').closest('tr') as HTMLElement;
    expect(within(table).queryByText('p1')).toBeNull();

    // …is marked as disqualified…
    expect(within(aceRow).getByText('DQ')).toBeInTheDocument();
    // …and the WHY is on screen, not merely the fact that they placed last.
    expect(
      within(aceRow).getByText(/Disqualified — ranked after every finisher/i)
    ).toBeInTheDocument();
    // The footnote spells out the rule and points at the ruling behind it.
    expect(screen.getByText(/disqualified by a marshaling ruling/i)).toBeInTheDocument();

    // The on-track numbers stay readable — the RD must still see what was flown.
    expect(within(aceRow).getByText('5')).toBeInTheDocument(); // laps
    expect(within(aceRow).getByText('39.000')).toBeInTheDocument(); // best lap
    // The non-DQ winner carries no marking.
    const boltRow = within(table).getByText('Bolt').closest('tr') as HTMLElement;
    expect(within(boltRow).queryByText('DQ')).toBeNull();

    // A non-best-lap win condition adds its deciding-metric column.
    const headers = within(table)
      .getAllByRole('columnheader')
      .map((h) => h.textContent?.trim());
    expect(headers).toEqual(['Pos', 'Pilot', 'Laps', 'Best consec', 'Best lap']);
    expect(within(aceRow).getByText('44.000')).toBeInTheDocument();
  });

  it('marks a voided heat so it cannot read as a normal result', async () => {
    const table = await renderHeatView({ ...CLEAN_HEAT_RESULT, voided: true });

    // An alert-level banner states it plainly, before the table.
    const banner = screen.getByRole('alert');
    expect(banner).toHaveTextContent(/Heat voided/i);
    expect(banner).toHaveTextContent(/does not count toward the round or class standings/i);
    // The placements are still shown for reference, and still by callsign.
    expect(within(table).getByText('AceOne')).toBeInTheDocument();
    expect(within(table).getByText('Bolt')).toBeInTheDocument();
  });

  it('surfaces a failed per-heat result read as an error with a retry, not an empty heat', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [QUAL_HEAT]),
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
      // No `heatFetches` seed → the heat-scope read fails.
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('round:r1'));
    await fireEvent.change(select, { target: { value: 'heat:r1-h1' } });

    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(/Couldn't load the standings/i);
    expect(within(alert).getByRole('button', { name: 'Try again' })).toBeInTheDocument();
  });

  it('lists only SCORED heats — an unfinalized heat has no result to show', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [QUAL] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [
        QUAL_HEAT,
        { ...QUAL_HEAT, heat: 'r1-h2', phase: 'Running' as const }
      ]),
      roundRankingImpl: rankingImpl,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    const values = (within(select).getAllByRole('option') as HTMLOptionElement[]).map(
      (o) => o.value
    );
    expect(values).toContain('heat:r1-h1');
    expect(values).not.toContain('heat:r1-h2');
  });
});
