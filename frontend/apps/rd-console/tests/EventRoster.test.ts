import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, EventMeta, LiveRaceState, Pilot } from '@gridfpv/types';
import EventRoster from '../src/screens/EventRoster.svelte';
import { makeTestSession } from './support.js';

const ACE: Pilot = { id: 'p1', callsign: 'Ace', name: 'Alice', vtx_types: [], attributes: {} };
const BEE: Pilot = { id: 'p2', callsign: 'Bee', vtx_types: [], attributes: {} };

const OPEN: Class = { id: 'open', name: 'Open Class', source: 'Custom' };

/** An event that already rosters Ace (so its checkbox seeds checked). */
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: ['p1'],
  classes: []
};

describe('EventRoster — present pilots (in-event roster + inline CRUD)', () => {
  it('seeds the checkboxes from the event roster and shows the count', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session } = makeTestSession({ listPilotsImpl, event: EVENT });
    render(EventRoster, { session });

    const aceBox = (await screen.findByLabelText('Roster Ace')) as HTMLInputElement;
    const beeBox = screen.getByLabelText('Roster Bee') as HTMLInputElement;
    expect(aceBox.checked).toBe(true);
    expect(beeBox.checked).toBe(false);

    // The header count reflects 1 of 2 present.
    expect(screen.getByText(/of 2 pilots present at this event/i)).toBeInTheDocument();
    expect(within(screen.getByText(/present at this event/i)).getByText('1')).toBeInTheDocument();

    // No change yet → Save disabled.
    expect(
      (screen.getByRole('button', { name: 'Save roster' }) as HTMLButtonElement).disabled
    ).toBe(true);
  });

  it('toggling a row saves the working roster via setEventRoster in directory order', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const setEventRosterImpl = vi.fn(async () => ({ ...EVENT, roster: ['p1', 'p2'] }));
    const { session } = makeTestSession({ listPilotsImpl, setEventRosterImpl, event: EVENT });
    render(EventRoster, { session });

    const beeBox = (await screen.findByLabelText('Roster Bee')) as HTMLInputElement;
    await fireEvent.click(beeBox);

    const save = screen.getByRole('button', { name: 'Save roster' }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);

    await waitFor(() => expect(setEventRosterImpl).toHaveBeenCalledTimes(1));
    expect(setEventRosterImpl).toHaveBeenCalledWith('http://d.local', 'e1', ['p1', 'p2'], 'tok');
    // currentEvent re-homes to the server's response, so the saved roster sticks.
    await waitFor(() => expect(session.currentEvent?.roster).toEqual(['p1', 'p2']));
  });

  it('unchecking a rostered pilot saves the smaller roster', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const setEventRosterImpl = vi.fn(async () => ({ ...EVENT, roster: [] }));
    const { session } = makeTestSession({ listPilotsImpl, setEventRosterImpl, event: EVENT });
    render(EventRoster, { session });

    const aceBox = (await screen.findByLabelText('Roster Ace')) as HTMLInputElement;
    await fireEvent.click(aceBox); // remove the only rostered pilot
    await fireEvent.click(screen.getByRole('button', { name: 'Save roster' }));

    await waitFor(() => expect(setEventRosterImpl).toHaveBeenCalledTimes(1));
    expect(setEventRosterImpl).toHaveBeenCalledWith('http://d.local', 'e1', [], 'tok');
  });

  it('an inline-created pilot becomes selectable without leaving the event', async () => {
    const created: Pilot = { id: 'p9', callsign: 'Newbie', vtx_types: [], attributes: {} };
    let calls = 0;
    const listPilotsImpl = vi.fn(async () => (calls++ === 0 ? [ACE] : [ACE, created]));
    const createPilotImpl = vi.fn(async () => created);
    const { session } = makeTestSession({
      listPilotsImpl,
      createPilotImpl,
      event: { ...EVENT, roster: [] }
    });
    render(EventRoster, { session });

    await screen.findByLabelText('Roster Ace');
    await fireEvent.click(screen.getByRole('button', { name: '+ Add pilot' }));

    const callsign = (await screen.findByLabelText('Callsign')) as HTMLInputElement;
    await fireEvent.input(callsign, { target: { value: 'Newbie' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add pilot' }));

    await waitFor(() => expect(createPilotImpl).toHaveBeenCalledTimes(1));
    // The newly-created pilot appears as a fresh, unchecked, selectable row.
    const newBox = (await screen.findByLabelText('Roster Newbie')) as HTMLInputElement;
    expect(newBox.checked).toBe(false);
    await fireEvent.click(newBox);
    expect(newBox.checked).toBe(true);
  });

  it('nudges to add pilots when the directory is empty', async () => {
    const listPilotsImpl = vi.fn(async () => []);
    const { session } = makeTestSession({ listPilotsImpl, event: { ...EVENT, roster: [] } });
    render(EventRoster, { session });
    await screen.findByText(/No pilots in the directory yet/i);
  });
});

describe('EventRoster — per-class membership', () => {
  // An event that runs the Open class and has Ace + Bee present.
  const EV_CLASS: EventMeta = { ...EVENT, roster: ['p1', 'p2'], classes: ['open'] };

  it('nudges to pick classes first when the event has none selected', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session } = makeTestSession({ listPilotsImpl, event: EVENT });
    render(EventRoster, { session });
    await screen.findByText(/no classes selected yet/i);
  });

  it('places a roster pilot into a class and saves via setClassMembership in roster order', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const listClassesImpl = vi.fn(async () => [OPEN]);
    const setClassMembershipImpl = vi.fn(async () => ({
      ...EV_CLASS,
      classes_membership: [{ class: 'open', pilots: [{ pilot: 'p1' }] }]
    }));
    const { session } = makeTestSession({
      listPilotsImpl,
      listClassesImpl,
      setClassMembershipImpl,
      event: EV_CLASS
    });
    render(EventRoster, { session });

    // The class section resolves the id → name and lists the roster pilots as checkboxes.
    const aceInOpen = (await screen.findByLabelText('Place Ace in Open Class')) as HTMLInputElement;
    expect(aceInOpen.checked).toBe(false);
    await fireEvent.click(aceInOpen);

    const save = screen.getByRole('button', { name: 'Save membership' }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);

    await waitFor(() => expect(setClassMembershipImpl).toHaveBeenCalledTimes(1));
    expect(setClassMembershipImpl).toHaveBeenCalledWith(
      'http://d.local',
      'e1',
      'open',
      ['p1'],
      'tok'
    );
    // currentEvent re-homes; the membership sticks (the box stays checked, Save goes disabled).
    await waitFor(() =>
      expect((screen.getByLabelText('Place Ace in Open Class') as HTMLInputElement).checked).toBe(
        true
      )
    );
    await waitFor(() =>
      expect(
        (screen.getByRole('button', { name: 'Save membership' }) as HTMLButtonElement).disabled
      ).toBe(true)
    );
  });

  it('seeds the class checkboxes from the saved membership', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const listClassesImpl = vi.fn(async () => [OPEN]);
    const { session } = makeTestSession({
      listPilotsImpl,
      listClassesImpl,
      event: { ...EV_CLASS, classes_membership: [{ class: 'open', pilots: [{ pilot: 'p2' }] }] }
    });
    render(EventRoster, { session });

    const beeInOpen = (await screen.findByLabelText('Place Bee in Open Class')) as HTMLInputElement;
    expect(beeInOpen.checked).toBe(true);
    expect((screen.getByLabelText('Place Ace in Open Class') as HTMLInputElement).checked).toBe(
      false
    );
  });
});

describe('EventRoster — binding', () => {
  const EV_PRESENT: EventMeta = { ...EVENT, roster: ['p1', 'p2'], classes: [] };

  it('shows a present pilot as unbound, then bound from the live heat registrations', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    // Ace is bound to competitor "node-1" in the current heat; Bee is not.
    const live: LiveRaceState = {
      phase: 'Running',
      progress: [{ competitor: 'node-1', pilot: 'p1', laps_completed: 0 }]
    };
    const { session } = makeTestSession({ listPilotsImpl, live, event: EV_PRESENT });
    render(EventRoster, { session });

    const bindList = await screen.findByRole('list', { name: 'Pilot bindings' });
    expect(within(bindList).getByText(/bound → node-1/i)).toBeInTheDocument();
    expect(within(bindList).getByText(/^unbound$/i)).toBeInTheDocument();
  });

  it('binds a competitor to a pilot via Register through the control path', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session, sendSpy } = makeTestSession({ listPilotsImpl, event: EV_PRESENT });
    render(EventRoster, { session });

    const adapter = (await screen.findByLabelText('Bind adapter')) as HTMLInputElement;
    const competitor = screen.getByLabelText('Bind competitor') as HTMLInputElement;
    const pilot = screen.getByLabelText('Bind pilot') as HTMLSelectElement;

    await fireEvent.input(adapter, { target: { value: 'rh-1' } });
    await fireEvent.input(competitor, { target: { value: 'node-3' } });
    await fireEvent.change(pilot, { target: { value: 'p2' } });

    const bindBtn = screen.getByRole('button', { name: 'Bind' }) as HTMLButtonElement;
    expect(bindBtn.disabled).toBe(false);
    await fireEvent.click(bindBtn);

    await waitFor(() => expect(sendSpy).toHaveBeenCalledTimes(1));
    expect(sendSpy).toHaveBeenCalledWith({
      Register: { adapter: 'rh-1', competitor: 'node-3', pilot: 'p2' }
    });
  });

  it('keeps Bind disabled until adapter, competitor, and pilot are all set', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session } = makeTestSession({ listPilotsImpl, event: EV_PRESENT });
    render(EventRoster, { session });

    // The adapter defaults to "sim", so only competitor + pilot are missing.
    const bindBtn = (await screen.findByRole('button', { name: 'Bind' })) as HTMLButtonElement;
    expect(bindBtn.disabled).toBe(true);
    await fireEvent.input(screen.getByLabelText('Bind competitor'), {
      target: { value: 'node-3' }
    });
    expect(bindBtn.disabled).toBe(true); // still no pilot
    await fireEvent.change(screen.getByLabelText('Bind pilot'), { target: { value: 'p1' } });
    expect(bindBtn.disabled).toBe(false);
  });
});
