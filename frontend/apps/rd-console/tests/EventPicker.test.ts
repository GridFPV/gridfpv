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

const EVENT: EventMeta = {
  id: 'friday-series',
  name: 'Friday Night Series',
  created_at: 0,
  persistent: true
};

/** A picker whose open list resolves [EVENT], with no active event. */
function renderPicker() {
  const { session } = makeTestSession({
    noEnter: true,
    listEventsImpl: vi.fn(async () => [EVENT]),
    getActiveEventImpl: vi.fn(async () => ({ event: null })),
    deleteEventImpl: vi.fn(async () => undefined)
  });
  render(EventPicker, { session });
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

/** What the Director answers when control needs a token and none was sent. */
const needsToken = () =>
  Object.assign(new Error('Control on this Director needs a token.'), {
    status: 401,
    code: 'Unauthorized'
  });

describe('EventPicker — the page frame', () => {
  it('is the Events page: no brand and no way home of its own (the nav above has both)', async () => {
    renderPicker();
    expect(screen.getByRole('heading', { level: 1, name: /Events/ })).toBeVisible();
    await screen.findByRole('button', { name: `Delete ${EVENT.name}` });
    expect(screen.queryByTitle('Home — GridFPV hub')).toBeNull();
    expect(screen.queryByRole('navigation', { name: 'Breadcrumb' })).toBeNull();
    // The event is listed by name, never by its id.
    expect(document.body.textContent).not.toContain(EVENT.id);
  });
});

describe('EventPicker — onopened: an event was opened or created here', () => {
  const NEW: EventMeta = {
    id: 'club-night-7f3a',
    name: 'Club Night',
    created_at: 1,
    persistent: true
  };

  /** A picker over a session that holds no token, so a 401 leads to the token prompt. */
  function renderGated(seams: Parameters<typeof makeTestSession>[0] = {}) {
    const { session } = makeTestSession({
      noEnter: true,
      listEventsImpl: vi.fn(async () => [EVENT]),
      getActiveEventImpl: vi.fn(async () => ({ event: null })),
      ...seams
    });
    session.clearToken();
    const onopened = vi.fn();
    render(EventPicker, { session, onopened });
    return { session, onopened };
  }

  const openRow = async () => {
    const row = (await screen.findByText(EVENT.name)).closest('button.event-row') as HTMLElement;
    await fireEvent.click(row);
  };

  async function createClubNight() {
    await fireEvent.click(await screen.findByRole('button', { name: '+ New event' }));
    const form = await screen.findByRole('form', { name: 'New event' });
    await fireEvent.input(within(form).getByLabelText('Event name'), {
      target: { value: 'Club Night' }
    });
    await fireEvent.submit(form);
    return form;
  }

  it('is called once the Director has made the event the active one', async () => {
    const setActiveEvent = vi.fn(async () => EVENT);
    const { session, onopened } = renderGated({ setActiveEventImpl: setActiveEvent });
    await openRow();
    await waitFor(() => expect(onopened).toHaveBeenCalledTimes(1));
    expect(setActiveEvent.mock.calls[0]).toContain(EVENT.id);
    expect(session.currentEvent).toEqual(EVENT);
  });

  it('is not called when the token prompt for opening is cancelled', async () => {
    const setActiveEvent = vi.fn(async () => Promise.reject(needsToken()));
    const { session, onopened } = renderGated({ setActiveEventImpl: setActiveEvent });
    const asked = vi.fn(async () => undefined);
    session.setTokenProvider(asked);
    await openRow();
    await waitFor(() => expect(asked).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(setActiveEvent).toHaveBeenCalledTimes(1));
    expect(onopened).not.toHaveBeenCalled();
    expect(session.currentEvent).toBeUndefined();
  });

  it('is not called when the Director refuses to open the event', async () => {
    const { session, onopened } = renderGated({
      setActiveEventImpl: vi.fn(async () =>
        Promise.reject(
          Object.assign(new Error('That event is gone.'), { status: 404, code: 'NotFound' })
        )
      )
    });
    await openRow();
    // The refusal lands (as a toast); nothing was opened.
    await new Promise((r) => setTimeout(r, 0));
    expect(onopened).not.toHaveBeenCalled();
    expect(session.currentEvent).toBeUndefined();
  });

  it('is called after a new event is created and opened', async () => {
    const createEvent = vi.fn(async () => NEW);
    const { session, onopened } = renderGated({
      createEventImpl: createEvent,
      setActiveEventImpl: vi.fn(async () => NEW)
    });
    await createClubNight();
    await waitFor(() => expect(onopened).toHaveBeenCalledTimes(1));
    expect(createEvent.mock.calls[0]).toContain('Club Night');
    expect(session.currentEvent).toEqual(NEW);
  });

  it('is not called when the token prompt for creating is cancelled, and the dialog says why', async () => {
    const createEvent = vi.fn(async () => Promise.reject(needsToken()));
    const { session, onopened } = renderGated({ createEventImpl: createEvent });
    const asked = vi.fn(async () => undefined);
    session.setTokenProvider(asked);
    const form = await createClubNight();
    expect(
      await within(form).findByText('A control token is required to create an event.')
    ).toBeVisible();
    expect(asked).toHaveBeenCalledTimes(1);
    expect(onopened).not.toHaveBeenCalled();
    expect(session.currentEvent).toBeUndefined();
  });

  it('is not called when the Director refuses the new event, and its sentence is shown', async () => {
    const SAID = 'An event called Club Night already exists.';
    const { onopened } = renderGated({
      createEventImpl: vi.fn(async () =>
        Promise.reject(Object.assign(new Error(SAID), { status: 409, code: 'Refused' }))
      )
    });
    const form = await createClubNight();
    expect(await within(form).findByText(SAID)).toBeVisible();
    expect(onopened).not.toHaveBeenCalled();
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
    render(EventPicker, { session });
  }

  it('offers a create-your-first-event call to action when no events exist', async () => {
    renderEmptyPicker();
    // A heading + a primary button, not a dashed "nothing here" apology: creating an event is
    // the whole job of a first run.
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
    expect(within(form).getByLabelText('Event name')).toBeInTheDocument();
  });

  it('shows no Practice row — the built-in event is gone', async () => {
    renderEmptyPicker();
    await screen.findByRole('heading', { name: 'Create your first event' });
    expect(screen.queryByRole('button', { name: /Practice/ })).toBeNull();
  });
});
