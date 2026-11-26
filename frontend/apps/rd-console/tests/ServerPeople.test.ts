/**
 * The account pages of a remote server: signing in, an invite link, the account, and a
 * club's members and invites.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
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

function open(hash: string, server: FakeServer = fakeServer()) {
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
const invite = (over = {}) => ({
  id: 'inv',
  kind: 'Personal' as const,
  club: NORTH,
  role: 'Pilot' as const,
  label: 'Ace',
  max_uses: 1,
  uses: 0,
  revoked: false,
  works: true,
  ...over
});

describe('the top of the page', () => {
  it('offers Sign in to someone who is not signed in', async () => {
    open('#/');
    const link = await screen.findByTestId('server-sign-in');
    expect(link).toHaveTextContent('Sign in');
    expect(link.getAttribute('href')).toBe('#/sign-in');
    expect(screen.queryByTestId('server-who')).toBeNull();
  });

  it('shows who is signed in, linked to their account', async () => {
    open('#/', fakeServer({ account: ACE }));
    const who = await screen.findByTestId('server-who');
    expect(who).toHaveTextContent('Ace');
    expect(who.getAttribute('href')).toBe('#/account');
    expect(screen.queryByTestId('server-sign-in')).toBeNull();
  });

  it('offers nothing when the server has no way to sign in', async () => {
    const { people } = open('#/', fakeServer({ providers: [] }));
    await waitFor(() => expect(people.providers).toHaveBeenCalled());
    await waitFor(() => expect(people.me).toHaveBeenCalled());
    expect(screen.queryByTestId('server-sign-in')).toBeNull();
    expect(document.body.textContent).not.toMatch(/sign in/i);
  });
});

describe('signing in', () => {
  it('has a button for each provider, which goes to the server to start', async () => {
    open('#/sign-in');
    const discord = await screen.findByRole('link', { name: 'Continue with Discord' });
    expect(discord.getAttribute('href')).toBe('/auth/discord/start');
    expect(screen.getByRole('link', { name: 'Continue with Google' }).getAttribute('href')).toBe(
      '/auth/google/start'
    );
    // No box for a password anywhere: Grid never takes one.
    expect(document.querySelector('input[type="password"]')).toBeNull();
    expect(document.querySelector('input')).toBeNull();
  });

  it('says why the last try did not work', async () => {
    open('#/sign-in?problem=need-invite');
    expect(await screen.findByTestId('sign-in-problem')).toHaveTextContent(
      'You need an invite to make an account here.'
    );
    go('#/sign-in?problem=taken');
    await waitFor(() =>
      expect(screen.getByTestId('sign-in-problem')).toHaveTextContent('another account')
    );
    go('#/sign-in');
    await waitFor(() => expect(screen.queryByTestId('sign-in-problem')).toBeNull());
  });

  it('shows a code it has never heard of as plain words, not as what was in the address', async () => {
    open('#/sign-in?problem=%3Cimg%20src%3Dx%20onerror%3Dalert(1)%3E');
    expect(await screen.findByTestId('sign-in-problem')).toHaveTextContent(
      'Signing in did not work. Try again in a moment.'
    );
    expect(document.querySelector('img')).toBeNull();
  });

  it('says so when signing in is not set up', async () => {
    open('#/sign-in', fakeServer({ providers: [] }));
    expect(await screen.findByTestId('sign-in-off')).toBeVisible();
    expect(screen.queryByRole('link', { name: /Continue with/ })).toBeNull();
  });

  it('tells someone already signed in that they are', async () => {
    open('#/sign-in', fakeServer({ account: ACE }));
    expect(await screen.findByText(/You are signed in as/)).toBeVisible();
    expect(screen.queryByRole('link', { name: /Continue with/ })).toBeNull();
  });
});

describe('an invite link', () => {
  it('says what it is for, and sends someone new to sign in with the invite along', async () => {
    const server = fakeServer();
    server.links['gfi_abc'] = invite();
    const { people } = open('#/invite/gfi_abc', server);
    expect(await screen.findByTestId('invite-for')).toHaveTextContent(
      'You are invited to join North FPV'
    );
    expect(people.invite).toHaveBeenCalledWith('gfi_abc');
    const discord = screen.getByRole('link', { name: 'Continue with Discord' });
    expect(discord.getAttribute('href')).toBe('/auth/discord/start?invite=gfi_abc');
    expect(screen.queryByRole('button', { name: /Accept/ })).toBeNull();
    // Looking at it did not use it.
    expect(people.acceptInvite).not.toHaveBeenCalled();
  });

  it('names the role when it is more than a pilot, and an owner invite for what it is', async () => {
    const server = fakeServer();
    server.links['gfi_dir'] = invite({ role: 'Director' });
    server.links['gfi_own'] = { ...invite(), kind: 'Owner', club: undefined, role: undefined };
    open('#/invite/gfi_dir', server);
    expect(await screen.findByTestId('invite-for')).toHaveTextContent(
      'join North FPV as a director'
    );
    go('#/invite/gfi_own');
    await waitFor(() =>
      expect(screen.getByTestId('invite-for')).toHaveTextContent('be an owner of this server')
    );
  });

  it('lets someone who is signed in accept with the account they have', async () => {
    const server = fakeServer({ account: { ...ACE, clubs: [] } });
    server.links['gfi_abc'] = invite({ club: SOUTH });
    const { people } = open('#/invite/gfi_abc', server);
    await fireEvent.click(await screen.findByRole('button', { name: 'Accept as Ace' }));
    await waitFor(() => expect(people.acceptInvite).toHaveBeenCalledWith('gfi_abc'));
    // They land on their account, which now has the club.
    await waitFor(() => expect(location.hash).toBe('#/account'));
    go('#/account');
    expect(
      await within(await screen.findByRole('list', { name: 'Your clubs' })).findByText('South FPV')
    ).toBeVisible();
    expect(screen.queryByRole('link', { name: /Continue with/ })).toBeNull();
  });

  it('says so when the link does not work, and offers no way to use it', async () => {
    open('#/invite/gfi_used-up');
    expect(await screen.findByText('This invite does not work')).toBeVisible();
    expect(screen.queryByRole('link', { name: /Continue with/ })).toBeNull();
    expect(screen.queryByRole('button', { name: /Accept/ })).toBeNull();
  });

  it("shows the server's words when accepting is refused, and stays on the page", async () => {
    const server = fakeServer({ account: ACE });
    server.links['gfi_abc'] = invite();
    const { people } = open('#/invite/gfi_abc', server);
    people.acceptInvite.mockRejectedValueOnce(
      new Error('That invite does not work. It may have been used.')
    );
    await fireEvent.click(await screen.findByRole('button', { name: 'Accept as Ace' }));
    expect(await screen.findByText(/It may have been used/)).toBeVisible();
    expect(location.hash).toBe('#/invite/gfi_abc');
  });

  it('never shows the link itself on the page', async () => {
    const server = fakeServer();
    server.links['gfi_very-secret-link'] = invite();
    open('#/invite/gfi_very-secret-link', server);
    await screen.findByTestId('invite-for');
    expect(document.body.textContent).not.toContain('gfi_very-secret-link');
  });
});

describe('the account page', () => {
  it('shows the name, the clubs with the role in each, and how the account signs in', async () => {
    open(
      '#/account',
      fakeServer({
        account: {
          ...ACE,
          clubs: [
            { club: NORTH, role: 'Director' },
            { club: SOUTH, role: 'Pilot' }
          ]
        }
      })
    );
    expect(await screen.findByTestId('account-name')).toHaveTextContent('Ace');
    const clubs = within(screen.getByRole('list', { name: 'Your clubs' })).getAllByRole('listitem');
    expect(clubs.map((row) => row.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'North FPV Director',
      'South FPV Pilot'
    ]);
    const signIns = screen.getByRole('list', { name: 'How you sign in' });
    expect(signIns.textContent).toContain('Discord');
    expect(signIns.textContent).toContain('ace_fpv');
    // The provider's own id for the person is not something to show.
    expect(document.body.textContent).not.toContain('111');
  });

  it('links a director to the clubs they run, and to no others', async () => {
    open(
      '#/account',
      fakeServer({
        account: {
          ...ACE,
          clubs: [
            { club: NORTH, role: 'Director' },
            { club: SOUTH, role: 'Pilot' }
          ]
        }
      })
    );
    const run = await screen.findByRole('list', { name: 'Clubs you run' });
    const links = within(run).getAllByRole('link');
    expect(links).toHaveLength(1);
    expect(links[0]).toHaveTextContent('Members and invites of North FPV');
    expect(links[0].getAttribute('href')).toBe('#/club/north');
  });

  it('links an owner to every club, and says they are an owner', async () => {
    open('#/account', fakeServer({ account: { ...ACE, owner: true, clubs: [] } }));
    expect(await screen.findByText('You are an owner of this server.')).toBeVisible();
    const links = await waitFor(() => {
      const found = within(screen.getByRole('list', { name: 'Clubs you run' })).getAllByRole(
        'link'
      );
      expect(found).toHaveLength(2);
      return found;
    });
    expect(links.map((link) => link.getAttribute('href'))).toEqual([
      '#/club/north',
      '#/club/south'
    ]);
  });

  it('gives a pilot no club to run', async () => {
    open('#/account', fakeServer({ account: { ...ACE, clubs: [{ club: NORTH, role: 'Pilot' }] } }));
    await screen.findByTestId('account-name');
    expect(screen.queryByRole('list', { name: 'Clubs you run' })).toBeNull();
  });

  it('changes the name', async () => {
    const { people } = open('#/account', fakeServer({ account: ACE }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Change name' }));
    const box = screen.getByLabelText('Your name') as HTMLInputElement;
    expect(box.value).toBe('Ace');
    await fireEvent.input(box, { target: { value: '  Ace of Spades ' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(people.rename).toHaveBeenCalledWith('  Ace of Spades '));
    await waitFor(() =>
      expect(screen.getByTestId('account-name')).toHaveTextContent('Ace of Spades')
    );
    // The top of the page follows.
    expect(screen.getByTestId('server-who')).toHaveTextContent('Ace of Spades');
  });

  it('offers to add the providers the account does not sign in with yet', async () => {
    open('#/account', fakeServer({ account: ACE }));
    const add = await screen.findByRole('link', { name: 'Add Google' });
    expect(add.getAttribute('href')).toBe('/auth/google/start?add=1');
    expect(screen.queryByRole('link', { name: 'Add Discord' })).toBeNull();
  });

  it('will not offer to remove the only sign-in, and removes one of two', async () => {
    const two = {
      ...ACE,
      sign_ins: [...ACE.sign_ins, { provider: 'google', subject: 'g-1', label: 'ace@example.org' }]
    };
    const first = open('#/account', fakeServer({ account: ACE }));
    await screen.findByTestId('account-name');
    expect(screen.queryByRole('button', { name: /^Remove/ })).toBeNull();
    cleanup();

    const { people } = open('#/account', fakeServer({ account: two }));
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Remove Google ace@example.org' })
    );
    await waitFor(() => expect(people.removeSignIn).toHaveBeenCalledWith('google', 'g-1'));
    await waitFor(() => expect(screen.queryByRole('button', { name: /^Remove/ })).toBeNull());
    expect(first.people.removeSignIn).not.toHaveBeenCalled();
  });

  it('reads the account again on the way in, so a role given since is shown', async () => {
    const { server } = open(
      '#/',
      fakeServer({ account: { ...ACE, clubs: [{ club: NORTH, role: 'Pilot' }] } })
    );
    await screen.findByTestId('server-who');
    // A director gives them another role while the page is open.
    server.account = { ...server.account!, clubs: [{ club: NORTH, role: 'Approver' }] };
    go('#/account');
    await waitFor(() =>
      expect(screen.getByRole('list', { name: 'Your clubs' }).textContent).toContain('Approver')
    );
  });

  it('signs out, and goes back to the events', async () => {
    const { people } = open('#/account', fakeServer({ account: ACE }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Sign out' }));
    await waitFor(() => expect(people.signOut).toHaveBeenCalled());
    await waitFor(() => expect(location.hash).toBe('#/'));
    expect(screen.queryByTestId('server-who')).toBeNull();
    expect(await screen.findByTestId('server-sign-in')).toBeVisible();
  });

  it('asks someone who is not signed in to sign in', async () => {
    open('#/account');
    expect(await screen.findByRole('link', { name: 'Continue with Discord' })).toBeVisible();
    expect(screen.queryByTestId('server-account')).toBeNull();
  });
});

describe("a club's page", () => {
  const members = () => screen.getAllByTestId('club-member');
  const roleOf = (name: string) => screen.getByLabelText(`Role of ${name}`) as HTMLSelectElement;

  it('lists the members with their roles, and marks the one who is looking', async () => {
    open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    expect(members()[0].textContent).toContain('Ace');
    expect(members()[0].textContent).toContain('(you)');
    expect(roleOf('Ace').value).toBe('Director');
    expect(roleOf('Bee').value).toBe('Pilot');
    // People are shown by name. An account's id is not shown.
    expect(document.body.textContent).not.toContain('acct_');
  });

  it('gives a member another role', async () => {
    const { people, server } = open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    await fireEvent.change(roleOf('Bee'), { target: { value: 'Approver' } });
    await waitFor(() =>
      expect(people.setRole).toHaveBeenCalledWith('north', 'acct_bee', 'Approver')
    );
    await waitFor(() => expect(roleOf('Bee').value).toBe('Approver'));
    expect(server.members.north[1].role).toBe('Approver');
  });

  it("shows the server's words when the last director would be lost, and puts the role back", async () => {
    open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    await fireEvent.change(roleOf('Ace'), { target: { value: 'Pilot' } });
    expect(await screen.findByTestId('club-error')).toHaveTextContent(
      'A club must keep at least one director.'
    );
    await waitFor(() => expect(roleOf('Ace').value).toBe('Director'));
  });

  it('removes a member only after asking, in a dialog of the page', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const { people } = open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    await fireEvent.click(screen.getByRole('button', { name: 'Remove Bee' }));
    const dialog = screen.getByRole('dialog');
    expect(dialog.textContent).toContain('Remove Bee from North FPV?');
    expect(people.removeMember).not.toHaveBeenCalled();

    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(members()).toHaveLength(2);

    await fireEvent.click(screen.getByRole('button', { name: 'Remove Bee' }));
    await fireEvent.click(
      within(screen.getByRole('dialog')).getByRole('button', { name: 'Remove' })
    );
    await waitFor(() => expect(people.removeMember).toHaveBeenCalledWith('north', 'acct_bee'));
    await waitFor(() => expect(members()).toHaveLength(1));
    expect(confirm).not.toHaveBeenCalled();
  });

  it('makes an invite for one person and shows its link once', async () => {
    const { people } = open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    const makeButton = screen.getByRole('button', { name: 'Make the invite' }) as HTMLButtonElement;
    expect(makeButton.disabled).toBe(true);

    await fireEvent.input(screen.getByLabelText('Who it is for'), { target: { value: ' Cee ' } });
    await fireEvent.change(screen.getByLabelText('Joins as'), { target: { value: 'Approver' } });
    await fireEvent.click(makeButton);
    await waitFor(() =>
      expect(people.makeInvite).toHaveBeenCalledWith('north', {
        kind: 'Personal',
        label: 'Cee',
        role: 'Approver'
      })
    );
    const link = await screen.findByTestId('invite-link');
    expect(link.textContent).toBe(`${location.origin}/#/invite/gfi_secret-1`);
    expect(screen.getByTestId('invite-made').textContent).toContain('It is shown this once.');

    // It is in the list by what it is for, without the link.
    const row = screen.getByTestId('club-invite');
    expect(row.textContent).toContain('Cee');
    expect(row.textContent).toContain('Approver');
    expect(row.textContent).toContain('Not used yet');
    expect(row.textContent).not.toContain('gfi_');

    // Done: the link is gone from the page for good.
    await fireEvent.click(screen.getByRole('button', { name: 'Done' }));
    expect(screen.queryByTestId('invite-link')).toBeNull();
    expect(document.body.textContent).not.toContain('gfi_secret-1');
  });

  it('makes an invite for anyone with the link, with a limit and an end', async () => {
    const { people } = open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    await fireEvent.click(screen.getByRole('radio', { name: 'Anyone with the link' }));
    expect(screen.queryByLabelText('Joins as')).toBeNull();
    await fireEvent.input(screen.getByLabelText('Where you will post it'), {
      target: { value: 'Club Discord' }
    });
    await fireEvent.input(screen.getByLabelText('How many people'), { target: { value: '30' } });
    await fireEvent.input(screen.getByLabelText('Days it lasts'), { target: { value: '14' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Make the invite' }));
    await waitFor(() =>
      expect(people.makeInvite).toHaveBeenCalledWith('north', {
        kind: 'Group',
        label: 'Club Discord',
        max_uses: 30,
        days: 14
      })
    );
    const row = await screen.findByTestId('club-invite');
    expect(row.textContent).toContain('Anyone with the link');
    expect(row.textContent).toContain('0 of 30 used');
  });

  it('will not make a group invite with no limit or no end', async () => {
    const { people } = open('#/club/north', fakeServer({ account: ACE }));
    await waitFor(() => expect(members()).toHaveLength(2));
    await fireEvent.click(screen.getByRole('radio', { name: 'Anyone with the link' }));
    await fireEvent.input(screen.getByLabelText('Where you will post it'), {
      target: { value: 'Club Discord' }
    });
    const makeButton = screen.getByRole('button', { name: 'Make the invite' }) as HTMLButtonElement;
    for (const [uses, days] of [
      ['0', '7'],
      ['501', '7'],
      ['20', '0'],
      ['20', '91']
    ]) {
      await fireEvent.input(screen.getByLabelText('How many people'), { target: { value: uses } });
      await fireEvent.input(screen.getByLabelText('Days it lasts'), { target: { value: days } });
      expect(makeButton.disabled, `${uses} people, ${days} days`).toBe(true);
    }
    expect(people.makeInvite).not.toHaveBeenCalled();
  });

  it('takes an invite back, and it stays on the list as taken back', async () => {
    const server = fakeServer({ account: ACE });
    server.invites.north = [
      invite({ id: 'inv9', label: 'Dee' }),
      invite({ id: 'inv8', label: 'Old', uses: 1, works: false })
    ];
    const { people } = open('#/club/north', server);
    await waitFor(() => expect(screen.getAllByTestId('club-invite')).toHaveLength(2));
    // One that is used up has nothing to take back.
    expect(screen.queryByRole('button', { name: 'Take back the invite for Old' })).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Take back the invite for Dee' }));
    await waitFor(() => expect(people.revokeInvite).toHaveBeenCalledWith('north', 'inv9'));
    await waitFor(() =>
      expect(screen.getAllByTestId('club-invite')[0].textContent).toContain('Taken back')
    );
    expect(screen.queryByRole('button', { name: /Take back/ })).toBeNull();
  });

  it("shows the server's refusal when the one looking is not a director of the club", async () => {
    // South has no members list for this account: the server refuses.
    open('#/club/south', fakeServer({ account: ACE }));
    expect(await screen.findByText('Only a director of this club can do that.')).toBeVisible();
    expect(screen.queryByTestId('club-member')).toBeNull();
  });

  it('asks someone who is not signed in to sign in, and says so for a club that is not there', async () => {
    open('#/club/north');
    expect(await screen.findByRole('link', { name: 'Continue with Discord' })).toBeVisible();
    cleanup();
    open('#/club/nowhere', fakeServer({ account: ACE }));
    expect(await screen.findByText('This club is not here')).toBeVisible();
  });
});
