/**
 * Accounts on a remote server, through real browsers (docs/primitives.md §13.2, §13.8).
 *
 * The server is the real `gridfpv-server` binary with its real page. The sign-in service is
 * a stand-in that holds it to the rules a real one would. Each person is a browser of
 * their own.
 *
 * One story, start to finish: the owner is made from the setup link, invites a director to
 * the club, the director invites a pilot, and each of them can do what their role allows
 * and no more.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Browser, type Page } from '@playwright/test';

import { startProvider, type FakeProvider, type Person } from '../test-harness/provider.js';
import { startServer, type RemoteServer } from '../test-harness/server.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let provider: FakeProvider;
let server: RemoteServer;

test.beforeAll(async () => {
  provider = await startProvider();
  server = await startServer('Recon FPV', dist, (baseUrl) => ({
    GRIDFPV_SERVER_PUBLIC_URL: baseUrl,
    ...provider.settings()
  }));
});
test.afterAll(async () => {
  await server?.stop();
  await provider?.stop();
});

const RYAN: Person = { sub: 'sub-ryan', name: 'Ryan' };
const DANA: Person = { sub: 'sub-dana', name: 'Dana' };
const ACE: Person = { sub: 'sub-ace', name: 'Ace' };

/** A browser of someone's own, with nothing in it. */
async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

/** Press the sign-in button as `who`: the stand-in service sends them straight back. */
async function continueAs(page: Page, who: Person): Promise<void> {
  provider.person = who;
  await page.getByRole('link', { name: 'Continue with Test login' }).click();
}

/** On a club's page: make an invite for one person, and give back its link. */
async function invite(page: Page, label: string, role: string): Promise<string> {
  await page.getByLabel('Who it is for').fill(label);
  await page.getByLabel('Joins as').selectOption(role);
  await page.getByRole('button', { name: 'Make the invite' }).click();
  const link = (await page.getByTestId('invite-link').innerText()).trim();
  await page.getByRole('button', { name: 'Done' }).click();
  await expect(page.getByTestId('invite-link')).toHaveCount(0);
  return link;
}

test('an owner, a director and a pilot each get their account by invite and their own powers', async ({
  browser
}) => {
  // ── Nobody has an account. Signing in with no invite makes none. ──────────
  const stranger = await fresh(browser);
  await stranger.goto(server.baseUrl);
  await stranger.getByTestId('server-sign-in').click();
  await continueAs(stranger, { sub: 'sub-stranger', name: 'Stranger' });
  await expect(stranger.getByTestId('sign-in-problem')).toContainText('You need an invite');
  await expect(stranger.getByTestId('server-who')).toHaveCount(0);

  // ── The owner: the setup link from the command line. ──────────────────────
  const printed = server.run(['owner', 'invite']);
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(printed)?.[0];
  expect(setup, printed).toBeTruthy();
  const ryan = await fresh(browser);
  await ryan.goto(setup!);
  await expect(ryan.getByTestId('invite-for')).toHaveText(
    'You are invited to be an owner of this server'
  );
  await continueAs(ryan, RYAN);
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');
  await ryan.getByTestId('server-who').click();
  await expect(ryan.getByText('You are an owner of this server.')).toBeVisible();

  // The setup link is spent.
  const late = await fresh(browser);
  await late.goto(setup!);
  await expect(late.getByText('This invite does not work')).toBeVisible();

  // ── The owner invites a director to the club. ─────────────────────────────
  await ryan.getByRole('link', { name: 'Members and invites of Recon FPV' }).click();
  await expect(ryan.getByTestId('server-club')).toBeVisible();
  await expect(ryan.getByTestId('club-member')).toHaveCount(0);
  const directorLink = await invite(ryan, 'Dana', 'Director');
  expect(directorLink).toContain(`${server.baseUrl}/#/invite/gfi_`);
  // The link is not on the page any more, only what it is for.
  await expect(ryan.getByTestId('club-invite')).toContainText('Dana');
  expect(await ryan.content()).not.toContain(directorLink.split('/').pop()!);

  const dana = await fresh(browser);
  await dana.goto(directorLink);
  await expect(dana.getByTestId('invite-for')).toHaveText(
    'You are invited to join Recon FPV as a director'
  );
  await continueAs(dana, DANA);
  await expect(dana.getByTestId('server-who')).toHaveText('Dana');

  // ── The director invites a pilot. ─────────────────────────────────────────
  await dana.getByTestId('server-who').click();
  await dana.getByRole('link', { name: 'Members and invites of Recon FPV' }).click();
  await expect(dana.getByTestId('club-member')).toHaveCount(1);
  await expect(dana.getByTestId('club-member')).toContainText('(you)');
  const pilotLink = await invite(dana, 'Ace', 'Pilot');

  const ace = await fresh(browser);
  await ace.goto(pilotLink);
  await expect(ace.getByTestId('invite-for')).toHaveText('You are invited to join Recon FPV');
  await continueAs(ace, ACE);
  await expect(ace.getByTestId('server-who')).toHaveText('Ace');

  // ── A pilot has no club to run, and the server refuses them if they try. ──
  await ace.getByTestId('server-who').click();
  await expect(ace.getByRole('list', { name: 'Your clubs' })).toContainText('Recon FPV');
  await expect(ace.getByRole('list', { name: 'Your clubs' })).toContainText('Pilot');
  await expect(ace.getByRole('list', { name: 'Clubs you run' })).toHaveCount(0);
  await ace.goto(`${server.baseUrl}/#/club/club`);
  await expect(ace.getByText('Only a director of this club can do that.')).toBeVisible();
  await expect(ace.getByTestId('club-member')).toHaveCount(0);
  const tried = await ace.request.post(`${server.baseUrl}/api/clubs/club/invites`, {
    data: { kind: 'Personal', label: 'Sneaky', role: 'Director' }
  });
  expect(tried.status()).toBe(403);

  // ── The director sees the pilot, and changes their role. ──────────────────
  await dana.reload();
  await expect(dana.getByTestId('club-member')).toHaveCount(2);
  await dana.getByLabel('Role of Ace').selectOption('Approver');
  await expect(dana.getByLabel('Role of Ace')).toHaveValue('Approver');
  await ace.goto(`${server.baseUrl}/#/account`);
  await expect(ace.getByRole('list', { name: 'Your clubs' })).toContainText('Approver');

  // The only director cannot make themselves a pilot: the server says why.
  await dana.getByLabel('Role of Dana').selectOption('Pilot');
  await expect(dana.getByTestId('club-error')).toContainText('at least one director');
  await expect(dana.getByLabel('Role of Dana')).toHaveValue('Director');

  // ── Signing out, and back in with no invite. ──────────────────────────────
  await ace.getByRole('button', { name: 'Sign out' }).click();
  await expect(ace.getByTestId('server-sign-in')).toBeVisible();
  await ace.goto(`${server.baseUrl}/#/account`);
  await expect(ace.getByTestId('server-account')).toHaveCount(0);
  await continueAs(ace, ACE);
  await expect(ace.getByTestId('server-who')).toHaveText('Ace');

  // The pilot's invite worked once: a second person cannot use it.
  const copycat = await fresh(browser);
  await copycat.goto(pilotLink);
  await expect(copycat.getByText('This invite does not work')).toBeVisible();
});

test('a way back from the sign-in service that this browser did not start signs nobody in', async ({
  browser
}) => {
  const printed = server.run(['owner', 'invite']);
  const secret = /#\/invite\/(\S+)/.exec(printed)![1];
  // An attacker starts a sign-in and captures the way back without following it.
  provider.person = { sub: 'sub-attacker', name: 'Attacker' };
  const theirs = (await browser.newContext()).request;
  const started = await theirs.get(`${server.baseUrl}/auth/oidc/start?invite=${secret}`, {
    maxRedirects: 0
  });
  const atProvider = await theirs.get(started.headers()['location'], { maxRedirects: 0 });
  const wayBack = atProvider.headers()['location'];
  expect(wayBack).toContain('/auth/oidc/callback?code=');

  // They send it to someone else, whose browser never started it.
  const victim = await fresh(browser);
  await victim.goto(wayBack);
  await expect(victim.getByTestId('sign-in-problem')).toContainText('Try again');
  await expect(victim.getByTestId('server-who')).toHaveCount(0);
  const me = (await (await victim.request.get(`${server.baseUrl}/api/me`)).json()) as object;
  expect(me).toEqual({});
});
