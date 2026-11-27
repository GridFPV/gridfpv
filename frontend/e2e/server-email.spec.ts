/**
 * Email on a remote server, through real browsers (docs/primitives.md §13.8).
 *
 * The server is the real `gridfpv-server` binary, sending real mail over a real connection
 * to a stand-in email server that keeps what it is handed.
 *
 * One story: a pilot makes an account with a username and password, gives her email
 * address, and confirms it with the link in the mail. She forgets her password, asks for
 * a link, and sets a new one with the link in the second mail. Nobody else is ever sent
 * anything.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Browser, type Page } from '@playwright/test';
import { openServerPage } from './serverPages.js';

import { startServer, type RemoteServer } from '../test-harness/server.js';
import { linkIn, startMail, type FakeMail } from '../test-harness/smtp.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let mail: FakeMail;
let server: RemoteServer;

test.beforeAll(async () => {
  mail = await startMail();
  server = await startServer('Recon FPV', dist, (baseUrl) => ({
    GRIDFPV_SERVER_PUBLIC_URL: baseUrl,
    ...mail.settings()
  }));
});
test.afterAll(async () => {
  await server?.stop();
  await mail?.stop();
});

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

const FIRST = 'pias first long password';
const SECOND = 'pias second long password';

async function signIn(page: Page, username: string, password: string) {
  await page.goto(`${server.baseUrl}/#/sign-in`);
  await page.getByLabel('Username', { exact: true }).fill(username);
  await page.getByLabel('Password', { exact: true }).fill(password);
  await page.getByTestId('password-form').getByRole('button', { name: 'Sign in' }).click();
}

test('a pilot confirms her email and sets a forgotten password with a link sent to it', async ({
  browser
}) => {
  test.setTimeout(120_000);
  server.run(['password', 'on']);
  server.run(['sign-up', 'open']);

  // ── She makes her account. ────────────────────────────────────────────────
  const pia = await fresh(browser);
  await pia.goto(`${server.baseUrl}/#/sign-in`);
  await pia.getByRole('tab', { name: 'Create account' }).click();
  await pia.getByLabel('Username', { exact: true }).fill('pia');
  await pia.getByLabel('Password', { exact: true }).fill(FIRST);
  await pia.getByLabel('The same password again').fill(FIRST);
  await pia.getByRole('button', { name: 'Create account' }).click();
  await expect(pia.getByTestId('account-name')).toHaveText('pia');

  // ── She gives her email address. A link is sent, and nothing else. ────────
  await openServerPage(pia, 'Sign-in');
  const card = pia.getByTestId('account-email');
  await card.getByRole('button', { name: 'Add an email address' }).click();
  await card.getByLabel('Email address').fill('not an address');
  await expect(card.getByRole('alert')).toContainText('not an email address');
  await card.getByLabel('Email address').fill('Pia@Example.org');
  await card.getByRole('button', { name: 'Save' }).click();
  await expect(card.getByTestId('email-state')).toHaveText('Not confirmed');
  await expect(card.getByTestId('email-address')).toHaveText('pia@example.org');
  await expect(card.getByTestId('email-waiting')).toContainText('A link is on its way.');
  const confirmMail = await mail.next('pia@example.org');
  expect(confirmMail.subject).toContain('Confirm your email');
  const confirmLink = linkIn(confirmMail);
  expect(confirmLink).toContain(`${server.baseUrl}/#/confirm-email/gfe_`);
  // The link is in the inbox only: never on the page.
  expect(await pia.content()).not.toContain('gfe_');

  // Until it is confirmed, asking for a password link sends nothing.
  const asked = await pia.request.post(`${server.baseUrl}/api/password/forgot`, {
    data: { who: 'pia' }
  });
  expect(asked.status()).toBe(204);
  await pia.waitForTimeout(500);
  expect(mail.mails).toHaveLength(1);

  // ── She opens the link, on another device that is not signed in. ──────────
  const phone = await fresh(browser);
  await phone.goto(confirmLink);
  await expect(phone.getByTestId('email-confirmed')).toHaveText('pia@example.org is confirmed.');
  // The link works once.
  await phone.reload();
  await expect(phone.getByTestId('email-link-dead')).toBeVisible();
  await pia.reload();
  await expect(pia.getByTestId('account-email').getByTestId('email-state')).toHaveText('Confirmed');

  // ── She forgets her password, and asks for a link. ────────────────────────
  const lost = await fresh(browser);
  await lost.goto(`${server.baseUrl}/#/sign-in`);
  await lost.getByLabel('Username', { exact: true }).fill('pia');
  await lost.getByRole('button', { name: 'Forgot your password?' }).click();
  const forgot = lost.getByTestId('forgot-form');
  await expect(forgot.getByLabel('Your username, or your email address')).toHaveValue('pia');
  await forgot.getByRole('button', { name: 'Send me a link' }).click();
  await expect(forgot.getByTestId('forgot-sent')).toContainText(
    'If that account has a confirmed email address, a link is on its way.'
  );
  const resetMail = await mail.next('pia@example.org', 1);
  expect(resetMail.subject).toContain('Set a new password');
  // The mail names her username, the other thing people forget.
  expect(resetMail.text).toContain('"pia"');
  const resetLink = linkIn(resetMail);
  expect(resetLink).toContain(`${server.baseUrl}/#/reconnect/gfr_`);

  // Someone naming a user who is not there is told the very same, and nothing is sent.
  const nosy = await fresh(browser);
  await nosy.goto(`${server.baseUrl}/#/sign-in`);
  await nosy.getByRole('button', { name: 'Forgot your password?' }).click();
  await nosy.getByLabel('Your username, or your email address').fill('nobody-at-all');
  await nosy.getByRole('button', { name: 'Send me a link' }).click();
  await expect(nosy.getByTestId('forgot-sent')).toHaveText(
    (await forgot.getByTestId('forgot-sent').innerText()).trim()
  );
  await nosy.waitForTimeout(500);
  expect(mail.mails).toHaveLength(2);

  // Her old password still works until she sets a new one.
  const still = await fresh(browser);
  await signIn(still, 'pia', FIRST);
  await expect(still.getByTestId('account-name')).toHaveText('pia');

  // ── She opens the link and sets a new password. ───────────────────────────
  await lost.goto(resetLink);
  await expect(lost.getByTestId('reconnect-for')).toHaveText('Get back in to pia');
  await lost.getByLabel('Username', { exact: true }).fill('pia');
  await lost.getByLabel('Password', { exact: true }).fill(SECOND);
  await lost.getByLabel('The same password again').fill(SECOND);
  await lost.getByRole('button', { name: 'Set the username and password' }).click();
  // She lands on how she signs in, as herself.
  await expect(lost).toHaveURL(/#\/account\/sign-in$/);
  await expect(lost.getByTestId('server-who')).toHaveText('pia');
  // The same account: her confirmed address is still on it.
  await expect(lost.getByTestId('account-email').getByTestId('email-state')).toHaveText(
    'Confirmed'
  );

  // Only the new password signs in, and the link worked once.
  const check = await fresh(browser);
  await signIn(check, 'pia', FIRST);
  await expect(check.getByTestId('password-error')).toBeVisible();
  await signIn(check, 'pia', SECOND);
  await expect(check.getByTestId('account-name')).toHaveText('pia');
  const reuse = await fresh(browser);
  await reuse.goto(resetLink);
  await expect(reuse.getByText(/does not work/)).toBeVisible();

  // ── She takes her address off. A link can no longer be sent. ──────────────
  await lost.getByTestId('account-email').getByRole('button', { name: 'Remove' }).click();
  await expect(
    lost.getByTestId('account-email').getByRole('button', { name: 'Add an email address' })
  ).toBeVisible();
});
