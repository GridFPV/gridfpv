/**
 * RaceGowRun — the solo run screen: the one button issues the right command sequence for each
 * phase, the laps and best-3 window render off the served lap list, and the round-wide best comes
 * from the served standings. The e2e (`e2e/racegow.spec.ts`) drives the real loop; this pins what
 * the screen sends and shows.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, HeatSummary, LapList, LiveRaceState, RoundStanding } from '@gridfpv/types';
import RaceGowRun from '../src/screens/RaceGowRun.svelte';
import { makeTestSession } from './support.js';

const EVENT: EventMeta = {
  id: 'track1-ab12',
  name: 'RaceGOW6 Track 1',
  created_at: 0,
  persistent: true,
  preset: 'racegow',
  timers: ['rh-1'],
  primary_timer: 'rh-1',
  roster: ['p1'],
  classes: ['mgp-whoop'],
  classes_membership: [{ class: 'mgp-whoop', pilots: [{ pilot: 'p1', channel: 5880 }] }],
  rounds: [
    {
      id: 'racegow-x1',
      label: 'RaceGOW',
      classes: ['mgp-whoop'],
      format: 'timed_qual',
      params: { rounds: '0' },
      win_condition: { BestConsecutive: { n: 3 } },
      seeding: 'FromRoster',
      channel_mode: 'Static'
    }
  ]
} as unknown as EventMeta;

function lapList(durations: number[]): LapList {
  return {
    competitors: [
      {
        competitor: { adapter: 'rotorhazard', competitor: 'p1' },
        laps: durations.map((d, i) => ({
          number: i + 1,
          duration_micros: d,
          at: (i + 1) * 10_000_000,
          start_ref: i,
          end_ref: i + 1
        }))
      }
    ]
  };
}

function heat(id: string, phase: HeatSummary['phase'], current = false): HeatSummary {
  return {
    heat: id,
    name: `RaceGOW Heat ${id.slice(-1)}`,
    lineup: ['p1'],
    round: 'racegow-x1',
    class: 'mgp-whoop',
    phase,
    is_current: current
  };
}

const idle: LiveRaceState = { phase: 'Scheduled' };

function setup(opts: {
  live?: LiveRaceState;
  heats?: HeatSummary[];
  laps?: Record<string, number[]>;
  standings?: RoundStanding[];
}) {
  const heats = opts.heats ?? [];
  const listHeatsImpl = vi.fn(async () => heats);
  const roundStandingsImpl = vi.fn(async () => opts.standings ?? []);
  const heatFetches = Object.fromEntries(
    Object.entries(opts.laps ?? {}).map(([id, d]) => [id, { laps: lapList(d) }])
  );
  const t = makeTestSession({
    event: EVENT,
    live: opts.live ?? idle,
    listHeatsImpl,
    roundStandingsImpl,
    listPilotsImpl: vi.fn(async () => [{ id: 'p1', callsign: 'Recon', vtx_types: [] }]),
    heatFetches
  });
  return { ...t, listHeatsImpl, roundStandingsImpl };
}

const props = { onhome: vi.fn(), onback: vi.fn(), onconsole: vi.fn(), ontimers: vi.fn() };

describe('RaceGowRun', () => {
  it('idle: "Start run" draws the next heat, focuses it, stages and starts it', async () => {
    // The fill lands a Scheduled heat the screen must then find in the heats list.
    const drawn = heat('racegow-x1-1', 'Scheduled');
    const { session, sendSpy, listHeatsImpl } = setup({ heats: [] });
    listHeatsImpl.mockResolvedValue([drawn]);
    render(RaceGowRun, { session, ...props });

    await fireEvent.click(await screen.findByRole('button', { name: 'Start run' }));

    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(4));
    expect(sendSpy.mock.calls.map(([c]) => c)).toEqual([
      { FillRound: { round: 'racegow-x1', mode: 'Next' } },
      { SetCurrentHeat: { heat: 'racegow-x1-1' } },
      { Stage: { heat: 'racegow-x1-1' } },
      { Start: { heat: 'racegow-x1-1' } }
    ]);
  });

  it('a drawn-but-unstarted heat is started without drawing another', async () => {
    const { session, sendSpy } = setup({
      live: { phase: 'Scheduled', current_heat: 'racegow-x1-1' },
      heats: [heat('racegow-x1-1', 'Scheduled', true)]
    });
    render(RaceGowRun, { session, ...props });
    await fireEvent.click(await screen.findByRole('button', { name: 'Start run' }));
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(2));
    expect(sendSpy.mock.calls.map(([c]) => c)).toEqual([
      { Stage: { heat: 'racegow-x1-1' } },
      { Start: { heat: 'racegow-x1-1' } }
    ]);
  });

  it('running: shows the laps and this run’s best 3 window; "Stop run" ends and finalizes', async () => {
    const { session, sendSpy } = setup({
      live: {
        phase: 'Running',
        current_heat: 'racegow-x1-1',
        active_pilots: ['p1'],
        race_started_at: 1_000_000
      },
      heats: [heat('racegow-x1-1', 'Running', true)],
      laps: { 'racegow-x1-1': [9_000_000, 7_000_000, 6_800_000, 6_800_000, 7_500_000] }
    });
    render(RaceGowRun, { session, ...props });

    // The served laps render, and the best window (laps 2–4 = 20.600) is called out.
    const laps = await screen.findByRole('list', { name: "This run's laps" });
    await waitFor(() => expect(within(laps).getAllByRole('listitem')).toHaveLength(5));
    expect(screen.getByLabelText("This run's best 3 consecutive laps")).toHaveTextContent('20.600');
    const rows = within(laps).getAllByRole('listitem');
    expect(rows[1]).toHaveClass('in-window');
    expect(rows[3]).toHaveClass('in-window');
    expect(rows[0]).not.toHaveClass('in-window');
    expect(rows[4]).not.toHaveClass('in-window');

    await fireEvent.click(screen.getByRole('button', { name: 'Stop run' }));
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(2));
    expect(sendSpy.mock.calls.map(([c]) => c)).toEqual([
      { ForceEnd: { heat: 'racegow-x1-1' } },
      { Finalize: { heat: 'racegow-x1-1' } }
    ]);
  });

  it('stopped but unsaved: "Save run" only finalizes', async () => {
    const { session, sendSpy } = setup({
      live: { phase: 'Unofficial', current_heat: 'racegow-x1-1' },
      heats: [heat('racegow-x1-1', 'Unofficial', true)]
    });
    render(RaceGowRun, { session, ...props });
    await fireEvent.click(await screen.findByRole('button', { name: 'Save run' }));
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(1));
    expect(sendSpy.mock.calls[0][0]).toEqual({ Finalize: { heat: 'racegow-x1-1' } });
  });

  it('after saved runs: the served best headlines, the runs table names the best run, and the submission line is ready', async () => {
    const { session } = setup({
      live: { phase: 'Final', current_heat: 'racegow-x1-2' },
      heats: [heat('racegow-x1-1', 'Final'), heat('racegow-x1-2', 'Final', true)],
      laps: {
        'racegow-x1-1': [8_000_000, 8_000_000, 8_000_000],
        'racegow-x1-2': [7_000_000, 7_000_000, 7_000_000, 9_000_000]
      },
      standings: [
        {
          competitor: 'p1',
          position: 1,
          best_lap_micros: 7_000_000,
          laps: 4,
          metric: { BestConsecutive: { n: 3, micros: 21_000_000 } }
        }
      ]
    });
    render(RaceGowRun, { session, ...props });

    // The next action is another run.
    expect(await screen.findByRole('button', { name: 'Fly again' })).toBeInTheDocument();
    // The headline is the SERVED round-wide best.
    await waitFor(() =>
      expect(screen.getByLabelText('Best 3 consecutive laps, all runs')).toHaveTextContent('21.000')
    );
    // The runs table: two saved runs, run 2 the best.
    const table = await screen.findByRole('table', { name: 'Runs' });
    await waitFor(() => expect(within(table).getAllByRole('row')).toHaveLength(3));
    const [, run1, run2] = within(table).getAllByRole('row');
    expect(run1).toHaveTextContent('Run 1');
    expect(run1).toHaveTextContent('24.000');
    expect(run2).toHaveTextContent('Run 2');
    expect(run2).toHaveTextContent('21.000');
    expect(run2).toHaveClass('best');
    // The submission line names the track, the pilot, the time and the run.
    const submission = screen.getByLabelText('Submission text') as HTMLTextAreaElement;
    expect(submission.value).toContain('RaceGOW6 Track 1 — Recon');
    expect(submission.value).toContain('Best 3 consecutive laps: 21.000 s (7.000 / 7.000 / 7.000)');
    expect(submission.value).toContain('Run 2 of 2, laps 1–3');
  });

  it('offers the overlay URL for OBS', async () => {
    const { session } = setup({});
    render(RaceGowRun, { session, ...props });
    expect(await screen.findByLabelText('Overlay URL')).toHaveTextContent('#/overlay/racegow');
  });
});
