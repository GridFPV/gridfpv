import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class } from '@gridfpv/types';
import ClassManager from '../src/screens/ClassManager.svelte';
import { makeTestSession } from './support.js';

/** A locked, fixed-id built-in (read-only: no Edit/Remove, shows a lock + org badge). */
const OPEN: Class = {
  id: 'mgp-open',
  name: 'Open Class',
  source: 'MultiGP',
  reference: 'https://www.multigp.com/class-specifications/',
  description: 'Unlimited.',
  builtin: true
};
/** A user-created Custom class (full CRUD). */
const HOUSE: Class = { id: 'c2', name: 'House', source: 'Custom' };

/** Open the Add form via the manager's exported `openAdd()` (the page button calls it). */
async function openAdd(manager: { openAdd: () => void }) {
  manager.openAdd();
  await screen.findByRole('form', { name: 'Add class' });
}

describe('ClassManager (#84)', () => {
  it('lists built-ins (locked, org badge, no Edit/Remove) and Custom rows (full controls)', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, { session });

    await screen.findByText('Open Class');
    const list = screen.getByRole('list', { name: 'Class directory' });
    const rows = within(list).getAllByRole('listitem');
    expect(rows).toHaveLength(2);

    // The built-in shows its MultiGP org badge + a reference link + a built-in/lock indicator, and
    // has NO Edit/Remove (it is read-only).
    expect(within(rows[0]).getByText('MultiGP')).toBeInTheDocument();
    expect(
      within(rows[0]).getByRole('link', { name: 'Reference for Open Class' })
    ).toBeInTheDocument();
    expect(within(rows[0]).getByLabelText('Built-in')).toBeInTheDocument();
    expect(within(rows[0]).queryByRole('button', { name: 'Edit' })).not.toBeInTheDocument();
    expect(within(rows[0]).queryByRole('button', { name: 'Remove' })).not.toBeInTheDocument();

    // The Custom row shows its badge and keeps full Edit/Remove controls.
    expect(within(rows[1]).getByText('Custom')).toBeInTheDocument();
    expect(within(rows[1]).queryByLabelText('Built-in')).not.toBeInTheDocument();
    expect(within(rows[1]).getByRole('button', { name: 'Edit' })).toBeInTheDocument();
    expect(within(rows[1]).getByRole('button', { name: 'Remove' })).toBeInTheDocument();
  });

  it('shows the empty state when the directory has no classes', async () => {
    const listClassesImpl = vi.fn(async () => []);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, { session });
    await screen.findByText(/No classes in the directory yet/i);
  });

  it('requires a non-empty name before it will create', async () => {
    const listClassesImpl = vi.fn(async () => []);
    const createClassImpl = vi.fn(async () => OPEN);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, createClassImpl });
    const { component } = render(ClassManager, { session });
    await screen.findByText(/No classes/i);

    await openAdd(component as unknown as { openAdd: () => void });
    const submit = screen.getByRole('button', { name: 'Add class' });
    expect(submit).toBeDisabled();
    expect(createClassImpl).not.toHaveBeenCalled();
  });

  it('creates a Custom class (no source picker — always Custom) with name + reference + description', async () => {
    let calls = 0;
    const created: Class = {
      id: 'c9',
      name: 'Spec',
      source: 'Custom',
      reference: 'ref-1',
      description: 'A spec class.'
    };
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [] : [created]));
    const createClassImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, createClassImpl });
    const { component } = render(ClassManager, { session });
    await screen.findByText(/No classes/i);
    await openAdd(component as unknown as { openAdd: () => void });

    // The add form offers no org source picker — new classes are always Custom.
    expect(screen.queryByLabelText('Source')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Add from MultiGP')).not.toBeInTheDocument();

    await fireEvent.input(screen.getByLabelText('Name'), { target: { value: 'Spec' } });
    await fireEvent.input(screen.getByLabelText('Reference'), { target: { value: 'ref-1' } });
    await fireEvent.input(screen.getByLabelText('Description'), {
      target: { value: 'A spec class.' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));

    await waitFor(() => expect(createClassImpl).toHaveBeenCalledTimes(1));
    expect(createClassImpl).toHaveBeenCalledWith(
      'http://d.local',
      { name: 'Spec', source: 'Custom', reference: 'ref-1', description: 'A spec class.' },
      'tok'
    );
    await screen.findByText('Spec');
  });

  it('edits a Custom class and CLEARS reference + description, sending null for each', async () => {
    const CUSTOM: Class = {
      id: 'c1',
      name: 'House',
      source: 'Custom',
      reference: 'ref',
      description: 'notes'
    };
    const listClassesImpl = vi.fn(async () => [CUSTOM]);
    const updateClassImpl = vi.fn(async () => ({
      ...CUSTOM,
      reference: undefined,
      description: undefined
    }));
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, updateClassImpl });
    render(ClassManager, { session });

    await screen.findByText('House');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    await screen.findByRole('form', { name: 'Edit class' });

    // Empty the reference + description fields → they clear via null.
    await fireEvent.input(screen.getByLabelText('Reference'), { target: { value: '' } });
    await fireEvent.input(screen.getByLabelText('Description'), { target: { value: '' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => expect(updateClassImpl).toHaveBeenCalledTimes(1));
    expect(updateClassImpl).toHaveBeenCalledWith(
      'http://d.local',
      'c1',
      { reference: null, description: null },
      'tok'
    );
  });

  it('removes a Custom class after a confirm step', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const deleteClassImpl = vi.fn(async () => undefined as unknown as void);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, deleteClassImpl });
    render(ClassManager, { session });

    await screen.findByText('House');
    const list = screen.getByRole('list', { name: 'Class directory' });
    // The Custom row (index 1) is the removable one; the built-in (index 0) has no Remove.
    const houseRow = within(list).getAllByRole('listitem')[1];
    await fireEvent.click(within(houseRow).getByRole('button', { name: 'Remove' }));

    await screen.findByText(/Remove class/i);
    const dialogs = screen.getAllByRole('button', { name: 'Remove' });
    await fireEvent.click(dialogs[dialogs.length - 1]);

    await waitFor(() => expect(deleteClassImpl).toHaveBeenCalledTimes(1));
    expect(deleteClassImpl).toHaveBeenCalledWith('http://d.local', 'c2', 'tok');
  });
});
