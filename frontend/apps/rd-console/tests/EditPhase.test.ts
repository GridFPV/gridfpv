/**
 * Editing a phase or a heat (docs/primitives.md §12.4): its name always, its format and
 * where it sits until a ranked race beneath it is saved.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { Command, EventView, PhaseDef, PhaseView } from '@gridfpv/types';
import type { LiveSession } from '../src/lib/eventLive.svelte.js';
import { moveChoices, phaseEditable } from '../src/lib/phases.js';
import PhasePage from '../src/screens/PhasePage.svelte';
import {
  FIRST_TO_3,
  OPEN_PRACTICE,
  SAVED_FORMATS,
  TWO_MINUTE_QUALI,
  eventView,
  group,
  heat,
  race
} from './competition.js';
import { openLive } from './support.js';

afterEach(cleanup);

/** Qualifying (ranked, first to 3) with two heats, a Finals beside it, and a bracket Grid made. */
const QUALI = group('quali', 'Qualifying', undefined, {
  def: {
    id: 'quali',
    label: 'Qualifying',
    kind: { Group: { combine: { Measures: FIRST_TO_3.ranking! } } },
    format: FIRST_TO_3
  },
  format: FIRST_TO_3
});
const HEAT_1 = heat('quali/heat-1', 'Heat 1', ['p-1a'], { parent: 'quali', format: FIRST_TO_3 });
const HEAT_2 = heat('quali/heat-2', 'Heat 2', ['p-2b'], { parent: 'quali', format: FIRST_TO_3 });
const FINALS = group('finals', 'Finals');
const BRACKET: PhaseView = {
  ...group('bracket', 'Whoop Bracket'),
  def: {
    id: 'bracket',
    label: 'Whoop Bracket',
    kind: { Group: { combine: 'Stack' } },
    display_hint: 'double-elimination'
  }
};
const ROUND = group('bracket/r1', 'Winners Round 1', 'bracket');
const PHASES = [QUALI, HEAT_1, HEAT_2, FINALS, BRACKET, ROUND];

describe('what about a phase can be edited', () => {
  it('everything, while no race beneath it is saved', () => {
    expect(phaseEditable(QUALI, PHASES, [])).toEqual({ shape: true });
    // A race that is running, stopped or discarded protects nothing.
    for (const state of ['Staging', 'Running', 'Stopped', 'Discarded'] as const) {
      expect(phaseEditable(QUALI, PHASES, [{ heat: 'quali/heat-1', state }]).shape).toBe(true);
    }
  });

  it('only the name, once a ranked race is saved in it or anywhere beneath it', () => {
    const saved = [{ heat: 'quali/heat-1', state: 'Saved' as const }];
    const quali = phaseEditable(QUALI, PHASES, saved);
    expect(quali.shape).toBe(false);
    expect(quali.why).toMatch(/A race here has been saved, so only its name can change/);
    expect(phaseEditable(HEAT_1, PHASES, saved).shape).toBe(false);
    // The heat beside it, and the phase beside its phase, are still free.
    expect(phaseEditable(HEAT_2, PHASES, saved).shape).toBe(true);
    expect(phaseEditable(FINALS, PHASES, saved).shape).toBe(true);
  });

  it('everything, in practice: an unranked heat has no result to protect', () => {
    const practice = heat('practice', 'Open Practice', ['Open'], { format: OPEN_PRACTICE });
    expect(phaseEditable(practice, [practice], [{ heat: 'practice', state: 'Saved' }]).shape).toBe(
      true
    );
  });

  it('only the name of a phase Grid made, and of anything inside one, and it says which', () => {
    expect(phaseEditable(BRACKET, PHASES, [])).toEqual({
      shape: false,
      why: 'Grid made this phase, so only its name can change.'
    });
    expect(phaseEditable(ROUND, PHASES, []).why).toBe(
      'It is part of Whoop Bracket, which Grid made, so only its name can change.'
    );
  });

  it('can be moved under any phase but itself, what is beneath it, and what is locked', () => {
    const inner = group('quali/day-2', 'Day 2', 'quali');
    const locked = group('archive', 'Archive', undefined, { locked: true });
    const all = [...PHASES, inner, locked];
    expect(moveChoices(QUALI, all).map((choice) => choice.label)).toEqual([
      'Finals',
      'Whoop Bracket',
      'Whoop Bracket › Winners Round 1'
    ]);
    expect(moveChoices(HEAT_1, all).map((choice) => choice.label)).toContain('Qualifying › Day 2');
  });
});

describe('EditPhaseDialog', () => {
  async function open(view: EventView, phase: string, over: Partial<LiveSession> = {}) {
    const { session, live } = await openLive(view, {
      listFormats: vi.fn(async () => SAVED_FORMATS),
      ...over
    });
    render(PhasePage, { live, phase, onrun: vi.fn() });
    return session;
  }
  const defined = (session: LiveSession): PhaseDef[] =>
    vi
      .mocked(session.sendCommand)
      .mock.calls.map(
        (call) =>
          (call[1] as Extract<Command, { Record: unknown }>).Record as {
            PhaseDefined: { def: PhaseDef };
          }
      )
      .map((record) => record.PhaseDefined.def);
  const view = (races: EventView['races'] = []) => eventView({ phases: PHASES, races });
  const dialog = () => screen.findByRole('form', { name: /^Edit / });

  it('renames a phase and changes nothing else about it', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    expect(within(form).getByLabelText('Name')).toHaveValue('Qualifying');
    await fireEvent.input(within(form).getByLabelText('Name'), {
      target: { value: '  Qualifying rounds ' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    expect(defined(session)[0]).toEqual({ ...QUALI.def, label: 'Qualifying rounds' });
  });

  it('opens on the format the phase has, and changing it also changes how the phase ranks', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    const format = within(form).getByLabelText('Format') as HTMLSelectElement;
    expect(format.value).toBe('fmt-first3-7q1x');
    await fireEvent.change(format, { target: { value: 'fmt-quali-4m8z' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    expect(defined(session)[0]).toEqual({
      id: 'quali',
      label: 'Qualifying',
      kind: { Group: { combine: { Measures: TWO_MINUTE_QUALI.ranking! } } },
      format: TWO_MINUTE_QUALI
    });
    // No format id reaches the page.
    expect(document.body.textContent).not.toContain('fmt-');
  });

  it('given an unranked format, the phase ranks nobody', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'fmt-practice-9k2d' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    expect(defined(session)[0].kind).toEqual({ Group: { combine: 'Stack' } });
    expect(defined(session)[0].format).toEqual(OPEN_PRACTICE);
  });

  it('moves a phase under another, and to the top again', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    const under = within(form).getByLabelText('Under') as HTMLSelectElement;
    expect(under.value).toBe('');
    // Not under itself or its own heats' phase, and by name.
    expect([...under.options].map((option) => option.textContent?.trim())).toEqual([
      'The top of the event',
      'Finals',
      'Whoop Bracket',
      'Whoop Bracket › Winners Round 1'
    ]);
    await fireEvent.change(under, { target: { value: 'finals' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    expect(defined(session)[0].parent).toBe('finals');
  });

  it('with nothing changed, Save sends nothing and closes', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    await dialog();
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(screen.queryByRole('form', { name: /^Edit / })).toBeNull());
    expect(defined(session)).toEqual([]);
  });

  it('does not take an empty name', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    await fireEvent.input(within(form).getByLabelText('Name'), { target: { value: '   ' } });
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
    await fireEvent.submit(form);
    expect(defined(session)).toEqual([]);
  });

  it('once a race is saved, offers the name alone and says why', async () => {
    const session = await open(view([race('quali/heat-1', 1, 'Saved')]), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    expect(within(form).queryByLabelText('Format')).toBeNull();
    expect(within(form).queryByLabelText('Under')).toBeNull();
    expect(within(form).getByTestId('edit-why')).toHaveTextContent(
      'A race here has been saved, so only its name can change.'
    );
    await fireEvent.input(within(form).getByLabelText('Name'), { target: { value: 'Quali' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    // The format and the place are sent as they were.
    expect(defined(session)[0]).toEqual({ ...QUALI.def, label: 'Quali' });
  });

  it('a phase Grid made offers the name alone', async () => {
    await open(view(), 'bracket');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Whoop Bracket' }));
    const form = await dialog();
    expect(within(form).queryByLabelText('Format')).toBeNull();
    expect(within(form).getByTestId('edit-why')).toHaveTextContent(
      'Grid made this phase, so only its name can change.'
    );
  });

  it('edits a heat from its row, and keeps its pilots', async () => {
    const session = await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Heat 2' }));
    const form = await dialog();
    await fireEvent.input(within(form).getByLabelText('Name'), { target: { value: 'Heat B' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    expect(defined(session)[0]).toEqual({ ...HEAT_2.def, label: 'Heat B' });
  });

  it('a heat in a phase can be moved to another phase, and is never offered the top', async () => {
    await open(view(), 'quali');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Heat 2' }));
    const form = await dialog();
    const under = within(form).getByLabelText('Under') as HTMLSelectElement;
    expect(under.value).toBe('quali');
    expect([...under.options].map((option) => option.textContent?.trim())).not.toContain(
      'The top of the event'
    );
  });

  it('a heat that stands alone from before can stay there, or be moved into a phase', async () => {
    const lone = heat('lone', 'Lone Heat', ['p-1a'], { format: FIRST_TO_3 });
    const loneGroup = group('holder', 'Holder');
    const session = await open(eventView({ phases: [loneGroup, lone] }), 'lone');
    await fireEvent.click(await screen.findByRole('button', { name: 'Edit Lone Heat' }));
    const form = await dialog();
    const under = within(form).getByLabelText('Under') as HTMLSelectElement;
    // Opening the dialog does not move it by itself.
    expect(under.value).toBe('');
    await fireEvent.change(under, { target: { value: 'holder' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(defined(session)).toHaveLength(1));
    expect(defined(session)[0].parent).toBe('holder');
  });

  it("shows the Director's words when the edit is refused, and stays open with what was typed", async () => {
    const session = await open(view(), 'quali', {
      sendCommand: vi.fn(async () => {
        throw new Error('Qualifying has a saved race, so only its name can change now.');
      })
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Edit Qualifying' }));
    const form = await dialog();
    await fireEvent.change(within(form).getByLabelText('Format'), {
      target: { value: 'fmt-quali-4m8z' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(
      await within(form).findByText('Qualifying has a saved race, so only its name can change now.')
    ).toBeVisible();
    expect((within(form).getByLabelText('Format') as HTMLSelectElement).value).toBe(
      'fmt-quali-4m8z'
    );
    expect(vi.mocked(session.sendCommand)).toHaveBeenCalledOnce();
  });

  it('a locked phase has no Edit', async () => {
    await open(
      eventView({
        phases: [
          { ...QUALI, locked: true, locked_here: true },
          { ...HEAT_1, locked: true }
        ]
      }),
      'quali'
    );
    await screen.findByTestId('back-to-phases');
    expect(screen.queryByRole('button', { name: 'Edit Qualifying' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Edit Heat 1' })).toBeNull();
  });
});
