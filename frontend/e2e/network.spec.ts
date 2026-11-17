/**
 * Other devices on the network, through real browsers (docs/primitives.md §11).
 *
 * One browser is the director's, on the Grid computer. A second opens Grid by the
 * computer's network address, the way a phone at the field does. That second browser must
 * get the visitor's page and nothing of the console, and the Director must refuse whatever
 * it asks for directly.
 *
 * A second pair of tests boots a Director with an access token and signs the visitor in as
 * the director.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Page } from '@playwright/test';

import { type Director, startDirector } from '../test-harness/director.js';
import { gotoEvents, gotoPage, openPage, pagesNav } from './nav.js';
import { E2E_EVENT_NAME, expect, test } from './observability.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const dist = resolve(here, '..', 'apps', 'rd-console', 'dist');

/** This computer's address on its network. A build machine and a laptop both have one. */
const lan = Object.values(networkInterfaces())
  .flat()
  .find(
    (entry) =>
      entry && entry.family === 'IPv4' && !entry.internal && !entry.address.startsWith('169.254.')
  )?.address;

function freePort(): Promise<number> {
  return new Promise((done, reject) => {
    const srv = createServer();
    srv.on('error', reject);
    srv.listen(0, '0.0.0.0', () => {
      const address = srv.address();
      const found = typeof address === 'object' && address ? address.port : 0;
      srv.close(() => done(found));
    });
  });
}

const state = (page: Page) => page.getByTestId('network-state');

/** On the Network page: pick a free port, switch on, and give back the address shown. */
async function switchOn(page: Page): Promise<string> {
  const port = await freePort();
  await page.getByLabel('Port', { exact: true }).fill(String(port));
  await page.getByRole('button', { name: 'Change port' }).click();
  await expect(page.getByLabel('Port', { exact: true })).toHaveValue(String(port));
  await page.getByRole('button', { name: 'Let other devices open Grid' }).click();
  await expect(state(page)).toHaveText('On', { timeout: 15_000 });
  const address = `http://${lan}:${port}/`;
  await expect(
    page.getByRole('list', { name: 'Addresses for other devices' }).getByRole('link', {
      name: address
    })
  ).toBeVisible();
  return address;
}

test.skip(lan === undefined, 'this computer has no network address to come in by');

test('RD lets other devices in; a visitor sees the event and none of the console; RD shuts them out', async ({
  page,
  browser
}) => {
  // The event the visitor should see by name.
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();

  await gotoPage(page, 'Network');
  await expect(page).toHaveURL(/#\/network$/);
  await expect(state(page)).toHaveText('Off');
  await expect(page.getByText('Only this computer can open Grid.')).toBeVisible();

  const address = await switchOn(page);

  // ── The other device ───────────────────────────────────────────────────────────────────
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const visitor = await phone.newPage();
  const asked: string[] = [];
  visitor.on('request', (request) => asked.push(new URL(request.url()).pathname));
  try {
    await visitor.goto(address);
    await expect(visitor.getByTestId('visitor-event')).toHaveText(E2E_EVENT_NAME, {
      timeout: 15_000
    });
    await expect(visitor.getByText(/You can look\./)).toBeVisible();
    // None of the console: no page row, no side switch, no director sign-in (no token is set).
    await expect(pagesNav(visitor)).toHaveCount(0);
    await expect(visitor.getByRole('navigation', { name: 'Setup or event' })).toHaveCount(0);
    await expect(visitor.getByRole('button', { name: 'I am the director' })).toHaveCount(0);
    // The page asked the Director for the one public thing and nothing else of the API.
    expect(asked.filter((path) => !path.startsWith('/assets/') && path !== '/')).toEqual([
      '/public/hello'
    ]);

    // A console link typed by hand is still the visitor's page.
    for (const hash of ['#/events', '#/network', '#/event/run', '#/pilots']) {
      await visitor.goto(`${address}${hash}`);
      await visitor.reload();
      await expect(visitor.getByTestId('visitor-event')).toBeVisible();
      await expect(pagesNav(visitor)).toHaveCount(0);
    }

    // Asking the Director directly gets nowhere.
    for (const path of ['events', 'pilots', 'network', 'active-event', 'video/settings']) {
      const read = await visitor.request.get(`${address}${path}`);
      expect(read.status(), `GET /${path}`).toBe(401);
    }
    const write = await visitor.request.put(`${address}network`, { data: { enabled: false } });
    expect(write.status()).toBe(401);
    const made = await visitor.request.post(`${address}events`, { data: { name: 'Sneaky' } });
    expect(made.status()).toBe(401);
    // From inside the page too, where the browser adds its own headers.
    const fromPage = await visitor.evaluate(async () => {
      const res = await fetch('/events', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ name: 'Sneaky' })
      });
      return res.status;
    });
    expect(fromPage).toBe(401);

    // Nothing changed, and the director's page still says on.
    await expect(state(page)).toHaveText('On');
    const events = (await (await page.request.get('/events')).json()) as { name: string }[];
    expect(events.map((event) => event.name)).not.toContain('Sneaky');

    // ── Shut them out ────────────────────────────────────────────────────────────────────
    await page.getByRole('button', { name: 'Turn off' }).click();
    await expect(state(page)).toHaveText('Off');
    await expect(page.getByRole('list', { name: 'Addresses for other devices' })).toHaveCount(0);
    await expect
      .poll(
        async () =>
          visitor.request
            .get(`${address}public/hello`, { timeout: 2000 })
            .then(() => 'answered')
            .catch(() => 'refused'),
        { timeout: 10_000 }
      )
      .toBe('refused');
  } finally {
    await phone.close();
    // Leave the shared Director as it was found.
    await page.request.put('/network', { data: { enabled: false } });
  }
});

test.describe('with an access token set', () => {
  const TOKEN = 'e2e-director-token';
  let director: Director;

  test.beforeAll(async () => {
    director = await startDirector({ token: TOKEN, assets: dist });
  });
  test.afterAll(async () => {
    await director?.stop();
  });

  /** Switch the listener on over the API, as the director, and give back the address. */
  async function listen(): Promise<string> {
    const port = await freePort();
    const res = await fetch(`${director.baseUrl}/network`, {
      method: 'PUT',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
      body: JSON.stringify({ enabled: true, port })
    });
    expect(res.ok).toBe(true);
    await expect
      .poll(async () => (await (await fetch(`${director.baseUrl}/network`)).json()).state)
      .toBe('On');
    return `http://${lan}:${port}/`;
  }

  test('a director signs in from another device, runs the console there, and signs out', async ({
    browser
  }) => {
    const address = await listen();
    const tablet = await browser.newContext();
    const page = await tablet.newPage();
    try {
      await page.goto(address);
      await expect(page.getByTestId('visitor-event')).toHaveText('No event is open');

      // A wrong token is refused in the Director's words, and the page stays the visitor's.
      await page.getByRole('button', { name: 'I am the director' }).click();
      await page.getByLabel('Access token').fill('not-the-token');
      await page.getByRole('button', { name: 'Sign in' }).click();
      await expect(page.getByRole('alert')).toHaveText("That is not the director's access token.");
      await expect(pagesNav(page)).toHaveCount(0);

      // The right one opens the console.
      await page.getByLabel('Access token').fill(TOKEN);
      await page.getByRole('button', { name: 'Sign in' }).click();
      await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
      // The token is nowhere a script could read it: not in storage, not in a readable cookie.
      const kept = await page.evaluate(() => ({
        cookie: document.cookie,
        local: JSON.stringify(localStorage),
        session: JSON.stringify(sessionStorage)
      }));
      expect(JSON.stringify(kept)).not.toContain(TOKEN);
      expect(kept.cookie).not.toContain('gridfpv_director');

      // A write from the other device, with no token typed again.
      await page
        .getByRole('button', { name: /Create your first event|New event/ })
        .first()
        .click();
      const form = page.getByRole('form', { name: 'New event' });
      await form.getByLabel('Event name').fill('From the tablet');
      await page.getByRole('button', { name: 'Create & enter' }).click();
      await expect(page.getByRole('heading', { name: 'From the tablet', level: 1 })).toBeVisible({
        timeout: 15_000
      });
      await expect
        .poll(async () =>
          (
            (await (
              await fetch(`${director.baseUrl}/events`, {
                headers: { Authorization: `Bearer ${TOKEN}` }
              })
            ).json()) as { name: string }[]
          ).map((event) => event.name)
        )
        .toContain('From the tablet');

      // It survives a reload and a new tab, which have no memory of the token.
      await page.reload();
      await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
      const second = await tablet.newPage();
      await second.goto(address);
      await expect(pagesNav(second)).toBeVisible({ timeout: 15_000 });
      await second.close();

      // Sign out on the Network page: back to the visitor's page, in every tab.
      // By the nav, not by address: `goto('/')` would land on the worker's own Director.
      await openPage(page, 'Network');
      await page.getByRole('button', { name: 'Sign out on this device' }).click();
      await expect(page.getByTestId('visitor-event')).toBeVisible({ timeout: 15_000 });
      await expect(pagesNav(page)).toHaveCount(0);
      expect((await page.request.get(`${address}events`)).status()).toBe(401);
    } finally {
      await tablet.close();
    }
  });

  test('another browser does not share the sign-in', async ({ browser }) => {
    const address = await listen();
    const tablet = await browser.newContext();
    const phone = await browser.newContext();
    try {
      const signedIn = await tablet.newPage();
      await signedIn.goto(address);
      await signedIn.getByRole('button', { name: 'I am the director' }).click();
      await signedIn.getByLabel('Access token').fill(TOKEN);
      await signedIn.getByRole('button', { name: 'Sign in' }).click();
      await expect(pagesNav(signedIn)).toBeVisible({ timeout: 15_000 });

      const other = await phone.newPage();
      await other.goto(address);
      await expect(other.getByTestId('visitor-event')).toBeVisible({ timeout: 15_000 });
      await expect(pagesNav(other)).toHaveCount(0);
      expect((await other.request.get(`${address}events`)).status()).toBe(401);
    } finally {
      await tablet.close();
      await phone.close();
    }
  });
});
