/**
 * EventPicker delete-dialog **selectable name box** proof (the small UX papercut). The hard-delete
 * dialog must make the exact event name easy to grab without weakening the type-to-confirm gate:
 * the name renders in a dedicated selectable box (`user-select: all`) the RD can click/triple-click
 * to select, then copy by hand. (There's no copy button: the Clipboard API needs a secure context
 * and fails over plain HTTP, which is how the Director is reached on a field laptop.)
 *
 * These render EventPicker against a mocked Session (the event seams resolve a single deletable
 * event) and drive the dialog.
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

describe('EventPicker — selectable event-name box in the delete dialog', () => {
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

  it('has no copy button (Clipboard API needs a secure context, unavailable over plain HTTP)', async () => {
    const dialog = await openDeleteDialog();
    expect(dialog.queryByRole('button', { name: /Copy event name/ })).toBeNull();
  });

  it('keeps the type-to-confirm gate: only typing the exact name enables delete', async () => {
    const dialog = await openDeleteDialog();
    const confirm = dialog.getByRole('button', { name: 'Delete permanently' });
    expect(confirm).toBeDisabled();

    // A wrong name keeps it disabled.
    await fireEvent.input(dialog.getByRole('textbox', { name: 'Confirm event name' }), {
      target: { value: 'not the name' }
    });
    expect(confirm).toBeDisabled();

    // Only typing the exact name enables it.
    await fireEvent.input(dialog.getByRole('textbox', { name: 'Confirm event name' }), {
      target: { value: EVENT.name }
    });
    await waitFor(() => expect(confirm).toBeEnabled());
  });
});
