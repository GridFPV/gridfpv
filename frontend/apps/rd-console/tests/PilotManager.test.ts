import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Pilot } from '@gridfpv/types';
import PilotManager from '../src/screens/PilotManager.svelte';
import { makeTestSession } from './support.js';

const ACE: Pilot = {
  id: 'p1',
  callsign: 'Ace',
  name: 'Alice',
  color: '#ff0000',
  country: 'US',
  vtx_types: ['Analog', 'HDZero'],
  attributes: { bib: '7' }
};
const BEE: Pilot = { id: 'p2', callsign: 'Bee', vtx_types: [], attributes: {} };

/** Open the Add form via the manager's exported `openAdd()` (the page button calls it). */
async function openAdd(manager: { openAdd: () => void }) {
  manager.openAdd();
  await screen.findByRole('form', { name: 'Add pilot' });
}

describe('PilotManager (#74)', () => {
  it('lists the directory with callsign, name, and per-row controls', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotManager, { session });

    await screen.findByText('Ace');
    expect(screen.getByText('Alice')).toBeInTheDocument();
    const list = screen.getByRole('list', { name: 'Pilot directory' });
    expect(within(list).getAllByRole('listitem')).toHaveLength(2);
  });

  it('renders every selected VTX type as a badge (and none when the set is empty)', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotManager, { session });

    await screen.findByText('Ace');
    const list = screen.getByRole('list', { name: 'Pilot directory' });
    const [aceRow, beeRow] = within(list).getAllByRole('listitem');
    // Ace has two VTX types → two badges.
    expect(within(aceRow).getByText('Analog')).toBeInTheDocument();
    expect(within(aceRow).getByText('HDZero')).toBeInTheDocument();
    // Bee has none → no VTX badges at all.
    expect(within(beeRow).queryByText('Analog')).not.toBeInTheDocument();
    expect(within(beeRow).queryByText('HDZero')).not.toBeInTheDocument();
    expect(within(beeRow).queryByText('DJI')).not.toBeInTheDocument();
  });

  it('toggles VTX chips and sends the selected set in the create body', async () => {
    let calls = 0;
    const created: Pilot = {
      id: 'p9',
      callsign: 'Neo',
      vtx_types: ['Analog', 'DJI'],
      attributes: {}
    };
    const listPilotsImpl = vi.fn(async () => (calls++ === 0 ? [] : [created]));
    const createPilotImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl, createPilotImpl });
    const { component } = render(PilotManager, { session });
    await screen.findByText(/No pilots/i);
    await openAdd(component as unknown as { openAdd: () => void });

    await fireEvent.input(screen.getByLabelText('Callsign'), { target: { value: 'Neo' } });

    const vtxGroup = screen.getByRole('group', { name: 'VTX types' });
    // Tick DJI then Analog (out of canonical order) — and tick HDZero then untick it.
    await fireEvent.click(within(vtxGroup).getByRole('button', { name: 'DJI' }));
    await fireEvent.click(within(vtxGroup).getByRole('button', { name: 'Analog' }));
    await fireEvent.click(within(vtxGroup).getByRole('button', { name: 'HDZero' }));
    await fireEvent.click(within(vtxGroup).getByRole('button', { name: 'HDZero' })); // toggled back off
    expect(within(vtxGroup).getByRole('button', { name: 'DJI' })).toHaveAttribute(
      'aria-pressed',
      'true'
    );
    expect(within(vtxGroup).getByRole('button', { name: 'HDZero' })).toHaveAttribute(
      'aria-pressed',
      'false'
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Add pilot' }));
    await waitFor(() => expect(createPilotImpl).toHaveBeenCalledTimes(1));
    expect(createPilotImpl).toHaveBeenCalledWith(
      'http://d.local',
      expect.objectContaining({ callsign: 'Neo', vtx_types: ['Analog', 'DJI'] }),
      'tok'
    );
  });

  it('shows the empty state when the directory has no pilots', async () => {
    const listPilotsImpl = vi.fn(async () => []);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl });
    render(PilotManager, { session });
    await screen.findByText(/No pilots in the directory yet/i);
  });

  it('requires a non-empty callsign before it will create', async () => {
    const listPilotsImpl = vi.fn(async () => []);
    const createPilotImpl = vi.fn(async () => ACE);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl, createPilotImpl });
    const { component } = render(PilotManager, { session });
    await screen.findByText(/No pilots/i);

    await openAdd(component as unknown as { openAdd: () => void });
    // The submit button is disabled while the callsign is blank.
    const submit = screen.getByRole('button', { name: 'Add pilot' });
    expect(submit).toBeDisabled();
    expect(createPilotImpl).not.toHaveBeenCalled();
  });

  it('creates a pilot with callsign + name + country + color + a custom attribute', async () => {
    let calls = 0;
    const created: Pilot = {
      id: 'p9',
      callsign: 'Neo',
      name: 'Thomas',
      country: 'US',
      color: '#00ff00',
      vtx_types: [],
      attributes: { sponsor: 'Acme' }
    };
    const listPilotsImpl = vi.fn(async () => (calls++ === 0 ? [] : [created]));
    const createPilotImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl, createPilotImpl });
    const { component } = render(PilotManager, { session });
    await screen.findByText(/No pilots/i);
    await openAdd(component as unknown as { openAdd: () => void });

    await fireEvent.input(screen.getByLabelText('Callsign'), { target: { value: 'Neo' } });
    await fireEvent.input(screen.getByLabelText('Real name'), { target: { value: 'Thomas' } });
    await fireEvent.input(screen.getByLabelText('Color'), { target: { value: '#00ff00' } });

    // Add a custom attribute row, fill it.
    await fireEvent.click(screen.getByRole('button', { name: '+ Add attribute' }));
    await fireEvent.input(screen.getByLabelText('Attribute 1 key'), {
      target: { value: 'sponsor' }
    });
    await fireEvent.input(screen.getByLabelText('Attribute 1 value'), {
      target: { value: 'Acme' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add pilot' }));

    await waitFor(() => expect(createPilotImpl).toHaveBeenCalledTimes(1));
    expect(createPilotImpl).toHaveBeenCalledWith(
      'http://d.local',
      expect.objectContaining({
        callsign: 'Neo',
        name: 'Thomas',
        color: '#00ff00',
        attributes: { sponsor: 'Acme' }
      }),
      'tok'
    );
    await screen.findByText('Neo');
  });

  it('removes an attribute row so it is omitted from the create body', async () => {
    const listPilotsImpl = vi.fn(async () => []);
    const createPilotImpl = vi.fn(async () => ACE);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl, createPilotImpl });
    const { component } = render(PilotManager, { session });
    await screen.findByText(/No pilots/i);
    await openAdd(component as unknown as { openAdd: () => void });

    await fireEvent.input(screen.getByLabelText('Callsign'), { target: { value: 'Ace' } });
    await fireEvent.click(screen.getByRole('button', { name: '+ Add attribute' }));
    await fireEvent.input(screen.getByLabelText('Attribute 1 key'), { target: { value: 'bib' } });
    await fireEvent.input(screen.getByLabelText('Attribute 1 value'), { target: { value: '7' } });
    // Remove it again.
    await fireEvent.click(screen.getByRole('button', { name: 'Remove attribute 1' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Add pilot' }));

    await waitFor(() => expect(createPilotImpl).toHaveBeenCalledTimes(1));
    expect(createPilotImpl).toHaveBeenCalledWith(
      'http://d.local',
      expect.objectContaining({ callsign: 'Ace', attributes: {} }),
      'tok'
    );
  });

  it('edits a pilot and CLEARS color + country, sending null for each', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE]);
    const updatePilotImpl = vi.fn(async () => ({ ...ACE, color: undefined, country: undefined }));
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl, updatePilotImpl });
    render(PilotManager, { session });

    await screen.findByText('Ace');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    await screen.findByRole('form', { name: 'Edit pilot' });

    // Clear color via its Clear button, clear country via the CountrySelect clear control.
    await fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Clear country' }));

    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => expect(updatePilotImpl).toHaveBeenCalledTimes(1));
    // The cleared fields go as explicit null; unchanged fields are omitted.
    expect(updatePilotImpl).toHaveBeenCalledWith(
      'http://d.local',
      'p1',
      { color: null, country: null },
      'tok'
    );
  });

  it('removes a pilot after a confirm step', async () => {
    const listPilotsImpl = vi.fn(async () => [ACE, BEE]);
    const deletePilotImpl = vi.fn(async () => undefined as unknown as void);
    const { session } = makeTestSession({ noEnter: true, listPilotsImpl, deletePilotImpl });
    render(PilotManager, { session });

    await screen.findByText('Ace');
    const list = screen.getByRole('list', { name: 'Pilot directory' });
    const aceRow = within(list).getAllByRole('listitem')[0];
    await fireEvent.click(within(aceRow).getByRole('button', { name: 'Remove' }));

    // A confirm dialog appears; confirm it.
    await screen.findByText(/Remove pilot/i);
    const dialogs = screen.getAllByRole('button', { name: 'Remove' });
    // The confirm dialog's Remove is the last-rendered one.
    await fireEvent.click(dialogs[dialogs.length - 1]);

    await waitFor(() => expect(deletePilotImpl).toHaveBeenCalledTimes(1));
    expect(deletePilotImpl).toHaveBeenCalledWith('http://d.local', 'p1', 'tok');
  });
});
