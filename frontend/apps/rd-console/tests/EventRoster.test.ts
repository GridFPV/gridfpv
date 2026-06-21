import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta, Pilot } from '@gridfpv/types';
import EventRoster from '../src/screens/EventRoster.svelte';
import { makeTestSession } from './support.js';

const ACE: Pilot = { id: 'p1', callsign: 'Ace', name: 'Alice', vtx_types: [], attributes: {} };
const BEE: Pilot = { id: 'p2', callsign: 'Bee', vtx_types: [], attributes: {} };

/** An event that already rosters Ace (so its checkbox seeds checked). */
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: ['p1']
};

describe('EventRoster (in-event roster + inline CRUD)', () => {
  it('seeds the checkboxes from the event roster and shows the count', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session } = makeTestSession({ listPilotsImpl, event: EVENT });
    render(EventRoster, { session });

    const aceBox = (await screen.findByLabelText('Roster Ace')) as HTMLInputElement;
    const beeBox = screen.getByLabelText('Roster Bee') as HTMLInputElement;
    expect(aceBox.checked).toBe(true);
    expect(beeBox.checked).toBe(false);

    // The header count reflects 1 of 2 rostered.
    expect(screen.getByText(/of 2 pilots rostered for this event/i)).toBeInTheDocument();
    expect(within(screen.getByText(/rostered for this event/i)).getByText('1')).toBeInTheDocument();

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
