import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class } from '@gridfpv/types';
import ClassManager from '../src/screens/ClassManager.svelte';
import { makeTestSession } from './support.js';

const OPEN: Class = {
  id: 'c1',
  name: 'Open',
  source: 'MultiGP',
  reference: 'https://multigp.com/open',
  description: 'Unlimited.'
};
const HOUSE: Class = { id: 'c2', name: 'House', source: 'Custom' };

/** Open the Add form via the manager's exported `openAdd()` (the page button calls it). */
async function openAdd(manager: { openAdd: () => void }) {
  manager.openAdd();
  await screen.findByRole('form', { name: 'Add class' });
}

describe('ClassManager (#84)', () => {
  it('lists the directory with name, a source badge, reference link, and per-row controls', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, { session });

    await screen.findByText('Open');
    const list = screen.getByRole('list', { name: 'Class directory' });
    const rows = within(list).getAllByRole('listitem');
    expect(rows).toHaveLength(2);
    // Open shows its MultiGP source badge + a reference link; House has neither extra.
    expect(within(rows[0]).getByText('MultiGP')).toBeInTheDocument();
    expect(within(rows[0]).getByRole('link', { name: 'Reference for Open' })).toBeInTheDocument();
    expect(within(rows[1]).getByText('Custom')).toBeInTheDocument();
    expect(within(rows[1]).queryByRole('link')).not.toBeInTheDocument();
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

  it('creates a class with name + source + reference + description', async () => {
    let calls = 0;
    const created: Class = {
      id: 'c9',
      name: 'Spec',
      source: 'Other',
      reference: 'ref-1',
      description: 'A spec class.'
    };
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [] : [created]));
    const createClassImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, createClassImpl });
    const { component } = render(ClassManager, { session });
    await screen.findByText(/No classes/i);
    await openAdd(component as unknown as { openAdd: () => void });

    await fireEvent.input(screen.getByLabelText('Name'), { target: { value: 'Spec' } });
    await fireEvent.change(screen.getByLabelText('Source'), { target: { value: 'Other' } });
    await fireEvent.input(screen.getByLabelText('Reference'), { target: { value: 'ref-1' } });
    await fireEvent.input(screen.getByLabelText('Description'), {
      target: { value: 'A spec class.' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));

    await waitFor(() => expect(createClassImpl).toHaveBeenCalledTimes(1));
    expect(createClassImpl).toHaveBeenCalledWith(
      'http://d.local',
      { name: 'Spec', source: 'Other', reference: 'ref-1', description: 'A spec class.' },
      'tok'
    );
    await screen.findByText('Spec');
  });

  it('pre-fills the form from the MultiGP quick-pick (source=MultiGP + reference)', async () => {
    let calls = 0;
    const created: Class = { id: 'c9', name: 'Tiny Whoop', source: 'MultiGP', reference: 'url' };
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [] : [created]));
    const createClassImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, createClassImpl });
    const { component } = render(ClassManager, { session });
    await screen.findByText(/No classes/i);
    await openAdd(component as unknown as { openAdd: () => void });

    // Pick a MultiGP class from the quick-pick select; the form fields fill in.
    await fireEvent.change(screen.getByLabelText('Add from MultiGP'), {
      target: { value: 'Tiny Whoop' }
    });
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('Tiny Whoop');
    expect((screen.getByLabelText('Source') as HTMLSelectElement).value).toBe('MultiGP');
    expect((screen.getByLabelText('Reference') as HTMLInputElement).value).toMatch(/^https?:\/\//);

    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));
    await waitFor(() => expect(createClassImpl).toHaveBeenCalledTimes(1));
    expect(createClassImpl).toHaveBeenCalledWith(
      'http://d.local',
      expect.objectContaining({ name: 'Tiny Whoop', source: 'MultiGP' }),
      'tok'
    );
  });

  it('edits a class and CLEARS reference + description, sending null for each', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN]);
    const updateClassImpl = vi.fn(async () => ({
      ...OPEN,
      reference: undefined,
      description: undefined
    }));
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, updateClassImpl });
    render(ClassManager, { session });

    await screen.findByText('Open');
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

  it('removes a class after a confirm step', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const deleteClassImpl = vi.fn(async () => undefined as unknown as void);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, deleteClassImpl });
    render(ClassManager, { session });

    await screen.findByText('Open');
    const list = screen.getByRole('list', { name: 'Class directory' });
    const openRow = within(list).getAllByRole('listitem')[0];
    await fireEvent.click(within(openRow).getByRole('button', { name: 'Remove' }));

    await screen.findByText(/Remove class/i);
    const dialogs = screen.getAllByRole('button', { name: 'Remove' });
    await fireEvent.click(dialogs[dialogs.length - 1]);

    await waitFor(() => expect(deleteClassImpl).toHaveBeenCalledTimes(1));
    expect(deleteClassImpl).toHaveBeenCalledWith('http://d.local', 'c1', 'tok');
  });
});
