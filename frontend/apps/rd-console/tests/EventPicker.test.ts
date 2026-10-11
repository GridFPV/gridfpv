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
  render(EventPicker, { session, onhome: noop, onracegow: noop });
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

describe('EventPicker — the brand home root (#118)', () => {
  it('renders the GridFPV brand top-left and clicking it goes home', async () => {
    const onhome = vi.fn();
    const { session } = makeTestSession({
      noEnter: true,
      listEventsImpl: vi.fn(async () => [EVENT]),
      getActiveEventImpl: vi.fn(async () => ({ event: null }))
    });
    render(EventPicker, { session, onhome, onracegow: noop });
    // The button's accessible name comes from its wordmark content; the title is the tooltip.
    const brand = await screen.findByTitle('Home — GridFPV hub');
    await fireEvent.click(brand);
    expect(onhome).toHaveBeenCalledTimes(1);
  });
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

describe('EventPicker — the empty picker is a first run, not an empty list (#414)', () => {
  /** Render the picker against a Director that holds NO events — a brand-new install. */
  function renderEmptyPicker() {
    const { session } = makeTestSession({
      noEnter: true,
      listEventsImpl: vi.fn(async () => []),
      getActiveEventImpl: vi.fn(async () => ({ event: null }))
    });
    render(EventPicker, { session, onhome: noop, onracegow: noop });
  }

  it('offers a create-your-first-event call to action when no events exist', async () => {
    renderEmptyPicker();
    // A heading + a primary button, not a dashed "nothing here" apology: with the built-in
    // Practice event gone, creating an event is the whole job of a first run.
    expect(
      await screen.findByRole('heading', { name: 'Create your first event' })
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create your first event' })).toBeInTheDocument();
  });

  it('the call to action opens the SAME create dialog the header button does', async () => {
    renderEmptyPicker();
    const cta = await screen.findByRole('button', { name: 'Create your first event' });
    await fireEvent.click(cta);
    const form = await screen.findByRole('form', { name: 'New event' });
    // "Set up event after creating" is ticked by default, so creating hands straight off to the
    // existing setup wizard (#97) — there is no second wizard here.
    const setup = screen.getByRole('checkbox', { name: 'Set up event after creating' });
    expect(setup).toBeChecked();
    expect(within(form).getByLabelText('Event name')).toBeInTheDocument();
  });

  it('shows no Practice row — the built-in event is gone', async () => {
    renderEmptyPicker();
    await screen.findByRole('heading', { name: 'Create your first event' });
    expect(screen.queryByRole('button', { name: /Practice/ })).toBeNull();
  });
});

/**
 * RaceGOW lives on the Events page, not the hub: "+ New event" asks club race vs track run, and a
 * run the preset built lists here with a badge. The club path must stay one click — the kind
 * chooser defaults to a race, with the name form already up.
 */
describe('EventPicker — a RaceGOW track run is a kind of new event', () => {
  const RUN: EventMeta = {
    id: 'track1-run',
    name: 'RaceGOW6 Track 1',
    created_at: 1000,
    persistent: true,
    preset: 'racegow',
    timers: ['rh-1'],
    roster: ['p1'],
    classes: ['mgp-whoop']
  };

  function renderWithRun() {
    const onracegow = vi.fn();
    const { session } = makeTestSession({
      noEnter: true,
      listEventsImpl: vi.fn(async () => [EVENT, RUN]),
      getActiveEventImpl: vi.fn(async () => ({ event: null }))
    });
    render(EventPicker, { session, onhome: noop, onracegow });
    return { onracegow };
  }

  it('badges the run the preset built, and only that one', async () => {
    renderWithRun();
    // Anchored: the delete button's name is "Delete <name>", and a loose match would hit both.
    const runRow = await screen.findByRole('button', { name: /^RaceGOW6 Track 1/ });
    expect(within(runRow).getByText('RaceGOW')).toBeInTheDocument();
    const clubRow = screen.getByRole('button', { name: /^Friday Night Series/ });
    expect(within(clubRow).queryByText('RaceGOW')).toBeNull();
  });

  it('the new-event dialog defaults to a club race, with the name form up', async () => {
    renderWithRun();
    await fireEvent.click(await screen.findByRole('button', { name: '+ New event' }));
    const form = await screen.findByRole('form', { name: 'New event' });
    expect(within(form).getByRole('radio', { name: 'Club race' })).toBeChecked();
    expect(within(form).getByRole('radio', { name: 'RaceGOW track run' })).not.toBeChecked();
    expect(within(form).getByLabelText('Event name')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create & enter' })).toBeInTheDocument();
  });

  it('choosing a track run swaps the form for a hand-off to the RaceGOW page', async () => {
    const { onracegow } = renderWithRun();
    await fireEvent.click(await screen.findByRole('button', { name: '+ New event' }));
    const form = await screen.findByRole('form', { name: 'New event' });
    await fireEvent.click(within(form).getByRole('radio', { name: 'RaceGOW track run' }));
    // The name form steps aside: the RaceGOW page's own form asks what a run needs.
    await waitFor(() => expect(within(form).queryByLabelText('Event name')).toBeNull());
    expect(screen.queryByRole('button', { name: 'Create & enter' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Set up track run' }));
    expect(onracegow).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });
});
