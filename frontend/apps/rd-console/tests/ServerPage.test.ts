/**
 * The Server page: the link to a remote server, and how much of each event is on it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { LinkSettings, LinkView } from '@gridfpv/types';
import ServerPage from '../src/screens/ServerPage.svelte';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const NOT_SET: LinkView = {
  url: '',
  has_token: false,
  on: false,
  trust: 'System',
  state: 'NotSet',
  events: []
};
const CONNECTED: LinkView = {
  url: 'https://grid.example.org',
  has_token: true,
  on: true,
  trust: 'System',
  state: 'Connected',
  club: { id: 'club', name: 'Recon FPV' },
  server_version: '0.6.0',
  events: [
    { id: 'friday', name: 'Friday Night', len: 40, sent: 40, state: 'Sent' },
    { id: 'sunday', name: 'Sunday Cup', len: 90, sent: 30, state: 'Sending' }
  ]
};
const TOKEN = 'gfs_the-secret-token';

/** A Director that keeps the link as it is set, the way the real one does. */
function fake(start: LinkView) {
  let view = start;
  const session = {
    getServerLink: vi.fn(async (): Promise<LinkView> => view),
    putServerLink: vi.fn(async (settings: LinkSettings): Promise<LinkView | undefined> => {
      const has_token = settings.token !== undefined ? settings.token !== '' : view.has_token;
      view = {
        ...view,
        url: settings.url,
        has_token,
        on: settings.on,
        trust: settings.trust ?? 'System',
        state: !settings.url || !has_token ? 'NotSet' : settings.on ? 'Connecting' : 'Off'
      };
      return view;
    }),
    replaceOnServer: vi.fn(async (): Promise<LinkView | undefined> => view)
  };
  return {
    session,
    set: (next: LinkView) => {
      view = next;
    }
  };
}

const state = () => screen.getByTestId('link-state').textContent;
const address = () => screen.getByLabelText('Address') as HTMLInputElement;
const token = () => screen.getByLabelText('Link token') as HTMLInputElement;
const power = () =>
  screen.getByRole('switch', { name: 'Send events to the server' }) as HTMLInputElement;

async function shown() {
  await waitFor(() => expect(screen.getByTestId('server-link')).toBeTruthy());
}

describe('ServerPage', () => {
  it('starts not set, with empty boxes and the switch off', async () => {
    const { session } = fake(NOT_SET);
    render(ServerPage, { session });
    await shown();
    expect(state()).toBe('Not set');
    expect(address().value).toBe('');
    expect(token().value).toBe('');
    expect(power().checked).toBe(false);
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull();
    expect(screen.queryByText('Events')).toBeNull();
  });

  it('saves the address and the token, and then never shows the token', async () => {
    const { session } = fake(NOT_SET);
    render(ServerPage, { session });
    await shown();
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org' } });
    await fireEvent.input(token(), { target: { value: TOKEN } });
    expect(token().type).toBe('password');
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(session.putServerLink).toHaveBeenCalledTimes(1));
    expect(session.putServerLink).toHaveBeenCalledWith({
      url: 'https://grid.example.org',
      token: TOKEN,
      on: false,
      trust: 'System'
    });
    await waitFor(() => expect(state()).toBe('Off'));
    expect(token().value).toBe('');
    expect(token().placeholder).toContain('Set');
    expect(document.body.innerHTML).not.toContain(TOKEN);
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull();
  });

  it('keeps the token that is set when something else is saved', async () => {
    const { session } = fake(CONNECTED);
    render(ServerPage, { session });
    await shown();
    // A slash after the same address is not a change: there is nothing to save.
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org/' } });
    expect(screen.queryByRole('button', { name: 'Save' })).toBeNull();

    await fireEvent.click(screen.getByRole('checkbox', { name: /uses its own certificate/ }));
    await fireEvent.input(screen.getByRole('textbox', { name: /certificate/ }), {
      target: { value: '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(session.putServerLink).toHaveBeenCalled());
    const sent = session.putServerLink.mock.calls[0][0];
    expect('token' in sent).toBe(false);
    expect(sent.url).toBe('https://grid.example.org');
  });

  it('says what is wrong while typing, and does not send it', async () => {
    const { session } = fake(NOT_SET);
    render(ServerPage, { session });
    await shown();
    await fireEvent.input(address(), { target: { value: 'grid.example.org' } });
    expect(screen.getByRole('alert').textContent).toContain('https://');
    expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org' } });
    await fireEvent.input(token(), { target: { value: 'not-a-token' } });
    expect(screen.getByRole('alert').textContent).toContain('gfs_');
    expect(session.putServerLink).not.toHaveBeenCalled();
  });

  it('will not switch on with nothing set, and says why', async () => {
    const { session } = fake(NOT_SET);
    render(ServerPage, { session });
    await shown();
    await fireEvent.click(power());
    await waitFor(() =>
      expect(screen.getByText('Give the address and the link token first.')).toBeTruthy()
    );
    expect(power().checked).toBe(false);
    expect(session.putServerLink).not.toHaveBeenCalled();
    // The refusal goes once the director starts typing what was missing.
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org' } });
    expect(screen.queryByText('Give the address and the link token first.')).toBeNull();
  });

  it('switches off and on at once, without a Save', async () => {
    const { session } = fake(CONNECTED);
    render(ServerPage, { session });
    await shown();
    expect(state()).toBe('Connected to Recon FPV');
    await fireEvent.click(power());
    await waitFor(() => expect(session.putServerLink).toHaveBeenCalledTimes(1));
    expect(session.putServerLink.mock.calls[0][0]).toEqual({
      url: 'https://grid.example.org',
      on: false,
      trust: 'System'
    });
    await waitFor(() => expect(state()).toBe('Off'));
    await fireEvent.click(power());
    await waitFor(() => expect(session.putServerLink).toHaveBeenCalledTimes(2));
    expect(session.putServerLink.mock.calls[1][0].on).toBe(true);
  });

  it('puts the switch back when the Director refuses', async () => {
    const { session } = fake(CONNECTED);
    session.putServerLink.mockRejectedValueOnce(
      new Error('The settings could not be saved: disk full')
    );
    render(ServerPage, { session });
    await shown();
    await fireEvent.click(power());
    await waitFor(() => expect(screen.getByText(/disk full/)).toBeTruthy());
    expect(power().checked).toBe(true);
  });

  it("shows the Director's own words when a save is refused, and keeps what was typed", async () => {
    const { session } = fake(NOT_SET);
    session.putServerLink.mockRejectedValueOnce(
      new Error(
        'The address must start with https://. Only a server on this same computer may use http://'
      )
    );
    render(ServerPage, { session });
    await shown();
    await fireEvent.input(address(), { target: { value: 'http://grid.example.org' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(screen.getByText(/Only a server on this same computer/)).toBeTruthy()
    );
    expect(address().value).toBe('http://grid.example.org');
  });

  it('lists each event with where it stands', async () => {
    const { session } = fake(CONNECTED);
    render(ServerPage, { session });
    await shown();
    const rows = screen.getAllByTestId('link-event');
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain('Friday Night');
    expect(rows[0].textContent).toContain('On the server');
    expect(rows[1].textContent).toContain('Sunday Cup');
    expect(rows[1].textContent).toContain('Sending, 30 of 90');
    expect(screen.queryByRole('button', { name: /Replace/ })).toBeNull();
    // An event is named, never shown by its id.
    expect(document.body.textContent).not.toContain('friday');
  });

  it('says why the server is not reached', async () => {
    const { session } = fake({
      ...CONNECTED,
      state: { Failed: { message: 'The server could not be reached: connection refused' } },
      events: []
    });
    render(ServerPage, { session });
    await shown();
    expect(state()).toBe('Not connected');
    expect(screen.getByTestId('link-problem').textContent).toContain('connection refused');
  });

  it("replaces the server's copy only after the director says so in the dialog", async () => {
    const { session } = fake({
      ...CONNECTED,
      events: [{ id: 'friday', name: 'Friday Night', len: 40, sent: 1, state: 'Different' }]
    });
    render(ServerPage, { session });
    await shown();
    expect(screen.getByText('A different event is on the server')).toBeTruthy();
    await fireEvent.click(screen.getByRole('button', { name: /Replace the server's copy/ }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Friday Night');
    expect(session.replaceOnServer).not.toHaveBeenCalled();

    // Cancel does nothing.
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(session.replaceOnServer).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: /Replace the server's copy/ }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Replace' })
    );
    await waitFor(() => expect(session.replaceOnServer).toHaveBeenCalledWith('friday'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('offers to send an event again when it was removed on the server', async () => {
    const { session } = fake({
      ...CONNECTED,
      events: [
        { id: 'friday', name: 'Friday Night', len: 40, sent: 0, state: 'Removed' },
        { id: 'sunday', name: 'Sunday Cup', len: 90, sent: 90, state: 'Sent' }
      ]
    });
    render(ServerPage, { session });
    await shown();
    const rows = screen.getAllByTestId('link-event');
    expect(rows[0].textContent).toContain('Removed on the server');
    // Only the removed one offers it, and it is not called a replace.
    expect(within(rows[1]).queryByRole('button')).toBeNull();
    expect(screen.queryByRole('button', { name: /Replace/ })).toBeNull();

    await fireEvent.click(within(rows[0]).getByRole('button', { name: /Send it again/ }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Send Friday Night to the server again?');
    expect(dialog.textContent).toContain('removed this event from the server');
    expect(dialog.textContent).not.toContain('different event');
    expect(session.replaceOnServer).not.toHaveBeenCalled();

    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(session.replaceOnServer).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: /Send it again/ }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Send it again' })
    );
    await waitFor(() => expect(session.replaceOnServer).toHaveBeenCalledWith('friday'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('takes a certificate when the server uses its own', async () => {
    const { session } = fake(CONNECTED);
    render(ServerPage, { session });
    await shown();
    expect(screen.queryByRole('textbox', { name: /certificate/ })).toBeNull();
    await fireEvent.click(screen.getByRole('checkbox', { name: /uses its own certificate/ }));
    const box = screen.getByRole('textbox', { name: /certificate/ });
    const pem = '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----';
    await fireEvent.input(box, { target: { value: pem } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(session.putServerLink).toHaveBeenCalled());
    expect(session.putServerLink.mock.calls[0][0].trust).toEqual({ ExtraAuthorities: { pem } });
  });

  it('follows the Director as the pushing moves on, and leaves a half-typed form alone', async () => {
    const { session, set } = fake({
      ...CONNECTED,
      state: 'Connecting',
      club: undefined,
      events: []
    });
    render(ServerPage, { session });
    await shown();
    expect(state()).toBe('Connecting');

    await fireEvent.input(address(), { target: { value: 'https://half-typed' } });
    set(CONNECTED);
    await vi.advanceTimersByTimeAsync(1100);
    await waitFor(() => expect(state()).toBe('Connected to Recon FPV'));
    expect(screen.getAllByTestId('link-event')).toHaveLength(2);
    expect(address().value).toBe('https://half-typed');

    // Cancel puts back what is set.
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(address().value).toBe('https://grid.example.org');
  });

  it('says so when the Director cannot be read', async () => {
    const { session } = fake(NOT_SET);
    session.getServerLink.mockRejectedValue(new Error('Grid is not answering'));
    render(ServerPage, { session });
    await waitFor(() => expect(screen.getByText('Grid is not answering')).toBeTruthy());
  });
});
