/**
 * The Formats page: the saved formats as a table that says what each one does in words, and
 * the dialog that makes, changes and copies one. Rendered against a session whose format
 * seams are stand-ins.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { Format, SavedFormat } from '@gridfpv/types';
import FormatsPage from '../src/screens/FormatsPage.svelte';
import { FIRST_TO_3, OPEN_PRACTICE, SAVED_FORMATS, TWO_MINUTE_QUALI } from './competition.js';
import { makeTestSession, type SessionSeams } from './support.js';

afterEach(() => {
  vi.unstubAllGlobals();
});

/**
 * A page over a Director that keeps formats the way the real one does: a save or a delete
 * changes what the next read answers.
 */
function open(start: SavedFormat[] = SAVED_FORMATS, seams: SessionSeams = {}) {
  let saved = [...start];
  const listFormats = vi.fn(async () => saved);
  const createFormat = vi.fn(async (_base: string, format: Format) => {
    const made = { id: `fmt-new-${saved.length}x9`, format };
    saved = [...saved, made];
    return made;
  });
  const updateFormat = vi.fn(async (_base: string, id: string, format: Format) => {
    saved = saved.map((other) => (other.id === id ? { id, format } : other));
    return { id, format };
  });
  const deleteFormat = vi.fn(async (_base: string, id: string) => {
    saved = saved.filter((other) => other.id !== id);
  });
  const { session } = makeTestSession({
    noEnter: true,
    listFormatsImpl: listFormats,
    createFormatImpl: createFormat,
    updateFormatImpl: updateFormat,
    deleteFormatImpl: deleteFormat,
    ...seams
  });
  render(FormatsPage, { session });
  return { session, listFormats, createFormat, updateFormat, deleteFormat };
}

/** The table as text: one string per row, cells joined by " | ". */
async function tableRows(): Promise<string[]> {
  const table = await screen.findByRole('table', { name: 'Formats' });
  return within(table)
    .getAllByRole('row')
    .slice(1)
    .map((row) =>
      [...row.querySelectorAll('th, td')]
        .slice(0, 5)
        .map((cell) => cell.textContent?.trim())
        .join(' | ')
    );
}

const rowOf = async (name: string): Promise<HTMLElement> =>
  (await screen.findByRole('rowheader', { name })).closest('tr') as HTMLElement;

describe('FormatsPage', () => {
  it('lists each saved format with what it does in words, and never its id', async () => {
    open();
    expect(await tableRows()).toEqual([
      'Open practice | Until stopped | Starts at once | Not ranked | Until you call it done',
      'First to 3 laps | First to 3 laps | 5 s countdown | Ranked by finishing order | One race',
      '2 minute qualifying | 2:00, then 10 s to finish the lap | 5 s countdown | Ranked by fastest 3 laps in a row | 3 races'
    ]);
    expect(document.body.textContent).not.toContain('fmt-');
    expect(screen.getByRole('link', { name: 'Help for Formats' })).toHaveAttribute(
      'href',
      '#/help/formats'
    );
  });

  it('says so when no format is saved', async () => {
    open([]);
    expect(await screen.findByTestId('no-formats')).toHaveTextContent('No format is saved.');
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('says so when the formats cannot be read', async () => {
    open([], {
      listFormatsImpl: vi.fn(async () =>
        Promise.reject(new Error('The Director is not answering.'))
      )
    });
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('The formats could not be read.');
    expect(alert).toHaveTextContent('The Director is not answering.');
    expect(screen.queryByTestId('no-formats')).toBeNull();
  });
});

describe('a new format', () => {
  it('opens a blank draft, says what it will do as it is filled in, and saves it', async () => {
    const { createFormat } = open();
    await tableRows();
    await fireEvent.click(screen.getByRole('button', { name: '+ New format' }));
    const form = await screen.findByRole('form', { name: 'New format' });
    expect(within(form).getByLabelText('Format name')).toHaveValue('');
    // Nothing is a format without a name, so nothing is read out yet.
    expect(within(form).getByTestId('format-reads')).toHaveTextContent('');

    await fireEvent.input(within(form).getByLabelText('Format name'), {
      target: { value: 'First to 5' }
    });
    // A field that does not apply to the kind of race is not shown until it does.
    expect(within(form).queryByLabelText('Laps to win')).toBeNull();
    await fireEvent.change(within(form).getByLabelText('The race ends'), {
      target: { value: 'Laps' }
    });
    await fireEvent.input(within(form).getByLabelText('Laps to win'), { target: { value: '5' } });
    await fireEvent.change(within(form).getByLabelText('The heat is done'), {
      target: { value: 'Races' }
    });
    await fireEvent.change(within(form).getByLabelText('Ranked by'), {
      target: { value: 'finish' }
    });
    expect(within(form).getByTestId('format-reads')).toHaveTextContent(
      'First to 5 laps · Ranked by finishing order · One race'
    );
    await fireEvent.click(screen.getByRole('button', { name: 'Save format' }));

    await waitFor(() => expect(createFormat).toHaveBeenCalledTimes(1));
    expect(createFormat.mock.calls[0][1]).toEqual({
      name: 'First to 5',
      race: { end: { FirstToLaps: 5 }, staging_micros: 5_000_000, first_lap: 'HoleShot' },
      completion: { SavedRaces: 1 },
      ranking: { sort: ['LastFinish'] }
    });
    // The dialog closes and the table is read again with the new format in it.
    await waitFor(() => expect(screen.queryByRole('form', { name: 'New format' })).toBeNull());
    expect(await screen.findByRole('rowheader', { name: 'First to 5' })).toBeVisible();
  });

  it('asks for a name, in the dialog, before anything is sent', async () => {
    const { createFormat } = open();
    await tableRows();
    await fireEvent.click(screen.getByRole('button', { name: '+ New format' }));
    const form = await screen.findByRole('form', { name: 'New format' });
    await fireEvent.click(screen.getByRole('button', { name: 'Save format' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent('Give the format a name.');
    expect(createFormat).not.toHaveBeenCalled();
  });

  it("shows the Director's sentence in the dialog when it refuses, and keeps what was typed", async () => {
    const SAID = 'A format called First to 3 laps is already saved.';
    const { listFormats } = open(SAVED_FORMATS, {
      createFormatImpl: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    await tableRows();
    const reads = listFormats.mock.calls.length;
    await fireEvent.click(screen.getByRole('button', { name: '+ New format' }));
    const form = await screen.findByRole('form', { name: 'New format' });
    await fireEvent.input(within(form).getByLabelText('Format name'), {
      target: { value: 'First to 3 laps' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save format' }));
    expect(await within(form).findByRole('alert')).toHaveTextContent(SAID);
    expect(screen.getByRole('form', { name: 'New format' })).toBeVisible();
    expect(within(form).getByLabelText('Format name')).toHaveValue('First to 3 laps');
    expect(screen.getByRole('button', { name: 'Save format' })).toBeEnabled();
    // Nothing was saved, so the table was not read again.
    expect(listFormats).toHaveBeenCalledTimes(reads);
  });

  it('Cancel closes the dialog and saves nothing', async () => {
    const { createFormat } = open();
    await tableRows();
    await fireEvent.click(screen.getByRole('button', { name: '+ New format' }));
    const form = await screen.findByRole('form', { name: 'New format' });
    await fireEvent.input(within(form).getByLabelText('Format name'), {
      target: { value: 'Scratch' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('form', { name: 'New format' })).toBeNull());
    expect(createFormat).not.toHaveBeenCalled();
  });
});

describe('edit, duplicate and delete', () => {
  it('Edit opens the format as it is saved and replaces that one', async () => {
    const { updateFormat, createFormat } = open();
    const row = await rowOf('2 minute qualifying');
    await fireEvent.click(within(row).getByRole('button', { name: 'Edit 2 minute qualifying' }));
    const form = await screen.findByRole('form', { name: 'Edit 2 minute qualifying' });
    expect(within(form).getByLabelText('Format name')).toHaveValue('2 minute qualifying');
    expect(within(form).getByLabelText('The race ends')).toHaveValue('Timed');
    expect(within(form).getByLabelText('Race time, in seconds')).toHaveValue(120);
    expect(within(form).getByLabelText('Seconds to finish the lap')).toHaveValue(10);
    expect(within(form).getByLabelText('The heat is done')).toHaveValue('Races');
    expect(within(form).getByLabelText('Number of races')).toHaveValue(3);
    expect(within(form).getByLabelText('Ranked by')).toHaveValue('consecutive');
    expect(within(form).getByLabelText('Laps in a row')).toHaveValue(3);

    await fireEvent.input(within(form).getByLabelText('Race time, in seconds'), {
      target: { value: '90' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save format' }));
    await waitFor(() => expect(updateFormat).toHaveBeenCalledTimes(1));
    const [, id, format] = updateFormat.mock.calls[0];
    expect(id).toBe('fmt-quali-4m8z');
    expect(format).toEqual({
      ...TWO_MINUTE_QUALI,
      race: {
        ...TWO_MINUTE_QUALI.race,
        end: { Timed: { micros: 90_000_000, grace_micros: 10_000_000 } }
      }
    });
    expect(createFormat).not.toHaveBeenCalled();
    expect(await tableRows()).toContain(
      '2 minute qualifying | 1:30, then 10 s to finish the lap | 5 s countdown | Ranked by fastest 3 laps in a row | 3 races'
    );
  });

  it('a format saved unchanged from Edit is the format that was read', async () => {
    const { updateFormat } = open();
    for (const saved of SAVED_FORMATS) {
      const row = await rowOf(saved.format.name);
      await fireEvent.click(within(row).getByRole('button', { name: `Edit ${saved.format.name}` }));
      await screen.findByRole('form', { name: `Edit ${saved.format.name}` });
      await fireEvent.click(screen.getByRole('button', { name: 'Save format' }));
      await waitFor(() =>
        expect(screen.queryByRole('form', { name: `Edit ${saved.format.name}` })).toBeNull()
      );
    }
    expect(updateFormat.mock.calls.map((call) => [call[1], call[2]])).toEqual([
      ['fmt-practice-9k2d', OPEN_PRACTICE],
      ['fmt-first3-7q1x', FIRST_TO_3],
      ['fmt-quali-4m8z', TWO_MINUTE_QUALI]
    ]);
  });

  it('Duplicate opens a copy under a name no format has, and saves it as a new one', async () => {
    const taken: SavedFormat[] = [
      ...SAVED_FORMATS,
      { id: 'fmt-copy-1a2b', format: { ...FIRST_TO_3, name: 'First to 3 laps copy' } }
    ];
    const { createFormat, updateFormat } = open(taken);
    const row = await rowOf('First to 3 laps');
    await fireEvent.click(within(row).getByRole('button', { name: 'Duplicate First to 3 laps' }));
    const form = await screen.findByRole('form', { name: 'New format' });
    expect(within(form).getByLabelText('Format name')).toHaveValue('First to 3 laps copy 2');
    expect(within(form).getByLabelText('Laps to win')).toHaveValue(3);
    await fireEvent.click(screen.getByRole('button', { name: 'Save format' }));
    await waitFor(() => expect(createFormat).toHaveBeenCalledTimes(1));
    expect(createFormat.mock.calls[0][1]).toEqual({
      ...FIRST_TO_3,
      name: 'First to 3 laps copy 2'
    });
    expect(updateFormat).not.toHaveBeenCalled();
  });

  it('Delete asks first, then deletes that format and reads the table again', async () => {
    const { deleteFormat } = open();
    const row = await rowOf('Open practice');
    await fireEvent.click(within(row).getByRole('button', { name: /^Delete / }));
    expect(deleteFormat).not.toHaveBeenCalled();
    await fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(deleteFormat).toHaveBeenCalledTimes(1));
    expect(deleteFormat.mock.calls[0][1]).toBe('fmt-practice-9k2d');
    await waitFor(() =>
      expect(screen.queryByRole('rowheader', { name: 'Open practice' })).toBeNull()
    );
    expect(screen.getByRole('rowheader', { name: 'First to 3 laps' })).toBeVisible();
  });

  it('a refused delete leaves the format in the table', async () => {
    const deleteFormat = vi.fn(async () =>
      Promise.reject(Object.assign(new Error('No.'), { status: 409, code: 'Refused' }))
    );
    open(SAVED_FORMATS, { deleteFormatImpl: deleteFormat });
    const row = await rowOf('Open practice');
    await fireEvent.click(within(row).getByRole('button', { name: /^Delete / }));
    await fireEvent.click(within(row).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(deleteFormat).toHaveBeenCalledTimes(1));
    // The row is back to a plain Delete, and the format is still listed.
    expect(await within(row).findByRole('button', { name: /^Delete / })).toBeEnabled();
    expect(screen.getByRole('rowheader', { name: 'Open practice' })).toBeVisible();
  });
});
