/**
 * Other devices on the network, through real browsers (docs/primitives.md §11.10).
 *
 * One browser is the director's, on the Grid computer. It switches on the two doors on the
 * Network page: **Public** and **Remote RD**, each on its own port.
 *
 * A second browser opens the Public address, the way a phone at the field does. It must get
 * the visitor's page and nothing of the console, the Director must refuse whatever it asks
 * for directly, and no token, cookie or page address may make it the director there.
 *
 * A third opens the Remote RD address, signs in with the access token read off the Network
 * page, and runs the console. Its sign-in must not carry to the Public address.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';

import type { Locator, Page } from '@playwright/test';

import { gotoEvents, gotoPage, openPage, pagesNav } from './nav.js';
import { E2E_EVENT_NAME, expect, test } from './observability.js';

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

type DoorName = 'Public' | 'Remote RD';

/** One door's card on the Network page. */
const card = (page: Page, door: DoorName): Locator =>
  page.getByTestId(door === 'Public' ? 'door-public' : 'door-remote-rd');
const toggle = (page: Page, door: DoorName): Locator =>
  page.getByRole('switch', { name: `Enable ${door}` });

/** On the Network page: give a door a free port, switch it on, and give back its address. */
async function switchOn(page: Page, door: DoorName): Promise<string> {
  const port = await freePort();
  const box = card(page, door).getByLabel('Port', { exact: true });
  await box.fill(String(port));
  await card(page, door).getByRole('button', { name: 'Change port' }).click();
  await expect(box).toHaveValue(String(port));
  await toggle(page, door).click();
  await expect(toggle(page, door)).toBeChecked();
  const shown = `${lan}:${port}`;
  await expect(
    page.getByRole('list', { name: `${door} addresses` }).getByRole('link', { name: shown })
  ).toBeVisible({ timeout: 15_000 });
  return `http://${shown}/`;
}

/** Shut both doors over the API, so the shared Director is left as it was found. */
async function shutBoth(page: Page) {
  for (const door of ['public', 'remote']) {
    await page.request.put(`/network/${door}`, { data: { enabled: false } });
  }
}

test.skip(lan === undefined, 'this computer has no network address to come in by');

test('Public: a visitor sees the event and none of the console, whatever they try; RD shuts them out', async ({
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
  await expect(toggle(page, 'Public')).not.toBeChecked();
  await expect(toggle(page, 'Remote RD')).not.toBeChecked();
  await expect(page.getByText('What other devices can do')).toHaveCount(0);

  const address = await switchOn(page, 'Public');
  // The other switch is its own: still off.
  await expect(toggle(page, 'Remote RD')).not.toBeChecked();

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
    await expect(visitor.getByRole('button', { name: 'Pilot', exact: true })).toBeVisible();
    // None of the console: no page row, no side switch, no word of the director.
    await expect(pagesNav(visitor)).toHaveCount(0);
    await expect(visitor.getByRole('navigation', { name: 'Setup or event' })).toHaveCount(0);
    await expect(visitor.getByText(/director/i)).toHaveCount(0);
    // The page asked the Director for public things only, and nothing of the console's API.
    const api = asked.filter((path) => !path.startsWith('/assets/') && path !== '/');
    expect(api).toContain('/public/hello');
    for (const path of api) {
      expect(path.startsWith('/public/'), `the visitor's page asked for ${path}`).toBe(true);
    }

    // A console link typed by hand is still the visitor's page. So is the address a
    // director used to sign in at: this door has no sign-in.
    for (const hash of ['#/events', '#/network', '#/event/run', '#/pilots', '#/director']) {
      await visitor.goto(`${address}${hash}`);
      await visitor.reload();
      await expect(visitor.getByTestId('visitor-event')).toBeVisible();
      await expect(pagesNav(visitor)).toHaveCount(0);
      await expect(visitor.getByLabel('Access token')).toHaveCount(0);
      await expect(visitor.getByText(/director/i)).toHaveCount(0);
    }

    // Asking the Director directly gets nowhere.
    for (const path of ['events', 'pilots', 'network', 'active-event', 'video/settings']) {
      const read = await visitor.request.get(`${address}${path}`);
      expect(read.status(), `GET /${path}`).toBe(401);
    }
    for (const door of ['public', 'remote']) {
      const write = await visitor.request.put(`${address}network/${door}`, {
        data: { enabled: door === 'remote' }
      });
      expect(write.status(), `PUT /network/${door}`).toBe(401);
    }
    expect((await visitor.request.put(`${address}network/token`, { data: {} })).status()).toBe(401);
    const made = await visitor.request.post(`${address}events`, { data: { name: 'Sneaky' } });
    expect(made.status()).toBe(401);
    // The director's sign-in does not answer here.
    const signIn = await visitor.request.post(`${address}public/session`, {
      headers: { Authorization: 'Bearer anything-at-all' }
    });
    expect(signIn.status()).toBe(401);
    expect(signIn.headers()['set-cookie']).toBeUndefined();
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
    await expect(toggle(page, 'Public')).toBeChecked();
    await expect(toggle(page, 'Remote RD')).not.toBeChecked();
    const events = (await (await page.request.get('/events')).json()) as { name: string }[];
    expect(events.map((event) => event.name)).not.toContain('Sneaky');

    // ── Shut them out ────────────────────────────────────────────────────────────────────
    await toggle(page, 'Public').click();
    await expect(toggle(page, 'Public')).not.toBeChecked();
    await expect(page.getByRole('list', { name: 'Public addresses' })).toHaveCount(0);
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
    await shutBoth(page);
  }
});

test('Remote RD: a director signs in with the token from the Network page, runs the console, and the sign-in stays at that door', async ({
  page,
  browser
}) => {
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  await gotoPage(page, 'Network');

  const publicAddress = await switchOn(page, 'Public');
  // No token until the door is first opened; then it is there to read.
  const remoteAddress = await switchOn(page, 'Remote RD');
  const token = (await page.getByTestId('access-token').innerText()).trim();
  expect(token).toMatch(/^[A-Z2-9]{4}-[A-Z2-9]{4}-[A-Z2-9]{4}$/);

  const tablet = await browser.newContext();
  const phone = await browser.newContext();
  try {
    // ── The Remote RD address is the sign-in, and nothing about the race ─────────────────
    const remote = await tablet.newPage();
    const asked: string[] = [];
    remote.on('request', (request) => asked.push(new URL(request.url()).pathname));
    await remote.goto(remoteAddress);
    await expect(remote.getByLabel('Access token')).toBeVisible({ timeout: 15_000 });
    await expect(remote.getByText(E2E_EVENT_NAME)).toHaveCount(0);
    await expect(remote.getByTestId('visitor-event')).toHaveCount(0);
    await expect(pagesNav(remote)).toHaveCount(0);
    // Before the token is shown, the Director gives nothing away at this door.
    for (const path of ['events', 'network', 'public/event', 'public/event/state']) {
      expect((await remote.request.get(`${remoteAddress}${path}`)).status(), path).toBe(401);
    }
    const hello = await (await remote.request.get(`${remoteAddress}public/hello`)).json();
    expect(hello.event).toBeUndefined();
    // The sign-in page asked for nothing it would be refused.
    expect(asked.filter((path) => path.startsWith('/public/event'))).toEqual([]);

    // A wrong token is refused in the Director's words, and the page stays the sign-in.
    await remote.getByLabel('Access token').fill('not-the-token');
    await remote.getByRole('button', { name: 'Sign in' }).click();
    await expect(remote.getByRole('alert')).toHaveText("That is not the director's access token.");
    await expect(pagesNav(remote)).toHaveCount(0);

    // The right one opens the console.
    await remote.getByLabel('Access token').fill(token);
    await remote.getByRole('button', { name: 'Sign in' }).click();
    await expect(pagesNav(remote)).toBeVisible({ timeout: 15_000 });
    // The token is nowhere a script could read it: not in storage, not in a readable cookie.
    const kept = await remote.evaluate(() => ({
      cookie: document.cookie,
      local: JSON.stringify(localStorage),
      session: JSON.stringify(sessionStorage)
    }));
    expect(JSON.stringify(kept)).not.toContain(token);
    expect(kept.cookie).not.toContain('gridfpv_director');

    // The console there is the whole one: it can read and change.
    expect((await remote.request.get(`${remoteAddress}events`)).status()).toBe(200);
    const made = await remote.request.post(`${remoteAddress}events`, {
      data: { name: 'From the tablet' },
      headers: { Origin: remoteAddress.replace(/\/$/, '') }
    });
    expect(made.ok()).toBe(true);

    // It survives a reload and a new tab, which have no memory of the token.
    await remote.reload();
    await expect(pagesNav(remote)).toBeVisible({ timeout: 15_000 });
    const second = await tablet.newPage();
    await second.goto(remoteAddress);
    await expect(pagesNav(second)).toBeVisible({ timeout: 15_000 });
    await second.close();

    // The Network page there shows both doors, but never the token it signed in with.
    await openPage(remote, 'Network');
    await expect(toggle(remote, 'Remote RD')).toBeChecked();
    await expect(remote.getByTestId('access-token')).toHaveCount(0);
    await expect(remote.getByText(token)).toHaveCount(0);
    const told = await (await remote.request.get(`${remoteAddress}network`)).text();
    expect(told).not.toContain(token);
    // And it cannot change the token.
    const changed = await remote.request.put(`${remoteAddress}network/token`, {
      data: { token: 'mine-now-thanks' },
      headers: { Origin: remoteAddress.replace(/\/$/, '') }
    });
    expect(changed.status()).toBe(403);

    // ── The same browser at the Public address is a visitor ──────────────────────────────
    // A browser sends its cookies to every port of an address, so this one arrives with
    // the director's sign-in. It must count for nothing there.
    const sameBrowser = await tablet.newPage();
    await sameBrowser.goto(publicAddress);
    await expect(sameBrowser.getByTestId('visitor-event')).toBeVisible({ timeout: 15_000 });
    await expect(pagesNav(sameBrowser)).toHaveCount(0);
    expect((await sameBrowser.request.get(`${publicAddress}events`)).status()).toBe(401);
    // Nor does the token itself, sent the way a script would.
    expect(
      (
        await sameBrowser.request.get(`${publicAddress}events`, {
          headers: { Authorization: `Bearer ${token}` }
        })
      ).status()
    ).toBe(401);
    await sameBrowser.close();

    // ── Another browser does not share the sign-in ───────────────────────────────────────
    const other = await phone.newPage();
    await other.goto(remoteAddress);
    await expect(other.getByLabel('Access token')).toBeVisible({ timeout: 15_000 });
    await expect(pagesNav(other)).toHaveCount(0);
    expect((await other.request.get(`${remoteAddress}events`)).status()).toBe(401);

    // ── A new token, made on the Grid computer, signs the tablet out ─────────────────────
    await page.getByRole('button', { name: 'Change…' }).click();
    const dialog = page.getByRole('dialog');
    await expect(dialog).toContainText('Every device signed in with the old token is signed out.');
    await dialog.getByRole('button', { name: 'Change', exact: true }).click();
    await expect(page.getByTestId('access-token')).not.toHaveText(token, { timeout: 15_000 });
    const next = (await page.getByTestId('access-token').innerText()).trim();
    await expect
      .poll(
        async () =>
          remote.request
            .get(`${remoteAddress}events`, { timeout: 2000 })
            .then((res) => res.status())
            .catch(() => 0),
        { timeout: 15_000 }
      )
      .toBe(401);
    await remote.goto(remoteAddress);
    await expect(remote.getByLabel('Access token')).toBeVisible({ timeout: 15_000 });
    // The old token no longer signs in; the new one does.
    await remote.getByLabel('Access token').fill(token);
    await remote.getByRole('button', { name: 'Sign in' }).click();
    await expect(remote.getByRole('alert')).toHaveText("That is not the director's access token.");
    await remote.getByLabel('Access token').fill(next);
    await remote.getByRole('button', { name: 'Sign in' }).click();
    await expect(pagesNav(remote)).toBeVisible({ timeout: 15_000 });

    // ── Sign out on the Network page: back to the sign-in ────────────────────────────────
    // By the nav, not by address: `goto('/')` would land on the worker's own Director.
    await openPage(remote, 'Network');
    await remote.getByRole('button', { name: 'Sign out on this device' }).click();
    await expect(remote.getByLabel('Access token')).toBeVisible({ timeout: 15_000 });
    await expect(pagesNav(remote)).toHaveCount(0);
    expect((await remote.request.get(`${remoteAddress}events`)).status()).toBe(401);

    // ── Switched off, the door is gone ───────────────────────────────────────────────────
    await toggle(page, 'Remote RD').click();
    await expect(toggle(page, 'Remote RD')).not.toBeChecked();
    await expect
      .poll(
        async () =>
          remote.request
            .get(`${remoteAddress}public/hello`, { timeout: 2000 })
            .then(() => 'answered')
            .catch(() => 'refused'),
        { timeout: 10_000 }
      )
      .toBe('refused');
    // The Public door is untouched by it.
    expect((await phone.request.get(`${publicAddress}public/hello`)).status()).toBe(200);
  } finally {
    await tablet.close();
    await phone.close();
    await shutBoth(page);
    // The event the tablet made is not left for the next spec to trip on.
    const events = (await (await page.request.get('/events')).json()) as {
      id: string;
      name: string;
    }[];
    for (const event of events.filter((event) => event.name === 'From the tablet')) {
      await page.request.delete(`/events/${event.id}`);
    }
  }
});
