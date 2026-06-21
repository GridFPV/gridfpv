import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Class, EventMeta } from '@gridfpv/types';
import EventClasses from '../src/screens/EventClasses.svelte';
import { makeTestSession } from './support.js';

const OPEN: Class = { id: 'c1', name: 'Open', source: 'MultiGP' };
const HOUSE: Class = { id: 'c2', name: 'House', source: 'Custom' };

/** An event that already selects Open (so its checkbox seeds checked). */
const EVENT: EventMeta = {
  id: 'e1',
  name: 'Friday',
  created_at: 0,
  persistent: true,
  timers: ['mock'],
  roster: [],
  classes: ['c1']
};

describe('EventClasses (in-event selection + inline CRUD)', () => {
  it('seeds the checkboxes from the event selection and shows the count', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const { session } = makeTestSession({ listClassesImpl, event: EVENT });
    render(EventClasses, { session });

    const openBox = (await screen.findByLabelText('Select Open')) as HTMLInputElement;
    const houseBox = screen.getByLabelText('Select House') as HTMLInputElement;
    expect(openBox.checked).toBe(true);
    expect(houseBox.checked).toBe(false);

    // The header count reflects 1 of 2 selected.
    expect(screen.getByText(/of 2 classes selected for this event/i)).toBeInTheDocument();
    expect(within(screen.getByText(/selected for this event/i)).getByText('1')).toBeInTheDocument();

    // No change yet → Save disabled.
    expect(
      (screen.getByRole('button', { name: 'Save classes' }) as HTMLButtonElement).disabled
    ).toBe(true);
  });

  it('toggling a row saves the working selection via setEventClasses in directory order', async () => {
    const listClassesImpl = vi.fn(async () => [OPEN, HOUSE]);
    const setEventClassesImpl = vi.fn(async () => ({ ...EVENT, classes: ['c1', 'c2'] }));
    const { session } = makeTestSession({ listClassesImpl, setEventClassesImpl, event: EVENT });
    render(EventClasses, { session });

    const houseBox = (await screen.findByLabelText('Select House')) as HTMLInputElement;
    await fireEvent.click(houseBox);

    const save = screen.getByRole('button', { name: 'Save classes' }) as HTMLButtonElement;
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);

    await waitFor(() => expect(setEventClassesImpl).toHaveBeenCalledTimes(1));
    expect(setEventClassesImpl).toHaveBeenCalledWith('http://d.local', 'e1', ['c1', 'c2'], 'tok');
    await waitFor(() => expect(session.currentEvent?.classes).toEqual(['c1', 'c2']));
  });

  it('an inline-created class becomes selectable without leaving the event', async () => {
    const created: Class = { id: 'c9', name: 'Spec', source: 'MultiGP' };
    let calls = 0;
    const listClassesImpl = vi.fn(async () => (calls++ === 0 ? [OPEN] : [OPEN, created]));
    const createClassImpl = vi.fn(async () => created);
    const { session } = makeTestSession({
      listClassesImpl,
      createClassImpl,
      event: { ...EVENT, classes: [] }
    });
    render(EventClasses, { session });

    await screen.findByLabelText('Select Open');
    await fireEvent.click(screen.getByRole('button', { name: '+ Add class' }));

    const name = (await screen.findByLabelText('Name')) as HTMLInputElement;
    await fireEvent.input(name, { target: { value: 'Spec' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Add class' }));

    await waitFor(() => expect(createClassImpl).toHaveBeenCalledTimes(1));
    const newBox = (await screen.findByLabelText('Select Spec')) as HTMLInputElement;
    expect(newBox.checked).toBe(false);
    await fireEvent.click(newBox);
    expect(newBox.checked).toBe(true);
  });

  it('nudges to add classes when the directory is empty', async () => {
    const listClassesImpl = vi.fn(async () => []);
    const { session } = makeTestSession({ listClassesImpl, event: { ...EVENT, classes: [] } });
    render(EventClasses, { session });
    await screen.findByText(/No classes in the directory yet/i);
  });
});
