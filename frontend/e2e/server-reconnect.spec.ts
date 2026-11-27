/**
 * Getting back in when a sign-in is lost, through real browsers (docs/primitives.md
 * §13.8).
 *
 * The server is the real `gridfpv-server` binary with its real page, and people sign in
 * through a stand-in sign-in service. No Director is needed.
 *
 * One story: a pilot joins with an invite. The owner takes the pilot's sign-in off, and
 * the pilot is out at once and cannot get back in with it. The owner makes a re-connect
 * link. The pilot opens it, signs in as someone the server has never seen, and is in the
 * same account. The link works once. Then the same from the command line, which is how
 * an owner who is locked out gets back in.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Browser, Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

import { startProvider, type FakeProvider } from '../test-harness/provider.js';
import { startServer, type RemoteServer } from '../test-harness/server.js';
import { expect, test } from './observability.js';

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

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

const linkIn = (text: string, kind: 'invite' | 'reconnect'): string =>
  new RegExp(`http://\\S+/#/${kind}/\\S+`).exec(text)![0];

test('an owner gets a pilot who lost their sign-in back into the same account', async ({
  browser
}) => {
  test.setTimeout(120_000);

  // ── The owner signs in, and invites Mateo. ────────────────────────────────
  const ryan = await fresh(browser);
  await ryan.goto(linkIn(server.run(['owner', 'invite']), 'invite'));
  provider.person = { sub: 'sub-ryan', name: 'Ryan' };
  await ryan.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');
  await ryan.getByTestId('server-who').click();
  await openServerPart(ryan, 'Club');
  await openServerPage(ryan, 'Invites');
  await ryan.getByLabel('Who it is for').fill('Mateo');
  await ryan.getByRole('button', { name: 'Make the invite' }).click();
  const invite = (await ryan.getByTestId('invite-link').innerText()).trim();

  const mateo = await fresh(browser);
  await mateo.goto(invite);
  provider.person = { sub: 'sub-mateo', name: 'Mateo' };
  await mateo.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(mateo.getByTestId('server-who')).toHaveText('Mateo');

  // ── The owner takes Mateo's sign-in off. It was the only one. ─────────────
  await ryan.reload();
  await openServerPage(ryan, 'Members');
  await ryan.getByRole('button', { name: 'How Mateo signs in' }).click();
  const help = ryan.getByRole('dialog');
  await expect(help.getByTestId('member-sign-in')).toHaveCount(1);
  await expect(help.getByTestId('member-sign-in')).toContainText('Test login');
  await help.getByRole('button', { name: /^Take off Test login/ }).click();
  await expect(help.getByTestId('drop-sure')).toContainText("off Mateo's account?");
  await expect(help).toContainText('It is their only one');
  await help.getByRole('button', { name: 'Take it off' }).click();
  await expect(help.getByTestId('no-sign-ins')).toHaveText(
    'Mateo has no way to sign in. Send them a re-connect link.'
  );

  // Mateo is out at once, and the old sign-in opens nothing.
  await mateo.reload();
  await expect(mateo.getByTestId('server-sign-in')).toBeVisible();
  await mateo.getByTestId('server-sign-in').click();
  await mateo.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(mateo.getByTestId('sign-in-problem')).toContainText('You need an invite');
  await expect(mateo.getByTestId('server-who')).toHaveCount(0);

  // ── The owner makes a re-connect link. It is shown once. ──────────────────
  await help.getByRole('button', { name: 'Make a re-connect link' }).click();
  const link = (await help.getByTestId('reconnect-link').innerText()).trim();
  expect(link).toMatch(/\/#\/reconnect\/gfr_[A-Za-z0-9_-]{20,}$/);
  await expect(help.getByTestId('reconnect-waiting')).toContainText('A link is waiting, until');
  await help.getByRole('button', { name: 'Done' }).click();
  await ryan.getByRole('button', { name: 'How Mateo signs in' }).click();
  await expect(help.getByTestId('reconnect-waiting')).toBeVisible();
  expect(await ryan.content()).not.toContain(link);
  await help.getByRole('button', { name: 'Done' }).click();

  // ── Mateo opens it and signs in as someone the server has never seen. ─────
  await mateo.goto(link);
  await expect(mateo.getByTestId('reconnect-for')).toHaveText('Get back in to Mateo');
  provider.person = { sub: 'sub-mateo-new', name: 'Mateo on Google' };
  await mateo.getByRole('link', { name: 'Continue with Test login' }).click();
  // The same account: its name, and its place in the club.
  await expect(mateo.getByTestId('server-who')).toHaveText('Mateo');
  await expect(mateo).toHaveURL(/#\/account\/sign-in$/);
  await expect(mateo.getByTestId('server-account')).toContainText('Mateo on Google');
  await openServerPage(mateo, 'Clubs');
  await expect(mateo.getByTestId('server-account')).toContainText('Recon FPV');
  // The members list still has one Mateo, not two.
  await ryan.reload();
  await expect(ryan.getByTestId('club-member').filter({ hasText: 'Mateo' })).toHaveCount(1);
  await ryan.getByRole('button', { name: 'How Mateo signs in' }).click();
  await expect(help.getByTestId('member-sign-in')).toContainText('Mateo on Google');
  await expect(help.getByTestId('reconnect-waiting')).toHaveCount(0);
  await help.getByRole('button', { name: 'Done' }).click();

  // The link worked once.
  const late = await fresh(browser);
  await late.goto(link);
  await expect(late.getByText('This link does not work')).toBeVisible();
  await expect(late.getByRole('link', { name: /Continue with/ })).toHaveCount(0);

  // ── The same from the command line, for an owner who is locked out. ───────
  const listed = server.run(['account', 'list']);
  const row = listed.split('\n').find((line) => /\bRyan\b/.test(line))!;
  expect(row).toContain('owner');
  expect(listed).toMatch(/\bMateo\b.*Mateo on Google/);
  const id = row.trim().split(/\s+/)[0];
  const fromShell = linkIn(server.run(['account', 'reconnect', id]), 'reconnect');
  const laptop = await fresh(browser);
  await laptop.goto(fromShell);
  await expect(laptop.getByTestId('reconnect-for')).toHaveText('Get back in to Ryan');
  provider.person = { sub: 'sub-ryan-new', name: 'Ryan again' };
  await laptop.getByRole('link', { name: 'Continue with Test login' }).click();
  await expect(laptop.getByTestId('server-who')).toHaveText('Ryan');
  // Still an owner: the club page opens, with the owner's button.
  await laptop.getByTestId('server-who').click();
  await openServerPart(laptop, 'Club');
  await expect(laptop.getByRole('button', { name: 'How Mateo signs in' })).toBeVisible();
});
