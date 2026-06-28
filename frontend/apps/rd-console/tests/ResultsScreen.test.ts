import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type {
  Class,
  ClassStandings,
  EventMeta,
  HeatSummary,
  Pilot,
  RankEntry,
  RoundDef
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

// A non-bracket qualifying round, and a single-level single-elim tournament seeded from it. The
// tournament root's label carries the "‹Bracket› — ‹Level›" convention so it reads by its name.
const QUAL: RoundDef = {
  id: 'r1',
  label: 'Qualifying',
  classes: ['c1'],
  format: 'timed_qual',
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
