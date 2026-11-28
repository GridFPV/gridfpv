/**
 * Discord and Google, set up by an owner on the page (docs/primitives.md §13.8). The page
 * is the real one, over a stand-in server.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { eventView } from './competition.js';
import { ACE, NORTH, SOUTH, fakePeople, fakeServer, type FakeServer } from './serverPeopleFake.js';
import { fakeLiveSession } from './support.js';

beforeEach(() => {
  location.hash = '';
});
afterEach(() => {
  cleanup();
  location.hash = '';
  vi.restoreAllMocks();
});

/** An owner who signs in with a password, on a server with passwords on. */
const OWNER: MyAccount = {
  ...ACE,
  id: 'acct_oz',
  name: 'Oz',
  owner: true,
  clubs: [],
  sign_ins: [{ provider: 'password', subject: 'oz', label: 'oz' }]
};
/** A server with nothing in its settings file. */
const bare = (over: Partial<FakeServer> = {}) =>
  fakeServer({ account: OWNER, providers: [], passwordOn: true, ...over });

function open(server: FakeServer) {
  location.hash = '#/admin/server/sign-in';
  const people = fakePeople(server);
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load: {
      clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH, SOUTH]),
      events: vi.fn(async (): Promise<ServerEvent[]> => [])
    },
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server };
}

const rows = async () => {
  const card = await screen.findByTestId('sign-in-services');
  return within(card).findAllByTestId('sign-in-service');
};
const row = async (name: string) => {
  const found = (await rows()).find((one) => one.textContent?.includes(name));
  if (!found) throw new Error(`no row for ${name}`);
  return found;
};
const stateOf = async (name: string) =>
  within(await row(name)).getByTestId('service-state').textContent;
const press = async (name: string) => fireEvent.click(await screen.findByRole('button', { name }));
const box = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
const type = (label: string, value: string) => fireEvent.input(box(label), { target: { value } });
const save = () => screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;

describe("the server's sign-in services", () => {
  it('is for an owner only', async () => {
    const { people } = open(fakeServer({ account: ACE }));
    expect(await screen.findByText(/Only an owner of this server/)).toBeVisible();
    expect(screen.queryByTestId('sign-in-services')).toBeNull();
    expect(people.signInServices).not.toHaveBeenCalled();
  });

  it('lists Discord and Google as off on a new server, each with a way to set it up', async () => {
    open(bare());
    expect((await rows()).map((one) => one.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'Discord Off Set up',
      'Google Off Set up'
    ]);
    expect(screen.queryByTestId('services-no-address')).toBeNull();
  });

  it('sets Discord up: the redirect to give Discord, and the two values of the app', async () => {
    const { people, server } = open(bare());
    await press('Set up Discord');
    const dialog = await screen.findByRole('dialog');
    expect(dialog.textContent).toContain('Sign in with Discord');
    // The address Discord must be given, and where to make the app.
    expect(screen.getByTestId('service-redirect').textContent).toBe(
      'https://grid.example.org/auth/discord/callback'
    );
    expect(
      within(dialog).getByRole('link', { name: 'Discord Developer Portal' }).getAttribute('href')
    ).toBe('https://discord.com/developers/applications');
    // Both values are needed the first time.
    expect(save().disabled).toBe(true);
    await type('Client ID', ' 1234567890 ');
    expect(save().disabled).toBe(true);
    await type('Client secret', ' s3cret-value ');
    expect(box('Client secret').type).toBe('password');
    expect(save().disabled).toBe(false);
    await fireEvent.click(save());
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.setSignInService).toHaveBeenCalledWith('discord', {
      client_id: '1234567890',
      client_secret: 's3cret-value'
    });
    expect(await stateOf('Discord')).toBe('On');
    expect(await stateOf('Google')).toBe('Off');
    expect(
      within(await row('Discord'))
        .getByRole('button')
        .textContent?.trim()
    ).toBe('Change');
    // People are offered it at once.
    expect(server.providers).toEqual([{ id: 'discord', name: 'Discord' }]);
    await waitFor(() => expect(people.providers).toHaveBeenCalledTimes(2));
    // The secret is nowhere on the page.
    expect(document.body.innerHTML).not.toContain('s3cret-value');
  });

  it('keeps the secret when only the client ID is changed', async () => {
    const { people, server } = open(
      bare({ apps: { google: { client_id: 'old-id', client_secret: 'kept-secret' } } })
    );
    expect(await stateOf('Google')).toBe('On');
    await press('Change Google');
    expect(box('Client ID').value).toBe('old-id');
    expect(box('Client secret').value).toBe('');
    expect(box('Client secret').placeholder).toBe('Kept');
    expect(document.body.innerHTML).not.toContain('kept-secret');
    await type('Client ID', 'new-id');
    await fireEvent.click(save());
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.setSignInService).toHaveBeenCalledWith('google', { client_id: 'new-id' });
    expect(server.apps.google).toEqual({ client_id: 'new-id', client_secret: 'kept-secret' });
  });

  it('says what the server refused, in the box, and keeps what was typed', async () => {
    const { server } = open(bare());
    await press('Set up Google');
    await type('Client ID', 'two words');
    await type('Client secret', 'abc');
    await fireEvent.click(save());
    expect((await screen.findByTestId('service-error')).textContent).toContain(
      'does not look like a client ID'
    );
    expect(box('Client ID').value).toBe('two words');
    expect(server.apps.google).toBeUndefined();
    // Typing again clears what was said.
    await type('Client ID', 'one-word');
    expect(screen.queryByTestId('service-error')).toBeNull();
    // Cancel changes nothing.
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(await stateOf('Google')).toBe('Off');
  });

  it('shows a service from the settings file, and lets the page take over from it', async () => {
    const { server } = open(
      bare({
        providers: [
          { id: 'discord', name: 'Discord' },
          { id: 'oidc', name: 'Club login' }
        ]
      })
    );
    expect(await stateOf('Discord')).toBe('On, from the settings file');
    expect(await stateOf('Google')).toBe('Off');
    // A service of the owner's own is shown, and is not set up here.
    expect(await stateOf('Club login')).toBe('On, from the settings file');
    expect(within(await row('Club login')).queryByRole('button')).toBeNull();

    // The file's secret is not the page's to keep: a new one is needed.
    await press('Change Discord');
    expect(box('Client ID').value).toBe('discord-env-id');
    expect(box('Client secret').placeholder).toBe('');
    expect(save().disabled).toBe(true);
    // There is nothing of the page's to remove yet.
    expect(screen.queryByRole('button', { name: 'Remove…' })).toBeNull();
    await type('Client secret', 'page-secret');
    await fireEvent.click(save());
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(await stateOf('Discord')).toBe('On');

    // Removed after being asked, the file's is used again.
    await press('Change Discord');
    await fireEvent.click(screen.getByRole('button', { name: 'Remove…' }));
    const sure = await screen.findByRole('dialog');
    expect(sure.textContent).toContain('Remove Discord?');
    expect(server.apps.discord).toBeDefined();
    await fireEvent.click(within(sure).getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(server.apps.discord).toBeUndefined();
    expect(await stateOf('Discord')).toBe('On, from the settings file');
  });

  it('does not let an owner remove the only way they sign in, and says why', async () => {
    const { server } = open(
      bare({
        account: { ...OWNER, sign_ins: [{ provider: 'discord', subject: '1', label: 'Oz' }] },
        passwordOn: false,
        apps: { discord: { client_id: 'id', client_secret: 'secret' } }
      })
    );
    await press('Change Discord');
    await fireEvent.click(screen.getByRole('button', { name: 'Remove…' }));
    const sure = await screen.findByRole('dialog');
    await fireEvent.click(within(sure).getByRole('button', { name: 'Remove' }));
    expect((await within(sure).findByText(/lock yourself out/)).textContent).toContain(
      'This is the only way you sign in.'
    );
    expect(server.apps.discord).toBeDefined();
    // Cancel goes back to the box, with nothing said there.
    await fireEvent.click(within(sure).getByRole('button', { name: 'Cancel' }));
    expect(await screen.findByLabelText('Client ID')).toBeVisible();
    expect(screen.queryByText(/lock yourself out/)).toBeNull();
  });

  it('says so when the server does not know its own address', async () => {
    open(bare({ knowsAddress: false }));
    expect((await screen.findByTestId('services-no-address')).textContent).toContain(
      'GRIDFPV_SERVER_PUBLIC_URL'
    );
    await press('Set up Discord');
    await screen.findByRole('dialog');
    expect(screen.queryByTestId('service-redirect')).toBeNull();
  });

  it('says so when the services cannot be read', async () => {
    const server = bare();
    location.hash = '#/admin/server/sign-in';
    const people = fakePeople(server);
    people.signInServices.mockRejectedValueOnce(new Error('The server did not answer.'));
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: {
        clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH, SOUTH]),
        events: vi.fn(async (): Promise<ServerEvent[]> => [])
      },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    const card = await screen.findByTestId('sign-in-services');
    expect(await within(card).findByText('The server did not answer.')).toBeVisible();
    expect(within(card).queryByTestId('sign-in-service')).toBeNull();
  });
});
