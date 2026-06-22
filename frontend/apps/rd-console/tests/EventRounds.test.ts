import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type {
  ChannelCatalogEntry,
  Class,
  EventMeta,
  HeatSummary,
  Pilot,
  RoundDef
} from '@gridfpv/types';
import EventRounds from '../src/screens/EventRounds.svelte';
import { makeTestSession } from './support.js';

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const SPEC: Class = { id: 'c2', name: 'Spec', source: 'Custom' };

const ACE: Pilot = { id: 'p1', callsign: 'AceOne', vtx_types: [], attributes: {} };
const BOLT: Pilot = { id: 'p2', callsign: 'Bolt', vtx_types: [], attributes: {} };

const QUAL: RoundDef = {
  id: 'r1',
  label: 'Qualifying R1',
  classes: ['c1'],
  format: 'timed_qual',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster',
  channel_mode: 'Static'
};

/** An event selecting both classes, with one existing round. */
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: ['c1', 'c2'],
  rounds: [QUAL]
};

const FORMATS = ['double_elim', 'multi_main', 'round_robin', 'single_elim', 'timed_qual', 'zippyq'];

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

function baseImpls() {
  return {
    listClassesImpl: vi.fn(async () => [OPEN, SPEC]),
    listFormatsImpl: vi.fn(async () => FORMATS)
  };
}

describe('EventRounds (define rounds — classes, format, seeding)', () => {
  it('lists the event rounds with resolved class names, format, win condition, and seeding', async () => {
    const { session } = makeTestSession({ ...baseImpls(), event: EVENT });
    render(EventRounds, { session });

    // The round label now appears both in the Rounds list and the Heats section, so scope the
    // assertions to the Rounds card (the section whose heading is "Rounds").
    const roundsCard = screen.getByRole('heading', { name: 'Rounds' }).closest('section')!;
    await within(roundsCard).findByText('Qualifying R1');
    // Format and the resolved class name show; FromRoster summarises as "From roster".
    expect(within(roundsCard).getByText('timed_qual')).toBeInTheDocument();
    expect(within(roundsCard).getByText('Open')).toBeInTheDocument();
    expect(within(roundsCard).getByText('From roster')).toBeInTheDocument();
    expect(within(roundsCard).getByText(/Timed · 120s/)).toBeInTheDocument();
    // The round index renders.
    expect(within(roundsCard).getByText('1')).toBeInTheDocument();
  });

  it('adds a round via createRound and reflects it immediately', async () => {
    const impls = baseImpls();
    const created: RoundDef = {
      id: 'r2',
      label: 'Open Practice',
      classes: ['c1', 'c2'],
      format: 'zippyq',
      params: {},
      win_condition: 'BestLap',
      seeding: 'FromRoster',
      channel_mode: 'PerHeat'
    };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => created);
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));

    await fireEvent.input(await screen.findByLabelText('Label'), {
      target: { value: 'Open Practice' }
    });
    // Tick both classes (open / practice).
    await fireEvent.click(screen.getByLabelText('Eligible Open'));
    await fireEvent.click(screen.getByLabelText('Eligible Spec'));
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'zippyq' } });
    await fireEvent.change(screen.getByLabelText('Win condition'), {
      target: { value: 'BestLap' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, eventId, req] = createRoundImpl.mock.calls[0];
    expect(eventId).toBe('e1');
    expect(req).toMatchObject({
      label: 'Open Practice',
      classes: ['c1', 'c2'],
      format: 'zippyq',
      win_condition: 'BestLap',
      seeding: 'FromRoster'
    });
    // The new round appears in the Rounds list (its label also seeds the Heats section).
    const roundsCard = screen.getByRole('heading', { name: 'Rounds' }).closest('section')!;
    await within(roundsCard).findByText('Open Practice');
  });

  it('reveals the FromRanking selector (source round + top N) and authors it', async () => {
    const impls = baseImpls();
    const updated: RoundDef = {
      ...QUAL,
      id: 'r2',
      label: 'Mains',
      format: 'single_elim',
      seeding: { FromRanking: { source_round: 'r1', top_n: 8 } }
    };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => updated);
    const { session } = makeTestSession({ ...impls, createRoundImpl, event: EVENT });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Mains' } });
    await fireEvent.click(screen.getByLabelText('Eligible Open'));
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'single_elim' } });

    // No source-round dropdown until From ranking is chosen.
    expect(screen.queryByLabelText('Source round')).not.toBeInTheDocument();
    await fireEvent.change(screen.getByLabelText('Seeding'), {
      target: { value: 'FromRanking' }
    });
    // The source-round selector reveals, listing the existing round.
    const source = (await screen.findByLabelText('Source round')) as HTMLSelectElement;
    await fireEvent.change(source, { target: { value: 'r1' } });
    await fireEvent.input(screen.getByLabelText('Top N'), { target: { value: '4' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req.seeding).toEqual({ FromRanking: { source_round: 'r1', top_n: 4 } });
  });

  it('edits an existing round via updateRound, seeded from its current fields', async () => {
    const impls = baseImpls();
    const updateRoundImpl = vi.fn(async (_b, _e, _id, req) => ({ ...QUAL, ...req }));
    const { session } = makeTestSession({ ...impls, updateRoundImpl, event: EVENT });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    const label = (await screen.findByLabelText('Label')) as HTMLInputElement;
    expect(label.value).toBe('Qualifying R1');
    await fireEvent.input(label, { target: { value: 'Qualifying R2' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save round' }));

    await waitFor(() => expect(updateRoundImpl).toHaveBeenCalledTimes(1));
    const [, , roundId, req] = updateRoundImpl.mock.calls[0];
    expect(roundId).toBe('r1');
    expect(req.label).toBe('Qualifying R2');
  });

  it('removes a round via deleteRound', async () => {
    const impls = baseImpls();
    const deleteRoundImpl = vi.fn(async (_b, _e, _id) => ({ ...EVENT, rounds: [] }));
    const { session } = makeTestSession({ ...impls, deleteRoundImpl, event: EVENT });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Remove' }));

    await waitFor(() => expect(deleteRoundImpl).toHaveBeenCalledTimes(1));
    expect(deleteRoundImpl.mock.calls[0][2]).toBe('r1');
    await waitFor(() => expect(session.currentEvent?.rounds).toEqual([]));
  });
});

describe('EventRounds (Heats — fill round, heats list, manual build)', () => {
  // An event whose Open class has two members, so a round can draw a field / a manual heat.
  const EVENT_WITH_MEMBERS: EventMeta = {
    ...EVENT,
    roster: ['p1', 'p2'],
    classes_membership: [{ class: 'c1', pilots: [{ pilot: 'p1' }, { pilot: 'p2' }] }]
  };

  function heatsImpls(heats: HeatSummary[] = []) {
    return {
      ...baseImpls(),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => heats)
    };
  }

  it("lists a round's heats with lineup callsigns, status, and the current marker", async () => {
    const heat: HeatSummary = {
      heat: 'q-1',
      lineup: ['p1', 'p2'],
      class: 'c1',
      round: 'r1',
      phase: 'Running',
      is_current: true
    };
    const { session } = makeTestSession({ ...heatsImpls([heat]), event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    // The heat appears under its round with resolved callsigns, a Running status, and Current.
    const heatRow = (await screen.findByText('q-1')).closest('.heat-row') as HTMLElement;
    expect(within(heatRow).getByText('AceOne')).toBeInTheDocument();
    expect(within(heatRow).getByText('Bolt')).toBeInTheDocument();
    expect(within(heatRow).getByText('Running')).toBeInTheDocument();
    expect(within(heatRow).getByText('Current')).toBeInTheDocument();
  });

  it("renders each pilot's assigned channel as a band+channel label, custom MHz, or — (Slice 4b)", async () => {
    const heat: HeatSummary = {
      heat: 'q-1',
      lineup: ['p1', 'p2'],
      class: 'c1',
      round: 'r1',
      // p1 → a catalog channel (Raceband R1); p2 → a custom raw MHz with no catalog entry.
      frequencies: [
        ['p1', 5658],
        ['p2', 5685]
      ],
      phase: 'Scheduled',
      is_current: false
    };
    const { session } = makeTestSession({
      ...heatsImpls([heat]),
      listChannelsImpl: vi.fn(async () => CATALOG),
      event: EVENT_WITH_MEMBERS
    });
    render(EventRounds, { session });

    const heatRow = (await screen.findByText('q-1')).closest('.heat-row') as HTMLElement;
    // The catalog frequency resolves to its band+channel; the custom one falls back to raw MHz.
    await waitFor(() => expect(within(heatRow).getByText('Raceband R1')).toBeInTheDocument());
    expect(within(heatRow).getByText('5685 MHz')).toBeInTheDocument();
  });

  it('shows — for a sim/free-text heat that carries no frequencies (Slice 4b)', async () => {
    const heat: HeatSummary = {
      heat: 'q-2',
      lineup: ['p1', 'p2'],
      class: 'c1',
      round: 'r1',
      phase: 'Scheduled',
      is_current: false
    };
    const { session } = makeTestSession({
      ...heatsImpls([heat]),
      listChannelsImpl: vi.fn(async () => CATALOG),
      event: EVENT_WITH_MEMBERS
    });
    render(EventRounds, { session });

    const heatRow = (await screen.findByText('q-2')).closest('.heat-row') as HTMLElement;
    // Both pilots show the dash (no channel assigned).
    const dashes = within(heatRow).getAllByText('—');
    expect(dashes.length).toBe(2);
  });

  it('fills a round via FillRound and re-reads the heats list', async () => {
    const impls = heatsImpls([]);
    const newHeat: HeatSummary = {
      heat: 'q-1',
      lineup: ['p1', 'p2'],
      class: 'c1',
      round: 'r1',
      phase: 'Scheduled',
      is_current: true
    };
    // First read (on mount) is empty; after the fill, the engine has appended a heat.
    impls.listHeatsImpl.mockResolvedValueOnce([]).mockResolvedValue([newHeat]);

    const { session, sendSpy } = makeTestSession({ ...impls, event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Fill next heat' }));

    // A FillRound command tagged with the round was sent.
    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    expect(sendSpy.mock.calls[0][0]).toEqual({ FillRound: { round: 'r1' } });
    // The newly-scheduled heat shows up after the re-read.
    await screen.findByText('q-1');
  });

  it('builds a heat by hand from the round’s eligible members (tagged, no free text)', async () => {
    const impls = heatsImpls([]);
    const { session, sendSpy } = makeTestSession({ ...impls, event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));

    // The round defaults to the first; type a heat id and pick both eligible members.
    await fireEvent.input(await screen.findByLabelText('Build heat id'), {
      target: { value: 'q-1' }
    });
    await fireEvent.click(screen.getByLabelText('Select AceOne'));
    await fireEvent.click(screen.getByLabelText('Select Bolt'));
    await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));

    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    // A ScheduleHeat tagged with the round + its single class, lineup of the chosen pilot refs.
    expect(sendSpy.mock.calls[0][0]).toEqual({
      ScheduleHeat: { heat: 'q-1', lineup: ['p1', 'p2'], class: 'c1', round: 'r1' }
    });
  });
});

describe('EventRounds (per-round standings + advance-to-bracket — Slice 5/6b)', () => {
  // An event whose Open class has two members so a round has a field, with one Qualifying round.
  const EVENT_WITH_MEMBERS: EventMeta = {
    ...EVENT,
    roster: ['p1', 'p2'],
    classes_membership: [{ class: 'c1', pilots: [{ pilot: 'p1' }, { pilot: 'p2' }] }]
  };

  function baseHeatsImpls() {
    return {
      ...baseImpls(),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT]),
      listHeatsImpl: vi.fn(async () => [] as HeatSummary[])
    };
  }

  it("toggles a round's standings, resolving competitors to callsigns in ranking order", async () => {
    const roundRankingImpl = vi.fn(async (_b, _e, _round) => [
      { competitor: 'p1', position: 1 },
      { competitor: 'p2', position: 2 }
    ]);
    const { session } = makeTestSession({
      ...baseHeatsImpls(),
      roundRankingImpl,
      event: EVENT_WITH_MEMBERS
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Standings' }));
    await waitFor(() => expect(roundRankingImpl).toHaveBeenCalled());
    // The round id was read; the ranking renders as ordered callsigns.
    expect(roundRankingImpl.mock.calls[0][2]).toBe('r1');
    const panel = (await screen.findByLabelText(/Standings for Qualifying R1/i)) as HTMLElement;
    expect(within(panel).getByText('AceOne')).toBeInTheDocument();
    expect(within(panel).getByText('Bolt')).toBeInTheDocument();
  });

  it('surfaces an inline note when a round has no ranking yet (unscored 400s)', async () => {
    const roundRankingImpl = vi.fn(async () => {
      throw new Error('GET …/ranking failed: HTTP 400');
    });
    const { session } = makeTestSession({
      ...baseHeatsImpls(),
      roundRankingImpl,
      event: EVENT_WITH_MEMBERS
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Standings' }));
    await waitFor(() => expect(screen.getByText(/No ranking yet/i)).toBeInTheDocument());
  });

  it('advances a round to a single_elim bracket seeded FromRanking with a power-of-two top_n default', async () => {
    // A 3-member field defaults top_n to 2 (largest power-of-two ≤ 3).
    const EVENT_3: EventMeta = {
      ...EVENT_WITH_MEMBERS,
      roster: ['p1', 'p2', 'p3'],
      classes_membership: [
        { class: 'c1', pilots: [{ pilot: 'p1' }, { pilot: 'p2' }, { pilot: 'p3' }] }
      ]
    };
    const created: RoundDef = {
      id: 'r2',
      label: 'Qualifying R1 — Bracket',
      classes: ['c1'],
      format: 'single_elim',
      params: {},
      win_condition: QUAL.win_condition,
      seeding: { FromRanking: { source_round: 'r1', top_n: 2 } },
      channel_mode: 'PerHeat'
    };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => created);
    const { session, sendSpy } = makeTestSession({
      ...baseHeatsImpls(),
      createRoundImpl,
      event: EVENT_3
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Advance to bracket' }));

    // The top_n field defaults to the power-of-two ≤ 3 → 2.
    const topN = (await screen.findByLabelText('Top N advance')) as HTMLInputElement;
    expect(topN.value).toBe('2');
    // The label defaults to "<round> — Bracket".
    const label = screen.getByLabelText('Bracket label') as HTMLInputElement;
    expect(label.value).toBe('Qualifying R1 — Bracket');

    await fireEvent.click(screen.getByRole('button', { name: 'Create & fill bracket' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req).toMatchObject({
      classes: ['c1'],
      format: 'single_elim',
      seeding: { FromRanking: { source_round: 'r1', top_n: 2 } }
    });
    // After creating, the bracket's first heat is filled (a FillRound on the new round).
    await waitFor(() => expect(sendSpy.mock.calls.some((c) => 'FillRound' in c[0])).toBe(true));
    expect(sendSpy.mock.calls.find((c) => 'FillRound' in c[0])![0]).toEqual({
      FillRound: { round: 'r2' }
    });
  });
});
