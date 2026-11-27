/**
 * The Server page: the clubs Grid can send to, which club each event goes to, and how
 * much of each event is on its server.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type {
  CertificateLook,
  CertificateRequest,
  ClubSettings,
  LinkClub,
  LinkEvent,
  LinkView
} from '@gridfpv/types';
import ServerPage from '../src/screens/ServerPage.svelte';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const RECON: LinkClub = {
  id: 'k1',
  name: '',
  url: 'https://grid.example.org',
  trust: 'System',
  club: { id: 'club', name: 'Recon FPV' }
};
const NORTH: LinkClub = {
  id: 'k2',
  name: '',
  url: 'https://grid.example.org',
  trust: 'System',
  club: { id: 'c9', name: 'North FPV' }
};
const FRIDAY: LinkEvent = {
  id: 'friday',
  name: 'Friday Night',
  club: 'k1',
  len: 40,
  sent: 40,
  state: 'Sent'
};
const SUNDAY: LinkEvent = {
  id: 'sunday',
  name: 'Sunday Cup',
  club: 'k1',
  len: 90,
  sent: 30,
  state: 'Sending'
};
const noClub = (event: LinkEvent): LinkEvent => {
  const { club: _club, ...rest } = event;
  return { ...rest, len: 0, sent: 0, state: 'NoClub' };
};

/** Nothing saved, and Friday open. */
const EMPTY: LinkView = {
  clubs: [],
  open: 'friday',
  state: 'NotSet',
  events: [noClub(FRIDAY)]
};
/** Recon saved, Friday open and going to it, and all of it there. */
const CONNECTED: LinkView = {
  clubs: [RECON],
  open: 'friday',
  club: 'k1',
  state: 'Connected',
  server_version: '0.6.0',
  events: [FRIDAY, SUNDAY]
};
const TOKEN = 'gfs_the-secret-token';
const PEM = '-----BEGIN CERTIFICATE-----\nMIIBown\n-----END CERTIFICATE-----\n';
const FINGERPRINT = Array.from({ length: 32 }, (_, i) =>
  (i * 7 + 16).toString(16).toUpperCase().padStart(2, '0')
).join(':');
const OWN: CertificateLook = {
  fingerprint: FINGERPRINT,
  pem: PEM,
  trusted: false,
  stands_alone: true
};

/**
 * A Director that keeps the clubs and where each event goes, the way the real one does:
 * the connection is for the club of the open event.
 */
function fake(start: LinkView) {
  let view = start;
  let made = 0;
  const settle = (next: LinkView): LinkView => {
    const open = next.events.find((event) => event.id === next.open);
    const club = next.clubs.some((saved) => saved.id === open?.club) ? open?.club : undefined;
    const { club: _club, ...rest } = next;
    const state: LinkView['state'] =
      club === undefined ? 'NotSet' : club === view.club ? next.state : 'Connecting';
    return { ...rest, ...(club === undefined ? {} : { club }), state };
  };
  const session = {
    getServerLink: vi.fn(async (): Promise<LinkView> => view),
    saveLinkClub: vi.fn(
      async (id: string | undefined, settings: ClubSettings): Promise<LinkView | undefined> => {
        const saved: LinkClub = {
          id: id ?? `new${++made}`,
          name: settings.name,
          url: settings.url,
          trust: settings.trust ?? 'System'
        };
        const clubs =
          id === undefined
            ? [...view.clubs, saved]
            : view.clubs.map((club) =>
                club.id === id ? { ...saved, ...(club.club ? { club: club.club } : {}) } : club
              );
        view = settle({ ...view, clubs });
        return view;
      }
    ),
    removeLinkClub: vi.fn(async (id: string): Promise<LinkView | undefined> => {
      view = settle({
        ...view,
        clubs: view.clubs.filter((club) => club.id !== id),
        events: view.events.map((event) => (event.club === id ? noClub(event) : event))
      });
      return view;
    }),
    setEventLinkClub: vi.fn(
      async (eventId: string, club: string | null): Promise<LinkView | undefined> => {
        view = settle({
          ...view,
          events: view.events.map((event) =>
            event.id !== eventId
              ? event
              : club === null
                ? noClub(event)
                : { ...event, club, state: 'Waiting' as const }
          )
        });
        return view;
      }
    ),
    replaceOnServer: vi.fn(async (): Promise<LinkView | undefined> => view),
    // A server that made its own certificate, which Grid does not take by itself.
    lookAtServerCertificate: vi.fn(
      async (_request: CertificateRequest): Promise<CertificateLook | undefined> => OWN
    )
  };
  return {
    session,
    now: () => view,
    set: (next: LinkView) => {
      view = next;
    }
  };
}

const state = () => screen.getByTestId('link-state').textContent;
const line = () => screen.getByTestId('link-line').textContent;
const address = () => screen.getByLabelText('Address') as HTMLInputElement;
const token = () => screen.getByLabelText('Link token') as HTMLInputElement;
const name = () => screen.getByLabelText('Name') as HTMLInputElement;
const save = () => screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;
const clubs = () => screen.queryAllByTestId('link-club');
const events = () => screen.getAllByTestId('link-event');
const where = (event: string) =>
  screen.getByRole('combobox', { name: `Where ${event} is sent` }) as HTMLSelectElement;
const choices = (select: HTMLSelectElement) =>
  Array.from(select.options, (option) => option.textContent?.trim());

async function shown() {
  await waitFor(() => expect(screen.getByTestId('server-link')).toBeTruthy());
}

/** Render the page over a fake Director, and wait for it. */
async function page(start: LinkView) {
  const director = fake(start);
  render(ServerPage, { session: director.session });
  await shown();
  return director;
}

async function addClub(url: string, secret: string, called = '') {
  await fireEvent.click(screen.getByRole('button', { name: 'Add a club' }));
  await fireEvent.input(address(), { target: { value: url } });
  await fireEvent.input(token(), { target: { value: secret } });
  if (called) await fireEvent.input(name(), { target: { value: called } });
  await fireEvent.click(save());
}

describe('ServerPage with nothing saved', () => {
  it('says the open event is not sent, and offers to add a club', async () => {
    await page(EMPTY);
    expect(state()).toBe('Not sent');
    expect(line()).toBe('Friday Night is not sent to a server. Save a club first.');
    expect(screen.getByTestId('no-clubs')).toBeTruthy();
    expect(clubs()).toHaveLength(0);
    // The form is not open until asked for.
    expect(screen.queryByLabelText('Address')).toBeNull();
    // There is no club to pick, so the box is closed.
    expect(where('Friday Night').disabled).toBe(true);
    expect(choices(where('Friday Night'))).toEqual(['Not sent']);
    expect(events()[0].textContent).not.toContain('Waiting');
  });

  it('says so when no event is open', async () => {
    await page({ clubs: [RECON], state: 'NotSet', events: [] });
    expect(state()).toBe('No event open');
    expect(screen.getByRole('heading', { name: 'No event is open' })).toBeTruthy();
    expect(line()).toContain('Open an event');
    expect(screen.queryByText('Events')).toBeNull();
  });

  it('saves a club, and then never shows the token', async () => {
    const { session } = await page(EMPTY);
    await fireEvent.click(screen.getByRole('button', { name: 'Add a club' }));
    expect(screen.getByRole('form', { name: 'Add a club' })).toBeTruthy();
    expect(token().type).toBe('password');
    // Nothing typed: nothing to save, and no complaint yet.
    expect(save().disabled).toBe(true);
    expect(screen.queryByRole('alert')).toBeNull();

    await fireEvent.input(address(), { target: { value: 'https://grid.example.org/' } });
    await fireEvent.input(token(), { target: { value: `  ${TOKEN} ` } });
    await fireEvent.click(save());
    await waitFor(() => expect(session.saveLinkClub).toHaveBeenCalledTimes(1));
    expect(session.saveLinkClub).toHaveBeenCalledWith(undefined, {
      name: '',
      url: 'https://grid.example.org',
      token: TOKEN,
      trust: 'System'
    });
    // The form closes, and the club is listed. Until the server says its name, the
    // address stands in for one.
    await waitFor(() => expect(clubs()).toHaveLength(1));
    expect(screen.queryByLabelText('Address')).toBeNull();
    expect(clubs()[0].textContent).toContain('grid.example.org');
    expect(document.body.innerHTML).not.toContain(TOKEN);
    // Saving a club sends nothing: the event still goes nowhere.
    expect(state()).toBe('Not sent');
    expect(line()).toContain('Pick a club for it under Events');
    expect(where('Friday Night').disabled).toBe(false);
    expect(session.setEventLinkClub).not.toHaveBeenCalled();
  });

  it('says what is wrong while typing, and does not send it', async () => {
    const { session } = await page(EMPTY);
    await fireEvent.click(screen.getByRole('button', { name: 'Add a club' }));
    await fireEvent.input(address(), { target: { value: 'grid.example.org' } });
    expect(screen.getByRole('alert').textContent).toContain('https://');
    expect(save().disabled).toBe(true);
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org' } });
    expect(screen.getByRole('alert').textContent).toContain('Give the link token');
    await fireEvent.input(token(), { target: { value: 'not-a-token' } });
    expect(screen.getByRole('alert').textContent).toContain('gfs_');
    expect(save().disabled).toBe(true);
    await fireEvent.input(token(), { target: { value: TOKEN } });
    expect(screen.queryByRole('alert')).toBeNull();
    expect(save().disabled).toBe(false);
    expect(session.saveLinkClub).not.toHaveBeenCalled();
  });

  it("shows the Director's own words when a save is refused, and keeps what was typed", async () => {
    const { session } = await page(EMPTY);
    session.saveLinkClub.mockRejectedValueOnce(
      new Error(
        'The address must start with https://. Only a server on this same computer may use http://'
      )
    );
    await addClub('http://grid.example.org', TOKEN);
    await waitFor(() =>
      expect(screen.getByText(/Only a server on this same computer/)).toBeTruthy()
    );
    expect(address().value).toBe('http://grid.example.org');
    expect(clubs()).toHaveLength(0);
    // The refusal goes once the director starts typing again.
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org' } });
    expect(screen.queryByText(/Only a server on this same computer/)).toBeNull();
  });

  it('cancelling the form saves nothing', async () => {
    const { session } = await page(EMPTY);
    await fireEvent.click(screen.getByRole('button', { name: 'Add a club' }));
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByLabelText('Address')).toBeNull();
    expect(session.saveLinkClub).not.toHaveBeenCalled();
    // Opened again, it is empty.
    await fireEvent.click(screen.getByRole('button', { name: 'Add a club' }));
    expect(address().value).toBe('');
  });
});

describe('ServerPage with clubs saved', () => {
  it('names each club as the server names it, or as the director named it', async () => {
    await page({ ...CONNECTED, clubs: [RECON, { ...NORTH, name: 'North (mine)' }] });
    expect(clubs()).toHaveLength(2);
    expect(clubs()[0].textContent).toContain('Recon FPV');
    expect(clubs()[0].textContent).toContain('https://grid.example.org');
    expect(clubs()[1].textContent).toContain('North (mine)');
    expect(clubs()[1].textContent).not.toContain('North FPV');
    // Only the club that is connected says so.
    expect(clubs()[0].textContent).toContain('Connected');
    expect(clubs()[1].textContent).not.toContain('Connected');
    // A club is never shown by its id, here or on the server.
    for (const id of ['k1', 'k2', 'c9']) expect(document.body.textContent).not.toContain(id);
  });

  it('says which club the open event goes to, and that it is connected', async () => {
    await page(CONNECTED);
    expect(screen.getByRole('heading', { name: 'Friday Night' })).toBeTruthy();
    expect(state()).toBe('Connected to Recon FPV');
    expect(line()).toBe('Friday Night is sent to Recon FPV.');
    expect(screen.queryByTestId('link-problem')).toBeNull();
  });

  it('says why the server is not reached', async () => {
    await page({
      ...CONNECTED,
      state: { Failed: { message: 'The server could not be reached: connection refused' } }
    });
    expect(state()).toBe('Not connected');
    expect(screen.getByTestId('link-problem').textContent).toContain('connection refused');
    expect(clubs()[0].textContent).not.toContain('Connected');
  });

  it('adds a second club with a name of its own', async () => {
    const { session } = await page(CONNECTED);
    await addClub('https://grid.example.org', 'gfs_for-north', 'North');
    await waitFor(() => expect(clubs()).toHaveLength(2));
    expect(session.saveLinkClub).toHaveBeenCalledWith(undefined, {
      name: 'North',
      url: 'https://grid.example.org',
      token: 'gfs_for-north',
      trust: 'System'
    });
    expect(clubs()[1].textContent).toContain('North');
    // Both can now be picked for an event.
    expect(choices(where('Friday Night'))).toEqual(['Not sent', 'Recon FPV', 'North']);
    // The open event still goes where it did.
    expect(state()).toBe('Connected to Recon FPV');
  });

  it('changes a club and keeps the token that is set', async () => {
    const { session } = await page(CONNECTED);
    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    expect(screen.getByRole('form', { name: 'Change Recon FPV' })).toBeTruthy();
    expect(address().value).toBe('https://grid.example.org');
    expect(token().value).toBe('');
    expect(token().placeholder).toContain('Set');
    // Nothing changed yet: nothing to save. A slash after the same address is no change.
    expect(save().disabled).toBe(true);
    await fireEvent.input(address(), { target: { value: 'https://grid.example.org/' } });
    expect(save().disabled).toBe(true);

    await fireEvent.input(name(), { target: { value: 'Home club' } });
    await fireEvent.click(save());
    await waitFor(() => expect(session.saveLinkClub).toHaveBeenCalledTimes(1));
    const [id, sent] = session.saveLinkClub.mock.calls[0];
    expect(id).toBe('k1');
    expect('token' in sent).toBe(false);
    expect(sent).toEqual({ name: 'Home club', url: 'https://grid.example.org', trust: 'System' });
    await waitFor(() => expect(clubs()[0].textContent).toContain('Home club'));
    // The new name is the one used everywhere.
    expect(state()).toBe('Connected to Home club');
    expect(choices(where('Friday Night'))).toEqual(['Not sent', 'Home club']);
  });

  it('asks for the token again when the address is another server', async () => {
    const { session } = await page(CONNECTED);
    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    await fireEvent.input(address(), { target: { value: 'https://other.example.org' } });
    expect(screen.getByRole('alert').textContent).toContain('not sent to another server');
    expect(save().disabled).toBe(true);
    await fireEvent.input(token(), { target: { value: TOKEN } });
    expect(save().disabled).toBe(false);
    await fireEvent.click(save());
    await waitFor(() => expect(session.saveLinkClub).toHaveBeenCalledTimes(1));
    expect(session.saveLinkClub.mock.calls[0][1].token).toBe(TOKEN);
  });

  it('removes a club only after the director says so, and says what goes with it', async () => {
    const { session } = await page(CONNECTED);
    await fireEvent.click(screen.getByRole('button', { name: 'Remove Recon FPV' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Remove Recon FPV?');
    expect(within(dialog).getByTestId('remove-club-events').textContent).toContain(
      '2 events go to this club. They will not be sent any more.'
    );
    expect(dialog.textContent).toContain('What is already on the server stays there.');
    expect(session.removeLinkClub).not.toHaveBeenCalled();

    // Cancel does nothing.
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(session.removeLinkClub).not.toHaveBeenCalled();
    expect(clubs()).toHaveLength(1);

    await fireEvent.click(screen.getByRole('button', { name: 'Remove Recon FPV' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Remove' })
    );
    await waitFor(() => expect(session.removeLinkClub).toHaveBeenCalledWith('k1'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(clubs()).toHaveLength(0);
    expect(state()).toBe('Not sent');
    expect(where('Friday Night').value).toBe('');
  });

  it('counts one event as one, and says nothing of events when none go to the club', async () => {
    await page({ ...CONNECTED, clubs: [RECON, NORTH], events: [FRIDAY] });
    await fireEvent.click(screen.getByRole('button', { name: 'Remove Recon FPV' }));
    expect(screen.getByTestId('remove-club-events').textContent).toContain(
      '1 event goes to this club. It will not be sent any more.'
    );
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' })
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    await fireEvent.click(screen.getByRole('button', { name: 'Remove North FPV' }));
    expect(screen.queryByTestId('remove-club-events')).toBeNull();
  });

  it('says why when removing is refused, and keeps the dialog open', async () => {
    const { session } = await page(CONNECTED);
    session.removeLinkClub.mockRejectedValueOnce(new Error('The settings could not be saved.'));
    await fireEvent.click(screen.getByRole('button', { name: 'Remove Recon FPV' }));
    const dialog = screen.getByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Remove' }));
    expect(await within(dialog).findByText('The settings could not be saved.')).toBeTruthy();
    expect(clubs()).toHaveLength(1);
  });
});

describe('ServerPage: where each event goes', () => {
  const TWO: LinkView = {
    clubs: [RECON, NORTH],
    open: 'friday',
    club: 'k1',
    state: 'Connected',
    events: [
      FRIDAY,
      { id: 'sunday', name: 'Sunday Cup', club: 'k2', len: 0, sent: 0, state: 'Waiting' },
      noClub({ ...FRIDAY, id: 'garage', name: 'Garage Laps' })
    ]
  };

  it('lists every event with its club and where it stands', async () => {
    await page(TWO);
    const rows = events();
    expect(rows).toHaveLength(3);
    expect(rows[0].textContent).toContain('Friday Night');
    expect(rows[0].textContent).toContain('Open');
    expect(rows[0].textContent).toContain('On the server');
    expect(where('Friday Night').value).toBe('k1');
    // An event of a club that is not connected waits, and says for what.
    expect(rows[1].textContent).toContain('Sunday Cup');
    expect(rows[1].textContent).toContain('Waiting');
    expect(rows[1].textContent).toContain('Sent when an event of North FPV is open.');
    expect(rows[1].textContent).not.toContain('Open');
    expect(where('Sunday Cup').value).toBe('k2');
    // An event that goes nowhere says so in its box, and has no badge.
    expect(where('Garage Laps').value).toBe('');
    expect(rows[2].textContent).not.toContain('Waiting');
    expect(rows[2].textContent).not.toContain('On the server');
    expect(choices(where('Garage Laps'))).toEqual(['Not sent', 'Recon FPV', 'North FPV']);
    // An event is named, never shown by its id.
    for (const id of ['friday', 'sunday', 'garage']) {
      expect(document.body.textContent).not.toContain(id);
    }
  });

  it('shows how far an event has got', async () => {
    await page(CONNECTED);
    expect(events()[1].textContent).toContain('Sending, 30 of 90');
    expect(screen.queryByRole('button', { name: /Replace/ })).toBeNull();
  });

  it('sends an event to the club picked, at once', async () => {
    const { session } = await page(TWO);
    await fireEvent.change(where('Garage Laps'), { target: { value: 'k2' } });
    await waitFor(() => expect(session.setEventLinkClub).toHaveBeenCalledWith('garage', 'k2'));
    await waitFor(() => expect(where('Garage Laps').value).toBe('k2'));
    expect(events()[2].textContent).toContain('Sent when an event of North FPV is open.');
    // The open event was not touched.
    expect(state()).toBe('Connected to Recon FPV');
  });

  it('stops sending an event when Not sent is picked', async () => {
    const { session } = await page(TWO);
    await fireEvent.change(where('Sunday Cup'), { target: { value: '' } });
    await waitFor(() => expect(session.setEventLinkClub).toHaveBeenCalledWith('sunday', null));
    await waitFor(() => expect(events()[1].textContent).not.toContain('Waiting'));
    expect(where('Sunday Cup').value).toBe('');
  });

  it('goes over to another club when the open event is sent there', async () => {
    const { session } = await page(TWO);
    await fireEvent.change(where('Friday Night'), { target: { value: 'k2' } });
    await waitFor(() => expect(session.setEventLinkClub).toHaveBeenCalledWith('friday', 'k2'));
    await waitFor(() => expect(state()).toBe('Connecting'));
    expect(line()).toBe('Friday Night is sent to North FPV.');
  });

  it('disconnects when the open event is sent nowhere', async () => {
    await page(TWO);
    await fireEvent.change(where('Friday Night'), { target: { value: '' } });
    await waitFor(() => expect(state()).toBe('Not sent'));
    expect(line()).toContain('Pick a club for it under Events');
  });

  it('puts the box back when the Director refuses, and says why', async () => {
    const { session } = await page(TWO);
    session.setEventLinkClub.mockRejectedValueOnce(
      new Error('That club is not saved here any more. Look again.')
    );
    await fireEvent.change(where('Garage Laps'), { target: { value: 'k2' } });
    await waitFor(() => expect(screen.getByText(/not saved here any more/)).toBeTruthy());
    expect(where('Garage Laps').value).toBe('');
  });

  it('says why an event could not be sent', async () => {
    await page({
      ...CONNECTED,
      events: [{ ...FRIDAY, state: { Failed: { message: 'The server said: too big' } } }]
    });
    expect(events()[0].textContent).toContain('Could not be sent');
    expect(events()[0].textContent).toContain('The server said: too big');
  });

  it("replaces the server's copy only after the director says so in the dialog", async () => {
    const { session } = await page({
      ...CONNECTED,
      events: [{ ...FRIDAY, sent: 1, state: 'Different' }]
    });
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
    const { session } = await page({
      ...CONNECTED,
      events: [
        { ...FRIDAY, sent: 0, state: 'Removed' },
        { ...SUNDAY, sent: 90, state: 'Sent' }
      ]
    });
    const rows = events();
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
});

describe("ServerPage: the server's own certificate", () => {
  /** Open the form for Recon and say the server uses its own certificate. */
  const openBox = async () => {
    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    await fireEvent.click(screen.getByRole('checkbox', { name: /uses its own certificate/ }));
    return screen.getByRole('button', {
      name: "Look at the server's certificate"
    }) as HTMLButtonElement;
  };

  it('takes a pasted certificate', async () => {
    const { session } = await page(CONNECTED);
    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    expect(screen.queryByRole('textbox', { name: /certificate/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Look at the server/ })).toBeNull();
    await fireEvent.click(screen.getByRole('checkbox', { name: /uses its own certificate/ }));
    const pem = '-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----';
    await fireEvent.input(screen.getByRole('textbox', { name: /certificate/ }), {
      target: { value: pem }
    });
    await fireEvent.click(save());
    await waitFor(() => expect(session.saveLinkClub).toHaveBeenCalled());
    const [id, sent] = session.saveLinkClub.mock.calls[0];
    expect(id).toBe('k1');
    expect(sent.trust).toEqual({ ExtraAuthorities: { pem } });
    expect('token' in sent).toBe(false);
  });

  it('cannot look with no address', async () => {
    await page(EMPTY);
    await fireEvent.click(screen.getByRole('button', { name: 'Add a club' }));
    await fireEvent.click(screen.getByRole('checkbox', { name: /uses its own certificate/ }));
    const look = screen.getByRole('button', {
      name: "Look at the server's certificate"
    }) as HTMLButtonElement;
    expect(look.disabled).toBe(true);
    await fireEvent.input(address(), { target: { value: 'https://home.example.org' } });
    expect(look.disabled).toBe(false);
  });

  it('shows the fingerprint, and keeps the certificate only when the director says and saves', async () => {
    const { session } = await page(CONNECTED);
    await fireEvent.click(await openBox());
    // Grid looked at the address in the form, with the trust in the form.
    await waitFor(() =>
      expect(session.lookAtServerCertificate).toHaveBeenCalledWith({
        url: 'https://grid.example.org',
        trust: 'System'
      })
    );
    const dialog = await screen.findByRole('dialog');
    // The whole fingerprint is shown, in rows short enough to read out.
    const rows = within(dialog).getByTestId('certificate-fingerprint').querySelectorAll('span');
    expect(rows).toHaveLength(4);
    expect(Array.from(rows, (row) => row.textContent).join(':')).toBe(FINGERPRINT);
    expect(within(dialog).getByTestId('certificate-verdict').textContent).toContain(
      'made this certificate for itself'
    );
    expect(dialog.textContent).toContain('every letter and number is the');

    // Cancel: nothing is trusted by looking.
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(
      (screen.getByRole('textbox', { name: /certificate/ }) as HTMLTextAreaElement).value
    ).toBe('');
    expect(session.saveLinkClub).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: "Look at the server's certificate" }));
    await fireEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', {
        name: 'Accept this certificate'
      })
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    // Exactly the certificate that was shown is in the box. It is not kept until saved.
    expect(
      (screen.getByRole('textbox', { name: /certificate/ }) as HTMLTextAreaElement).value
    ).toBe(PEM.trim());
    expect(session.saveLinkClub).not.toHaveBeenCalled();
    await fireEvent.click(save());
    await waitFor(() => expect(session.saveLinkClub).toHaveBeenCalledTimes(1));
    const [id, sent] = session.saveLinkClub.mock.calls[0];
    expect(id).toBe('k1');
    expect(sent.trust).toEqual({ ExtraAuthorities: { pem: PEM.trim() } });
    expect(sent.url).toBe('https://grid.example.org');
    expect(sent.token).toBeUndefined();
  });

  it('says there is nothing to accept when Grid trusts the certificate already', async () => {
    const { session } = await page(CONNECTED);
    session.lookAtServerCertificate.mockResolvedValueOnce({ ...OWN, trusted: true });
    await fireEvent.click(await openBox());
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByTestId('certificate-verdict').textContent).toContain(
      'already trusts'
    );
    expect(within(dialog).queryByRole('button', { name: 'Accept this certificate' })).toBeNull();
    expect(within(dialog).queryByTestId('certificate-fingerprint')).toBeNull();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Done' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(session.saveLinkClub).not.toHaveBeenCalled();
  });

  it('does not offer to accept a certificate that would not be enough', async () => {
    const { session } = await page(CONNECTED);
    session.lookAtServerCertificate.mockResolvedValueOnce({ ...OWN, stands_alone: false });
    await fireEvent.click(await openBox());
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByTestId('certificate-verdict').textContent).toContain(
      'would not be enough'
    );
    expect(dialog.textContent).toContain('certificate authority');
    // The fingerprint is still shown: it is what to tell whoever runs the server.
    expect(within(dialog).getByTestId('certificate-fingerprint')).toBeTruthy();
    expect(within(dialog).queryByRole('button', { name: 'Accept this certificate' })).toBeNull();
  });

  it('looks with the certificate that is already kept with the club', async () => {
    const { session } = await page({
      ...CONNECTED,
      clubs: [{ ...RECON, trust: { ExtraAuthorities: { pem: PEM.trim() } } }]
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    await fireEvent.click(screen.getByRole('button', { name: "Look at the server's certificate" }));
    await waitFor(() =>
      expect(session.lookAtServerCertificate).toHaveBeenCalledWith({
        url: 'https://grid.example.org',
        trust: { ExtraAuthorities: { pem: PEM.trim() } }
      })
    );
  });

  it("shows Grid's words when the server cannot be looked at, and opens nothing", async () => {
    const { session } = await page(CONNECTED);
    session.lookAtServerCertificate.mockRejectedValueOnce(
      new Error('The server could not be reached: connection refused')
    );
    await fireEvent.click(await openBox());
    expect(await screen.findByTestId('certificate-problem')).toHaveTextContent(
      'The server could not be reached: connection refused'
    );
    expect(screen.queryByRole('dialog')).toBeNull();
    // A second look clears what the first one said.
    await fireEvent.click(screen.getByRole('button', { name: "Look at the server's certificate" }));
    await screen.findByRole('dialog');
    expect(screen.queryByTestId('certificate-problem')).toBeNull();
  });
});

describe('ServerPage over time', () => {
  it('follows the Director as the sending moves on, and leaves a half-typed form alone', async () => {
    const { set } = await page({
      ...CONNECTED,
      state: 'Connecting',
      events: [
        { ...FRIDAY, len: 0, sent: 0, state: 'Waiting' },
        { ...SUNDAY, len: 0, sent: 0, state: 'Waiting' }
      ]
    });
    expect(state()).toBe('Connecting');
    // An event of the club being connected to is only waiting its turn.
    expect(events()[0].textContent).not.toContain('Sent when');

    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    await fireEvent.input(address(), { target: { value: 'https://half-typed' } });
    set(CONNECTED);
    await vi.advanceTimersByTimeAsync(1100);
    await waitFor(() => expect(state()).toBe('Connected to Recon FPV'));
    expect(events()[0].textContent).toContain('On the server');
    expect(address().value).toBe('https://half-typed');

    // Cancel, and the form opens again on what is set.
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Change Recon FPV' }));
    expect(address().value).toBe('https://grid.example.org');
  });

  it('follows the director opening another event', async () => {
    const { set } = await page(CONNECTED);
    set({
      ...CONNECTED,
      clubs: [RECON, NORTH],
      open: 'sunday',
      club: 'k2',
      events: [
        { ...FRIDAY, len: 0, sent: 0, state: 'Waiting' },
        { ...SUNDAY, club: 'k2', len: 90, sent: 90, state: 'Sent' }
      ]
    });
    await vi.advanceTimersByTimeAsync(4100);
    await waitFor(() => expect(state()).toBe('Connected to North FPV'));
    expect(screen.getByRole('heading', { name: 'Sunday Cup' })).toBeTruthy();
    expect(events()[0].textContent).toContain('Sent when an event of Recon FPV is open.');
    expect(events()[1].textContent).toContain('Open');
  });

  it('says so when the Director cannot be read', async () => {
    const { session } = fake(EMPTY);
    session.getServerLink.mockRejectedValue(new Error('Grid is not answering'));
    render(ServerPage, { session });
    await waitFor(() => expect(screen.getByText('Grid is not answering')).toBeTruthy());
  });
});
