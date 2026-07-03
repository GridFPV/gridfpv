import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type {
  ChannelCatalogEntry,
  Class,
  EventMeta,
  HeatSummary,
  Pilot,
  RoundDef,
  Timer
} from '@gridfpv/types';
import EventRounds from '../src/screens/EventRounds.svelte';
import { makeTestSession } from './support.js';

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const SPEC: Class = { id: 'c2', name: 'Spec', source: 'Custom' };

const ACE: Pilot = { id: 'p1', callsign: 'AceOne', vtx_types: [] };
const BOLT: Pilot = { id: 'p2', callsign: 'Bolt', vtx_types: [] };

const QUAL: RoundDef = {
  id: 'r1',
  label: 'Qualifying R1',
  classes: ['c1'],
  format: 'timed_qual',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster',
  channel_mode: 'Static',
  staging_timer_secs: 300,
  start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
  grace_window: { Duration: { micros: 3000000 } },
  protest_window: 'Off'
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

// The offered format set (`GET /formats`) — ZippyQ is shelved (#218) so it is **not** offered.
const FORMATS = [
  'double_elim',
  'head_to_head',
  'multi_main',
  'round_robin',
  'single_elim',
  'timed_qual'
];

// The format param schemas the screen reads (`GET /formats`). `timed_qual` declares its `rounds`
// param labelled **"Heats per pilot"** (the qualifying-metric param is gone — the metric is derived
// from the win condition) plus a synthetic enum `tiebreak` to exercise enum-field rendering;
// `head_to_head` declares group size + a scoring enum (the points table is authored by a dedicated
// editor, not a generic param); the rest declare none, so they show no Format options section. Params
// surface inline as proper labeled fields (Rounds form redesign item 4).
const SCHEMAS = FORMATS.map((name) => {
  if (name === 'timed_qual') {
    return {
      name,
      params: [
        { key: 'rounds', label: 'Heats per pilot', kind: 'number' as const, default: '3' },
        {
          key: 'tiebreak',
          label: 'Tiebreak',
          kind: 'enum' as const,
          options: ['best_lap', 'count_back'],
          default: 'best_lap'
        }
      ]
    };
  }
  if (name === 'head_to_head') {
    return {
      name,
      params: [
        { key: 'group_size', label: 'Group size', kind: 'number' as const, default: '2' },
        { key: 'rotations', label: 'Heats per group', kind: 'number' as const, default: '1' },
        {
          key: 'scoring',
          label: 'Scoring',
          kind: 'enum' as const,
          options: ['placement', 'points'],
          default: 'placement'
        }
      ]
    };
  }
  return { name, params: [] };
});

const CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

function baseImpls() {
  return {
    listClassesImpl: vi.fn(async () => [OPEN, SPEC]),
    listFormatsImpl: vi.fn(async () => FORMATS),
    listFormatSchemasImpl: vi.fn(async () => SCHEMAS)
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
    // The friendly format name shows (not the raw `timed_qual` key); the resolved class name shows;
    // FromRoster summarises as "From roster". `timed_qual`'s friendly label is "Time Trials" (#218).
    expect(within(roundsCard).getByText('Time Trials')).toBeInTheDocument();
    expect(within(roundsCard).queryByText('timed_qual')).toBeNull();
    expect(within(roundsCard).getByText('Open')).toBeInTheDocument();
    expect(within(roundsCard).getByText('From roster')).toBeInTheDocument();
    expect(within(roundsCard).getByText(/Timed — Most Laps · 120s/)).toBeInTheDocument();
    // The round index renders.
    expect(within(roundsCard).getByText('1')).toBeInTheDocument();
  });

  it('adds a round via createRound and reflects it immediately', async () => {
    const impls = baseImpls();
    const created: RoundDef = {
      id: 'r2',
      label: 'H2H Heats',
      classes: ['c1', 'c2'],
      format: 'head_to_head',
      params: {},
      win_condition: { FirstToLaps: { n: 3 } },
      seeding: 'FromRoster',
      channel_mode: 'PerHeat',
      staging_timer_secs: 300,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
      grace_window: { Duration: { micros: 3000000 } },
      protest_window: 'Off'
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
      target: { value: 'H2H Heats' }
    });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'head_to_head' } });
    // The eligible class is a single-select dropdown (Rounds form redesign item 6).
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    // Head-to-Head offers only Timed / First-to-N; pick First-to-N (3 laps, the default).
    await fireEvent.change(screen.getByLabelText('Win condition'), {
      target: { value: 'FirstToLaps' }
    });
    await fireEvent.input(screen.getByLabelText('Laps'), { target: { value: '3' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, eventId, req] = createRoundImpl.mock.calls[0];
    expect(eventId).toBe('e1');
    // The single-select class stores a one-element `classes` list.
    expect(req).toMatchObject({
      label: 'H2H Heats',
      classes: ['c1'],
      format: 'head_to_head',
      win_condition: { FirstToLaps: { n: 3 } },
      seeding: 'FromRoster'
    });
    // First-to-N self-terminates (the lap target ends the heat), so no round time limit.
    expect(req.time_limit_secs).toBeUndefined();
    // The new round appears in the Rounds list (its label also seeds the Heats section).
    const roundsCard = screen.getByRole('heading', { name: 'Rounds' }).closest('section')!;
    await within(roundsCard).findByText('H2H Heats');
  });

  it('reveals the FromRanking source-rounds multi-select (+ top N) and authors it', async () => {
    const impls = baseImpls();
    const updated: RoundDef = {
      ...QUAL,
      id: 'r2',
      label: 'Mains',
      format: 'single_elim',
      seeding: { FromRanking: { source_rounds: ['r1'], top_n: 8 } }
    };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => updated);
    const { session } = makeTestSession({ ...impls, createRoundImpl, event: EVENT });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Mains' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'single_elim' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });

    // No source-rounds picker until From ranking is chosen.
    expect(screen.queryByLabelText('Seed from Qualifying R1')).not.toBeInTheDocument();
    await fireEvent.change(screen.getByLabelText('Seeding'), {
      target: { value: 'FromRanking' }
    });
    // The source-rounds multi-select reveals as a checkbox per prior round (issue #51).
    const source = (await screen.findByLabelText('Seed from Qualifying R1')) as HTMLInputElement;
    await fireEvent.click(source);
    await fireEvent.input(screen.getByLabelText('Top N'), { target: { value: '4' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req.seeding).toEqual({ FromRanking: { source_rounds: ['r1'], top_n: 4 } });
  });

  it('authors a FromRanking seed from MULTIPLE source rounds (issue #51)', async () => {
    // Two prior rounds exist; selecting both stores them as `source_rounds` in event-definition
    // order (the server then aggregates best-per-pilot across them).
    const impls = baseImpls();
    const second: RoundDef = { ...QUAL, id: 'r1b', label: 'Qualifying 2' };
    const twoRoundEvent: EventMeta = { ...EVENT, rounds: [QUAL, second] };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({
      ...QUAL,
      id: 'r3',
      label: 'Mains',
      format: 'single_elim'
    }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: twoRoundEvent
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Mains' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'single_elim' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    await fireEvent.change(screen.getByLabelText('Seeding'), {
      target: { value: 'FromRanking' }
    });

    // Tick both source rounds.
    await fireEvent.click(await screen.findByLabelText('Seed from Qualifying R1'));
    await fireEvent.click(await screen.findByLabelText('Seed from Qualifying 2'));
    await fireEvent.input(screen.getByLabelText('Top N'), { target: { value: '8' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req.seeding).toEqual({ FromRanking: { source_rounds: ['r1', 'r1b'], top_n: 8 } });
  });

  it("caps the FromRanking cut at the source round's field, not an arbitrary number", async () => {
    // The source round raced two heats with 3 distinct pilots — "top 10" of a 3-pilot ranking is
    // meaningless, so the input advertises the bound and the authored seed clamps to it.
    const impls = baseImpls();
    const raced: HeatSummary[] = [
      {
        heat: 'q-1',
        lineup: ['p1', 'p2'],
        class: 'c1',
        round: 'r1',
        phase: 'Final',
        is_current: false
      },
      { heat: 'q-2', lineup: ['p3'], class: 'c1', round: 'r1', phase: 'Final', is_current: false }
    ];
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({
      ...QUAL,
      id: 'r2',
      label: 'Mains',
      format: 'single_elim'
    }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      listHeatsImpl: vi.fn(async () => raced),
      event: EVENT
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Mains' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'single_elim' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    await fireEvent.change(screen.getByLabelText('Seeding'), { target: { value: 'FromRanking' } });
    await fireEvent.click(await screen.findByLabelText('Seed from Qualifying R1'));

    const topN = (await screen.findByLabelText('Top N')) as HTMLInputElement;
    await waitFor(() => expect(topN.max).toBe('3'));
    await fireEvent.input(topN, { target: { value: '10' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req2] = createRoundImpl.mock.calls[0];
    expect(req2.seeding).toEqual({ FromRanking: { source_rounds: ['r1'], top_n: 3 } });
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

  it('authors the heat-lifecycle config (staging mm:ss, start min/max, grace) on create', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Heat 1' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });

    // Set a 2:30 staging window, a 1.0–4.0s start hold (entered in seconds, stored as ms — item 3),
    // and a 4s grace.
    await fireEvent.input(screen.getByLabelText('Staging minutes'), { target: { value: '2' } });
    await fireEvent.input(screen.getByLabelText('Staging seconds'), { target: { value: '30' } });
    await fireEvent.input(screen.getByLabelText('Start min delay seconds'), {
      target: { value: '1' }
    });
    await fireEvent.input(screen.getByLabelText('Start max delay seconds'), {
      target: { value: '4' }
    });
    await fireEvent.input(screen.getByLabelText('Grace window seconds'), {
      target: { value: '4' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req.staging_timer_secs).toBe(150); // 2:30
    // The seconds inputs are converted back to the stored ms on submit.
    expect(req.start_procedure).toEqual({
      mode: 'randomized-delay',
      min_delay_ms: 1000,
      max_delay_ms: 4000
    });
    expect(req.grace_window).toEqual({ Duration: { micros: 4_000_000 } });
  });

  it('round-trips the heat-lifecycle config through edit (reads RoundDef fields back into the form)', async () => {
    const impls = baseImpls();
    // A round with non-default lifecycle config: 1:15 staging, 2500–6000ms start, 5s grace.
    const tuned: RoundDef = {
      ...QUAL,
      staging_timer_secs: 75,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2500, max_delay_ms: 6000 },
      grace_window: { Duration: { micros: 5_000_000 } },
      protest_window: 'Off'
    };
    const updateRoundImpl = vi.fn(async (_b, _e, _id, req) => ({ ...tuned, ...req }));
    const { session } = makeTestSession({
      ...impls,
      updateRoundImpl,
      event: { ...EVENT, rounds: [tuned] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    // The form seeds from the round's current lifecycle config; the ms delays read back as seconds.
    expect((screen.getByLabelText('Staging minutes') as HTMLInputElement).value).toBe('1');
    expect((screen.getByLabelText('Staging seconds') as HTMLInputElement).value).toBe('15');
    expect((screen.getByLabelText('Start min delay seconds') as HTMLInputElement).value).toBe(
      '2.5'
    );
    expect((screen.getByLabelText('Start max delay seconds') as HTMLInputElement).value).toBe('6');
    expect((screen.getByLabelText('Grace window seconds') as HTMLInputElement).value).toBe('5');

    // Edit the staging seconds and save — the same values round-trip back out on the request (the
    // seconds inputs convert back to the stored ms).
    await fireEvent.input(screen.getByLabelText('Staging seconds'), { target: { value: '45' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save round' }));

    await waitFor(() => expect(updateRoundImpl).toHaveBeenCalledTimes(1));
    const [, , , req] = updateRoundImpl.mock.calls[0];
    expect(req.staging_timer_secs).toBe(105); // 1:45
    expect(req.start_procedure).toEqual({
      mode: 'randomized-delay',
      min_delay_ms: 2500,
      max_delay_ms: 6000
    });
    expect(req.grace_window).toEqual({ Duration: { micros: 5_000_000 } });
  });

  it('surfaces the chosen format’s params inline as labeled fields, seeded from their defaults', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Q' } });
    // timed_qual declares the `rounds` (number) + `tiebreak` (enum) params; zippyq declares none.
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });

    // The format's params show inline as proper labeled fields, seeded from their schema defaults —
    // no "+ Add param" control any more (Rounds form redesign item 4).
    expect(screen.queryByLabelText('Add param')).not.toBeInTheDocument();
    const roundsInput = (await screen.findByLabelText('Heats per pilot value')) as HTMLInputElement;
    expect(roundsInput.type).toBe('number');
    expect(roundsInput.value).toBe('3'); // schema default
    await fireEvent.input(roundsInput, { target: { value: '5' } });

    const tb = (await screen.findByLabelText('Tiebreak value')) as HTMLSelectElement;
    expect(tb.value).toBe('best_lap'); // schema default
    await fireEvent.change(tb, { target: { value: 'count_back' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req.params).toEqual({ rounds: '5', tiebreak: 'count_back' });
  });

  it('re-seeds the Format options when switching formats — dropping stale prior-format params', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Q' } });
    // timed_qual shows its "Heats per pilot" param…
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    expect(await screen.findByLabelText('Heats per pilot value')).toBeInTheDocument();
    // …switching to Head-to-Head re-seeds the params: the timed_qual param is gone, replaced by the
    // head_to_head params (Group size), and the stale timed_qual params don't reach the request.
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'head_to_head' } });
    await waitFor(() =>
      expect(screen.queryByLabelText('Heats per pilot value')).not.toBeInTheDocument()
    );
    expect(await screen.findByLabelText('Group size value')).toBeInTheDocument();

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    // Only head_to_head's params (its schema defaults), not timed_qual's rounds/tiebreak.
    expect(createRoundImpl.mock.calls[0][2].params).toEqual({
      group_size: '2',
      rotations: '1',
      scoring: 'placement'
    });
  });

  it('authors a multi-rotation Head-to-Head: the rotations param reaches the request', async () => {
    // "Heats per group" (rotations) — the same groups race N times, points accumulating (ProSpec
    // style). The knob is a plain schema param, so it serializes into `params` like the rest.
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'ProSpec' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'head_to_head' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    await fireEvent.input(await screen.findByLabelText('Heats per group value'), {
      target: { value: '3' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    expect(createRoundImpl.mock.calls[0][2].params).toEqual({
      group_size: '2',
      rotations: '3',
      scoring: 'placement'
    });
  });

  it('defaults the channel-mode toggle to Per-heat on a new round and carries the choice', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    const mode = (await screen.findByLabelText('Channel mode')) as HTMLSelectElement;
    expect(mode.value).toBe('PerHeat');
    await fireEvent.input(screen.getByLabelText('Label'), { target: { value: 'Q' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    await fireEvent.change(mode, { target: { value: 'Static' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    expect(createRoundImpl.mock.calls[0][2].channel_mode).toBe('Static');
  });

  it('for a qualifying format, the win condition IS the metric — no separate metric field, no First-to-N option', async () => {
    // The qualifying metric is derived from the win condition (Rounds form redesign), so a
    // qualifying format (timed_qual / round_robin) shows NO separate "qualifying metric" field and
    // the win-condition dropdown offers only the qualifying-applicable conditions.
    const { session } = makeTestSession({ ...baseImpls(), event: { ...EVENT, rounds: [] } });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Q' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });

    // No separate metric field is rendered (it was the old `metric` enum param / a "metric" control).
    expect(screen.queryByLabelText(/qualifying metric/i)).toBeNull();
    expect(screen.queryByLabelText(/ranking metric/i)).toBeNull();

    // The win-condition dropdown offers only the qualifying conditions — Timed and the converged
    // Best-of-N (best of N laps; N=1 = best lap). First-to-N is hidden.
    const win = (await screen.findByLabelText('Win condition')) as HTMLSelectElement;
    const options = Array.from(win.options).map((o) => o.value);
    expect(options).toEqual(['Timed', 'BestOfN']);
    expect(options).not.toContain('FirstToLaps');
  });

  it('shows the First-to-N win condition for a non-qualifying (head-to-head) format', async () => {
    // A racing (non-qualifying) format keeps the full win-condition catalogue, including First-to-N.
    const { session } = makeTestSession({ ...baseImpls(), event: { ...EVENT, rounds: [] } });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Mains' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'head_to_head' } });

    const win = (await screen.findByLabelText('Win condition')) as HTMLSelectElement;
    expect(Array.from(win.options).map((o) => o.value)).toContain('FirstToLaps');
  });

  it('Add-round offers only the three round types — not tournament structures (D17 taxonomy)', async () => {
    const { session } = makeTestSession({ ...baseImpls(), event: { ...EVENT, rounds: [] } });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    const fmt = (await screen.findByLabelText('Format')) as HTMLSelectElement;
    const opts = Array.from(fmt.options).map((o) => o.value);
    // Only the directly-addable round types, in taxonomy order (Open Practice → Time Trials →
    // Head-to-Head); the base mock has no open_practice, so it's timed_qual then head_to_head.
    expect(opts).toEqual(['timed_qual', 'head_to_head']);
    // …tournament structures are composed via the builder, never added directly.
    expect(opts).not.toContain('round_robin');
    expect(opts).not.toContain('single_elim');
    expect(opts).not.toContain('double_elim');
    expect(opts).not.toContain('multi_main');
  });

  it('head-to-head Points scoring shows the per-position points editor and submits the table', async () => {
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...baseImpls(),
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'H2H' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'head_to_head' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    // Group size is a dropdown (capped at the timer's nodes); pick 4 pilots per heat.
    await fireEvent.change(screen.getByLabelText('Group size value'), { target: { value: '4' } });

    // Placement scoring (the default) shows no points editor…
    expect(screen.queryByLabelText('Points for 1st place')).toBeNull();
    // …switching to Points reveals one row per finishing position (= group size), seeded with the
    // steep MultiGP-style default; there is no 5th-place row for a group of 4.
    await fireEvent.change(screen.getByLabelText('Scoring value'), { target: { value: 'points' } });
    const first = (await screen.findByLabelText('Points for 1st place')) as HTMLInputElement;
    expect(first.value).toBe('10');
    expect(screen.getByLabelText('Points for 4th place')).toBeInTheDocument();
    expect(screen.queryByLabelText('Points for 5th place')).toBeNull();
    // Edit the win value to a steeper 25.
    await fireEvent.input(first, { target: { value: '25' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const req = createRoundImpl.mock.calls[0][2];
    expect(req.format).toBe('head_to_head');
    expect(req.params.group_size).toBe('4');
    expect(req.params.scoring).toBe('points');
    expect(req.params.points).toBe('25, 6, 4, 3');
  });

  it('shows the rounds param as "Heats per pilot" for a qualifying format', async () => {
    // The `rounds` param keeps its key/default/behaviour but is relabeled "Heats per pilot" so it no
    // longer clashes with the Round entity (Rounds form redesign).
    const { session } = makeTestSession({ ...baseImpls(), event: { ...EVENT, rounds: [] } });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Q' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });

    // The field renders as "Heats per pilot" (its value input is labelled "<label> value"), not "Rounds".
    expect(await screen.findByLabelText('Heats per pilot value')).toBeInTheDocument();
    expect(screen.queryByLabelText('Rounds value')).toBeNull();
  });

  it('defaults the grace window to 30s on a new round (Rounds form redesign item 5)', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    // The grace input shows 30 by default; the submitted request carries 30s as micros.
    expect((await screen.findByLabelText('Grace window seconds')).getAttribute('value') ?? '');
    expect((screen.getByLabelText('Grace window seconds') as HTMLInputElement).value).toBe('30');
    await fireEvent.input(screen.getByLabelText('Label'), { target: { value: 'Q' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    expect(createRoundImpl.mock.calls[0][2].grace_window).toEqual({
      Duration: { micros: 30_000_000 }
    });
  });

  it('stores exactly one class from the single-select dropdown (Rounds form redesign item 6)', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), {
      target: { value: 'Spec Qual' }
    });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });
    // The class control is a single `<select>` of the event's classes — pick the Spec class.
    const classSelect = screen.getByLabelText('Eligible class') as HTMLSelectElement;
    expect(classSelect.tagName).toBe('SELECT');
    await fireEvent.change(classSelect, { target: { value: 'c2' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    // Stored as the existing one-element `classes` list.
    expect(createRoundImpl.mock.calls[0][2].classes).toEqual(['c2']);
  });

  it('seeds the class dropdown from the first class of the round being edited', async () => {
    const { session } = makeTestSession({ ...baseImpls(), event: EVENT });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    // QUAL targets class c1 — the dropdown seeds to it.
    const classSelect = (await screen.findByLabelText('Eligible class')) as HTMLSelectElement;
    expect(classSelect.value).toBe('c1');
  });

  it('seeds the channel-mode toggle from the round being edited', async () => {
    const { session } = makeTestSession({ ...baseImpls(), event: EVENT });
    render(EventRounds, { session });

    // QUAL persists with channel_mode: 'Static' — editing it seeds the toggle to Static.
    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    const editMode = (await screen.findByLabelText('Channel mode')) as HTMLSelectElement;
    expect(editMode.value).toBe('Static');
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

  // ── The add-round form is a modal Dialog (ux(rounds)) ─────────────────────────────────────────
  // The form lives in a modal Dialog: it is not shown (no open dialog in the a11y tree) until
  // "+ Add round" opens it; cancel / a successful submit close it. A closed native <dialog> drops
  // out of the accessibility tree, so `queryByRole('dialog')` is null while it is closed.
  it('does not show the add-round dialog until "+ Add round" is clicked', async () => {
    const { session } = makeTestSession({ ...baseImpls(), event: { ...EVENT, rounds: [] } });
    render(EventRounds, { session });

    // The trigger is present, but no dialog is shown (a closed native <dialog> drops out of the
    // a11y tree), so the form is not presented to the user.
    await screen.findByRole('button', { name: '+ Add round' });
    expect(screen.queryByRole('dialog')).toBeNull();

    await fireEvent.click(screen.getByRole('button', { name: '+ Add round' }));
    // Opening reveals the modal: the dialog + its form fields are now present.
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    expect(screen.getByLabelText('Label')).toBeInTheDocument();
    expect(screen.getByLabelText('Format')).toBeInTheDocument();
    expect(screen.getByRole('form', { name: 'Add round' })).toBeInTheDocument();
  });

  it('cancel closes the add-round dialog without creating a round', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), {
      target: { value: 'Throwaway' }
    });
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    // Cancel closes the dialog and submits nothing.
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(createRoundImpl).not.toHaveBeenCalled();
  });

  it('closes the add-round dialog after a successful create', async () => {
    const impls = baseImpls();
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({
      ...QUAL,
      id: 'r2',
      label: 'New One'
    }));
    const { session } = makeTestSession({
      ...impls,
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'New One' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    // On success the dialog closes (leaves the a11y tree).
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });
});

describe('EventRounds (open practice — no win condition + time limit)', () => {
  // The open-practice format + a primary timer with two channel seats (so the active-channels picker
  // is populated). The format dropdown reads from the schemas, so `open_practice` must be among them.
  const OP_FORMATS = [...FORMATS, 'open_practice'];
  const OP_SCHEMAS = OP_FORMATS.map((name) =>
    name === 'open_practice'
      ? { name, params: [] }
      : (SCHEMAS.find((s) => s.name === name) ?? { name, params: [] })
  );
  const TIMER: Timer = {
    id: 'mock',
    name: 'Mock',
    kind: { Mock: { laps: 3, lap_ms: 1000 } },
    status: 'Ready',
    channel_capability: 'Flexible',
    node_count: 2,
    available_channels: [5658, 5800]
  };

  function opImpls() {
    return {
      ...baseImpls(),
      listFormatsImpl: vi.fn(async () => OP_FORMATS),
      listFormatSchemasImpl: vi.fn(async () => OP_SCHEMAS),
      listTimersImpl: vi.fn(async () => [TIMER]),
      listChannelsImpl: vi.fn(async () => CATALOG)
    };
  }

  it('hides the win condition, shows a Time limit, and submits open practice with no win condition', async () => {
    const created: RoundDef = {
      id: 'op1',
      label: 'Open Practice',
      classes: [],
      format: 'open_practice',
      params: {},
      win_condition: 'BestLap',
      seeding: { AllChannels: { channels: [0, 1] } },
      channel_mode: 'PerHeat',
      staging_timer_secs: 300,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
      grace_window: { Duration: { micros: 3000000 } },
      time_limit_secs: 5400,
      protest_window: 'Off'
    };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => created);
    const { session } = makeTestSession({
      ...opImpls(),
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), {
      target: { value: 'Open Practice' }
    });
    await fireEvent.change(screen.getByLabelText('Format'), {
      target: { value: 'open_practice' }
    });

    // The win-condition input is gone; the single-minutes Time limit appears instead.
    await waitFor(() => expect(screen.queryByLabelText('Win condition')).toBeNull());
    expect(screen.getByLabelText('Time limit minutes')).toBeInTheDocument();
    // No separate hours field anymore — minutes only.
    expect(screen.queryByLabelText('Time limit hours')).toBeNull();

    // Pick both active channels (the picker is driven by the primary timer's node seats).
    await fireEvent.click(await screen.findByLabelText(/Channel .*5658/));
    await fireEvent.click(screen.getByLabelText(/Channel .*5800/));

    // Set a 90-minute practice duration (stored as 90*60 = 5400s).
    await fireEvent.input(screen.getByLabelText('Time limit minutes'), { target: { value: '90' } });

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    // No win condition is sent; the AllChannels seeding + the time limit (in seconds) are.
    expect(req.win_condition).toBeUndefined();
    expect(req.classes).toEqual([]);
    expect(req.seeding).toEqual({ AllChannels: { channels: [0, 1] } });
    expect(req.time_limit_secs).toBe(5400); // 90 min * 60
  });

  it('treats a blank time-limit minutes field as no limit (undefined)', async () => {
    const created: RoundDef = {
      id: 'op2',
      label: 'Open Practice',
      classes: [],
      format: 'open_practice',
      params: {},
      win_condition: 'BestLap',
      seeding: { AllChannels: { channels: [0, 1] } },
      channel_mode: 'PerHeat',
      staging_timer_secs: 300,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
      grace_window: { Duration: { micros: 3000000 } },
      protest_window: 'Off'
    };
    const createRoundImpl = vi.fn(async (_b, _e, _req) => created);
    const { session } = makeTestSession({
      ...opImpls(),
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), {
      target: { value: 'Open Practice' }
    });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'open_practice' } });
    await waitFor(() => expect(screen.queryByLabelText('Win condition')).toBeNull());
    await fireEvent.click(await screen.findByLabelText(/Channel .*5658/));
    await fireEvent.click(screen.getByLabelText(/Channel .*5800/));

    // Leave the minutes field blank → no limit.
    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));
    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, , req] = createRoundImpl.mock.calls[0];
    expect(req.time_limit_secs).toBeUndefined();
  });

  it('reflects an open-practice round: shows its time-limit summary and no Fill control', async () => {
    const op: RoundDef = {
      id: 'op1',
      label: 'Free Practice',
      classes: [],
      format: 'open_practice',
      params: {},
      win_condition: 'BestLap',
      seeding: { AllChannels: { channels: [0, 1] } },
      channel_mode: 'PerHeat',
      staging_timer_secs: 300,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
      grace_window: { Duration: { micros: 3000000 } },
      time_limit_secs: 3600,
      protest_window: 'Off'
    };
    const { session } = makeTestSession({
      ...opImpls(),
      listHeatsImpl: vi.fn(async () => []),
      event: { ...EVENT, rounds: [op] }
    });
    render(EventRounds, { session });

    // The Rounds list shows the time-limit summary ("1h") in place of a win condition.
    const roundsCard = screen.getByRole('heading', { name: 'Rounds' }).closest('section')!;
    await within(roundsCard).findByText('Free Practice');
    expect(within(roundsCard).getByText('1h')).toBeInTheDocument();

    // The Heats area drops the manual fill control for the open-practice round — neither the
    // single-step "Add next heat" nor the deterministic "Generate heats" (#216).
    const heatsCard = screen.getByRole('heading', { name: 'Heats' }).closest('section')!;
    expect(within(heatsCard).queryByRole('button', { name: 'Add next heat' })).toBeNull();
    expect(within(heatsCard).queryByRole('button', { name: 'Generate heats' })).toBeNull();
  });

  it('round-trips an open-practice time limit through edit', async () => {
    const op: RoundDef = {
      id: 'op1',
      label: 'Free Practice',
      classes: [],
      format: 'open_practice',
      params: {},
      win_condition: 'BestLap',
      seeding: { AllChannels: { channels: [0] } },
      channel_mode: 'PerHeat',
      staging_timer_secs: 300,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
      grace_window: { Duration: { micros: 3000000 } },
      time_limit_secs: 5400,
      protest_window: 'Off'
    };
    const updateRoundImpl = vi.fn(async (_b, _e, _id, req) => ({ ...op, ...req }));
    const { session } = makeTestSession({
      ...opImpls(),
      updateRoundImpl,
      event: { ...EVENT, rounds: [op] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    // The time limit seeds back into the single Minutes input (5400s = 90 min).
    await waitFor(() =>
      expect((screen.getByLabelText('Time limit minutes') as HTMLInputElement).value).toBe('90')
    );
    expect(screen.queryByLabelText('Time limit hours')).toBeNull();
  });

  it('blocks submit when a Timed round has its race time cleared (P1-6)', async () => {
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({ ...QUAL, id: 'r2' }));
    const { session } = makeTestSession({
      ...baseImpls(),
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), { target: { value: 'Timed R1' } });
    await fireEvent.change(screen.getByLabelText('Format'), { target: { value: 'timed_qual' } });
    await fireEvent.change(screen.getByLabelText('Eligible class'), { target: { value: 'c1' } });
    // Time Trials defaults the win condition to Timed; the "Race time (seconds)" field shows.
    const raceTime = await screen.findByLabelText('Race time seconds');
    const addBtn = () => screen.getByRole('button', { name: 'Add round' }) as HTMLButtonElement;

    // With a valid race time, the form is submittable.
    expect(addBtn().disabled).toBe(false);

    // Clearing the race time blocks submit (it would otherwise build a NaN/0-µs window).
    await fireEvent.input(raceTime, { target: { value: '' } });
    expect(addBtn().disabled).toBe(true);
    await fireEvent.click(addBtn());
    expect(createRoundImpl).not.toHaveBeenCalled();

    // Restoring a valid race time re-enables submit.
    await fireEvent.input(raceTime, { target: { value: '90' } });
    expect(addBtn().disabled).toBe(false);
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

    // The heat is named "<round label> Heat <N>" (not the raw id) — the only heat in r1 → Heat 1.
    const nameEl = await screen.findByText('Qualifying R1 Heat 1');
    expect(nameEl).toHaveClass('heat-id');
    expect(screen.queryByText('q-1')).toBeNull();
    // The heat appears under its round with resolved callsigns, a Running status, and Current.
    const heatRow = nameEl.closest('.heat-row') as HTMLElement;
    expect(within(heatRow).getByText('AceOne')).toBeInTheDocument();
    expect(within(heatRow).getByText('Bolt')).toBeInTheDocument();
    expect(within(heatRow).getByText('Running')).toBeInTheDocument();
    expect(within(heatRow).getByText('Current')).toBeInTheDocument();
  });

  it('names each non-open-practice heat "<round label> Heat <N>" by its position in the round', async () => {
    // Two heats in the same round → "Qualifying R1 Heat 1" and "Qualifying R1 Heat 2" by order.
    const heats: HeatSummary[] = [
      {
        heat: 'q-a',
        lineup: ['p1', 'p2'],
        class: 'c1',
        round: 'r1',
        phase: 'Final',
        is_current: false
      },
      {
        heat: 'q-b',
        lineup: ['p1', 'p2'],
        class: 'c1',
        round: 'r1',
        phase: 'Scheduled',
        is_current: false
      }
    ];
    const { session } = makeTestSession({ ...heatsImpls(heats), event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    expect(await screen.findByText('Qualifying R1 Heat 1')).toBeInTheDocument();
    expect(await screen.findByText('Qualifying R1 Heat 2')).toBeInTheDocument();
    // The raw heat ids are not shown.
    expect(screen.queryByText('q-a')).toBeNull();
    expect(screen.queryByText('q-b')).toBeNull();
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

    const heatRow = (await screen.findByText('Qualifying R1 Heat 1')).closest(
      '.heat-row'
    ) as HTMLElement;
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

    const heatRow = (await screen.findByText('Qualifying R1 Heat 1')).closest(
      '.heat-row'
    ) as HTMLElement;
    // Both pilots show the dash (no channel assigned).
    const dashes = within(heatRow).getAllByText('—');
    expect(dashes.length).toBe(2);
  });

  it("names an open-practice round's auto-created heat 'Practice Heat' (not its id)", async () => {
    // An open-practice round (open_practice format + AllChannels seeding) auto-creates one heat;
    // it should display as "Practice Heat" rather than the generated heat id.
    const OP_ROUND: RoundDef = {
      id: 'op1',
      label: 'Open Practice',
      classes: [],
      format: 'open_practice',
      params: {},
      win_condition: 'BestLap',
      seeding: { AllChannels: { channels: [0, 1] } },
      channel_mode: 'PerHeat',
      staging_timer_secs: 300,
      start_procedure: { mode: 'randomized-delay', min_delay_ms: 2000, max_delay_ms: 5000 },
      grace_window: { Duration: { micros: 3000000 } },
      protest_window: 'Off'
    };
    const heat: HeatSummary = {
      heat: 'op1-heat-7x2',
      lineup: ['p1', 'p2'],
      round: 'op1',
      phase: 'Scheduled',
      is_current: false
    };
    const { session } = makeTestSession({
      ...heatsImpls([heat]),
      event: { ...EVENT_WITH_MEMBERS, rounds: [OP_ROUND] }
    });
    render(EventRounds, { session });

    // The heat shows the friendly name; the generated id is not rendered.
    const nameEl = await screen.findByText('Practice Heat');
    expect(nameEl).toHaveClass('heat-id');
    expect(screen.queryByText('op1-heat-7x2')).toBeNull();
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

    // r1 is a timed_qual round — a deterministic format, so its control is "Generate heats" (#216).
    await fireEvent.click(await screen.findByRole('button', { name: 'Generate heats' }));

    // A FillRound command tagged with the round was sent — in fill-all mode for the deterministic round.
    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    expect(sendSpy.mock.calls[0][0]).toEqual({ FillRound: { round: 'r1', mode: 'All' } });
    // The newly-scheduled heat shows up after the re-read, named by its round + position.
    await screen.findByText('Qualifying R1 Heat 1');
  });

  it('generate-all fills MULTIPLE heats from one click for a deterministic round (#216)', async () => {
    const impls = heatsImpls([]);
    const filled: HeatSummary[] = [
      {
        heat: 'q-1',
        lineup: ['p1', 'p2'],
        class: 'c1',
        round: 'r1',
        phase: 'Scheduled',
        is_current: true
      },
      {
        heat: 'q-2',
        lineup: ['p1', 'p2'],
        class: 'c1',
        round: 'r1',
        phase: 'Scheduled',
        is_current: false
      }
    ];
    // Empty on mount; after the one generate-all action the whole round's heats are present.
    impls.listHeatsImpl.mockResolvedValueOnce([]).mockResolvedValue(filled);

    const { session, sendSpy } = makeTestSession({ ...impls, event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    // One "Generate heats" click → one fill-all command → both heats appear.
    await fireEvent.click(await screen.findByRole('button', { name: 'Generate heats' }));
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(1));
    expect(sendSpy.mock.calls[0][0]).toEqual({ FillRound: { round: 'r1', mode: 'All' } });
    await screen.findByText('Qualifying R1 Heat 1');
    await screen.findByText('Qualifying R1 Heat 2');
  });

  it('builds a heat by hand without typing an id (auto-generated, round-scoped)', async () => {
    const impls = heatsImpls([]);
    const { session, sendSpy } = makeTestSession({ ...impls, event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));

    // There is no heat-id field to fill — the RD only picks a round (defaults to the first) and a
    // lineup. Submit is enabled by round + lineup alone.
    expect(screen.queryByLabelText('Build heat id')).toBeNull();
    expect(screen.getByRole('button', { name: 'Schedule heat' })).toBeDisabled();

    await fireEvent.click(screen.getByLabelText('Select AceOne'));
    await fireEvent.click(screen.getByLabelText('Select Bolt'));
    expect(screen.getByRole('button', { name: 'Schedule heat' })).toBeEnabled();
    await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));

    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    // A ScheduleHeat tagged with the round + its single class, lineup of the chosen pilot refs.
    // The optional name was left blank, so no custom label is sent (the heat keeps its auto-name).
    const sent = sendSpy.mock.calls[0][0] as { ScheduleHeat: { heat: string } };
    expect(sent.ScheduleHeat).toMatchObject({
      lineup: ['p1', 'p2'],
      class: 'c1',
      round: 'r1',
      label: undefined
    });
    // The id is auto-generated: non-empty and round-scoped (the readable `<round>-h-…` style).
    expect(sent.ScheduleHeat.heat).toBeTruthy();
    expect(sent.ScheduleHeat.heat).toMatch(/^r1-h-/);
  });

  it('caps the hand-built heat lineup at the primary timer node count, with a note when full', async () => {
    const CARLA: Pilot = { id: 'p3', callsign: 'Carla', vtx_types: [] };
    const event3: EventMeta = {
      ...EVENT,
      roster: ['p1', 'p2', 'p3'],
      classes_membership: [
        { class: 'c1', pilots: [{ pilot: 'p1' }, { pilot: 'p2' }, { pilot: 'p3' }] }
      ]
    };
    const timer2: Timer = {
      id: 'mock',
      name: 'Mock',
      kind: { Mock: { laps: 3, lap_ms: 1000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 2,
      available_channels: [5658, 5800]
    };
    const impls = {
      ...heatsImpls([]),
      listPilotsImpl: vi.fn(async () => [ACE, BOLT, CARLA]),
      listTimersImpl: vi.fn(async () => [timer2])
    };
    const { session } = makeTestSession({ ...impls, event: event3 });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));
    // The 2-node timer caps the lineup at 2: after two picks the third is disabled + a note shows.
    await fireEvent.click(await screen.findByLabelText('Select AceOne'));
    await fireEvent.click(screen.getByLabelText('Select Bolt'));
    expect(screen.getByLabelText('Select Carla')).toBeDisabled();
    expect(screen.getByText(/All 2 nodes on the primary timer/)).toBeInTheDocument();
    // Unselecting one frees a node — the third is selectable again.
    await fireEvent.click(screen.getByLabelText('Select Bolt'));
    expect(screen.getByLabelText('Select Carla')).toBeEnabled();
  });

  it('generates a unique id across two quick builds (no collision)', async () => {
    const impls = heatsImpls([]);
    const { session, sendSpy } = makeTestSession({ ...impls, event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    const buildOne = async () => {
      await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));
      await fireEvent.click(screen.getByLabelText('Select AceOne'));
      await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));
    };

    await buildOne();
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(1));
    await buildOne();
    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(2));

    const first = (sendSpy.mock.calls[0][0] as { ScheduleHeat: { heat: string } }).ScheduleHeat
      .heat;
    const second = (sendSpy.mock.calls[1][0] as { ScheduleHeat: { heat: string } }).ScheduleHeat
      .heat;
    expect(first).not.toEqual(second);
  });

  it('sends the optional heat name as the ScheduleHeat label (custom build-heat name)', async () => {
    const impls = heatsImpls([]);
    const { session, sendSpy } = makeTestSession({ ...impls, event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));
    await fireEvent.input(await screen.findByLabelText('Build heat name'), {
      target: { value: 'Featured Heat' }
    });
    await fireEvent.click(screen.getByLabelText('Select AceOne'));
    await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));

    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    const sent = sendSpy.mock.calls[0][0] as { ScheduleHeat: { heat: string } };
    expect(sent.ScheduleHeat).toMatchObject({
      lineup: ['p1'],
      class: 'c1',
      round: 'r1',
      label: 'Featured Heat'
    });
    expect(sent.ScheduleHeat.heat).toMatch(/^r1-h-/);
  });

  // ── The build-heat form is a modal Dialog (ux(rounds)) ────────────────────────────────────────
  it('does not show the build-heat dialog until "+ Build heat" is clicked', async () => {
    const { session } = makeTestSession({ ...heatsImpls([]), event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    // No dialog shown (and so the build form is not presented) until the trigger is clicked.
    await screen.findByRole('button', { name: '+ Build heat' });
    expect(screen.queryByRole('dialog')).toBeNull();

    await fireEvent.click(screen.getByRole('button', { name: '+ Build heat' }));
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    expect(screen.getByRole('form', { name: 'Build heat' })).toBeInTheDocument();
    expect(screen.getByLabelText('Build round')).toBeInTheDocument();
  });

  it('cancel closes the build-heat dialog without scheduling a heat', async () => {
    const { session, sendSpy } = makeTestSession({ ...heatsImpls([]), event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));
    await fireEvent.click(await screen.findByLabelText('Select AceOne'));
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(sendSpy).not.toHaveBeenCalled();
  });

  it('closes the build-heat dialog after a successful schedule', async () => {
    const { session, sendSpy } = makeTestSession({ ...heatsImpls([]), event: EVENT_WITH_MEMBERS });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Build heat' }));
    await fireEvent.click(await screen.findByLabelText('Select AceOne'));
    await fireEvent.click(screen.getByRole('button', { name: 'Schedule heat' }));

    await waitFor(() => expect(sendSpy).toHaveBeenCalled());
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });
});

describe('EventRounds (per-round standings — Slice 5/6b)', () => {
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

  it('shows finalize-progress instead of an all-tied (every pilot position 1) ranking', async () => {
    // No heats finalized → the ranking ties everyone at position 1. That reads as broken, so the
    // panel shows the finalize-progress message rather than the all-P1 list.
    const roundRankingImpl = vi.fn(async (_b, _e, _round) => [
      { competitor: 'p1', position: 1 },
      { competitor: 'p2', position: 1 }
    ]);
    const { session } = makeTestSession({
      ...baseHeatsImpls(),
      roundRankingImpl,
      event: EVENT_WITH_MEMBERS
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Standings' }));
    await waitFor(() => expect(roundRankingImpl).toHaveBeenCalled());
    const panel = (await screen.findByLabelText(/Standings for Qualifying R1/i)) as HTMLElement;
    expect(within(panel).getByText(/Ranking appears as you finalize/i)).toBeInTheDocument();
    // The misleading all-tied list is NOT rendered.
    expect(within(panel).queryByText('AceOne')).toBeNull();
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
});

// ── Open-practice format: the active-channels picker (open-practice Slice 2) ───────────────────

// A primary timer with 4 node seats, the first three configured to Raceband R1/R2 + a custom MHz.
const PRACTICE_TIMER: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready',
  channel_capability: 'Flexible',
  node_count: 4,
  available_channels: [5658, 5800, 5732]
};
// The schemas including open_practice so the format dropdown offers it.
const OP_SCHEMAS = [...SCHEMAS, { name: 'open_practice', params: [] }];
// The channel catalog the picker resolves node channels through (5732 is custom → raw MHz).
const OP_CATALOG: ChannelCatalogEntry[] = [
  { band: 'Raceband', channel: 'R1', mhz: 5658 },
  { band: 'Fatshark', channel: 'F4', mhz: 5800 }
];

describe('EventRounds — open-practice active-channels picker', () => {
  it('swaps to an active-channels picker and saves AllChannels with the selected node indices', async () => {
    const createRoundImpl = vi.fn(async (_b, _e, _req) => ({
      ...QUAL,
      id: 'r2',
      label: 'Open Practice',
      classes: [],
      format: 'open_practice',
      seeding: { AllChannels: { channels: [0, 2] } }
    }));
    const { session } = makeTestSession({
      listClassesImpl: vi.fn(async () => [OPEN, SPEC]),
      listFormatsImpl: vi.fn(async () => [...FORMATS, 'open_practice']),
      listFormatSchemasImpl: vi.fn(async () => OP_SCHEMAS),
      listTimersImpl: vi.fn(async () => [PRACTICE_TIMER]),
      listChannelsImpl: vi.fn(async () => OP_CATALOG),
      createRoundImpl,
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.input(await screen.findByLabelText('Label'), {
      target: { value: 'Open Practice' }
    });
    // Pick the open-practice format → the class picker disappears, the channel picker appears.
    await fireEvent.change(screen.getByLabelText('Format'), {
      target: { value: 'open_practice' }
    });
    await waitFor(() => expect(screen.queryByLabelText('Eligible Open')).not.toBeInTheDocument());

    // Seats labelled by band+channel·MHz, with a bare node for the unconfigured 4th seat.
    expect(await screen.findByLabelText('Channel Raceband R1 · 5658')).toBeInTheDocument();
    expect(screen.getByLabelText('Channel Fatshark F4 · 5800')).toBeInTheDocument();
    expect(screen.getByLabelText('Channel Node 4')).toBeInTheDocument();

    // Activate node 0 (Raceband R1) and node 2 (5732 → custom MHz).
    await fireEvent.click(screen.getByLabelText('Channel Raceband R1 · 5658'));
    await fireEvent.click(screen.getByLabelText('Channel 5732 MHz'));

    await fireEvent.click(screen.getByRole('button', { name: 'Add round' }));

    await waitFor(() => expect(createRoundImpl).toHaveBeenCalledTimes(1));
    const [, eventId, req] = createRoundImpl.mock.calls[0];
    expect(eventId).toBe('e1');
    expect(req).toMatchObject({
      label: 'Open Practice',
      classes: [],
      format: 'open_practice',
      seeding: { AllChannels: { channels: [0, 2] } }
    });
  });

  it('reflects an edited open-practice round’s existing channel selection', async () => {
    const OP_ROUND: RoundDef = {
      ...QUAL,
      id: 'r9',
      label: 'Practice',
      classes: [],
      format: 'open_practice',
      seeding: { AllChannels: { channels: [1] } }
    };
    const { session } = makeTestSession({
      listClassesImpl: vi.fn(async () => [OPEN, SPEC]),
      listFormatsImpl: vi.fn(async () => [...FORMATS, 'open_practice']),
      listFormatSchemasImpl: vi.fn(async () => OP_SCHEMAS),
      listTimersImpl: vi.fn(async () => [PRACTICE_TIMER]),
      listChannelsImpl: vi.fn(async () => OP_CATALOG),
      event: { ...EVENT, rounds: [OP_ROUND] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: 'Edit' }));
    // The saved active channel (node 1 → Fatshark F4) seeds checked; node 0 is not.
    const f4 = (await screen.findByLabelText('Channel Fatshark F4 · 5800')) as HTMLInputElement;
    const r1 = screen.getByLabelText('Channel Raceband R1 · 5658') as HTMLInputElement;
    expect(f4.checked).toBe(true);
    expect(r1.checked).toBe(false);
  });

  it('nudges to set a timer when the event has no primary timer with channels', async () => {
    const { session } = makeTestSession({
      listClassesImpl: vi.fn(async () => [OPEN, SPEC]),
      listFormatsImpl: vi.fn(async () => [...FORMATS, 'open_practice']),
      listFormatSchemasImpl: vi.fn(async () => OP_SCHEMAS),
      // No timers in the registry → no primary timer resolves.
      listTimersImpl: vi.fn(async () => []),
      listChannelsImpl: vi.fn(async () => OP_CATALOG),
      event: { ...EVENT, rounds: [] }
    });
    render(EventRounds, { session });

    await fireEvent.click(await screen.findByRole('button', { name: '+ Add round' }));
    await fireEvent.change(await screen.findByLabelText('Format'), {
      target: { value: 'open_practice' }
    });
    // The picker degrades to a nudge to set a timer, and the round can't be saved.
    expect(await screen.findByText(/No primary timer for this event/)).toBeInTheDocument();
    expect((screen.getByRole('button', { name: 'Add round' }) as HTMLButtonElement).disabled).toBe(
      true
    );
  });
});
