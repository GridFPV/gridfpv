import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, EventMeta, Pilot, Timer } from '@gridfpv/types';
import EventSetupWizard from '../src/screens/EventSetupWizard.svelte';
import { makeTestSession } from './support.js';

/**
 * The setup wizard (race redesign Slice 7) is pure orchestration over the existing stage
 * components/commands — so these tests cover the **stepper frame**: navigation (Back/Next), Skip,
 * the clickable progress indicator, and the readiness ("ready to race?") summary read off the live
 * event. The embedded stages have their own deep tests; here we only confirm the wizard surfaces
 * each one (by its heading) and walks between them.
 */

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const MOCK: Timer = {
  id: 'mock',
  name: 'Mock',
  kind: { Mock: { laps: 3, lap_ms: 30000 } },
  status: 'Ready'
} as unknown as Timer;
const ACE: Pilot = { id: 'p1', callsign: 'Ace', source: 'Custom' } as unknown as Pilot;

/** Inert directory reads so the embedded stages mount without throwing. */
function impls() {
  return {
    listClassesImpl: vi.fn(async () => [OPEN]),
    listPilotsImpl: vi.fn(async () => [ACE]),
    listTimersImpl: vi.fn(async () => [MOCK]),
    listFormatsImpl: vi.fn(async () => ['timed_qual']),
    listFormatSchemasImpl: vi.fn(async () => [{ name: 'timed_qual', params: [] }]),
    listChannelsImpl: vi.fn(async () => []),
    listHeatsImpl: vi.fn(async () => [])
  };
}

/** An empty (freshly-created) event — nothing configured yet. */
const EMPTY_EVENT: EventMeta = {
  id: 'e1',
  name: 'Spring Cup',
  created_at: 0,
  persistent: true,
  timers: [],
  roster: [],
  classes: []
};

/** A fully-configured event — every readiness check should be met. */
const READY_EVENT: EventMeta = {
  ...EMPTY_EVENT,
  timers: ['mock'],
  roster: ['p1'],
  classes: ['c1'],
  classes_membership: [{ class: 'c1', pilots: [{ pilot: 'p1' }] }],
  rounds: [{ id: 'r1', label: 'Qual', classes: ['c1'] }]
} as unknown as EventMeta;

describe('EventSetupWizard (guided first-pass stepper)', () => {
  it('opens on the first step (Timer & channels) and shows the event name', async () => {
    const { session } = makeTestSession({ ...impls(), event: EMPTY_EVENT });
    render(EventSetupWizard, { session, open: true });

    // The header names the event being set up and the first step is now Timer & channels (the
    // per-pilot channels assigned in the next step come from the timer, so it's picked first).
    expect(screen.getByText(/Set up · Spring Cup/)).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Timer & channels', level: 2 })).toBeInTheDocument();
    // It embeds the real Timers stage (its card heading + the Mock timer's checkbox).
    expect(
      await screen.findByRole('heading', { name: 'Timers for this event' })
    ).toBeInTheDocument();
    expect(await screen.findByLabelText('Use Mock')).toBeInTheDocument();
  });

  it('walks Next through the stages to Review, then Back', async () => {
    // Seed a timer so the Timer-step gate is satisfied and Next is enabled.
    const { session } = makeTestSession({
      ...impls(),
      event: { ...EMPTY_EVENT, timers: ['mock'] }
    });
    render(EventSetupWizard, { session, open: true });

    const nextBtn = () => screen.getByRole('button', { name: 'Next' });
    // Timer & channels → Classes & Roster → First round → Review.
    await screen.findByRole('heading', { name: 'Timers for this event' });
    await fireEvent.click(nextBtn());
    expect(await screen.findByRole('heading', { name: 'Present pilots' })).toBeInTheDocument();
    await fireEvent.click(nextBtn());
    expect(await screen.findByRole('heading', { name: 'Rounds', level: 3 })).toBeInTheDocument();
    await fireEvent.click(nextBtn());
    // Review: no Next, a Finish button instead.
    expect(screen.getByRole('heading', { name: 'Ready to race?' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Next' })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Finish setup' })).toBeInTheDocument();

    // Back returns to the First round step.
    await fireEvent.click(screen.getByRole('button', { name: 'Back' }));
    expect(await screen.findByRole('heading', { name: 'Rounds', level: 3 })).toBeInTheDocument();
  });

  it('advances through every stage with only Next — no Save button anywhere (auto-save)', async () => {
    const { session } = makeTestSession({
      ...impls(),
      event: { ...EMPTY_EVENT, timers: ['mock'] }
    });
    render(EventSetupWizard, { session, open: true });

    const noSaveButtons = () => {
      // The embedded stages auto-save; none surfaces a selection Save button in the wizard.
      expect(screen.queryByRole('button', { name: 'Save classes' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Save roster' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Save placement' })).toBeNull();
      expect(screen.queryByRole('button', { name: 'Save selection' })).toBeNull();
    };

    // Step 1: Timer & channels.
    await screen.findByRole('heading', { name: 'Timers for this event' });
    noSaveButtons();
    // Next → Classes & Roster.
    await fireEvent.click(screen.getByRole('button', { name: 'Next' }));
    await screen.findByRole('heading', { name: 'Present pilots' });
    noSaveButtons();
    // Next → First round.
    await fireEvent.click(screen.getByRole('button', { name: 'Next' }));
    await screen.findByRole('heading', { name: 'Rounds', level: 3 });
    noSaveButtons();
    // Next → Review, finishing with only Next clicks.
    await fireEvent.click(screen.getByRole('button', { name: 'Next' }));
    expect(screen.getByRole('button', { name: 'Finish setup' })).toBeInTheDocument();
  });

  it('Skip advances a step just like Next (steps are optional)', async () => {
    // From the Timer step, Skip (like Next) requires a timer; seed one so it's enabled.
    const { session } = makeTestSession({
      ...impls(),
      event: { ...EMPTY_EVENT, timers: ['mock'] }
    });
    render(EventSetupWizard, { session, open: true });

    await screen.findByRole('heading', { name: 'Timers for this event' });
    await fireEvent.click(screen.getByRole('button', { name: 'Skip' }));
    expect(await screen.findByRole('heading', { name: 'Present pilots' })).toBeInTheDocument();
  });

  it('gates the Timer step: Next/Skip disabled with a hint until ≥1 timer is selected', async () => {
    // A fresh event with no timers selected → the gate blocks advancing off step 1. Ticking the
    // Mock re-homes the event with `timers: ['mock']`, which `timerCount` reads to clear the gate.
    const setEventTimersImpl = vi.fn(async () => ({ ...EMPTY_EVENT, timers: ['mock'] }));
    const { session } = makeTestSession({ ...impls(), setEventTimersImpl, event: EMPTY_EVENT });
    render(EventSetupWizard, { session, open: true });

    await screen.findByRole('heading', { name: 'Timers for this event' });
    expect((screen.getByRole('button', { name: 'Next' }) as HTMLButtonElement).disabled).toBe(true);
    expect((screen.getByRole('button', { name: 'Skip' }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText(/Select at least one timer to continue/i)).toBeInTheDocument();

    // Ticking the Mock timer satisfies the gate → Next/Skip enable and the hint clears.
    await fireEvent.click(await screen.findByLabelText('Use Mock'));
    await waitFor(() =>
      expect((screen.getByRole('button', { name: 'Next' }) as HTMLButtonElement).disabled).toBe(
        false
      )
    );
    expect((screen.getByRole('button', { name: 'Skip' }) as HTMLButtonElement).disabled).toBe(
      false
    );
    expect(screen.queryByText(/Select at least one timer to continue/i)).toBeNull();
  });

  it('the progress indicator jumps directly to a step', async () => {
    const { session } = makeTestSession({ ...impls(), event: EMPTY_EVENT });
    render(EventSetupWizard, { session, open: true });

    const steps = screen.getByRole('list', { name: 'Setup steps' });
    await fireEvent.click(within(steps).getByRole('button', { name: /First round/ }));
    expect(await screen.findByRole('heading', { name: 'Rounds', level: 3 })).toBeInTheDocument();
  });

  it('Back is disabled on the first step', () => {
    const { session } = makeTestSession({ ...impls(), event: EMPTY_EVENT });
    render(EventSetupWizard, { session, open: true });
    expect((screen.getByRole('button', { name: 'Back' }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('the readiness summary reports every item unmet for a fresh event', async () => {
    const { session } = makeTestSession({ ...impls(), event: EMPTY_EVENT });
    render(EventSetupWizard, { session, open: true });

    // Jump to Review via the progress indicator.
    const steps = screen.getByRole('list', { name: 'Setup steps' });
    await fireEvent.click(within(steps).getByRole('button', { name: /Review/ }));

    const list = await screen.findByLabelText('Ready to race?');
    // None of the four checks is met; the verdict invites finishing anyway.
    const unmet = within(list).getAllByText(/^0 (selected|placed|chosen|defined)$/);
    expect(unmet).toHaveLength(4);
    expect(screen.getByText(/the unmet items are optional/i)).toBeInTheDocument();
  });

  it('the readiness summary reports ready when the event is fully configured', async () => {
    const { session } = makeTestSession({ ...impls(), event: READY_EVENT });
    render(EventSetupWizard, { session, open: true });

    const steps = screen.getByRole('list', { name: 'Setup steps' });
    await fireEvent.click(within(steps).getByRole('button', { name: /Review/ }));

    await waitFor(() =>
      expect(screen.getByText('This event is ready to race.')).toBeInTheDocument()
    );
    expect(screen.getByText('1 selected')).toBeInTheDocument();
    expect(screen.getByText('1 placed')).toBeInTheDocument();
    expect(screen.getByText('1 chosen')).toBeInTheDocument();
    expect(screen.getByText('1 defined')).toBeInTheDocument();
  });

  it('Finish closes the wizard and fires onclose', async () => {
    const { session } = makeTestSession({ ...impls(), event: READY_EVENT });
    const onclose = vi.fn();
    const { rerender } = render(EventSetupWizard, { session, open: true, onclose });

    const steps = screen.getByRole('list', { name: 'Setup steps' });
    await fireEvent.click(within(steps).getByRole('button', { name: /Review/ }));
    await fireEvent.click(screen.getByRole('button', { name: 'Finish setup' }));

    expect(onclose).toHaveBeenCalledOnce();
    // The bound `open` flips false → the overlay unmounts.
    await rerender({ session, open: false, onclose });
    expect(screen.queryByRole('dialog', { name: 'Event setup wizard' })).not.toBeInTheDocument();
  });

  it('renders nothing while closed', () => {
    const { session } = makeTestSession({ ...impls(), event: EMPTY_EVENT });
    render(EventSetupWizard, { session, open: false });
    expect(screen.queryByRole('dialog', { name: 'Event setup wizard' })).not.toBeInTheDocument();
  });
});
