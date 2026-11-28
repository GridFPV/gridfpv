/**
 * Open sign-up and the pilot profile on a remote server, through real browsers
 * (docs/primitives.md §13.7).
 *
 * The server is the real `gridfpv-server` binary with its real page. The sign-in service is
 * a stand-in. Each person is a browser of their own.
 *
 * One story: a server asks for an invite until its owner says otherwise. The owner lets
 * anyone in. A pilot nobody invited signs in, gets an account in no club, and fills in
 * their pilot profile. The owner goes back to invites, and the next stranger gets nothing.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Browser, type Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

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

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

async function continueAs(page: Page, who: Person): Promise<void> {
  provider.person = who;
  await page.getByRole('link', { name: 'Continue with Test login' }).click();
}

test('an owner lets anyone make an account, and a pilot fills in their own profile', async ({
  browser
}) => {
  // ── A server starts by asking for an invite. ──────────────────────────────
  expect(server.run(['sign-up'])).toContain('An account needs an invite.');
  const early = await fresh(browser);
  await early.goto(`${server.baseUrl}/#/sign-in`);
  await expect(early.getByTestId('sign-up-line')).toHaveText('New here? You need an invite.');
  await continueAs(early, { sub: 'sub-early', name: 'Early' });
  await expect(early.getByTestId('sign-in-problem')).toContainText('You need an invite');

  // ── The owner lets anyone in, on the page. ────────────────────────────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await continueAs(ryan, { sub: 'sub-ryan', name: 'Ryan' });
  await openServerPart(ryan, 'Server');
  await openServerPage(ryan, 'Sign-in');
  const setting = ryan.getByTestId('sign-up-setting');
  await expect(setting.getByTestId('sign-up-state')).toHaveText('Invite only');
  await setting.getByRole('switch', { name: 'Let anyone make an account' }).click();
  const sure = ryan.getByRole('dialog');
  await expect(sure).toContainText('Let anyone make an account?');
  await sure.getByRole('button', { name: 'Let anyone in' }).click();
  await expect(sure).toHaveCount(0);
  await expect(setting.getByTestId('sign-up-state')).toHaveText('Anyone');
  // The command line says the same, and it holds after the page is opened again.
  expect(server.run(['sign-up'])).toContain('Anyone who can sign in may make an account.');
  await ryan.reload();
  await expect(ryan.getByTestId('sign-up-setting').getByTestId('sign-up-state')).toHaveText(
    'Anyone'
  );

  // ── A pilot nobody invited signs in, and has an account in no club. ───────
  const pia = await fresh(browser);
  await pia.goto(server.baseUrl);
  await pia.getByTestId('server-sign-in').click();
  // Nothing about invites now: signing in is all it takes.
  await expect(pia.getByRole('link', { name: 'Continue with Test login' })).toBeVisible();
  await expect(pia.getByTestId('sign-up-line')).toHaveCount(0);
  await continueAs(pia, { sub: 'sub-pia', name: 'Pia' });
  // She lands on her account, where the profile is.
  await expect(pia.getByTestId('account-name')).toHaveText('Pia');
  await expect.poll(() => new URL(pia.url()).hash).toBe('#/account');
  // She is an owner of nothing, and runs no club.
  await expect(pia.getByText('Server owner')).toHaveCount(0);
  await expect(pia.getByRole('navigation', { name: 'Area' })).toHaveCount(0);
  await pia.goto(`${server.baseUrl}/#/admin`);
  await expect(pia.getByTestId('admin-nothing')).toBeVisible();
  await pia.goto(`${server.baseUrl}/#/account`);

  // ── She fills in her pilot profile. ───────────────────────────────────────
  const profile = pia.getByTestId('pilot-profile');
  await expect(profile.getByTestId('profile-empty')).toContainText('Shown as Pia.');
  await profile.getByRole('button', { name: 'Fill it in' }).click();
  await pia.getByLabel('Colour', { exact: true }).fill('purple');
  await expect(profile.getByRole('alert')).toContainText('A colour is a # and six');
  await expect(profile.getByRole('button', { name: 'Save' })).toBeDisabled();
  await pia.getByLabel('Callsign').fill('PiaFPV');
  await pia.getByLabel('Colour', { exact: true }).fill('#aa00ff');
  await pia.getByLabel('MultiGP id').fill('24680');
  await pia.getByLabel('Velocidrone id').fill('pia.sim');
  await profile.getByRole('button', { name: 'Save' }).click();
  await expect(profile.getByTestId('profile-callsign')).toHaveText('PiaFPV');
  await expect(profile.getByTestId('profile-color')).toContainText('#AA00FF');
  await expect(profile.getByTestId('profile-multigp')).toHaveText('24680');
  await expect(profile.getByTestId('profile-velocidrone')).toHaveText('pia.sim');
  // It is kept: there after the page is opened again, and for a second browser of hers.
  await pia.reload();
  await expect(pia.getByTestId('pilot-profile').getByTestId('profile-callsign')).toHaveText(
    'PiaFPV'
  );
  const phone = await fresh(browser);
  await phone.goto(`${server.baseUrl}/#/sign-in`);
  await continueAs(phone, { sub: 'sub-pia', name: 'Pia' });
  await phone.getByTestId('server-who').click();
  await expect(phone.getByTestId('pilot-profile').getByTestId('profile-multigp')).toHaveText(
    '24680'
  );
  // The server made one account for her, not two.
  const accounts = server.run(['account', 'list']).split('\n');
  expect(accounts.filter((line) => line.includes('Pia'))).toHaveLength(1);

  // One part is cleared by emptying it.
  await phone.getByTestId('pilot-profile').getByRole('button', { name: 'Change' }).click();
  await phone.getByLabel('Velocidrone id').fill('');
  await phone.getByTestId('pilot-profile').getByRole('button', { name: 'Save' }).click();
  await expect(phone.getByTestId('pilot-profile').getByTestId('profile-velocidrone')).toHaveText(
    'Not set'
  );

  // ── The owner goes back to invites. The next stranger gets nothing. ───────
  await ryan
    .getByTestId('sign-up-setting')
    .getByRole('switch', { name: 'Let anyone make an account' })
    .click();
  await expect(ryan.getByTestId('sign-up-setting').getByTestId('sign-up-state')).toHaveText(
    'Invite only'
  );
  const late = await fresh(browser);
  await late.goto(`${server.baseUrl}/#/sign-in`);
  await expect(late.getByTestId('sign-up-line')).toContainText('You need an invite');
  await continueAs(late, { sub: 'sub-late', name: 'Late' });
  await expect(late.getByTestId('sign-in-problem')).toContainText('You need an invite');
  expect(server.run(['account', 'list'])).not.toContain('Late');
  // Pia's account, made while the door was open, still signs in.
  await pia.reload();
  await expect(pia.getByTestId('account-name')).toHaveText('Pia');

  // ── The command line opens it again. ──────────────────────────────────────
  expect(server.run(['sign-up', 'open'])).toContain('Anyone who can sign in');
  await late.goto(`${server.baseUrl}/#/sign-in`);
  await late.reload();
  await expect(late.getByRole('link', { name: 'Continue with Test login' })).toBeVisible();
  await expect(late.getByTestId('sign-up-line')).toHaveCount(0);
  await continueAs(late, { sub: 'sub-late', name: 'Late' });
  await expect(late.getByTestId('account-name')).toHaveText('Late');
});
