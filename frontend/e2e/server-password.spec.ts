/**
 * A server with no Discord and no Google: everyone signs in with a username and a
 * password the server keeps (docs/primitives.md §13.8). Through real browsers, with the
 * real `gridfpv-server` binary and no sign-in service of any kind.
 *
 * One story: passwords are off on a new server. The owner switches them on from the
 * command line and makes their own account from the setup link. They let anyone make an
 * account. A pilot makes one, forgets the password, and gets back in with a re-connect
 * link the owner makes.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Browser, type Page } from '@playwright/test';

import { startServer, type RemoteServer } from '../test-harness/server.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let server: RemoteServer;

test.beforeAll(async () => {
  // No provider is set up. The server only knows its own address.
  server = await startServer('Recon FPV', dist, (baseUrl) => ({
    GRIDFPV_SERVER_PUBLIC_URL: baseUrl
  }));
});
test.afterAll(async () => {
  await server?.stop();
});

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

const OWNER_PW = 'four words make one';
const PIA_PW = 'pias first long password';
const PIA_NEW = 'pias second long password';

async function fill(page: Page, username: string, password: string, again = false) {
  await page.getByLabel('Username', { exact: true }).fill(username);
  await page.getByLabel('Password', { exact: true }).fill(password);
  if (again) await page.getByLabel('The same password again').fill(password);
}

test('a server with no sign-in service runs on usernames and passwords alone', async ({
  browser
}) => {
  test.setTimeout(120_000);

  // ── A new server has no way to sign in at all. ────────────────────────────
  expect(server.run(['password'])).toContain('Signing in with a password is off.');
  const early = await fresh(browser);
  await early.goto(`${server.baseUrl}/#/sign-in`);
  await expect(early.getByTestId('sign-in-off')).toBeVisible();
  await expect(early.getByTestId('password-form')).toHaveCount(0);

  // ── The owner switches passwords on, and makes their account from the setup link. ──
  expect(server.run(['password', 'on'])).toContain('People may sign in with a username');
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await ryan.getByRole('button', { name: 'Make one with a username and password' }).click();
  // What is wrong is said before anything is sent.
  await fill(ryan, 'Ryan', 'short');
  await expect(ryan.getByTestId('password-form').getByRole('alert')).toContainText('at least 10');
  await fill(ryan, 'Ryan', OWNER_PW, true);
  await ryan.getByRole('button', { name: 'Make the account' }).click();
  await expect(ryan.getByText('You are an owner of this server.')).toBeVisible();
  // The username is how the account signs in, in lower case. The password is nowhere.
  const signIns = ryan.getByRole('list', { name: 'How you sign in' });
  await expect(signIns).toContainText('Username and password');
  await expect(signIns).toContainText('ryan');
  expect(await ryan.content()).not.toContain(OWNER_PW);
  // The setup link worked once.
  const again = await fresh(browser);
  await again.goto(setup);
  await expect(again.getByText(/does not work/)).toBeVisible();

  // ── Signing out and in again. A wrong password says so, and no more. ──────
  await ryan.getByRole('button', { name: 'Sign out' }).click();
  await expect(ryan.getByTestId('server-sign-in')).toBeVisible();
  await ryan.getByTestId('server-sign-in').click();
  await fill(ryan, 'ryan', 'not the right password');
  await ryan.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
  await expect(ryan.getByTestId('password-error')).toHaveText(
    'That username and password do not match.'
  );
  await expect(ryan.getByLabel('Password', { exact: true })).toHaveValue('');
  await expect(ryan.getByTestId('server-who')).toHaveCount(0);
  // The username in any case, the password exactly.
  await fill(ryan, 'RYAN', OWNER_PW);
  await ryan.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');
  // Still signed in after the page is opened again.
  await ryan.reload();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');

  // ── The owner cannot switch passwords off: it is their only way in. ───────
  await ryan.getByTestId('server-who').click();
  const setting = ryan.getByTestId('password-setting');
  await expect(setting.getByTestId('password-state')).toHaveText('On');
  await setting.getByRole('button', { name: 'Switch passwords off…' }).click();
  await ryan.getByRole('dialog').getByRole('button', { name: 'Switch off' }).click();
  await expect(ryan.getByRole('dialog')).toContainText('you would lock yourself out');
  await ryan.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();
  await expect(setting.getByTestId('password-state')).toHaveText('On');

  // ── Nobody makes an account with no invite, until the owner lets anyone in. ──
  const pia = await fresh(browser);
  await pia.goto(`${server.baseUrl}/#/sign-in`);
  await expect(pia.getByTestId('password-form')).toBeVisible();
  await expect(pia.getByRole('button', { name: /Make one with a username/ })).toHaveCount(0);
  await ryan
    .getByTestId('sign-up-setting')
    .getByRole('button', { name: 'Let anyone make an account…' })
    .click();
  await ryan.getByRole('dialog').getByRole('button', { name: 'Let anyone in' }).click();
  await expect(ryan.getByTestId('sign-up-setting').getByTestId('sign-up-state')).toHaveText(
    'Anyone'
  );

  // ── A pilot makes her own account. ────────────────────────────────────────
  await pia.reload();
  await pia.getByRole('button', { name: 'Make one with a username and password' }).click();
  // A username someone has is refused, in words.
  await fill(pia, 'ryan', PIA_PW, true);
  await pia.getByRole('button', { name: 'Make the account' }).click();
  await expect(pia.getByTestId('password-error')).toContainText('already belongs to an account');
  await fill(pia, 'pia', PIA_PW, true);
  await pia.getByRole('button', { name: 'Make the account' }).click();
  await expect(pia.getByTestId('account-name')).toHaveText('pia');
  await expect(pia.getByText('You are not in a club yet.')).toBeVisible();
  await expect(pia.getByTestId('password-setting')).toHaveCount(0);
  // The owner's password was not changed by the try at their username.
  expect(server.run(['account', 'list'])).toContain('pia');

  // ── She changes her password. The old one stops working. ──────────────────
  await pia.getByRole('button', { name: 'Change password' }).click();
  await pia.getByLabel('The password you have now').fill(PIA_PW);
  await pia.getByLabel('New password').fill(PIA_NEW);
  await pia.getByLabel('The same password again').fill(PIA_NEW);
  await pia.getByRole('button', { name: 'Change the password' }).click();
  await expect(pia.getByTestId('password-form')).toHaveCount(0);
  const phone = await fresh(browser);
  await phone.goto(`${server.baseUrl}/#/sign-in`);
  await fill(phone, 'pia', PIA_PW);
  await phone.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
  await expect(phone.getByTestId('password-error')).toBeVisible();
  await fill(phone, 'pia', PIA_NEW);
  await phone.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
  // In no club yet, she lands on her account.
  await expect(phone.getByTestId('account-name')).toHaveText('pia');

  // ── She forgets it. The owner makes a re-connect link on the command line. ──
  const piasId = /^(acct_\S+)\s+pia\b/m.exec(server.run(['account', 'list']))![1];
  const link = /http:\/\/\S+\/#\/reconnect\/\S+/.exec(
    server.run(['account', 'reconnect', piasId])
  )![0];
  const lost = await fresh(browser);
  await lost.goto(link);
  await expect(lost.getByTestId('reconnect-for')).toHaveText('Get back in to pia');
  await expect(lost.getByTestId('reconnect-password')).toBeVisible();
  const third = 'the third long password';
  await fill(lost, 'pia', third, true);
  await lost.getByRole('button', { name: 'Set the username and password' }).click();
  await expect(lost.getByTestId('account-name')).toHaveText('pia');
  // It is the same account, not a second one.
  expect(
    server
      .run(['account', 'list'])
      .split('\n')
      .filter((l) => /\bpia\b/.test(l))
  ).toHaveLength(1);
  // The link worked once, and only the newest password signs in.
  const reuse = await fresh(browser);
  await reuse.goto(link);
  await expect(reuse.getByText(/does not work/)).toBeVisible();
  const check = await fresh(browser);
  await check.goto(`${server.baseUrl}/#/sign-in`);
  await fill(check, 'pia', PIA_NEW);
  await check.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
  await expect(check.getByTestId('password-error')).toBeVisible();
  await fill(check, 'pia', third);
  await check.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
  await expect(check.getByTestId('account-name')).toHaveText('pia');

  // ── Switched off on the command line, nobody signs in with a password. ────
  server.run(['password', 'off']);
  const late = await fresh(browser);
  await late.goto(`${server.baseUrl}/#/sign-in`);
  await expect(late.getByTestId('sign-in-off')).toBeVisible();
  const refused = await late.request.post(`${server.baseUrl}/api/sign-in/password`, {
    data: { username: 'pia', password: third }
  });
  expect(refused.status()).toBe(403);
});
