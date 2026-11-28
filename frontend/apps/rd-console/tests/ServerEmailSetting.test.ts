/**
 * The email account a server sends from, set by an owner on the page
 * (docs/primitives.md §13.8). The page is the real one, over a stand-in server.
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

const OWNER: MyAccount = { ...ACE, id: 'acct_oz', name: 'Oz', owner: true, clubs: [] };

function open(server: FakeServer, hash = '#/admin/server/email') {
  location.hash = hash;
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

const go = (hash: string) => {
  location.hash = hash;
  window.dispatchEvent(new Event('hashchange'));
};
const card = () => screen.findByTestId('email-setting');
const box = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
const type = (label: string, value: string) => fireEvent.input(box(label), { target: { value } });
const save = () => screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;
const state = () => screen.getByTestId('email-on');

const PROTON = {
  host: 'smtp.example.org',
  port: 587,
  user: 'grid@example.org',
  password: 'app-password-9f3k2',
  from: 'Grid <grid@example.org>',
  tls: 'starttls' as const
};

describe("the server's email settings", () => {
  it('is for an owner only', async () => {
    const { people } = open(fakeServer({ account: ACE }));
    await screen.findByTestId('server-who');
    expect(screen.queryByTestId('email-setting')).toBeNull();
    expect(people.mailSettings).not.toHaveBeenCalled();
  });

  it('starts off, with an empty form and nothing to test', async () => {
    open(fakeServer({ account: OWNER }));
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('Off'));
    expect(box('Server').value).toBe('');
    expect(box('From').value).toBe('');
    // The usual port is shown as a hint, and follows the kind of TLS.
    expect(box('Port').placeholder).toBe('587');
    await fireEvent.change(screen.getByLabelText('Security'), { target: { value: 'tls' } });
    expect(box('Port').placeholder).toBe('465');
    expect(save().disabled).toBe(true);
    expect(screen.queryByTestId('email-test')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Remove…' })).toBeNull();
  });

  it('saves what is typed, and the password is not on the page after', async () => {
    const { people, server } = open(fakeServer({ account: OWNER }));
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('Off'));
    await type('Server', '  smtp.example.org ');
    await type('From', 'Grid <grid@example.org>');
    await type('Username', 'grid@example.org');
    await type('Password', PROTON.password);
    expect(box('Password').type).toBe('password');
    await fireEvent.click(save());
    // No port typed: none is sent, and the server picks the usual one.
    await waitFor(() =>
      expect(people.setMailSettings).toHaveBeenCalledWith({
        host: 'smtp.example.org',
        from: 'Grid <grid@example.org>',
        tls: 'starttls',
        user: 'grid@example.org',
        password: PROTON.password
      })
    );
    await waitFor(() => expect(state()).toHaveTextContent('On'));
    expect(screen.getByTestId('email-saved')).toBeVisible();
    expect(box('Port').value).toBe('587');
    // The password box is empty again, and says one is kept.
    expect(box('Password').value).toBe('');
    expect(box('Password').placeholder).toBe('Kept');
    expect(document.body.innerHTML).not.toContain(PROTON.password);
    expect(server.mail?.password).toBe(PROTON.password);
  });

  it('keeps the password when it is saved again with none typed', async () => {
    const { people, server } = open(fakeServer({ account: OWNER, mail: PROTON, emailOn: true }));
    await card();
    await waitFor(() => expect(box('Server').value).toBe('smtp.example.org'));
    expect(box('Password').value).toBe('');
    await type('From', 'Recon <grid@example.org>');
    await fireEvent.click(save());
    await waitFor(() => expect(people.setMailSettings).toHaveBeenCalled());
    expect(people.setMailSettings.mock.calls[0][0]).not.toHaveProperty('password');
    expect(server.mail?.password).toBe(PROTON.password);
    expect(server.mail?.from).toBe('Recon <grid@example.org>');
  });

  it("says the server's words when the settings are refused, and keeps what was typed", async () => {
    const { server } = open(fakeServer({ account: OWNER }));
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('Off'));
    await type('Server', 'smtp.example.org');
    await type('From', 'nobody');
    await fireEvent.click(save());
    expect(await screen.findByTestId('email-setting-error')).toHaveTextContent(
      'Who the mail is from is not an address.'
    );
    expect(box('From').value).toBe('nobody');
    expect(server.mail).toBeUndefined();
    // Typing again clears what was said.
    await type('From', 'Grid <grid@example.org>');
    expect(screen.queryByTestId('email-setting-error')).toBeNull();
  });

  it('will not send a port that is not one', async () => {
    const { people } = open(fakeServer({ account: OWNER }));
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('Off'));
    await type('Server', 'smtp.example.org');
    await type('From', 'Grid <grid@example.org>');
    await type('Port', '99999');
    expect(screen.getByRole('alert')).toHaveTextContent('A port is a number from 1 to 65535.');
    expect(save().disabled).toBe(true);
    await type('Port', '2525');
    expect(save().disabled).toBe(false);
    await fireEvent.click(save());
    await waitFor(() =>
      expect(people.setMailSettings).toHaveBeenCalledWith(expect.objectContaining({ port: 2525 }))
    );
  });

  it('sends a test, and says where it went', async () => {
    const { people, server } = open(fakeServer({ account: OWNER, mail: PROTON, emailOn: true }));
    const test = await screen.findByTestId('email-test');
    const send = within(test).getByRole('button', { name: 'Send' }) as HTMLButtonElement;
    expect(send.disabled).toBe(true);
    await fireEvent.input(within(test).getByLabelText('Send a test to'), {
      target: { value: ' oz@example.org ' }
    });
    await fireEvent.click(send);
    await waitFor(() => expect(people.testMail).toHaveBeenCalledWith('oz@example.org'));
    expect(await within(test).findByTestId('email-test-sent')).toHaveTextContent(
      'Sent to oz@example.org.'
    );
    expect(server.mailTests).toEqual(['oz@example.org']);
    // It worked, and the page does not say otherwise.
    expect(within(test).queryByTestId('email-test-error')).toBeNull();
  });

  it("shows the email server's own words when the test does not go", async () => {
    const { people } = open(fakeServer({ account: OWNER, mail: PROTON, emailOn: true }));
    const test = await screen.findByTestId('email-test');
    people.testMail.mockRejectedValueOnce(
      new Error('The email server did not take the mail: 535 authentication failed')
    );
    await fireEvent.input(within(test).getByLabelText('Send a test to'), {
      target: { value: 'oz@example.org' }
    });
    await fireEvent.click(within(test).getByRole('button', { name: 'Send' }));
    expect(await within(test).findByTestId('email-test-error')).toHaveTextContent(
      '535 authentication failed'
    );
    expect(within(test).queryByTestId('email-test-sent')).toBeNull();
  });

  it('removes the settings only after asking, in a dialog of the page', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const { people, server } = open(fakeServer({ account: OWNER, mail: PROTON, emailOn: true }));
    await card();
    await fireEvent.click(await screen.findByRole('button', { name: 'Remove…' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('Remove the email settings?');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.removeMailSettings).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: 'Remove…' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Remove' })
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(state()).toHaveTextContent('Off');
    expect(box('Server').value).toBe('');
    expect(server.mail).toBeUndefined();
    expect(screen.queryByTestId('email-test')).toBeNull();
    expect(confirm).not.toHaveBeenCalled();
  });

  it('shows settings from the server file, which saving here takes the place of', async () => {
    const env = {
      host: 'mail.example.net',
      port: 465,
      from: 'Club <club@example.net>',
      tls: 'tls' as const
    };
    const { server } = open(fakeServer({ account: OWNER, envMail: env, emailOn: true }));
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('On, from the settings file'));
    expect(box('Server').value).toBe('mail.example.net');
    expect(box('Port').value).toBe('465');
    // There is nothing of the page's to remove, and the test works with what there is.
    expect(screen.queryByRole('button', { name: 'Remove…' })).toBeNull();
    expect(screen.getByTestId('email-test')).toBeVisible();
    await type('Server', 'smtp.example.org');
    await fireEvent.click(save());
    await waitFor(() => expect(state()).toHaveTextContent(/^On$/));
    expect(server.mail?.host).toBe('smtp.example.org');
    // Removed, the file's settings show again.
    await fireEvent.click(screen.getByRole('button', { name: 'Remove…' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Remove' })
    );
    await waitFor(() => expect(state()).toHaveTextContent('On, from the settings file'));
    expect(box('Server').value).toBe('mail.example.net');
  });

  it('warns when the server does not know its own address', async () => {
    open(fakeServer({ account: OWNER, mail: PROTON, knowsAddress: false }));
    expect(await screen.findByTestId('email-no-address')).toHaveTextContent(
      'GRIDFPV_SERVER_PUBLIC_URL'
    );
  });

  it('lets people give an address at once, without the page being opened again', async () => {
    open(fakeServer({ account: OWNER }));
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('Off'));
    // With no email, the account's own page has no email card.
    go('#/account/sign-in');
    await screen.findByRole('list', { name: 'How you sign in' });
    expect(screen.queryByTestId('account-email')).toBeNull();
    go('#/admin/server/email');
    await card();
    await waitFor(() => expect(state()).toHaveTextContent('Off'));
    await type('Server', 'smtp.example.org');
    await type('From', 'Grid <grid@example.org>');
    await fireEvent.click(save());
    await waitFor(() => expect(state()).toHaveTextContent('On'));
    go('#/account/sign-in');
    expect(await screen.findByTestId('account-email')).toBeVisible();
  });

  it('says so when the settings cannot be read', async () => {
    const server = fakeServer({ account: OWNER });
    location.hash = '#/admin/server/email';
    const people = fakePeople(server);
    people.mailSettings.mockRejectedValue(new Error('The server is not answering.'));
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: {
        clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH, SOUTH]),
        events: vi.fn(async (): Promise<ServerEvent[]> => [])
      },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    expect(await within(await card()).findByText('The server is not answering.')).toBeVisible();
    // No form to fill in over settings that could not be seen.
    expect(screen.queryByRole('form', { name: 'Email settings' })).toBeNull();
  });
});
