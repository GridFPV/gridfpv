/**
 * Setting up a server's email on its own page, through a real browser
 * (docs/primitives.md §13.8).
 *
 * The server is the real `gridfpv-server` binary, started with no email settings at all.
 * Its owner gives it an email account on the Email page, and it then sends real mail over
 * a real connection to a stand-in email server that keeps what it is handed.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Browser, type Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

import { startServer, type RemoteServer } from '../test-harness/server.js';
import { linkIn, startMail, type FakeMail } from '../test-harness/smtp.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let mail: FakeMail;
let server: RemoteServer;

test.beforeAll(async () => {
  mail = await startMail();
  // No GRIDFPV_SERVER_SMTP_* at all: email is set up on the page or not at all.
  server = await startServer('Recon FPV', dist, (baseUrl) => ({
    GRIDFPV_SERVER_PUBLIC_URL: baseUrl
  }));
});
test.afterAll(async () => {
  await server?.stop();
  await mail?.stop();
});

async function fresh(browser: Browser): Promise<Page> {
  return (await browser.newContext()).newPage();
}

const OWNER_PW = 'the owners long password';

test('an owner sets up email on the page, tests it, and people can use it at once', async ({
  browser
}) => {
  test.setTimeout(120_000);
  server.run(['password', 'on']);

  // ── The owner makes their account. The server sends no email yet. ─────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await ryan.getByRole('tab', { name: 'Create account' }).click();
  await ryan.getByLabel('Username', { exact: true }).fill('Ryan');
  await ryan.getByLabel('Password', { exact: true }).fill(OWNER_PW);
  await ryan.getByLabel('The same password again').fill(OWNER_PW);
  await ryan.getByRole('button', { name: 'Create account' }).click();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');
  // With no email, there is no email card on the account, and no way to ask for a link.
  await openServerPage(ryan, 'Sign-in');
  await expect(ryan.getByRole('list', { name: 'How you sign in' })).toBeVisible();
  await expect(ryan.getByTestId('account-email')).toHaveCount(0);
  const visitor = await fresh(browser);
  await visitor.goto(`${server.baseUrl}/#/sign-in`);
  await expect(visitor.getByTestId('password-form')).toBeVisible();
  await expect(visitor.getByRole('button', { name: 'Forgot your password?' })).toHaveCount(0);

  // ── Server → Email. It is off. ────────────────────────────────────────────
  await openServerPart(ryan, 'Server');
  await openServerPage(ryan, 'Email');
  const setting = ryan.getByTestId('email-setting');
  await expect(setting.getByTestId('email-on')).toHaveText('Off');
  await expect(ryan.getByTestId('email-test')).toHaveCount(0);

  // What makes no sense is refused in words, and nothing is kept.
  await ryan.getByLabel('Server', { exact: true }).fill('smtp.example.org');
  await ryan.getByLabel('From', { exact: true }).fill('Grid Test <grid@example.org>');
  await ryan.getByLabel('Security').selectOption('none');
  await setting.getByRole('button', { name: 'Save' }).click();
  await expect(ryan.getByTestId('email-setting-error')).toContainText(
    'only for an email server on this same computer'
  );
  await expect(setting.getByTestId('email-on')).toHaveText('Off');

  // ── The stand-in email server, on this computer. ──────────────────────────
  await ryan.getByLabel('Server', { exact: true }).fill('127.0.0.1');
  await ryan.getByLabel('Port', { exact: true }).fill(String(mail.port));
  await setting.getByRole('button', { name: 'Save' }).click();
  await expect(setting.getByTestId('email-on')).toHaveText('On');
  await expect(setting.getByTestId('email-saved')).toBeVisible();

  // ── A test goes out for real. ─────────────────────────────────────────────
  const trial = ryan.getByTestId('email-test');
  await trial.getByLabel('Send a test to').fill('ryan@example.org');
  await trial.getByRole('button', { name: 'Send' }).click();
  await expect(trial.getByTestId('email-test-sent')).toHaveText('Sent to ryan@example.org.');
  await expect(trial).not.toContainText('JSON');
  const tested = await mail.next('ryan@example.org');
  expect(tested.subject).toContain('A test from your Grid server');

  // ── It holds after the page is opened again, and the command line agrees. ─
  await ryan.reload();
  await expect(ryan.getByTestId('email-setting').getByTestId('email-on')).toHaveText('On');
  await expect(ryan.getByLabel('Server', { exact: true })).toHaveValue('127.0.0.1');
  await expect(ryan.getByLabel('Port', { exact: true })).toHaveValue(String(mail.port));

  // ── People can use it at once: nothing was restarted. ─────────────────────
  await openServerPart(ryan, 'Account');
  await openServerPage(ryan, 'Sign-in');
  const card = ryan.getByTestId('account-email');
  await card.getByRole('button', { name: 'Add an email address' }).click();
  await card.getByLabel('Email address').fill('ryan@example.org');
  await card.getByRole('button', { name: 'Save' }).click();
  await expect(card.getByTestId('email-state')).toHaveText('Not confirmed');
  const confirmMail = await mail.next('ryan@example.org', 1);
  const confirmLink = linkIn(confirmMail);
  expect(confirmLink).toContain(`${server.baseUrl}/#/confirm-email/gfe_`);
  await ryan.goto(confirmLink);
  await expect(ryan.getByTestId('email-confirmed')).toHaveText('ryan@example.org is confirmed.');
  // And the sign-in page now offers a link for a forgotten password.
  await visitor.reload();
  await expect(visitor.getByRole('button', { name: 'Forgot your password?' })).toBeVisible();

  // ── A wrong setting: the test says why, in the email server's own words. ──
  await ryan.goto(`${server.baseUrl}/#/admin/server/email`);
  await ryan.getByLabel('Port', { exact: true }).fill('1');
  await ryan.getByTestId('email-setting').getByRole('button', { name: 'Save' }).click();
  await expect(ryan.getByTestId('email-saved')).toBeVisible();
  await ryan.waitForTimeout(5_500);
  await ryan.getByTestId('email-test').getByLabel('Send a test to').fill('ryan@example.org');
  await ryan.getByTestId('email-test').getByRole('button', { name: 'Send' }).click();
  await expect(ryan.getByTestId('email-test-error')).toContainText('did not take the mail');

  // ── Removed, after being asked. The server sends no email again. ──────────
  await ryan.getByTestId('email-setting').getByRole('button', { name: 'Remove…' }).click();
  await ryan.getByRole('dialog').getByRole('button', { name: 'Remove' }).click();
  await expect(ryan.getByRole('dialog')).toHaveCount(0);
  await expect(ryan.getByTestId('email-setting').getByTestId('email-on')).toHaveText('Off');
  await expect(ryan.getByTestId('email-test')).toHaveCount(0);
  await visitor.reload();
  await expect(visitor.getByTestId('password-form')).toBeVisible();
  await expect(visitor.getByRole('button', { name: 'Forgot your password?' })).toHaveCount(0);
  // Only the two mails ever went out.
  expect(mail.mails).toHaveLength(2);
});
