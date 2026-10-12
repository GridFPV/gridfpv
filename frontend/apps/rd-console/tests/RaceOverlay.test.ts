/**
 * RaceOverlay — the OBS browser source: idle with no heat on the timer; the long form (clock,
 * recent laps, this heat's best window, the served round best) for a one-pilot heat of a
 * best-consecutive round; and a row per pilot for a full heat of a race round.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { EventMeta, HeatSummary, LapList, LiveRaceState, Pilot } from '@gridfpv/types';
import RaceOverlay from '../src/screens/RaceOverlay.svelte';
import { makeTestSession } from './support.js';

const EVENT: EventMeta = {
  id: 'club-ab12',
  name: 'Tuesday Club Night',
  created_at: 0,
  persistent: true,
  timers: ['rh-1'],
  roster: ['p1', 'p2', 'p3'],
  classes: ['mgp-whoop'],
  rounds: [
    {
      id: 'tt-1',
      label: 'Time trial',
      classes: ['mgp-whoop'],
      format: 'timed_qual',
      params: { rounds: '0' },
      win_condition: { BestConsecutive: { n: 3 } },
      seeding: 'FromRoster'
    },
    {
      id: 'race-1',
      label: 'Mains',
      classes: ['mgp-whoop'],
      format: 'race',
      params: {},
      win_condition: { FirstToLaps: { n: 4 } },
      seeding: 'FromRoster'
    }
  ]
} as unknown as EventMeta;

const HEATS: HeatSummary[] = [
  {
    heat: 'tt-1-3',
    name: 'Time trial Heat 3',
    lineup: ['p1'],
    round: 'tt-1',
    phase: 'Running',
    is_current: true
  },
  {
    heat: 'race-1-1',
    name: 'Mains Heat 1',
    lineup: ['p1', 'p2', 'p3'],
    round: 'race-1',
    phase: 'Running',
    is_current: false
  }
];

const PILOTS: Pilot[] = [
  { id: 'p1', callsign: 'Recon', vtx_types: [] },
  { id: 'p2', callsign: 'Bee', vtx_types: [] },
  { id: 'p3', callsign: 'Cee', vtx_types: [] }
] as unknown as Pilot[];

function lapsOf(ref: string, seconds: number[]) {
  return {
    competitor: { adapter: 'rotorhazard', competitor: ref },
    laps: seconds.map((s, i) => ({
      number: i + 1,
      duration_micros: Math.round(s * 1_000_000),
      at: (i + 1) * 10_000_000,
      start_ref: i,
      end_ref: i + 1
    }))
  };
}

describe('RaceOverlay', () => {
  it('renders idle with no event, and marks the document as an overlay', () => {
    const { session } = makeTestSession({ noEnter: true });
    render(RaceOverlay, { session });
    expect(screen.getByText('waiting for a race…')).toBeInTheDocument();
    expect(document.documentElement.dataset.overlay).toBe('race');
    expect(screen.queryByRole('timer')).toBeNull();
  });

  it('names the event while it waits for a heat', () => {
    const { session } = makeTestSession({ event: EVENT, live: { phase: 'Scheduled' } });
    render(RaceOverlay, { session });
    expect(screen.getByText('Tuesday Club Night · waiting for a race…')).toBeInTheDocument();
    expect(screen.queryByRole('timer')).toBeNull();
  });

  it('shows a one-pilot best-consecutive heat in long form: name, LIVE, recent laps, both bests', async () => {
    const SOLO: LapList = { competitors: [lapsOf('p1', [9, 7, 6.8, 6.8, 7.5, 7.1, 7.0, 6.9])] };
    const live: LiveRaceState = {
      phase: 'Running',
      current_heat: 'tt-1-3',
      active_pilots: ['p1'],
      race_started_at: 1_000_000
    };
    const { session } = makeTestSession({
      event: EVENT,
      live,
      listPilotsImpl: vi.fn(async () => PILOTS),
      listHeatsImpl: vi.fn(async () => HEATS),
      roundStandingsImpl: vi.fn(async () => [
        {
          competitor: 'p1',
          position: 1,
          best_lap_micros: 6_800_000,
          laps: 8,
          metric: { BestConsecutive: { n: 3, micros: 20_500_000 } }
        }
      ]),
      heatFetches: { 'tt-1-3': { laps: SOLO } }
    });
    render(RaceOverlay, { session });

    // The heat is named by the shared resolver's friendly name, never its id.
    expect(await screen.findByText('Tuesday Club Night · Time trial Heat 3')).toBeInTheDocument();
    expect(screen.getByText('LIVE')).toBeInTheDocument();
    // No fixed end on this round: the clock counts up.
    expect(screen.getByRole('timer', { name: /Race time/ })).toBeInTheDocument();
    expect(await screen.findByText('Recon')).toBeInTheDocument();
    // Only the last six of eight laps fit the frame; they keep their real lap numbers.
    const laps = await screen.findByRole('list', { name: 'Recent laps' });
    await waitFor(() => expect(within(laps).getAllByRole('listitem')).toHaveLength(6));
    expect(within(laps).getByText('L3')).toBeInTheDocument();
    expect(within(laps).getByText('L8')).toBeInTheDocument();
    expect(within(laps).queryByText('L2')).not.toBeInTheDocument();
    // This heat's best window (laps 2–4 = 20.600) and the served round best.
    expect(screen.getByLabelText("This heat's best 3 consecutive laps")).toHaveTextContent(
      '20.600'
    );
    await waitFor(() =>
      expect(screen.getByLabelText('Best 3 consecutive laps, all heats')).toHaveTextContent(
        '20.500'
      )
    );
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('shows a full heat as a row per pilot in running order, with laps, last and best lap', async () => {
    const FIELD: LapList = {
      competitors: [lapsOf('p1', [7, 6.5]), lapsOf('p2', [8]), lapsOf('p3', [7.2, 7.1, 7.0])]
    };
    const live: LiveRaceState = {
      phase: 'Running',
      current_heat: 'race-1-1',
      active_pilots: ['p1', 'p2', 'p3'],
      running_order: ['p3', 'p1', 'p2'],
      race_started_at: 1_000_000
    };
    const { session } = makeTestSession({
      event: EVENT,
      live,
      listPilotsImpl: vi.fn(async () => PILOTS),
      listHeatsImpl: vi.fn(async () => HEATS),
      heatFetches: { 'race-1-1': { laps: FIELD } }
    });
    render(RaceOverlay, { session });

    expect(await screen.findByText('Tuesday Club Night · Mains Heat 1')).toBeInTheDocument();
    const table = await screen.findByRole('table', { name: 'Pilots in this heat' });
    const rows = within(table).getAllByRole('row').slice(1); // minus the header
    await waitFor(() => expect(within(rows[0]).getByText('Cee')).toBeInTheDocument());
    expect(within(rows[1]).getByText('Recon')).toBeInTheDocument();
    expect(within(rows[2]).getByText('Bee')).toBeInTheDocument();
    // Cee: 3 laps, last 7.000, best 7.000. Recon: 2 laps, last 6.500, best 6.500.
    await waitFor(() => expect(rows[0]).toHaveTextContent(/3.*7\.000.*7\.000/));
    expect(rows[1]).toHaveTextContent(/2.*6\.500.*6\.500/);
    expect(rows[2]).toHaveTextContent(/1.*8\.000.*8\.000/);
    // A race round scores no consecutive window: the column is the best lap.
    expect(within(table).getByRole('columnheader', { name: 'Best' })).toBeInTheDocument();
    expect(screen.queryByRole('list', { name: 'Recent laps' })).toBeNull();
  });

  it('takes the light contrast from its theme prop', () => {
    const { session } = makeTestSession({ noEnter: true });
    render(RaceOverlay, { session, theme: 'light' });
    expect(screen.getByLabelText('Race overlay')).toHaveAttribute('data-theme', 'light');
  });
});
