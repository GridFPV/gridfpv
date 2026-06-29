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
import { heatResult, standings, eventOutcome } from './fixtures.js';
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

describe('Results — phase-aware views (round / tournament selector)', () => {
  it('lists tournaments, then rounds, then per-class — and defaults to the in-progress tournament', async () => {
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
    // Order: the tournament (by bracket name "Pro"), then the non-bracket round, then the class.
    expect(options.map((o) => o.textContent?.trim())).toEqual(['Pro', 'Qualifying', 'Open']);
    expect(options.map((o) => o.value)).toEqual(['tournament:b1', 'round:r1', 'class:c1']);
    // The bracket has run, so the default selection is that tournament.
    await waitFor(() => expect(select.value).toBe('tournament:b1'));
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
    await waitFor(() => expect(select.value).toBe('tournament:b1'));

    await fireEvent.change(select, { target: { value: 'round:r1' } });
    const table = (await screen.findByLabelText(/Qualifying standings/i)) as HTMLElement;
    expect(within(table).getByText('Bolt')).toBeInTheDocument();
    expect(within(table).getByText('AceOne')).toBeInTheDocument();
  });

  it('selecting a tournament shows the bracket tree, champion chip, and final standings', async () => {
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
    await waitFor(() => expect(select.value).toBe('tournament:b1'));

    // The champion chip (the bracket final ranks p1 → AceOne first).
    await waitFor(() => expect(screen.getByText('Champion')).toBeInTheDocument());
    expect(screen.getAllByText('AceOne').length).toBeGreaterThan(0);
    // The bracket tree column for the final level.
    expect(screen.getByRole('heading', { name: 'Final' })).toBeInTheDocument();
    // The final standings table, callsigns resolved.
    const finals = (await screen.findByLabelText(/Pro final standings/i)) as HTMLElement;
    expect(within(finals).getByText('AceOne')).toBeInTheDocument();
    expect(within(finals).getByText('Bolt')).toBeInTheDocument();
  });

  it('final standings rank the FULL field (not just finalists) — champion first, by seed, callsigns resolved', async () => {
    // A 4-pilot, 2-level bracket: a 2-heat semis (root, seeded FromRanking off the qualifier) → a
    // one-heat final. The qualifier ranks the seeds; the SF losers should rank by that seed order.
    const NITRO: Pilot = { id: 'p1', callsign: 'Nitro', vtx_types: [] };
    const SURGE: Pilot = { id: 'p2', callsign: 'Surge', vtx_types: [] };
    const VOLT: Pilot = { id: 'p3', callsign: 'Volt', vtx_types: [] };
    const DASH: Pilot = { id: 'p4', callsign: 'Dash', vtx_types: [] };
    const event: EventMeta = { ...EVENT, roster: ['p1', 'p2', 'p3', 'p4'] };

    const qual: RoundDef = { ...QUAL, id: 'q1' };
    const semis: RoundDef = {
      ...BRACKET_FINAL,
      id: 's1',
      label: 'Pro — Semifinals',
      seeding: { FromRanking: { source_rounds: ['q1'], top_n: 4 } }
    };
    const final: RoundDef = {
      ...BRACKET_FINAL,
      id: 'f1',
      label: 'Pro — Final',
      seeding: { FromHeatWinners: { source_round: 's1' } }
    };
    // Seeds Volt=1, Surge=2, Dash=3, Nitro=4. Semis pair Volt vs Nitro and Surge vs Dash; the final
    // is Nitro vs Surge; champion = Nitro. Full-field order: Nitro(champ), Surge(runner-up), then SF
    // losers by seed — Volt (seed 1) before Dash (seed 3).
    const heats: HeatSummary[] = [
      {
        heat: 's1-h1',
        lineup: ['p3', 'p1'],
        round: 's1',
        class: 'c1',
        phase: 'Final',
        is_current: false
      },
      {
        heat: 's1-h2',
        lineup: ['p2', 'p4'],
        round: 's1',
        class: 'c1',
        phase: 'Final',
        is_current: false
      },
      {
        heat: 'f1-h1',
        lineup: ['p1', 'p2'],
        round: 'f1',
        class: 'c1',
        phase: 'Final',
        is_current: false
      }
    ];
    const ranking = vi.fn(
      async (_b: string, _e: string, roundId: string): Promise<RankEntry[]> =>
        roundId === 'q1'
          ? [
              { competitor: 'p3', position: 1 }, // Volt seed 1
              { competitor: 'p2', position: 2 }, // Surge seed 2
              { competitor: 'p4', position: 3 }, // Dash seed 3
              { competitor: 'p1', position: 4 } // Nitro seed 4
            ]
          : roundId === 'f1'
            ? [
                { competitor: 'p1', position: 1 }, // Nitro champion
                { competitor: 'p2', position: 2 }
              ]
            : []
    );

    const { session } = makeTestSession({
      event: { ...event, rounds: [qual, semis, final] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [NITRO, SURGE, VOLT, DASH]),
      listHeatsImpl: vi.fn(async () => heats),
      roundRankingImpl: ranking,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('tournament:s1'));

    const finals = (await screen.findByLabelText(/Pro final standings/i)) as HTMLElement;
    // The full field — all FOUR pilots — appears, not just the two finalists.
    await waitFor(() => {
      expect(within(finals).getByText('Nitro')).toBeInTheDocument();
      expect(within(finals).getByText('Surge')).toBeInTheDocument();
      expect(within(finals).getByText('Volt')).toBeInTheDocument();
      expect(within(finals).getByText('Dash')).toBeInTheDocument();
    });
    const rows = within(finals).getAllByRole('row').slice(1); // drop the header
    expect(rows).toHaveLength(4);
    // Order: champion Nitro, runner-up Surge, then SF losers by seed (Volt seed 1 before Dash seed 3).
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining('Nitro'),
      expect.stringContaining('Surge'),
      expect.stringContaining('Volt'),
      expect.stringContaining('Dash')
    ]);
    // Raw refs never leak.
    expect(within(finals).queryByText('p1')).not.toBeInTheDocument();
    expect(within(finals).queryByText('p3')).not.toBeInTheDocument();
  });

  it('points-order a tournament tier (a 4-up heat): champion first, the rest by POINTS not seed', async () => {
    // A single 4-up heat that IS the whole bracket. Seeds (off the qualifier) run OPPOSITE to the
    // heat finish, so a points-ordered standing proves the screen tiebreaks by points, not seed.
    const NITRO: Pilot = { id: 'p1', callsign: 'Nitro', vtx_types: [] };
    const SURGE: Pilot = { id: 'p2', callsign: 'Surge', vtx_types: [] };
    const VOLT: Pilot = { id: 'p3', callsign: 'Volt', vtx_types: [] };
    const DASH: Pilot = { id: 'p4', callsign: 'Dash', vtx_types: [] };
    const event: EventMeta = { ...EVENT, roster: ['p1', 'p2', 'p3', 'p4'] };

    const qual: RoundDef = { ...QUAL, id: 'q1' };
    const bracket: RoundDef = {
      ...BRACKET_FINAL,
      id: 'b1',
      label: 'Pro — Final',
      params: { heat_size: '4' },
      seeding: { FromRanking: { source_rounds: ['q1'], top_n: 4 } }
    };
    const heats: HeatSummary[] = [
      {
        heat: 'b1-h1',
        lineup: ['p1', 'p2', 'p3', 'p4'],
        round: 'b1',
        class: 'c1',
        phase: 'Final',
        is_current: false
      }
    ];
    // Qualifier seeds Dash=1, Volt=2, Surge=3, Nitro=4 (opposite the finish). The final ranks Nitro 1st.
    const ranking = vi.fn(
      async (_b: string, _e: string, roundId: string): Promise<RankEntry[]> =>
        roundId === 'q1'
          ? [
              { competitor: 'p4', position: 1 }, // Dash seed 1
              { competitor: 'p3', position: 2 }, // Volt seed 2
              { competitor: 'p2', position: 3 }, // Surge seed 3
              { competitor: 'p1', position: 4 } // Nitro seed 4
            ]
          : roundId === 'b1'
            ? [
                { competitor: 'p1', position: 1 }, // Nitro champion
                { competitor: 'p2', position: 2 },
                { competitor: 'p3', position: 3 },
                { competitor: 'p4', position: 4 }
              ]
            : []
    );
    // The scored heat result: Nitro→1 (10pts), Surge→2 (6), Volt→3 (4), Dash→4 (3). So the non-champion
    // tier orders Surge, Volt, Dash by points — the REVERSE of their seed order (Dash, Volt, Surge).
    const result: HeatResult = {
      places: [
        {
          competitor: { adapter: 'rh', competitor: 'p1' },
          position: 1,
          laps: 3,
          metric: { BestLapMicros: 1 }
        },
        {
          competitor: { adapter: 'rh', competitor: 'p2' },
          position: 2,
          laps: 3,
          metric: { BestLapMicros: 1 }
        },
        {
          competitor: { adapter: 'rh', competitor: 'p3' },
          position: 3,
          laps: 3,
          metric: { BestLapMicros: 1 }
        },
        {
          competitor: { adapter: 'rh', competitor: 'p4' },
          position: 4,
          laps: 3,
          metric: { BestLapMicros: 1 }
        }
      ]
    } as unknown as HeatResult;

    const { session } = makeTestSession({
      event: { ...event, rounds: [qual, bracket] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [NITRO, SURGE, VOLT, DASH]),
      listHeatsImpl: vi.fn(async () => heats),
      roundRankingImpl: ranking,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    // Re-stub fetch so the per-heat result read (session.fetchHeatResult) returns our scored result —
    // this backs the points tiebreak. Must be in place before render so the effect reads it.
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) =>
        String(url).includes('b1-h1')
          ? ({
              ok: true,
              json: async () => ({ body: { HeatResult: result } })
            } as unknown as Response)
          : ({ ok: false, json: async () => ({}) } as unknown as Response)
      )
    );
    render(Results, { session });
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('tournament:b1'));

    const finals = (await screen.findByLabelText(/Pro final standings/i)) as HTMLElement;
    // The non-champion tier orders by points (Surge, Volt, Dash), not seed (which would be Dash first).
    await waitFor(() => {
      const rows = within(finals).getAllByRole('row').slice(1);
      expect(rows.map((r) => r.textContent)).toEqual([
        expect.stringContaining('Nitro'),
        expect.stringContaining('Surge'),
        expect.stringContaining('Volt'),
        expect.stringContaining('Dash')
      ]);
    });
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

describe('Results — round-robin tournament view', () => {
  // A round_robin round seeded from the qualifier; two scored rotation heats (p1 wins both).
  const RR: RoundDef = {
    ...QUAL,
    id: 'rr1',
    label: 'Sprint Series',
    format: 'round_robin',
    params: { rounds: '2', heat_size: '2', points: '10,6,4,3,2,1' },
    seeding: { FromRanking: { source_rounds: ['r1'], top_n: 2 } }
  };
  const RR_HEATS: HeatSummary[] = [
    {
      heat: 'rr-r1-h1',
      lineup: ['p1', 'p2'],
      round: 'rr1',
      class: 'c1',
      phase: 'Final',
      is_current: false
    },
    {
      heat: 'rr-r2-h1',
      lineup: ['p1', 'p2'],
      round: 'rr1',
      class: 'c1',
      phase: 'Final',
      is_current: false
    }
  ];
  const rrRanking = vi.fn(async () => [
    { competitor: 'p1', position: 1 },
    { competitor: 'p2', position: 2 }
  ]);
  // The dedicated standings endpoint that always computes best lap regardless of win condition: p1
  // has one (→ "41.250"), p2 has none (null → "—").
  const rrStandings = vi.fn(
    async (): Promise<RoundStanding[]> => [
      {
        competitor: 'p1',
        position: 1,
        best_lap_micros: 41_250_000,
        laps: 3,
        metric: { MostLaps: { laps: 3 } }
      },
      {
        competitor: 'p2',
        position: 2,
        best_lap_micros: null,
        laps: 3,
        metric: { MostLaps: { laps: 3 } }
      }
    ]
  );
  const rrResult = (rows: [string, number][]) => ({
    body: {
      HeatResult: {
        places: rows.map(([competitor, position]) => ({
          competitor: { adapter: 'rh', competitor },
          position,
          laps: 3,
          metric: { BestLapMicros: 30_000_000 }
        }))
      }
    }
  });

  it('lists the round-robin round in the selector and renders standings (Points) + the rotation grid', async () => {
    const { session } = makeTestSession({
      event: { ...EVENT, rounds: [RR] },
      listClassesImpl: vi.fn(async () => [OPEN]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => RR_HEATS),
      roundRankingImpl: rrRanking,
      roundStandingsImpl: rrStandings,
      classStandingsImpl: vi.fn(async () => STANDINGS)
    });
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => {
        const m = /snapshot\/heat\/([^?]+)/.exec(String(url));
        const heat = m ? decodeURIComponent(m[1]) : '';
        return RR_HEATS.some((h) => h.heat === heat)
          ? ({
              ok: true,
              json: async () =>
                rrResult([
                  ['p1', 1],
                  ['p2', 2]
                ])
            } as unknown as Response)
          : ({ ok: false, json: async () => ({}) } as unknown as Response);
      })
    );
    render(Results, { session });

    // The view defaults to the round-robin tournament (it has run).
    const select = (await screen.findByLabelText('Results view')) as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe('roundrobin:rr1'));

    // The standings table shows a Points column and resolved callsigns (never raw refs).
    const table = await screen.findByRole('table', { name: /Round-robin standings/i });
    expect(within(table).getByText('Points')).toBeInTheDocument();
    await waitFor(() => expect(within(table).getByText('AceOne')).toBeInTheDocument());
    expect(within(table).getByText('Bolt')).toBeInTheDocument();
    expect(within(table).queryByText('p1')).toBeNull();

    // The Best-lap column sources from the standings endpoint (not the win-condition heat metric):
    // AceOne's real best lap shows; Bolt (no lap) shows the em dash.
    await waitFor(() => expect(within(table).getByText('41.250')).toBeInTheDocument());
    expect(within(table).getByText('—')).toBeInTheDocument();

    // The per-rotation grid renders both rotations.
    expect(screen.getByText('Rotation 1')).toBeInTheDocument();
    expect(screen.getByText('Rotation 2')).toBeInTheDocument();
  });
});

describe('Results — event-level projections (kept from #56)', () => {
  it('renders a heat result, ranking, and bracket from typed fixtures', () => {
    render(Results, { heatResult, standings, outcome: eventOutcome });
    expect(screen.getAllByText('ALICE').length).toBeGreaterThan(0);
    expect(screen.getByText('Final')).toBeInTheDocument();
    expect(screen.getByText('Semifinals')).toBeInTheDocument();
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
