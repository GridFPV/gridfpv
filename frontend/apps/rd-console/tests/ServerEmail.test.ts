/**
 * Email on a remote server (docs/primitives.md §13.8): an account's own address, the link
 * that confirms it, and asking for a link to set a forgotten password.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { FORGOT_SENT, emailProblem } from '../src/lib/serverPeople.js';
import { formatServerHash, parseServerHash } from '../src/lib/serverHome.js';
import { eventView } from './competition.js';
import { ACE, NORTH, SOUTH, fakePeople, fakeServer, type FakeServer } from './serverPeopleFake.js';
import { fakeLiveSession } from './support.js';

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  location.hash = '';
});
afterEach(() => {
  cleanup();
  location.hash = '';
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function open(hash: string, server: FakeServer) {
  location.hash = hash;
  const people = fakePeople(server);
  const load = {
    clubs: vi.fn(async (): Promise<PushClub[]> => [NORTH, SOUTH]),
    events: vi.fn(async (): Promise<ServerEvent[]> => [])
  };
  render(ServerHome, {
    baseUrl: 'https://grid.example.org',
    load,
    people,
    sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
  });
  return { people, server };
}

const go = (hash: string) => {
  location.hash = hash;
  window.dispatchEvent(new Event('hashchange'));
};
const card = () => screen.findByTestId('account-email');
const WITH: MyAccount = { ...ACE, email: { address: 'ace@example.org', confirmed: true } };
const WAITING: MyAccount = { ...ACE, email: { address: 'ace@example.org', confirmed: false } };

describe('the words and addresses of email', () => {
  it('says what is not an email address, and nothing while the box is empty', () => {
    expect(emailProblem('')).toBeUndefined();
    expect(emailProblem(' ace@example.org ')).toBeUndefined();
    expect(emailProblem('ace+grid@mail.example.org')).toBeUndefined();
    for (const bad of ['ace', 'ace@', '@example.org', 'ace@localhost', 'a b@example.org']) {
      expect(emailProblem(bad), bad).toContain('not an email address');
    }
  });

  it('has an address for the link that confirms one', () => {
    expect(parseServerHash('#/confirm-email/gfe_abc')).toEqual({
      kind: 'confirm-email',
      secret: 'gfe_abc'
    });
    expect(formatServerHash({ kind: 'confirm-email', secret: 'gfe_a b' })).toBe(
      '#/confirm-email/gfe_a%20b'
    );
    // With nothing after it, it is not that page.
    expect(parseServerHash('#/confirm-email')).toEqual({ kind: 'list' });
  });
});

describe('the email card on the account page', () => {
  it('is not shown on a server that sends no email', async () => {
    open('#/account', fakeServer({ account: ACE }));
    await screen.findByTestId('server-account');
    expect(screen.queryByTestId('account-email')).toBeNull();
  });

  it('takes an address, and says a link is on its way', async () => {
    const { people, server } = open('#/account', fakeServer({ account: ACE, emailOn: true }));
    const block = await card();
    expect(block).toHaveTextContent('Nobody else is shown it.');
    expect(within(block).queryByTestId('email-state')).toBeNull();
    await fireEvent.click(within(block).getByRole('button', { name: 'Add an email address' }));
    const box = within(block).getByLabelText('Email address') as HTMLInputElement;
    const save = within(block).getByRole('button', { name: 'Save' }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    await fireEvent.input(box, { target: { value: 'not an address' } });
    expect(within(block).getByRole('alert')).toHaveTextContent('not an email address');
    expect(save.disabled).toBe(true);
    await fireEvent.input(box, { target: { value: ' Ace@Example.org ' } });
    expect(save.disabled).toBe(false);
    await fireEvent.click(save);
    await waitFor(() => expect(people.setEmail).toHaveBeenCalledWith('Ace@Example.org'));

    await waitFor(() =>
      expect(within(block).getByTestId('email-state')).toHaveTextContent('Not confirmed')
    );
    expect(within(block).getByTestId('email-address')).toHaveTextContent('ace@example.org');
    expect(within(block).getByTestId('email-waiting')).toHaveTextContent(
      'A link is on its way. Open it to confirm the address is yours.'
    );
    expect(Object.values(server.emailLinks)).toEqual(['ace@example.org']);
    // The link itself is never on the page: only the inbox has it.
    expect(document.body.innerHTML).not.toContain('gfe_');
  });

  it('sends the link again when asked', async () => {
    const { people, server } = open('#/account', fakeServer({ account: WAITING, emailOn: true }));
    const block = await card();
    expect(within(block).getByTestId('email-waiting')).toHaveTextContent(
      'A link was sent to this address.'
    );
    await fireEvent.click(within(block).getByRole('button', { name: 'Send the link again' }));
    await waitFor(() => expect(people.setEmail).toHaveBeenCalledWith('ace@example.org'));
    await waitFor(() =>
      expect(within(block).getByTestId('email-waiting')).toHaveTextContent('A link is on its way.')
    );
    expect(Object.keys(server.emailLinks)).toHaveLength(1);
  });

  it('shows a confirmed address, with nothing to send again', async () => {
    open('#/account', fakeServer({ account: WITH, emailOn: true }));
    const block = await card();
    expect(within(block).getByTestId('email-state')).toHaveTextContent('Confirmed');
    expect(within(block).queryByTestId('email-waiting')).toBeNull();
    expect(within(block).queryByRole('button', { name: 'Send the link again' })).toBeNull();
  });

  it('changes the address, which then needs confirming again', async () => {
    const { people } = open('#/account', fakeServer({ account: WITH, emailOn: true }));
    const block = await card();
    await fireEvent.click(within(block).getByRole('button', { name: 'Change' }));
    const box = within(block).getByLabelText('Email address') as HTMLInputElement;
    expect(box.value).toBe('ace@example.org');
    await fireEvent.input(box, { target: { value: 'new@example.org' } });
    await fireEvent.click(within(block).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(people.setEmail).toHaveBeenCalledWith('new@example.org'));
    await waitFor(() =>
      expect(within(block).getByTestId('email-state')).toHaveTextContent('Not confirmed')
    );
    expect(within(block).getByTestId('email-address')).toHaveTextContent('new@example.org');
  });

  it('cancels a change and keeps what there was', async () => {
    const { people } = open('#/account', fakeServer({ account: WITH, emailOn: true }));
    const block = await card();
    await fireEvent.click(within(block).getByRole('button', { name: 'Change' }));
    await fireEvent.input(within(block).getByLabelText('Email address'), {
      target: { value: 'other@example.org' }
    });
    await fireEvent.click(within(block).getByRole('button', { name: 'Cancel' }));
    expect(people.setEmail).not.toHaveBeenCalled();
    expect(within(block).getByTestId('email-address')).toHaveTextContent('ace@example.org');
  });

  it('takes the address off', async () => {
    const { people, server } = open('#/account', fakeServer({ account: WITH, emailOn: true }));
    const block = await card();
    await fireEvent.click(within(block).getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(people.removeEmail).toHaveBeenCalled());
    await waitFor(() =>
      expect(within(block).getByRole('button', { name: 'Add an email address' })).toBeTruthy()
    );
    expect(server.account?.email).toBeUndefined();
  });

  it("says the server's words when the mail could not be sent, and keeps what was typed", async () => {
    const { people } = open('#/account', fakeServer({ account: ACE, emailOn: true }));
    people.setEmail.mockRejectedValueOnce(
      new Error('The email could not be sent. Check the address, or try again later.')
    );
    const block = await card();
    await fireEvent.click(within(block).getByRole('button', { name: 'Add an email address' }));
    const box = within(block).getByLabelText('Email address') as HTMLInputElement;
    await fireEvent.input(box, { target: { value: 'ace@example.org' } });
    await fireEvent.click(within(block).getByRole('button', { name: 'Save' }));
    expect(await within(block).findByTestId('email-error')).toHaveTextContent(
      'The email could not be sent.'
    );
    expect(box.value).toBe('ace@example.org');
    // The refusal goes when they type again.
    await fireEvent.input(box, { target: { value: 'ace2@example.org' } });
    expect(within(block).queryByTestId('email-error')).toBeNull();
  });

  it('still shows an address, to take off, on a server that stopped sending email', async () => {
    open('#/account', fakeServer({ account: WITH }));
    const block = await card();
    expect(within(block).getByTestId('email-address')).toHaveTextContent('ace@example.org');
    expect(within(block).getByRole('button', { name: 'Remove' })).toBeTruthy();
  });
});

describe('the link that confirms an address', () => {
  it('confirms it on opening, and the account page then says so', async () => {
    const server = fakeServer({
      account: WAITING,
      emailOn: true,
      emailLinks: { gfe_abc: 'ace@example.org' }
    });
    const { people } = open('#/confirm-email/gfe_abc', server);
    expect(await screen.findByTestId('email-confirmed')).toHaveTextContent(
      'ace@example.org is confirmed.'
    );
    expect(people.confirmEmail).toHaveBeenCalledWith('gfe_abc');
    expect(people.confirmEmail).toHaveBeenCalledTimes(1);
    go('#/account');
    await waitFor(async () =>
      expect(within(await card()).getByTestId('email-state')).toHaveTextContent('Confirmed')
    );
  });

  it('works for someone who is not signed in', async () => {
    open(
      '#/confirm-email/gfe_abc',
      fakeServer({ emailOn: true, emailLinks: { gfe_abc: 'ace@example.org' } })
    );
    expect(await screen.findByTestId('email-confirmed')).toHaveTextContent('ace@example.org');
  });

  it('says so when the link does not work, and how to get another', async () => {
    open('#/confirm-email/gfe_used', fakeServer({ emailOn: true }));
    expect(await screen.findByTestId('email-link-dead')).toHaveTextContent(
      'This link does not work'
    );
    expect(screen.getByTestId('confirm-email')).toHaveTextContent('Send the link again');
    expect(screen.queryByTestId('email-confirmed')).toBeNull();
  });
});

describe('a forgotten password on the sign-in page', () => {
  const on = (over: Partial<FakeServer> = {}) => fakeServer({ passwordOn: true, ...over });

  it('says to ask an owner on a server that sends no email', async () => {
    open('#/sign-in', on());
    expect(await screen.findByTestId('forgot-owner')).toHaveTextContent(
      'Forgot your password? Ask an owner of this server for a re-connect link.'
    );
    expect(screen.queryByRole('button', { name: 'Forgot your password?' })).toBeNull();
  });

  it('asks for a link by username, and says the same whoever was named', async () => {
    const { people, server } = open('#/sign-in', on({ emailOn: true }));
    await screen.findByTestId('password-form');
    await fireEvent.input(screen.getByLabelText('Username'), { target: { value: 'ace' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Forgot your password?' }));
    const form = screen.getByTestId('forgot-form');
    // What was typed as the username comes along.
    const who = within(form).getByLabelText(
      'Your username, or your email address'
    ) as HTMLInputElement;
    expect(who.value).toBe('ace');
    await fireEvent.click(within(form).getByRole('button', { name: 'Send me a link' }));
    await waitFor(() => expect(people.forgotPassword).toHaveBeenCalledWith('ace'));
    expect(await within(form).findByTestId('forgot-sent')).toHaveTextContent(FORGOT_SENT);
    expect(server.forgotten).toEqual(['ace']);
    // Nothing more to send. The way back to signing in is there.
    expect(within(form).queryByRole('button', { name: 'Send me a link' })).toBeNull();
    await fireEvent.click(within(form).getByRole('button', { name: 'Back to signing in' }));
    expect(screen.getByTestId('password-form')).toBeTruthy();
    expect(screen.queryByTestId('forgot-form')).toBeNull();
  });

  it('takes an email address as well as a username', async () => {
    const { people } = open('#/sign-in', on({ emailOn: true }));
    await screen.findByTestId('password-form');
    await fireEvent.click(screen.getByRole('button', { name: 'Forgot your password?' }));
    const form = screen.getByTestId('forgot-form');
    const send = within(form).getByRole('button', { name: 'Send me a link' }) as HTMLButtonElement;
    // Nothing to ask about until something is typed.
    expect(send.disabled).toBe(true);
    await fireEvent.input(within(form).getByLabelText('Your username, or your email address'), {
      target: { value: ' ace@example.org ' }
    });
    await fireEvent.click(send);
    await waitFor(() => expect(people.forgotPassword).toHaveBeenCalledWith('ace@example.org'));
  });

  it("shows the server's words when it cannot be asked", async () => {
    const { people } = open('#/sign-in', on({ emailOn: true }));
    people.forgotPassword.mockRejectedValueOnce(new Error('The server is not answering.'));
    await screen.findByTestId('password-form');
    await fireEvent.click(screen.getByRole('button', { name: 'Forgot your password?' }));
    const form = screen.getByTestId('forgot-form');
    await fireEvent.input(within(form).getByLabelText('Your username, or your email address'), {
      target: { value: 'ace' }
    });
    await fireEvent.click(within(form).getByRole('button', { name: 'Send me a link' }));
    expect(await within(form).findByTestId('password-error')).toHaveTextContent(
      'The server is not answering.'
    );
    expect(within(form).queryByTestId('forgot-sent')).toBeNull();
  });

  it('is not offered where a password is being set, only where one is typed to sign in', async () => {
    open('#/reconnect/gfr_abc', on({ emailOn: true, reconnects: { gfr_abc: 'acct_ace' } }));
    await screen.findByTestId('password-form');
    expect(screen.queryByRole('button', { name: 'Forgot your password?' })).toBeNull();
    expect(screen.queryByTestId('forgot-owner')).toBeNull();
  });
});
