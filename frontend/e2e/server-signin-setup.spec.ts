/**
 * Setting Discord up on a remote server's page, through real browsers
 * (docs/primitives.md §13.8). The server is the real `gridfpv-server` binary, with no
 * sign-in service in its environment.
 *
 * One story: the owner signs in with a password and sets Discord up on the page. People
 * are offered Discord at once, and the server sends them to Discord with the app the
 * owner gave. The owner changes the app without typing its secret again, then removes it.
 *
 * Nothing here talks to Discord: the test reads where the server would send a browser,
 * and stops there. The whole round trip is tested against a stand-in in the server's own
 * tests.
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Browser, type Page } from '@playwright/test';
import { openServerPage, openServerPart } from './serverPages.js';

import { startServer, type RemoteServer } from '../test-harness/server.js';

const dist = resolve(dirname(fileURLToPath(import.meta.url)), '..', 'apps', 'rd-console', 'dist');

let server: RemoteServer;

test.beforeAll(async () => {
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
const SECRET = 'e2e-app-secret-7Hq2';

/** Where the server sends a browser that starts signing in with Discord. */
async function startOf(page: Page): Promise<URL> {
  const answer = await page.request.get(`${server.baseUrl}/auth/discord/start`, {
    maxRedirects: 0
  });
  expect(answer.status()).toBe(303);
  return new URL(answer.headers()['location'], server.baseUrl);
}

test('an owner sets Discord up on the page, changes it, and removes it', async ({ browser }) => {
  test.setTimeout(120_000);
  server.run(['password', 'on']);

  // ── The owner makes their account with a password. ────────────────────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await ryan.getByRole('tab', { name: 'Create account' }).click();
  await ryan.getByLabel('Username', { exact: true }).fill('Ryan');
  await ryan.getByLabel('Password', { exact: true }).fill(OWNER_PW);
  await ryan.getByLabel('The same password again').fill(OWNER_PW);
  await ryan.getByRole('button', { name: 'Create account' }).click();
  await expect(ryan.getByTestId('server-who')).toHaveText('Ryan');

  // ── Nobody is offered Discord yet. ────────────────────────────────────────
  const visitor = await fresh(browser);
  await visitor.goto(`${server.baseUrl}/#/sign-in`);
  await expect(visitor.getByTestId('password-form')).toBeVisible();
  await expect(visitor.getByRole('link', { name: 'Continue with Discord' })).toHaveCount(0);
  expect((await startOf(visitor)).hash).toBe('#/sign-in?problem=not-set-up');

  // ── The owner sets it up. ─────────────────────────────────────────────────
  await openServerPart(ryan, 'Server');
  await openServerPage(ryan, 'Sign-in');
  const card = ryan.getByTestId('sign-in-services');
  const discord = card.getByTestId('sign-in-service').filter({ hasText: 'Discord' });
  const google = card.getByTestId('sign-in-service').filter({ hasText: 'Google' });
  await expect(discord.getByTestId('service-state')).toHaveText('Off');
  await expect(google.getByTestId('service-state')).toHaveText('Off');
  await discord.getByRole('button', { name: 'Set up Discord' }).click();
  const box = ryan.getByRole('dialog');
  // The address to give Discord is this server's own.
  await expect(box.getByTestId('service-redirect')).toHaveText(
    `${server.baseUrl}/auth/discord/callback`
  );
  await expect(box.getByRole('button', { name: 'Save' })).toBeDisabled();
  await box.getByLabel('Client ID').fill('112233445566778899');
  await box.getByLabel('Client secret').fill(SECRET);
  await box.getByRole('button', { name: 'Save' }).click();
  await expect(box).toHaveCount(0);
  await expect(discord.getByTestId('service-state')).toHaveText('On');
  await expect(google.getByTestId('service-state')).toHaveText('Off');

  // ── People are offered it at once, with no restart. ───────────────────────
  await visitor.reload();
  const offer = visitor.getByRole('link', { name: 'Continue with Discord' });
  await expect(offer).toBeVisible();
  await expect(visitor.getByRole('link', { name: 'Continue with Google' })).toHaveCount(0);
  expect(new URL((await offer.getAttribute('href'))!, server.baseUrl).pathname).toBe(
    '/auth/discord/start'
  );
  // The server sends the browser to Discord, with the owner's app and the way back.
  let to = await startOf(visitor);
  expect(`${to.origin}${to.pathname}`).toBe('https://discord.com/oauth2/authorize');
  expect(to.searchParams.get('client_id')).toBe('112233445566778899');
  expect(to.searchParams.get('redirect_uri')).toBe(`${server.baseUrl}/auth/discord/callback`);
  expect(to.searchParams.get('scope')).toBe('identify');
  // The secret goes to Discord only, never to a browser.
  expect(to.href).not.toContain(SECRET);

  // ── It is kept, and the secret is never shown again. ──────────────────────
  await ryan.reload();
  await expect(discord.getByTestId('service-state')).toHaveText('On');
  expect(await ryan.content()).not.toContain(SECRET);
  const listed = await ryan.request.get(`${server.baseUrl}/api/server/sign-in-services`);
  expect(await listed.text()).not.toContain(SECRET);
  // A visitor cannot read how it is set up.
  expect(
    (await visitor.request.get(`${server.baseUrl}/api/server/sign-in-services`)).status()
  ).toBe(401);

  // ── The client ID is changed. The secret box is left empty, and it is kept. ──
  await discord.getByRole('button', { name: 'Change Discord' }).click();
  await expect(box.getByLabel('Client ID')).toHaveValue('112233445566778899');
  await expect(box.getByLabel('Client secret')).toHaveValue('');
  await expect(box.getByLabel('Client secret')).toHaveAttribute('placeholder', 'Kept');
  await box.getByLabel('Client ID').fill('two words');
  await box.getByRole('button', { name: 'Save' }).click();
  await expect(box.getByTestId('service-error')).toContainText('does not look like a client ID');
  await box.getByLabel('Client ID').fill('998877665544332211');
  await box.getByRole('button', { name: 'Save' }).click();
  await expect(box).toHaveCount(0);
  to = await startOf(visitor);
  expect(to.searchParams.get('client_id')).toBe('998877665544332211');

  // ── Removed, after being asked. Discord is gone for everyone. ─────────────
  await discord.getByRole('button', { name: 'Change Discord' }).click();
  await box.getByRole('button', { name: 'Remove…' }).click();
  await expect(box).toContainText('Remove Discord?');
  await box.getByRole('button', { name: 'Remove', exact: true }).click();
  await expect(box).toHaveCount(0);
  await expect(discord.getByTestId('service-state')).toHaveText('Off');
  await visitor.reload();
  await expect(visitor.getByTestId('password-form')).toBeVisible();
  await expect(visitor.getByRole('link', { name: 'Continue with Discord' })).toHaveCount(0);
  expect((await startOf(visitor)).hash).toBe('#/sign-in?problem=not-set-up');
});
