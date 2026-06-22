import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, EventMeta, RoundDef } from '@gridfpv/types';
import EventRounds from '../src/screens/EventRounds.svelte';
import { makeTestSession } from './support.js';

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const SPEC: Class = { id: 'c2', name: 'Spec', source: 'Custom' };

const QUAL: RoundDef = {
  id: 'r1',
  label: 'Qualifying R1',
  classes: ['c1'],
  format: 'timed_qual',
  params: {},
  win_condition: { Timed: { window_micros: 120_000_000 } },
  seeding: 'FromRoster'
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

    await screen.findByText('Qualifying R1');
    // Format and the resolved class name show; FromRoster summarises as "From roster".
    expect(screen.getByText('timed_qual')).toBeInTheDocument();
    expect(screen.getByText('Open')).toBeInTheDocument();
    expect(screen.getByText('From roster')).toBeInTheDocument();
    expect(screen.getByText(/Timed · 120s/)).toBeInTheDocument();
    // The round index renders.
    expect(screen.getByText('1')).toBeInTheDocument();
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
      seeding: 'FromRoster'
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
    // The new round appears in the list.
    await screen.findByText('Open Practice');
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

  it('shows the Slice 3 Heats placeholder', async () => {
    const { session } = makeTestSession({ ...baseImpls(), event: EVENT });
    render(EventRounds, { session });
    await screen.findByText(/Heat building — Slice 3/i);
  });
});
