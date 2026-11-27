/**
 * Signing in to a remote server with a username and a password it keeps
 * (docs/primitives.md §13.8): off until an owner switches it on.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { MyAccount, PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { passwordProblem, providerName, usernameProblem } from '../src/lib/serverPeople.js';
import { eventView } from './competition.js';
import {
  ACE,
  NORTH,
  PROVIDERS,
  SOUTH,
  fakePeople,
  fakeServer,
  type FakeServer
} from './serverPeopleFake.js';
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

const PW = 'correct horse battery';
const PW2 = 'another long password';
/** Ace, who signs in with the username `ace`. */
const ACE_PW: MyAccount = {
  ...ACE,
  sign_ins: [{ provider: 'password', subject: 'ace', label: 'ace' }]
};
const withAce = (over: Partial<FakeServer> = {}): FakeServer =>
  fakeServer({
    passwordOn: true,
    passwords: { ace: { password: PW, account: ACE_PW } },
    ...over
  });

const form = () => screen.findByTestId('password-form');
const box = (label: string) => screen.getByLabelText(label) as HTMLInputElement;
const type = (label: string, value: string) => fireEvent.input(box(label), { target: { value } });
/** The tabs above the form: signing in, or creating an account. */
const tab = async (name: string) => fireEvent.click(await screen.findByRole('tab', { name }));
const go = (hash: string) => {
  location.hash = hash;
  window.dispatchEvent(new Event('hashchange'));
};
const press = async (name: string | RegExp) =>
  fireEvent.click(within(await form()).getByRole('button', { name }));

describe('the words of a username and a password', () => {
  it('says what is wrong with a username, and nothing while it is empty', () => {
    expect(usernameProblem('')).toBeUndefined();
    expect(usernameProblem('ace')).toBeUndefined();
    expect(usernameProblem('  Ace_FPV-1.2 ')).toBeUndefined();
    for (const bad of ['ab', 'two words', 'ace@example.org', 'x'.repeat(33), 'åce']) {
      expect(usernameProblem(bad), bad).toContain('3 to 32');
    }
  });

  it('judges a password by its length, and by the second typing', () => {
    expect(passwordProblem('')).toBeUndefined();
    expect(passwordProblem('123456789')).toContain('at least 10');
    expect(passwordProblem('1234567890')).toBeUndefined();
    expect(passwordProblem('x'.repeat(201))).toContain('200');
    expect(passwordProblem(PW, PW)).toBeUndefined();
    // Not said until the second one is typed.
    expect(passwordProblem(PW, '')).toBeUndefined();
    expect(passwordProblem(PW, 'correct horse')).toBe('The two passwords are not the same.');
  });

  it('names a password sign-in in words', () => {
    expect(providerName('password', PROVIDERS)).toBe('Username and password');
    expect(providerName('discord', PROVIDERS)).toBe('Discord');
  });
});

describe('signing in with a password', () => {
  it('is not offered on a server that has it switched off', async () => {
    open('#/sign-in', fakeServer());
    await screen.findByRole('link', { name: 'Continue with Discord' });
    expect(screen.queryByTestId('password-form')).toBeNull();
  });

  it('signs in, and lands on the list of events', async () => {
    const { people } = open('#/sign-in', withAce());
    await form();
    // The other ways to sign in are still there.
    expect(screen.getByText('or')).toBeTruthy();
    expect(screen.getByRole('link', { name: 'Continue with Discord' })).toBeTruthy();
    expect(box('Password').type).toBe('password');
    // Nothing to send until both are typed.
    const button = within(await form()).getByRole('button', { name: 'Sign in' });
    expect((button as HTMLButtonElement).disabled).toBe(true);
    await type('Username', ' Ace ');
    await type('Password', PW);
    await fireEvent.click(button);
    await waitFor(() => expect(people.passwordSignIn).toHaveBeenCalledWith('Ace', PW));
    await waitFor(() => expect(screen.getByTestId('server-who')).toHaveTextContent('Ace'));
    expect(location.hash).toBe('#/');
    // The password is not left anywhere on the page.
    expect(document.body.innerHTML).not.toContain(PW);
  });

  it('says so when the password is wrong, and empties the password box', async () => {
    open('#/sign-in', withAce());
    await form();
    await type('Username', 'ace');
    await type('Password', 'not the password');
    await press('Sign in');
    expect(await screen.findByTestId('password-error')).toHaveTextContent(
      'That username and password do not match.'
    );
    expect(box('Password').value).toBe('');
    expect(box('Username').value).toBe('ace');
    expect(screen.queryByTestId('server-who')).toBeNull();
    // The refusal goes once they type again.
    await type('Password', 'x');
    expect(screen.queryByTestId('password-error')).toBeNull();
  });

  it('is the way in on a server with no other sign-in set up', async () => {
    open('#/', withAce({ providers: [] }));
    // The top of the page offers Sign in, though no provider is set up.
    const link = await screen.findByTestId('server-sign-in');
    await fireEvent.click(link);
    location.hash = '#/sign-in';
    window.dispatchEvent(new Event('hashchange'));
    await form();
    expect(screen.queryByTestId('sign-in-off')).toBeNull();
    expect(screen.queryByText('Or use one of these:')).toBeNull();
  });

  it('says signing in is not set up when there is no provider and no passwords', async () => {
    open('#/sign-in', fakeServer({ providers: [] }));
    expect(await screen.findByTestId('sign-in-off')).toBeTruthy();
    expect(screen.queryByTestId('password-form')).toBeNull();
    expect(screen.queryByTestId('server-sign-in')).toBeNull();
  });
});

describe('making an account with a password', () => {
  it('is not offered on the sign-in page of a server that asks for an invite', async () => {
    open('#/sign-in', withAce());
    await form();
    expect(screen.queryByRole('tab', { name: 'Create account' })).toBeNull();
  });

  it('makes an account in no club when the owner lets anyone in, and lands on it', async () => {
    const { people, server } = open('#/sign-in', withAce({ openSignUp: true }));
    await tab('Create account');
    expect(screen.getByRole('form', { name: 'Create account' })).toBeTruthy();
    const make = () =>
      within(screen.getByTestId('password-form')).getByRole('button', {
        name: 'Create account'
      }) as HTMLButtonElement;
    expect(make().disabled).toBe(true);

    await type('Username', 'two words');
    expect(screen.getByRole('alert')).toHaveTextContent('3 to 32');
    await type('Username', 'pia');
    await type('Password', 'short');
    expect(screen.getByRole('alert')).toHaveTextContent('at least 10');
    await type('Password', PW);
    expect(screen.queryByRole('alert')).toBeNull();
    // The second typing must match before anything is sent.
    expect(make().disabled).toBe(true);
    await type('The same password again', 'correct horse');
    expect(screen.getByRole('alert')).toHaveTextContent('The two passwords are not the same.');
    expect(make().disabled).toBe(true);
    await type('The same password again', PW);
    expect(make().disabled).toBe(false);
    await fireEvent.click(make());

    await waitFor(() =>
      expect(people.passwordSignUp).toHaveBeenCalledWith({ username: 'pia', password: PW })
    );
    await waitFor(() => expect(screen.getByTestId('account-name')).toHaveTextContent('pia'));
    expect(location.hash).toBe('#/account');
    expect(server.passwords.pia.password).toBe(PW);
    expect(document.body.innerHTML).not.toContain(PW);
  });

  it('goes back to signing in, with the passwords emptied', async () => {
    open('#/sign-in', withAce({ openSignUp: true }));
    await tab('Create account');
    await type('Password', PW);
    await tab('Sign in');
    expect(screen.getByRole('form', { name: 'Sign in' })).toBeTruthy();
    expect(box('Password').value).toBe('');
    expect(screen.queryByLabelText('The same password again')).toBeNull();
  });

  it("says the server's words when the username is taken, and keeps what was typed", async () => {
    open('#/sign-in', withAce({ openSignUp: true }));
    await tab('Create account');
    await type('Username', 'ace');
    await type('Password', PW2);
    await type('The same password again', PW2);
    await press('Create account');
    expect(await screen.findByTestId('password-error')).toHaveTextContent(
      'That sign-in already belongs to an account.'
    );
    expect(box('Username').value).toBe('ace');
    expect(screen.queryByTestId('server-who')).toBeNull();
  });

  it('uses an invite: the account is made and joins the club', async () => {
    const server = withAce();
    server.links.abc = {
      id: 'inv',
      kind: 'Personal',
      club: NORTH,
      role: 'Pilot',
      label: 'Pia',
      max_uses: 1,
      uses: 0,
      revoked: false,
      works: true
    };
    const { people } = open('#/invite/abc', server);
    await form();
    await tab('Create account');
    await type('Username', 'pia');
    await type('Password', PW);
    await type('The same password again', PW);
    await press('Create account');
    await waitFor(() =>
      expect(people.passwordSignUp).toHaveBeenCalledWith({
        username: 'pia',
        password: PW,
        invite: 'abc'
      })
    );
    // They land on their clubs, signed in under the name they typed.
    await waitFor(() => expect(screen.getByTestId('my-club')).toHaveTextContent('North FPV'));
    expect(location.hash).toBe('#/account/clubs');
    expect(screen.getByTestId('server-who')).toHaveTextContent('pia');
  });

  it('lets someone with an account sign in on an invite page, and then accept it', async () => {
    const server = withAce();
    server.links.abc = {
      id: 'inv',
      kind: 'Personal',
      club: SOUTH,
      role: 'Pilot',
      label: 'Ace',
      max_uses: 1,
      uses: 0,
      revoked: false,
      works: true
    };
    open('#/invite/abc', server);
    await form();
    await type('Username', 'ace');
    await type('Password', PW);
    await press('Sign in');
    await waitFor(() => expect(screen.getByTestId('server-who')).toHaveTextContent('Ace'));
  });
});

describe('a forgotten password', () => {
  it('is set again with a re-connect link, for the same account', async () => {
    const server = withAce({ reconnects: { gfr_abc: 'acct_ace' } });
    const { people } = open('#/reconnect/gfr_abc', server);
    await form();
    expect(screen.getByTestId('reconnect-password')).toHaveTextContent(
      'Type your username and a new password.'
    );
    // A link is not signed in with: it only sets a way back in.
    expect(screen.getByRole('form', { name: 'Set the username and password' })).toBeTruthy();
    expect(screen.queryByRole('tab', { name: 'Create account' })).toBeNull();
    await type('Username', 'ace');
    await type('Password', PW2);
    await type('The same password again', PW2);
    await press('Set the username and password');
    await waitFor(() =>
      expect(people.passwordSignUp).toHaveBeenCalledWith({
        username: 'ace',
        password: PW2,
        invite: 'gfr_abc'
      })
    );
    // They land on how they sign in, as the account the link was for.
    await waitFor(() => expect(location.hash).toBe('#/account/sign-in'));
    await waitFor(() => expect(screen.getByTestId('server-who')).toHaveTextContent('Ace'));
    expect(server.passwords.ace.password).toBe(PW2);
    expect(server.reconnects.gfr_abc).toBeUndefined();
  });

  it('is not mentioned on a server with passwords off', async () => {
    open('#/reconnect/gfr_abc', fakeServer({ reconnects: { gfr_abc: 'acct_ace' } }));
    await screen.findByTestId('reconnect-for');
    expect(screen.queryByTestId('reconnect-password')).toBeNull();
    expect(screen.queryByTestId('password-form')).toBeNull();
  });
});

describe('a password on the account page', () => {
  const signIns = () => screen.getByRole('list', { name: 'How you sign in' });

  it('is added to an account that signs in another way', async () => {
    const { people } = open('#/account/sign-in', fakeServer({ account: ACE, passwordOn: true }));
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Add a username and password' })
    );
    await type('Username', 'ace');
    await type('Password', PW);
    await type('The same password again', PW);
    await press('Add the username and password');
    await waitFor(() =>
      expect(people.setPassword).toHaveBeenCalledWith({ username: 'ace', password: PW })
    );
    // It is listed with the others, named in words, and the form is gone.
    await waitFor(() => expect(signIns()).toHaveTextContent('Username and password'));
    expect(signIns()).toHaveTextContent('Discord');
    expect(screen.queryByTestId('password-form')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Add a username and password' })).toBeNull();
  });

  it('is not offered on a server with passwords off', async () => {
    open('#/account/sign-in', fakeServer({ account: ACE }));
    await screen.findByTestId('server-account');
    expect(screen.queryByRole('button', { name: 'Add a username and password' })).toBeNull();
  });

  it('is changed by giving the one there is', async () => {
    const { people, server } = open('#/account/sign-in', withAce({ account: ACE_PW }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Change password' }));
    // The username is not asked for: it stays.
    expect(screen.queryByLabelText('Username')).toBeNull();
    await type('The password you have now', 'not the password');
    await type('New password', PW2);
    await type('The same password again', PW2);
    await press('Change the password');
    expect(await screen.findByTestId('password-error')).toHaveTextContent(
      'That is not the password you have now.'
    );
    expect(server.passwords.ace.password).toBe(PW);

    await type('The password you have now', PW);
    await press('Change the password');
    await waitFor(() =>
      expect(people.setPassword).toHaveBeenLastCalledWith({ current: PW, password: PW2 })
    );
    await waitFor(() => expect(screen.queryByTestId('password-form')).toBeNull());
    expect(server.passwords.ace.password).toBe(PW2);
    expect(document.body.innerHTML).not.toContain(PW2);
  });

  it('closes the form on Cancel and changes nothing', async () => {
    const { people } = open('#/account/sign-in', withAce({ account: ACE_PW }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Change password' }));
    await type('New password', PW2);
    await press('Cancel');
    expect(screen.queryByTestId('password-form')).toBeNull();
    expect(people.setPassword).not.toHaveBeenCalled();
  });

  it('cannot be taken off when it is the only way in', async () => {
    open('#/account/sign-in', withAce({ account: ACE_PW }));
    await screen.findByTestId('server-account');
    expect(within(signIns()).queryByRole('button', { name: /^Remove/ })).toBeNull();
  });
});

describe('the owner switching passwords on and off', () => {
  const OWNER: MyAccount = { ...ACE, id: 'acct_oz', name: 'Oz', owner: true, clubs: [] };
  const setting = () => screen.findByTestId('password-setting');
  const flip = (card: HTMLElement) =>
    within(card).getByRole('switch', {
      name: 'Let people sign in with a password'
    }) as HTMLInputElement;

  it('is not shown to someone who is not an owner', async () => {
    open('#/admin/sign-in', fakeServer({ account: ACE }));
    await screen.findByTestId('server-who');
    expect(screen.queryByTestId('password-setting')).toBeNull();
  });

  it('starts off, and is switched on with one press', async () => {
    const { people, server } = open('#/admin/sign-in', fakeServer({ account: OWNER }));
    const card = await setting();
    await waitFor(() =>
      expect(within(card).getByTestId('password-state')).toHaveTextContent('Off')
    );
    await fireEvent.click(flip(card));
    await waitFor(() => expect(people.setPasswordSignIn).toHaveBeenCalledWith(true));
    await waitFor(() => expect(within(card).getByTestId('password-state')).toHaveTextContent('On'));
    expect(server.passwordOn).toBe(true);
    // The owner can now add one to their own account, with no reload.
    go('#/account/sign-in');
    expect(await screen.findByRole('button', { name: 'Add a username and password' })).toBeTruthy();
  });

  it('is switched off only after the owner is told who is locked out', async () => {
    const { people, server } = open(
      '#/admin/sign-in',
      fakeServer({ account: OWNER, passwordOn: true })
    );
    const card = await setting();
    await waitFor(() => expect(flip(card).checked).toBe(true));
    await fireEvent.click(flip(card));
    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveTextContent('Anyone whose only way in is a password is locked out');
    expect(people.setPasswordSignIn).not.toHaveBeenCalled();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(server.passwordOn).toBe(true);

    await fireEvent.click(flip(card));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Switch off' })
    );
    await waitFor(() => expect(people.setPasswordSignIn).toHaveBeenCalledWith(false));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(within(card).getByTestId('password-state')).toHaveTextContent('Off');
  });

  it('will not let an owner lock themselves out, and says why', async () => {
    const owner: MyAccount = {
      ...OWNER,
      sign_ins: [{ provider: 'password', subject: 'oz', label: 'oz' }]
    };
    const { server } = open('#/admin/sign-in', fakeServer({ account: owner, passwordOn: true }));
    const card = await setting();
    await waitFor(() => expect(flip(card).checked).toBe(true));
    await fireEvent.click(flip(card));
    const dialog = screen.getByRole('dialog');
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Switch off' }));
    expect(await within(dialog).findByText(/you would lock yourself out/)).toBeTruthy();
    expect(server.passwordOn).toBe(true);
  });
});
