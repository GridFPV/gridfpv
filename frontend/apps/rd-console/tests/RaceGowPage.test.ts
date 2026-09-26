/**
 * RaceGowPage — the RaceGOW landing: the form defaults sensibly off the directories, refuses a
 * timer with no channels (pointing at the Timers page), builds the run through the session's
 * ordinary commands, and lists earlier runs to reopen.
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, Pilot, Timer } from '@gridfpv/types';
import RaceGowPage from '../src/screens/RaceGowPage.svelte';
import { makeTestSession } from './support.js';

const RECON: Pilot = { id: 'p1', callsign: 'Recon', vtx_types: [] };
const RH: Timer = {
  id: 'rh-1',
  name: 'Garage RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 1,
  available_channels: [5658, 5880],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
};
const BARE_RH: Timer = { ...RH, id: 'rh-2', name: 'Bare RH', available_channels: [] };
const MOCK: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [5658],
  manual_connect: false,
  calibration: [],
  disabled_nodes: []
};
const OLD_RUN: EventMeta = {
  id: 'track1-old1',
  name: 'RaceGOW6 Track 1',
  created_at: 1000,
  persistent: true,
  preset: 'racegow',
  timers: ['rh-1'],
  roster: ['p1'],
  classes: ['mgp-whoop']
};
const CLUB: EventMeta = {
  id: 'club-1',
  name: 'Club night',
  created_at: 2000,
  persistent: true,
  timers: ['rh-1'],
  roster: [],
  classes: []
};

function setup(opts: { pilots?: Pilot[]; timers?: Timer[]; events?: EventMeta[] } = {}) {
  const { session } = makeTestSession({
    noEnter: true,
    listPilotsImpl: vi.fn(async () => opts.pilots ?? [RECON]),
    listTimersImpl: vi.fn(async () => opts.timers ?? [MOCK, RH]),
    listChannelsImpl: vi.fn(async () => [
      { band: 'Raceband', channel: 'R1', mhz: 5658 },
      { band: 'Raceband', channel: 'R7', mhz: 5880 }
    ])
  });
  vi.spyOn(session, 'listEvents').mockResolvedValue(opts.events ?? [OLD_RUN, CLUB]);
  const props = { onhome: vi.fn(), onrun: vi.fn(), ontimers: vi.fn() };
  return { session, props };
}

describe('RaceGowPage', () => {
  it('defaults to the only pilot and the connected RotorHazard, with its first channel', async () => {
    const { session, props } = setup();
    render(RaceGowPage, { session, ...props });
    const pilot = (await screen.findByLabelText('Pilot')) as HTMLSelectElement;
    await waitFor(() => expect(pilot.value).toBe('p1'));
    const timer = screen.getByLabelText('Timer') as HTMLSelectElement;
    expect(timer.value).toBe('rh-1');
    const channel = screen.getByLabelText('Your channel') as HTMLSelectElement;
    expect(channel.value).toBe('5658');
    // Channel options carry the friendly band + channel, with the MHz beside it.
    expect(within(channel).getByRole('option', { name: /Raceband R7/ })).toBeInTheDocument();
    expect(screen.getByLabelText('Track')).toHaveValue('RaceGOW6 Track 1');
  });

  it('builds the run through the ordinary commands and hands off to the run screen', async () => {
    const { session, props } = setup();
    const created: EventMeta = { ...OLD_RUN, id: 'track1-new1' };
    const create = vi.spyOn(session, 'createEventAndEnter').mockResolvedValue(created);
    const timers = vi.spyOn(session, 'setEventTimers').mockResolvedValue(created);
    const primary = vi.spyOn(session, 'setPrimaryTimer').mockResolvedValue(created);
    const classes = vi.spyOn(session, 'setEventClasses').mockResolvedValue(created);
    const roster = vi.spyOn(session, 'setEventRoster').mockResolvedValue(created);
    const members = vi.spyOn(session, 'setClassMembership').mockResolvedValue(created);
    const round = vi.spyOn(session, 'createRound').mockResolvedValue({ id: 'r1' } as never);
    render(RaceGowPage, { session, ...props });

    const pilot = (await screen.findByLabelText('Pilot')) as HTMLSelectElement;
    await waitFor(() => expect(pilot.value).toBe('p1'));
    await fireEvent.change(screen.getByLabelText('Your channel'), { target: { value: '5880' } });
    await fireEvent.input(screen.getByLabelText('Track'), {
      target: { value: 'RaceGOW6 Track 2' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Set up run' }));

    await waitFor(() => expect(props.onrun).toHaveBeenCalledTimes(1));
    expect(create).toHaveBeenCalledWith('RaceGOW6 Track 2', { preset: 'racegow' });
    expect(timers).toHaveBeenCalledWith(['rh-1']);
    expect(primary).toHaveBeenCalledWith('rh-1');
    expect(classes).toHaveBeenCalledWith(['mgp-whoop']);
    expect(roster).toHaveBeenCalledWith(['p1']);
    expect(members).toHaveBeenCalledWith('mgp-whoop', [{ pilot: 'p1', channel: 5880 }]);
    expect(round).toHaveBeenCalledTimes(1);
  });

  it('adds a new pilot inline when there is none, then builds the run for them', async () => {
    const { session, props } = setup({ pilots: [] });
    const createPilot = vi
      .spyOn(session, 'createPilot')
      .mockResolvedValue({ id: 'p9', callsign: 'Newbie', vtx_types: [] });
    const created: EventMeta = { ...OLD_RUN, id: 'x', roster: ['p9'] };
    vi.spyOn(session, 'createEventAndEnter').mockResolvedValue(created);
    vi.spyOn(session, 'setEventTimers').mockResolvedValue(created);
    vi.spyOn(session, 'setPrimaryTimer').mockResolvedValue(created);
    vi.spyOn(session, 'setEventClasses').mockResolvedValue(created);
    const roster = vi.spyOn(session, 'setEventRoster').mockResolvedValue(created);
    vi.spyOn(session, 'setClassMembership').mockResolvedValue(created);
    vi.spyOn(session, 'createRound').mockResolvedValue({ id: 'r1' } as never);
    render(RaceGowPage, { session, ...props });

    // No pilots → the picker sits on "New pilot" and the callsign field is up.
    const callsign = await screen.findByLabelText('Callsign');
    await fireEvent.input(callsign, { target: { value: 'Newbie' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Set up run' }));

    await waitFor(() => expect(props.onrun).toHaveBeenCalledTimes(1));
    expect(createPilot).toHaveBeenCalledWith({ callsign: 'Newbie', vtx_types: [] });
    expect(roster).toHaveBeenCalledWith(['p9']);
  });

  it('refuses a timer with no channels and points at the Timers page', async () => {
    const { session, props } = setup({ timers: [BARE_RH] });
    const create = vi.spyOn(session, 'createEventAndEnter');
    render(RaceGowPage, { session, ...props });
    await screen.findByText('This timer has no channels set yet.');
    expect(screen.getByRole('button', { name: 'Set up run' })).toBeDisabled();
    expect(screen.queryByLabelText('Your channel')).not.toBeInTheDocument();
    await fireEvent.click(screen.getByRole('button', { name: 'Timers page' }));
    expect(props.ontimers).toHaveBeenCalledTimes(1);
    expect(create).not.toHaveBeenCalled();
  });

  it('lists only RaceGOW runs, and reopens one through chooseEvent', async () => {
    const { session, props } = setup();
    const choose = vi.spyOn(session, 'chooseEvent').mockResolvedValue(OLD_RUN);
    render(RaceGowPage, { session, ...props });
    const runs = await screen.findByRole('list', { name: 'Your runs' });
    expect(within(runs).getAllByRole('listitem')).toHaveLength(1);
    expect(within(runs).getByText('RaceGOW6 Track 1')).toBeInTheDocument();
    expect(within(runs).queryByText('Club night')).not.toBeInTheDocument();
    await fireEvent.click(within(runs).getByRole('button', { name: 'Open RaceGOW6 Track 1' }));
    await waitFor(() => expect(props.onrun).toHaveBeenCalledTimes(1));
    expect(choose).toHaveBeenCalledWith(OLD_RUN);
  });
});
