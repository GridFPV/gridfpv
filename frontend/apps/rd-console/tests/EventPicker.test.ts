/**
 * EventPicker delete-dialog **copyable name box** proof (the small UX papercut). The hard-delete
 * dialog must make the exact event name easy to grab without weakening the type-to-confirm gate:
 *  - the name renders in a dedicated selectable box (`user-select: all`), and
 *  - a copy button writes the name to the clipboard and shows brief "Copied" feedback.
 *
 * These render EventPicker against a mocked Session (the event seams resolve a single deletable
 * event) and drive the dialog. `navigator.clipboard` is mocked so the copy path is observable
 * without a real clipboard.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { EventMeta } from '@gridfpv/types';
import EventPicker from '../src/screens/EventPicker.svelte';
// The raw component source — jsdom does not inject Svelte's scoped <style>, so the `user-select:
// all` styling is asserted against the box's rule in the source (it's load-bearing for the UX).
import SOURCE from '../src/screens/EventPicker.svelte?raw';
import { makeTestSession } from './support.js';

const noop = () => {};

const EVENT: EventMeta = {
  id: 'friday-series',
  name: 'Friday Night Series',
  created_at: 0,
  persistent: true,
  timers: [],
  roster: [],
  classes: []
};

/** A picker whose open list resolves [EVENT], with no active event. */
function renderPicker() {
  const { session } = makeTestSession({
    noEnter: true,
    listEventsImpl: vi.fn(async () => [EVENT]),
    getActiveEventImpl: vi.fn(async () => ({ event: null })),
    deleteEventImpl: vi.fn(async () => undefined)
  });
  render(EventPicker, { session, onhome: noop });
}

/** Open the hard-delete dialog for EVENT and return its scope. */
async function openDeleteDialog() {
  renderPicker();
  const del = await screen.findByRole('button', { name: `Delete ${EVENT.name}` });
  await fireEvent.click(del);
  const dialog = await screen.findByRole('dialog');
  return within(dialog);
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('EventPicker — copyable event-name box in the delete dialog', () => {
  it('renders the event name in a user-select:all box', async () => {
    const dialog = await openDeleteDialog();
    const box = dialog.getByLabelText('Event name to copy');
    expect(box).toHaveTextContent(EVENT.name);
    expect(box.tagName.toLowerCase()).toBe('code');
    // The box carries the `.copy-name-value` class whose rule sets `user-select: all` (a single
    // click highlights the whole name). jsdom doesn't apply Svelte's scoped styles to
    // getComputedStyle, so assert the rule is wired: the class is on the box, and its block in the
    // component source declares user-select: all.
    expect(box.classList.contains('copy-name-value')).toBe(true);
    const rule = SOURCE.match(/\.copy-name-value\s*\{[^}]*\}/);
    expect(rule, 'expected a .copy-name-value style rule').not.toBeNull();
    expect(rule![0].replace(/\s+/g, '')).toContain('user-select:all');
  });

  it('copies the name to the clipboard and shows "Copied" feedback', async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal('navigator', { clipboard: { writeText } });

    const dialog = await openDeleteDialog();
    const copyBtn = dialog.getByRole('button', { name: `Copy event name “${EVENT.name}”` });
    expect(copyBtn).toHaveTextContent('Copy');

    await fireEvent.click(copyBtn);

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith(EVENT.name);
    await waitFor(() => expect(copyBtn).toHaveTextContent('Copied'));
  });

  it('keeps the type-to-confirm gate: the copy box does not enable delete on its own', async () => {
    vi.stubGlobal('navigator', { clipboard: { writeText: vi.fn(async () => undefined) } });

    const dialog = await openDeleteDialog();
    const confirm = dialog.getByRole('button', { name: 'Delete permanently' });
    expect(confirm).toBeDisabled();

    // Copying alone must NOT enable the red button.
    await fireEvent.click(dialog.getByRole('button', { name: /Copy event name/ }));
    expect(confirm).toBeDisabled();

    // Only typing the exact name enables it.
    await fireEvent.input(dialog.getByRole('textbox', { name: 'Confirm event name' }), {
      target: { value: EVENT.name }
    });
    await waitFor(() => expect(confirm).toBeEnabled());
  });

  it('gracefully no-ops when the clipboard API is unavailable', async () => {
    vi.stubGlobal('navigator', {});

    const dialog = await openDeleteDialog();
    const copyBtn = dialog.getByRole('button', { name: /Copy event name/ });
    // Must not throw, and stays in its default state (no "Copied").
    await fireEvent.click(copyBtn);
    expect(copyBtn).toHaveTextContent('Copy');
    expect(copyBtn).not.toHaveTextContent('Copied');
  });
});
