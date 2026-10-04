/**
 * RaceGowOverlay — the OBS browser source: idle with no RaceGOW run active, and the clock, recent
 * laps, this run's best window and the served session best once one is flying.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { EventMeta, LapList, LiveRaceState } from '@gridfpv/types';
import RaceGowOverlay from '../src/screens/RaceGowOverlay.svelte';
import { makeTestSession } from './support.js';

const RUN: EventMeta = {
  id: 'track1-ab12',
  name: 'RaceGOW6 Track 1',
  created_at: 0,
  persistent: true,
  preset: 'racegow',
  timers: ['rh-1'],
  roster: ['p1'],
  classes: ['mgp-whoop'],
  rounds: [
    {
      id: 'racegow-x1',
      label: 'RaceGOW',
      classes: ['mgp-whoop'],
      format: 'timed_qual',
      params: { rounds: '0' },
      win_condition: { BestConsecutive: { n: 3 } },
      seeding: 'FromRoster'
    }
  ]
} as unknown as EventMeta;

const LAPS: LapList = {
  competitors: [
    {
      competitor: { adapter: 'rotorhazard', competitor: 'p1' },
      laps: [9, 7, 6.8, 6.8, 7.5, 7.1, 7.0, 6.9].map((s, i) => ({
        number: i + 1,
        duration_micros: Math.round(s * 1_000_000),
        at: (i + 1) * 10_000_000,
        start_ref: i,
        end_ref: i + 1
      }))
    }
  ]
};

describe('RaceGowOverlay', () => {
  it('renders idle with no run active, and marks the document as an overlay', () => {
    const { session } = makeTestSession({ noEnter: true });
    render(RaceGowOverlay, { session });
    expect(screen.getByText('waiting for a run…')).toBeInTheDocument();
    expect(document.documentElement.dataset.overlay).toBe('racegow');
  });

  it('shows the track, the pilot, LIVE, the recent laps and both bests while flying', async () => {
    const live: LiveRaceState = {
      phase: 'Running',
      current_heat: 'racegow-x1-3',
      active_pilots: ['p1'],
      race_started_at: 1_000_000
    };
    const { session } = makeTestSession({
      event: RUN,
      live,
      listPilotsImpl: vi.fn(async () => [{ id: 'p1', callsign: 'Recon', vtx_types: [] }]),
      roundStandingsImpl: vi.fn(async () => [
        {
          competitor: 'p1',
          position: 1,
          best_lap_micros: 6_800_000,
          laps: 8,
          metric: { BestConsecutive: { n: 3, micros: 20_500_000 } }
        }
      ]),
      heatFetches: { 'racegow-x1-3': { laps: LAPS } }
    });
    render(RaceGowOverlay, { session });

    expect(await screen.findByText('RaceGOW6 Track 1 · Recon')).toBeInTheDocument();
    expect(screen.getByText('LIVE')).toBeInTheDocument();
    expect(screen.getByRole('timer', { name: /Run time/ })).toBeInTheDocument();
    // Only the last six of eight laps fit the frame; they keep their real lap numbers.
    const laps = await screen.findByRole('list', { name: 'Recent laps' });
    await waitFor(() => expect(within(laps).getAllByRole('listitem')).toHaveLength(6));
    expect(within(laps).getByText('L3')).toBeInTheDocument();
    expect(within(laps).getByText('L8')).toBeInTheDocument();
    expect(within(laps).queryByText('L2')).not.toBeInTheDocument();
    // This run's best window (laps 2–4 = 20.600) and the served session best.
    expect(screen.getByLabelText("This run's best 3 consecutive laps")).toHaveTextContent('20.600');
    await waitFor(() =>
      expect(screen.getByLabelText('Best 3 consecutive laps, all runs')).toHaveTextContent('20.500')
    );
  });
});
