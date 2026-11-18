import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class } from '@gridfpv/types';
import ClassManager from '../src/screens/ClassManager.svelte';
import { makeTestSession } from './support.js';

/** A class that came with Grid: a fixed id and an org tag, and otherwise like any other. */
const OPEN: Class = {
  id: 'mgp-open',
  name: 'Open Class',
  org: 'MultiGP',
  reference: 'https://www.multigp.com/class-specifications/',
  description: 'Unlimited.',
  builtin: true
};
/** A user-created Custom class (full CRUD). */
const HOUSE: Class = { id: 'c2', name: 'House' };

/** Open the Add form via the manager's exported `openAdd()` (the page button calls it). */
async function openAdd(manager: { openAdd: () => void }) {
  manager.openAdd();
  await screen.findByRole('form', { name: 'Add class' });
}

describe('ClassManager (#84)', () => {
  it('lists every class the same way: Edit and Remove on each, a tag only where there is an org', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, { session });

    await screen.findByText('Open Class');
    const list = screen.getByRole('list', { name: 'Class directory' });
    const rows = within(list).getAllByRole('listitem');
    expect(rows).toHaveLength(2);

    // The class that came with Grid shows its MultiGP org tag and a reference link, and
    // the same Edit and Remove as any other.
    expect(within(rows[0]).getByText('MultiGP')).toBeInTheDocument();
    expect(
      within(rows[0]).getByRole('link', { name: 'Reference for Open Class' })
    ).toBeInTheDocument();
    // The org badge says where it is from. There is no lock or "Built-in" label beside it.
    expect(within(rows[0]).queryByLabelText('Built-in')).not.toBeInTheDocument();
    expect(rows[0]).not.toHaveTextContent('Built-in');
    expect(within(rows[0]).getByRole('button', { name: 'Edit' })).toBeInTheDocument();
    expect(within(rows[0]).getByRole('button', { name: 'Remove' })).toBeInTheDocument();

    // A class with no org has no tag at all, and keeps full Edit/Remove controls.
    expect(within(rows[1]).queryByTestId('class-org')).toBeNull();
    expect(within(rows[1]).queryByText('Custom')).toBeNull();
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

  it('creates a class with a name, an org typed by the director, a reference and a description', async () => {
    let calls = 0;
    const created: Class = {
      id: 'c9',
      name: 'Spec',
      org: 'Tri-County FPV',
      reference: 'ref-1',
      description: 'A spec class.'
    };
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [] : [created]));
    const createClassImpl = vi.fn(async () => created);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, createClassImpl });
    const { component } = render(ClassManager, { session });
    await screen.findByText(/No classes/i);
    await openAdd(component as unknown as { openAdd: () => void });

    // The org is typed, not picked from a list, and nothing says a new class is "Custom".
    expect(screen.queryByLabelText('Source')).not.toBeInTheDocument();
    expect(screen.getByRole('form', { name: 'Add class' })).not.toHaveTextContent('Custom');
    await fireEvent.input(screen.getByLabelText('Org'), { target: { value: ' Tri-County FPV ' } });

    await fireEvent.input(screen.getByLabelText('Name'), { target: { value: 'Spec' } });
    await fireEvent.input(screen.getByLabelText('Reference'), { target: { value: 'ref-1' } });
    await fireEvent.input(screen.getByLabelText('Description'), {
      target: { value: 'A spec class.' }
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));

    await waitFor(() => expect(createClassImpl).toHaveBeenCalledTimes(1));
    expect(createClassImpl).toHaveBeenCalledWith(
      'http://d.local',
      { name: 'Spec', org: 'Tri-County FPV', reference: 'ref-1', description: 'A spec class.' },
      'tok'
    );
    await screen.findByText('Spec');
    // The org typed is the tag on the row.
    expect(screen.getByTestId('class-org')).toHaveTextContent('Tri-County FPV');
  });

  it('edits a Custom class and CLEARS reference + description, sending null for each', async () => {
    const CUSTOM: Class = {
      id: 'c1',
      name: 'House',
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
    // The director's own class is the second row.
    const houseRow = within(list).getAllByRole('listitem')[1];
    await fireEvent.click(within(houseRow).getByRole('button', { name: 'Remove' }));

    await screen.findByText(/Remove class/i);
    const dialogs = screen.getAllByRole('button', { name: 'Remove' });
    await fireEvent.click(dialogs[dialogs.length - 1]);

    await waitFor(() => expect(deleteClassImpl).toHaveBeenCalledTimes(1));
    expect(deleteClassImpl).toHaveBeenCalledWith('http://d.local', 'c2', 'tok');
  });
});

describe('ClassManager — hide / archive (manageHidden)', () => {
  it('offers Hide on every row (built-in too) and hides via setClassHidden', async () => {
    let calls = 0;
    const listClassesImpl = vi.fn(async () =>
      calls++ === 0 ? [OPEN, HOUSE] : [{ ...OPEN, hidden: true }, HOUSE]
    );
    const setClassHiddenImpl = vi.fn(async () => ({ ...OPEN, hidden: true }));
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, setClassHiddenImpl });
    render(ClassManager, { session, manageHidden: true });

    await screen.findByText('Open Class');
    const list = screen.getByRole('list', { name: 'Class directory' });
    const builtinRow = within(list).getAllByRole('listitem')[0];
    await fireEvent.click(within(builtinRow).getByRole('button', { name: 'Hide' }));

    await waitFor(() => expect(setClassHiddenImpl).toHaveBeenCalledTimes(1));
    expect(setClassHiddenImpl).toHaveBeenCalledWith('http://d.local', 'mgp-open', true, 'tok');

    // After the reload the built-in is marked Hidden and now offers Unhide.
    await waitFor(() => {
      const row = within(screen.getByRole('list', { name: 'Class directory' })).getAllByRole(
        'listitem'
      )[0];
      expect(within(row).getByText('Hidden')).toBeInTheDocument();
      expect(within(row).getByRole('button', { name: 'Unhide' })).toBeInTheDocument();
    });
  });

  it('does not show Hide controls when manageHidden is off (default)', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, { session });
    await screen.findByText('Open Class');
    expect(screen.queryByRole('button', { name: 'Hide' })).not.toBeInTheDocument();
  });
});

describe('ClassManager — the hidden filter (filterHidden)', () => {
  const HIDDEN: Class = { id: 'c3', name: 'Archived', hidden: true };

  it('filters hidden classes out of the offered list', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE, HIDDEN]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, { session, filterHidden: true });

    await screen.findByText('Open Class');
    expect(screen.getByText('House')).toBeInTheDocument();
    // The hidden class is excluded from the list.
    expect(screen.queryByText('Archived')).not.toBeInTheDocument();
  });

  it('keeps a hidden class that is already selected (keepRow), so it never silently vanishes', async () => {
    const listClassesImpl = vi.fn(async () => [HOUSE, HIDDEN]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    render(ClassManager, {
      session,
      filterHidden: true,
      keepRow: (c: Class) => c.id === 'c3'
    });

    await screen.findByText('House');
    // The already-selected hidden class is still offered (kept), even though it is hidden.
    expect(screen.getByText('Archived')).toBeInTheDocument();
  });
});

describe('ClassManager — the classes that came with Grid', () => {
  it('a class that came with Grid is removed after a confirm that says it can be put back', async () => {
    let calls = 0;
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [OPEN, HOUSE] : [HOUSE]));
    const deleteClassImpl = vi.fn(async () => undefined as unknown as void);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, deleteClassImpl });
    const removed = vi.spyOn(session, 'removedBuiltinClasses');
    removed.mockResolvedValueOnce(0).mockResolvedValue(1);
    render(ClassManager, { session, manageHidden: true });

    await screen.findByText('Open Class');
    expect(screen.queryByTestId('removed-builtins')).toBeNull();
    const builtinRow = within(screen.getByRole('list', { name: 'Class directory' })).getAllByRole(
      'listitem'
    )[0];
    await fireEvent.click(within(builtinRow).getByRole('button', { name: 'Remove' }));

    // Nothing is removed until the director says so.
    await screen.findByText(/It came with Grid\. You can put those classes back at any time/);
    expect(deleteClassImpl).not.toHaveBeenCalled();
    const buttons = screen.getAllByRole('button', { name: 'Remove' });
    await fireEvent.click(buttons[buttons.length - 1]);
    await waitFor(() =>
      expect(deleteClassImpl).toHaveBeenCalledWith('http://d.local', 'mgp-open', 'tok')
    );

    // It is gone from the list, and the page offers to put it back.
    await waitFor(() => expect(screen.queryByText('Open Class')).toBeNull());
    expect(await screen.findByTestId('removed-builtins')).toHaveTextContent(
      '1 class that came with Grid has been removed.'
    );
  });

  it('a class of the director’s own gets no word about built-ins when it is removed', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    vi.spyOn(session, 'removedBuiltinClasses').mockResolvedValue(0);
    render(ClassManager, { session, manageHidden: true });
    await screen.findByText('House');
    const houseRow = within(screen.getByRole('list', { name: 'Class directory' })).getAllByRole(
      'listitem'
    )[1];
    await fireEvent.click(within(houseRow).getByRole('button', { name: 'Remove' }));
    await screen.findByText(/Remove class/i);
    expect(screen.queryByText(/It came with Grid/)).toBeNull();
  });

  it('puts the built-in classes back, and the offer goes away', async () => {
    let calls = 0;
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [HOUSE] : [OPEN, HOUSE]));
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    const removed = vi.spyOn(session, 'removedBuiltinClasses');
    removed.mockResolvedValueOnce(3).mockResolvedValue(0);
    const restore = vi.spyOn(session, 'restoreBuiltinClasses').mockResolvedValue([OPEN, HOUSE]);
    render(ClassManager, { session, manageHidden: true });

    expect(await screen.findByTestId('removed-builtins')).toHaveTextContent(
      '3 classes that came with Grid have been removed.'
    );
    expect(screen.queryByText('Open Class')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Put them back' }));
    await waitFor(() => expect(restore).toHaveBeenCalledOnce());
    expect(await screen.findByText('Open Class')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByTestId('removed-builtins')).toBeNull());
  });

  it('a cancelled token prompt puts nothing back and keeps the offer', async () => {
    const listClassesImpl = vi.fn(async () => [HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    vi.spyOn(session, 'removedBuiltinClasses').mockResolvedValue(2);
    const restore = vi.spyOn(session, 'restoreBuiltinClasses').mockResolvedValue(undefined);
    render(ClassManager, { session, manageHidden: true });
    await fireEvent.click(await screen.findByRole('button', { name: 'Put them back' }));
    await waitFor(() => expect(restore).toHaveBeenCalledOnce());
    expect(screen.getByTestId('removed-builtins')).toBeInTheDocument();
    expect(listClassesImpl).toHaveBeenCalledOnce();
  });

  it('where classes are only picked, nothing is said of removed ones', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl });
    const removed = vi.spyOn(session, 'removedBuiltinClasses').mockResolvedValue(4);
    render(ClassManager, { session });
    await screen.findByText('Open Class');
    expect(screen.queryByTestId('removed-builtins')).toBeNull();
    expect(removed).not.toHaveBeenCalled();
  });

  it('a class that came with Grid opens for editing like any other, and the change is sent', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const updateClassImpl = vi.fn(async () => ({ ...OPEN, name: 'Open 5 inch' }));
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, updateClassImpl });
    render(ClassManager, { session, manageHidden: true });
    await screen.findByText('Open Class');
    const row = within(screen.getByRole('list', { name: 'Class directory' })).getAllByRole(
      'listitem'
    )[0];
    await fireEvent.click(within(row).getByRole('button', { name: 'Edit' }));
    const form = await screen.findByRole('form', { name: 'Edit class' });
    // Everything about it is there to change: its name, its org, its reference, its description.
    expect(within(form).getByLabelText('Name')).toHaveValue('Open Class');
    expect(within(form).getByLabelText('Org')).toHaveValue('MultiGP');
    expect(within(form).getByLabelText('Reference')).toHaveValue(
      'https://www.multigp.com/class-specifications/'
    );
    await fireEvent.input(within(form).getByLabelText('Name'), {
      target: { value: 'Open 5 inch' }
    });
    await fireEvent.input(within(form).getByLabelText('Org'), { target: { value: 'My Club' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() =>
      expect(updateClassImpl).toHaveBeenCalledWith(
        'http://d.local',
        'mgp-open',
        { name: 'Open 5 inch', org: 'My Club' },
        'tok'
      )
    );
  });
});

describe('ClassManager — the org tag', () => {
  it('refuses an org too long to be a tag before sending anything', async () => {
    const listClassesImpl = vi.fn(async () => []);
    const createClassImpl = vi.fn(async () => HOUSE);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, createClassImpl });
    const { component } = render(ClassManager, { session });
    await screen.findByText(/No classes/i);
    await openAdd(component as unknown as { openAdd: () => void });
    await fireEvent.input(screen.getByLabelText('Name'), { target: { value: 'Spec' } });
    await fireEvent.input(screen.getByLabelText('Org'), { target: { value: 'x'.repeat(25) } });
    expect(screen.getByText('An org name can have 24 characters at most.')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Add class' })).toBeDisabled();
    await fireEvent.submit(screen.getByRole('form', { name: 'Add class' }));
    expect(createClassImpl).not.toHaveBeenCalled();
  });

  it("opens a class's org for editing, and clearing it sends null", async () => {
    const mine: Class = { id: 'c2', name: 'House', org: 'My Club' };
    const listClassesImpl = vi.fn(async () => [mine]);
    const updateClassImpl = vi.fn(async () => ({ id: 'c2', name: 'House' }) as Class);
    const { session } = makeTestSession({ noEnter: true, listClassesImpl, updateClassImpl });
    render(ClassManager, { session });
    expect(await screen.findByTestId('class-org')).toHaveTextContent('My Club');
    await fireEvent.click(screen.getByRole('button', { name: 'Edit' }));
    const org = (await screen.findByLabelText('Org')) as HTMLInputElement;
    expect(org.value).toBe('My Club');
    await fireEvent.input(org, { target: { value: '' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() =>
      expect(updateClassImpl).toHaveBeenCalledWith('http://d.local', 'c2', { org: null }, 'tok')
    );
  });
});
