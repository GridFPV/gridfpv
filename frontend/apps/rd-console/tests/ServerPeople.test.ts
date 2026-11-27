/**
 * The account pages of a remote server: signing in, an invite link, the account, a
 * club's members, invites and pilots, and an account's results.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { PushClub, ServerEvent } from '@gridfpv/types';
import ServerHome from '../src/screens/ServerHome.svelte';
import { eventView } from './competition.js';
import {
  ACE,
  NORTH,
  SOUTH,
  clubPilot,
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
          ],
          primary: 'north'
        }
      })
    );
    expect(await screen.findByTestId('account-name')).toHaveTextContent('Ace');
    const clubs = within(screen.getByRole('list', { name: 'Your clubs' })).getAllByRole('listitem');
    // With two clubs, one is marked primary and the other can be made so.
    expect(clubs.map((row) => row.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'North FPV Director Primary',
      'South FPV Pilot Make primary'
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
    const run = await screen.findByRole('list', { name: 'Clubs you look after' });
    const links = within(run).getAllByRole('link');
    expect(links).toHaveLength(1);
    expect(links[0]).toHaveTextContent('Members, invites and pilots of North FPV');
    expect(links[0].getAttribute('href')).toBe('#/club/north');
  });

  it('links an owner to every club, and says they are an owner', async () => {
    open('#/account', fakeServer({ account: { ...ACE, owner: true, clubs: [] } }));
    expect(await screen.findByText('You are an owner of this server.')).toBeVisible();
    const links = await waitFor(() => {
      const found = within(screen.getByRole('list', { name: 'Clubs you look after' })).getAllByRole(
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
    expect(screen.queryByRole('list', { name: 'Clubs you look after' })).toBeNull();
  });

  it('links a claims approver to the pilots of their club, and to nothing more of it', async () => {
    open(
      '#/account',
      fakeServer({ account: { ...ACE, clubs: [{ club: NORTH, role: 'Approver' }] } })
    );
    const list = await screen.findByRole('list', { name: 'Clubs you look after' });
    const link = within(list).getByRole('link');
    expect(link).toHaveTextContent(/^Pilots of North FPV$/);
    expect(link.getAttribute('href')).toBe('#/club/north');
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
    expect(screen.queryByRole('button', { name: /Take back the invite/ })).toBeNull();
  });

  it('opens for nobody who does not look after the club, and asks the server nothing', async () => {
    // Ace is a director of North, and has no say in South.
    const { people } = open('#/club/south', fakeServer({ account: ACE }));
    expect(
      await screen.findByText('Only those who look after this club can open this page.')
    ).toBeVisible();
    expect(screen.queryByTestId('club-member')).toBeNull();
    expect(screen.queryByTestId('club-pilots')).toBeNull();
    expect(people.members).not.toHaveBeenCalled();
    expect(people.clubPilots).not.toHaveBeenCalled();
  });

  it("shows the server's refusal when the page thinks more of the account than the server does", async () => {
    // The page was told this account is a director of South. The server says otherwise.
    const account = { ...ACE, clubs: [{ club: SOUTH, role: 'Director' as const }] };
    open('#/club/south', fakeServer({ account }));
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

describe('an invite made for a pilot', () => {
  it('says on the link whose results come with it', async () => {
    open(
      '#/invite/gfi_x',
      fakeServer({ links: { gfi_x: invite({ pilot: { id: 'p-cat', callsign: 'Cat' } }) } })
    );
    const line = await screen.findByTestId('invite-pilot');
    expect(line).toHaveTextContent('This invite is for the pilot Cat.');
    expect(line).toHaveTextContent("puts Cat's race results on your account");
    // The pilot's id is a handle, not something to show.
    expect(document.body.textContent).not.toContain('p-cat');
  });

  it('says nothing of a pilot on an invite that is for none', async () => {
    open('#/invite/gfi_x', fakeServer({ links: { gfi_x: invite() } }));
    await screen.findByTestId('invite-for');
    expect(screen.queryByTestId('invite-pilot')).toBeNull();
  });

  it('is made by picking one of the pilots nobody holds', async () => {
    const server = fakeServer({ account: ACE });
    server.pilots.north = [
      clubPilot('p-cat', 'Cat'),
      clubPilot('p-dog', 'Dog', { holder: { id: 'acct_bee', name: 'Bee' } })
    ];
    const { people } = open('#/club/north', server);
    const box = (await screen.findByLabelText('Their pilot')) as HTMLSelectElement;
    // Dog is taken, so only Cat is offered.
    expect([...box.options].map((o) => o.textContent)).toEqual(['Not one of these', 'Cat']);
    await fireEvent.input(screen.getByLabelText('Who it is for'), { target: { value: 'Cat' } });
    await fireEvent.change(box, { target: { value: 'p-cat' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Make the invite' }));

    const made = await screen.findByTestId('invite-made');
    expect(made).toHaveTextContent("Whoever uses it gets Cat's results.");
    expect(people.makeInvite).toHaveBeenCalledWith('north', {
      kind: 'Personal',
      label: 'Cat',
      role: 'Pilot',
      pilot: 'p-cat'
    });
    expect(screen.getByTestId('club-invite')).toHaveTextContent('Pilot, as Cat');
    // The next invite starts with no pilot picked.
    expect((screen.getByLabelText('Their pilot') as HTMLSelectElement).value).toBe('');
  });

  it('sends no pilot when none is picked, and offers none for a group invite', async () => {
    const { people } = open('#/club/north', fakeServer({ account: ACE }));
    await screen.findByLabelText('Their pilot');
    await fireEvent.input(screen.getByLabelText('Who it is for'), { target: { value: 'New' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Make the invite' }));
    await screen.findByTestId('invite-made');
    expect(people.makeInvite).toHaveBeenCalledWith('north', {
      kind: 'Personal',
      label: 'New',
      role: 'Pilot'
    });
    await fireEvent.click(screen.getByLabelText('Anyone with the link'));
    expect(screen.queryByLabelText('Their pilot')).toBeNull();
  });
});

describe("a club's pilots", () => {
  const rows = () => screen.getAllByTestId('club-pilot');
  const withAsk = () => {
    const server = fakeServer({ account: ACE });
    server.pilots.north = [
      clubPilot('p-cat', 'Cat', { asks: [{ id: 'acct_bee', name: 'Bee' }] }),
      clubPilot('p-dog', 'Dog', { holder: { id: 'acct_bee', name: 'Bee' } })
    ];
    return server;
  };

  it('lists each pilot with who holds it, by name', async () => {
    open('#/club/north', withAsk());
    await waitFor(() => expect(rows()).toHaveLength(2));
    expect(rows()[0]).toHaveTextContent('Cat');
    expect(within(rows()[0]).getByTestId('pilot-holder')).toHaveTextContent('Nobody yet');
    expect(within(rows()[1]).getByTestId('pilot-holder')).toHaveTextContent('Bee');
    expect(document.body.textContent).not.toContain('acct_');
    expect(document.body.textContent).not.toContain('p-cat');
  });

  it('says so when the club has no pilots yet', async () => {
    const server = fakeServer({ account: ACE });
    server.pilots.north = [];
    open('#/club/north', server);
    expect(await screen.findByText(/No pilots yet\./)).toBeVisible();
  });

  it('answers an ask with yes, and the pilot is theirs', async () => {
    const { people, server } = open('#/club/north', withAsk());
    const ask = await screen.findByTestId('pilot-ask');
    expect(ask).toHaveTextContent('Bee says they are Cat');
    await fireEvent.click(screen.getByRole('button', { name: 'Yes, Bee is Cat' }));
    await waitFor(() => expect(screen.queryByTestId('pilot-ask')).toBeNull());
    expect(people.giveTo).toHaveBeenCalledWith('north', 'p-cat', 'acct_bee');
    expect(within(rows()[0]).getByTestId('pilot-holder')).toHaveTextContent('Bee');
    expect(server.pilots.north[0].holder).toEqual({ id: 'acct_bee', name: 'Bee' });
  });

  it('answers an ask with no, and nobody holds the pilot', async () => {
    const { people, server } = open('#/club/north', withAsk());
    await screen.findByTestId('pilot-ask');
    await fireEvent.click(screen.getByRole('button', { name: 'No, Bee is not Cat' }));
    await waitFor(() => expect(screen.queryByTestId('pilot-ask')).toBeNull());
    expect(people.refuseAsk).toHaveBeenCalledWith('north', 'p-cat', 'acct_bee');
    expect(people.giveTo).not.toHaveBeenCalled();
    expect(server.pilots.north[0].holder).toBeUndefined();
    expect(within(rows()[0]).getByTestId('pilot-holder')).toHaveTextContent('Nobody yet');
  });

  it('lets a director give a pilot to a member with no ask', async () => {
    const { people } = open('#/club/north', withAsk());
    const box = (await screen.findByLabelText('Give Cat to')) as HTMLSelectElement;
    expect([...box.options].map((o) => o.textContent)).toEqual(['Give to…', 'Ace', 'Bee']);
    // Dog is Bee's already, so Bee is not offered again for Dog.
    const dog = screen.getByLabelText('Give Dog to') as HTMLSelectElement;
    expect([...dog.options].map((o) => o.textContent)).toEqual(['Give to…', 'Ace']);

    await fireEvent.change(box, { target: { value: 'acct_ace' } });
    await waitFor(() =>
      expect(within(rows()[0]).getByTestId('pilot-holder')).toHaveTextContent('Ace')
    );
    expect(people.giveTo).toHaveBeenCalledWith('north', 'p-cat', 'acct_ace');
    // The box goes back to its prompt, ready for the next change.
    expect((screen.getByLabelText('Give Cat to') as HTMLSelectElement).value).toBe('');
  });

  it('takes a pilot back only after asking, in a dialog of the page', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const { people } = open('#/club/north', withAsk());
    await fireEvent.click(await screen.findByRole('button', { name: 'Take Dog back from Bee' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Take Dog back from Bee?');
    expect(people.freePilot).not.toHaveBeenCalled();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(people.freePilot).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: 'Take Dog back from Bee' }));
    await fireEvent.click(
      within(await screen.findByRole('dialog')).getByRole('button', { name: 'Take back' })
    );
    await waitFor(() =>
      expect(within(rows()[1]).getByTestId('pilot-holder')).toHaveTextContent('Nobody yet')
    );
    expect(people.freePilot).toHaveBeenCalledWith('north', 'p-dog');
    expect(confirm).not.toHaveBeenCalled();
  });

  it("shows the server's words when it refuses, and what is so afterwards", async () => {
    const { people } = open('#/club/north', withAsk());
    await screen.findByTestId('pilot-ask');
    people.giveTo.mockRejectedValueOnce(new Error('That pilot already belongs to an account.'));
    await fireEvent.click(screen.getByRole('button', { name: 'Yes, Bee is Cat' }));
    expect(await screen.findByTestId('pilots-error')).toHaveTextContent(
      'That pilot already belongs to an account.'
    );
    // Nothing was given: the ask is still there and the pilot has no holder.
    expect(screen.getByTestId('pilot-ask')).toBeVisible();
    expect(within(rows()[0]).getByTestId('pilot-holder')).toHaveTextContent('Nobody yet');
  });

  describe('for a claims approver', () => {
    const approver = () => {
      const server = withAsk();
      server.account = { ...ACE, clubs: [{ club: NORTH, role: 'Approver' }] };
      return server;
    };

    it('shows the pilots and the asks, and nothing of members or invites', async () => {
      const { people } = open('#/club/north', approver());
      await screen.findByTestId('pilot-ask');
      expect(screen.queryByTestId('club-member')).toBeNull();
      expect(screen.queryByRole('form', { name: 'New invite' })).toBeNull();
      // The server would refuse these to an approver, so they are not asked for.
      expect(people.members).not.toHaveBeenCalled();
      expect(people.clubInvites).not.toHaveBeenCalled();
    });

    it('answers an ask, and has no way to give or take with none', async () => {
      const { people } = open('#/club/north', approver());
      await screen.findByTestId('pilot-ask');
      expect(screen.queryByLabelText('Give Cat to')).toBeNull();
      expect(screen.queryByRole('button', { name: /^Take Dog back/ })).toBeNull();
      await fireEvent.click(screen.getByRole('button', { name: 'Yes, Bee is Cat' }));
      await waitFor(() => expect(screen.queryByTestId('pilot-ask')).toBeNull());
      expect(people.giveTo).toHaveBeenCalledWith('north', 'p-cat', 'acct_bee');
    });

    it('cannot answer their own ask', async () => {
      const server = approver();
      server.pilots.north[0].asks = [{ id: ACE.id, name: 'Ace' }];
      open('#/club/north', server);
      const ask = await screen.findByTestId('pilot-ask');
      expect(ask).toHaveTextContent('Someone else must answer your own ask.');
      expect(within(ask).queryByRole('button')).toBeNull();
    });
  });
});

describe('your pilots, on the account page', () => {
  const pilot = { ...ACE, clubs: [{ club: NORTH, role: 'Pilot' as const }] };
  const mine = () => screen.getAllByTestId('my-pilot');

  it('says there are none yet, and offers no results', async () => {
    open('#/account', fakeServer({ account: pilot }));
    const card = await screen.findByTestId('my-pilots');
    expect(card).toHaveTextContent('None yet.');
    expect(within(card).queryByRole('link', { name: 'Your results' })).toBeNull();
  });

  it('lists a pilot that is held, with a link to the results', async () => {
    const server = fakeServer({ account: pilot });
    server.pilots.north[0].holder = { id: ACE.id, name: 'Ace' };
    server.account = {
      ...pilot,
      pilots: [{ club: NORTH, id: 'p-cat', callsign: 'Cat', held: true }]
    };
    open('#/account', server);
    await waitFor(() => expect(mine()).toHaveLength(1));
    expect(mine()[0]).toHaveTextContent('Cat');
    expect(mine()[0]).toHaveTextContent('North FPV');
    expect(mine()[0]).not.toHaveTextContent('Waiting');
    expect(screen.getByRole('link', { name: 'Your results' }).getAttribute('href')).toBe(
      '#/results'
    );
    expect(document.body.textContent).not.toContain('p-cat');
  });

  it('finds a pilot in the club, asks for it, and then waits for a yes', async () => {
    const server = fakeServer({ account: pilot });
    server.pilots.north = [
      clubPilot('p-cat', 'Cat'),
      clubPilot('p-dog', 'Dog', { holder: { id: 'acct_bee', name: 'Bee' } })
    ];
    const { people } = open('#/account', server);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Find yourself in North FPV' })
    );
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Which pilot are you in North FPV?');
    // Dog belongs to someone, so it is not offered. A plain pilot is not told to whom.
    const offered = await within(dialog).findByRole('list', { name: 'Pilots of North FPV' });
    expect(within(offered).getAllByRole('listitem')).toHaveLength(1);
    expect(dialog).not.toHaveTextContent('Dog');
    expect(dialog).not.toHaveTextContent('Bee');

    await fireEvent.click(within(dialog).getByRole('button', { name: 'I am Cat' }));
    await waitFor(() => expect(mine()).toHaveLength(1));
    expect(people.askFor).toHaveBeenCalledWith('north', 'p-cat');
    expect(mine()[0]).toHaveTextContent('Cat');
    expect(mine()[0]).toHaveTextContent('Waiting for the club to say yes');
    // Asking gives no results.
    expect(screen.queryByRole('link', { name: 'Your results' })).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('does not offer again a pilot already asked for', async () => {
    const server = fakeServer({ account: pilot });
    server.pilots.north = [clubPilot('p-cat', 'Cat', { asks: [{ id: ACE.id, name: 'Ace' }] })];
    open('#/account', server);
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Find yourself in North FPV' })
    );
    const dialog = await screen.findByRole('dialog');
    expect(await within(dialog).findByText(/There is no pilot here to ask for\./)).toBeVisible();
  });

  it('stops asking', async () => {
    const server = fakeServer({ account: pilot });
    server.pilots.north[0].asks = [{ id: ACE.id, name: 'Ace' }];
    server.account = {
      ...pilot,
      pilots: [{ club: NORTH, id: 'p-cat', callsign: 'Cat', held: false }]
    };
    const { people } = open('#/account', server);
    await fireEvent.click(await screen.findByRole('button', { name: 'Stop asking for Cat' }));
    await waitFor(() => expect(screen.queryByTestId('my-pilot')).toBeNull());
    expect(people.dropClaim).toHaveBeenCalledWith('north', 'p-cat');
    expect(server.pilots.north[0].asks).toEqual([]);
  });

  it('lets go of a pilot only after asking, in a dialog of the page', async () => {
    const confirm = vi.spyOn(window, 'confirm');
    const server = fakeServer({ account: pilot });
    server.pilots.north[0].holder = { id: ACE.id, name: 'Ace' };
    server.account = {
      ...pilot,
      pilots: [{ club: NORTH, id: 'p-cat', callsign: 'Cat', held: true }]
    };
    const { people } = open('#/account', server);
    await fireEvent.click(await screen.findByRole('button', { name: 'Let go of Cat' }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('Let go of Cat?');
    expect(people.dropClaim).not.toHaveBeenCalled();
    await fireEvent.click(within(dialog).getByRole('button', { name: 'Let go' }));
    await waitFor(() => expect(screen.queryByTestId('my-pilot')).toBeNull());
    expect(people.dropClaim).toHaveBeenCalledWith('north', 'p-cat');
    expect(confirm).not.toHaveBeenCalled();
  });

  it("shows the server's words when asking is refused", async () => {
    const { people } = open('#/account', fakeServer({ account: pilot }));
    people.askFor.mockRejectedValueOnce(new Error('That pilot already belongs to an account.'));
    await fireEvent.click(
      await screen.findByRole('button', { name: 'Find yourself in North FPV' })
    );
    await fireEvent.click(await screen.findByRole('button', { name: 'I am Cat' }));
    expect(await screen.findByText('That pilot already belongs to an account.')).toBeVisible();
    expect(screen.queryByTestId('my-pilot')).toBeNull();
  });
});

describe('the results page', () => {
  const event = (id: string, name: string, over = {}) => ({
    id,
    name,
    created_at: 1,
    hidden: false,
    ...over
  });

  it('lists each event with where the pilot stood, linked to the event', async () => {
    const server = fakeServer({ account: ACE });
    server.results = [
      {
        club: NORTH,
        event: event('summer', 'Summer Race', { date: '2026-07-04', location: 'Main field' }),
        callsign: 'Cat',
        classes: [
          { class: 'Open', position: 2, of: 12, placed: true },
          { class: 'Spec', position: 1, of: 4, placed: false }
        ]
      },
      { club: NORTH, event: event('spring', 'Spring Race'), callsign: 'Kitten', classes: [] }
    ];
    open('#/results', server);
    const rows = await screen.findAllByTestId('result');
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent('Summer Race');
    expect(rows[0]).toHaveTextContent('2026-07-04 · Main field · North FPV');
    expect(rows[0]).toHaveTextContent('Flew as Cat');
    const places = within(rows[0]).getAllByTestId('result-place');
    expect(places[0]).toHaveTextContent('Open: 2nd of 12');
    // Racing has not placed anyone in Spec, so no place is claimed.
    expect(places[1]).toHaveTextContent('Spec: Not placed yet');
    expect(within(rows[0]).getByRole('link').getAttribute('href')).toBe('#/e/north/summer');
    // With no date or place, the line is the club alone.
    expect(rows[1]).toHaveTextContent('Flew as Kitten');
    expect(rows[1].textContent).not.toContain(' · ');
  });

  it('says so when there are none', async () => {
    open('#/results', fakeServer({ account: ACE }));
    expect(await screen.findByText('No results yet.')).toBeVisible();
  });

  it("shows the server's words when they cannot be read", async () => {
    const server = fakeServer({ account: ACE });
    const people = fakePeople(server);
    people.results.mockRejectedValue(new Error('The server could not read its accounts.'));
    location.hash = '#/results';
    render(ServerHome, {
      baseUrl: 'https://grid.example.org',
      load: { clubs: vi.fn(async () => [NORTH]), events: vi.fn(async () => []) },
      people,
      sourceFor: () => fakeLiveSession(eventView({}), { subscribeChanges: vi.fn(() => vi.fn()) })
    });
    expect(await screen.findByText('The server could not read its accounts.')).toBeVisible();
  });

  it('asks someone who is not signed in to sign in, and asks the server for nothing', async () => {
    const { people } = open('#/results');
    expect(await screen.findByRole('link', { name: 'Continue with Discord' })).toBeVisible();
    expect(people.results).not.toHaveBeenCalled();
  });
});
