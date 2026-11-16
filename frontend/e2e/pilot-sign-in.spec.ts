/**
 * A pilot signs in at the race (docs/primitives.md §11.3), through real browsers.
 *
 * The director's browser is on the Grid computer and shows each pilot's code. A phone and
 * a tablet come in by the computer's network address. The pilot types the code on one and
 * follows the QR code's link on the other, and the rules are checked from both sides: one
 * device at a time, a new code kills the old one, and being a pilot opens nothing of the
 * director's.
 */
import { createServer } from 'node:net';
import { networkInterfaces } from 'node:os';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test, type Page } from '@playwright/test';

import { type Director, startDirector } from '../test-harness/director.js';
import { openPage, pagesNav } from './nav.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const dist = resolve(here, '..', 'apps', 'rd-console', 'dist');

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

let director: Director;
let address: string;
let event: string;

async function direct<T = unknown>(method: string, path: string, body?: unknown): Promise<T> {
  const res = await fetch(`${director.baseUrl}${path}`, {
    method,
    headers: body === undefined ? {} : { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body)
  });
  const text = await res.text();
  if (!res.ok) throw new Error(`${method} ${path} answered ${res.status}: ${text}`);
  return (text ? JSON.parse(text) : undefined) as T;
}

test.skip(lan === undefined, 'this computer has no network address to come in by');

test.beforeAll(async () => {
  director = await startDirector({ token: false, assets: dist });
  const ace = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Ace' })).id;
  const bee = (await direct<{ id: string }>('POST', '/pilots', { callsign: 'Bee' })).id;
  const open = (
    await direct<{ id: string }>('POST', '/classes', { name: 'Open', source: 'Custom' })
  ).id;
  event = (await direct<{ id: string }>('POST', '/events', { name: 'Friday Night Whoops' })).id;
  await direct('PUT', '/active-event', { id: event });
  await direct('POST', `/events/${event}/commands`, {
    Record: { RosterSet: { class: open, pilots: [ace, bee] } }
  });
  const port = await freePort();
  await direct('PUT', '/network', { enabled: true, port });
  await expect
    .poll(async () => (await direct<{ state: string }>('GET', '/network')).state)
    .toBe('On');
  address = `http://${lan}:${port}/`;
});

test.afterAll(async () => {
  await director?.stop();
});

const signedIn = (page: Page) => page.getByTestId('pilot-signed-in');
const iAmAPilot = (page: Page) => page.getByRole('button', { name: 'I am a pilot' });

async function typeCode(page: Page, code: string): Promise<void> {
  await iAmAPilot(page).click();
  await page.getByLabel('Your code').fill(code);
  await page.getByRole('button', { name: 'Sign in' }).click();
}

test('the director shows a code, a pilot signs in on one device at a time, and a new code ends the old', async ({
  page,
  browser
}) => {
  test.setTimeout(150_000);
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  const tablet = await browser.newContext({ viewport: { width: 820, height: 1180 } });
  try {
    // ── The director's side: no code on the screen until it is asked for ────────────────
    await page.goto(director.baseUrl);
    await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
    await openPage(page, 'Details', 'Friday Night Whoops');
    const card = page.locator('section').filter({ hasText: 'Pilot sign-in' }).last();
    await expect(page.getByRole('table', { name: 'Pilot sign-in codes' })).toHaveCount(0);
    await card.getByRole('button', { name: 'Show codes' }).click();
    const codes = page.getByRole('table', { name: 'Pilot sign-in codes' });
    const row = (name: string) => codes.locator('tbody tr').filter({ hasText: name });
    await expect(row('Ace')).toBeVisible();
    await expect(row('Bee')).toBeVisible();
    const codeOf = async (name: string) =>
      ((await row(name).getByTestId('pilot-code').textContent()) ?? '').replace(/\s/g, '');
    const aceCode = await codeOf('Ace');
    const beeCode = await codeOf('Bee');
    expect(aceCode).toMatch(/^[A-Z2-9]{6}$/);
    expect(aceCode).not.toBe(beeCode);

    // ── The QR code ──────────────────────────────────────────────────────────────────────
    await row('Ace').getByRole('button', { name: 'Show the QR code for Ace' }).click();
    const dialog = page.getByRole('dialog');
    const picture = dialog.getByRole('img', { name: 'QR code that signs Ace in' });
    await expect(picture).toBeVisible();
    await expect
      .poll(() => picture.evaluate((img: HTMLImageElement) => img.naturalWidth))
      .toBeGreaterThan(0);
    await expect(dialog).toContainText(address);
    await expect(dialog.getByTestId('qr-code-text')).toHaveText(
      `${aceCode.slice(0, 3)} ${aceCode.slice(3)}`
    );
    await dialog.getByRole('button', { name: 'Done' }).click();

    // ── The phone: the pilot types the code, any way round ───────────────────────────────
    const onPhone = await phone.newPage();
    await onPhone.goto(address);
    await expect(onPhone.getByTestId('visitor-event')).toHaveText('Friday Night Whoops', {
      timeout: 15_000
    });
    await typeCode(onPhone, `${aceCode.slice(0, 3).toLowerCase()}-${aceCode.slice(3)}`);
    await expect(signedIn(onPhone)).toHaveText(/Signed in as Ace/);
    // The director's list notices by itself.
    await expect(row('Ace')).toContainText('Signed in', { timeout: 15_000 });
    await expect(row('Bee')).not.toContainText('Signed in');
    // It lasts over a reload, and no script on the page can read the sign-in.
    await onPhone.reload();
    await expect(signedIn(onPhone)).toHaveText(/Signed in as Ace/, { timeout: 15_000 });
    expect(await onPhone.evaluate(() => document.cookie)).not.toContain('gridfpv_pilot');

    // Being a pilot opens nothing of the director's.
    for (const path of ['events', 'pilots', `events/${event}/access`, `events/${event}/state`]) {
      expect((await onPhone.request.get(`${address}${path}`)).status(), path).toBe(401);
    }
    const sneak = await onPhone.request.post(`${address}events/${event}/commands`, {
      data: { StartRace: { heat: 'x' } }
    });
    expect(sneak.status()).toBe(401);

    // ── The tablet: the pilot follows the QR code's link ─────────────────────────────────
    const onTablet = await tablet.newPage();
    await onTablet.goto(`${address}#/join/${aceCode}`);
    await expect(signedIn(onTablet)).toHaveText(/Signed in as Ace/, { timeout: 15_000 });
    // The code is gone from the address.
    expect(onTablet.url()).not.toContain(aceCode);
    expect(new URL(onTablet.url()).hash).toBe('');
    // One device at a time: the phone is signed out.
    await onPhone.reload();
    await expect(iAmAPilot(onPhone)).toBeVisible({ timeout: 15_000 });
    await expect(signedIn(onPhone)).toHaveCount(0);

    // ── Two pilots, side by side ─────────────────────────────────────────────────────────
    await typeCode(onPhone, beeCode);
    await expect(signedIn(onPhone)).toHaveText(/Signed in as Bee/);
    await onTablet.reload();
    await expect(signedIn(onTablet)).toHaveText(/Signed in as Ace/, { timeout: 15_000 });

    // ── A new code for Ace: the old one is dead, and the tablet is signed out ────────────
    await row('Ace').getByRole('button', { name: 'Give Ace a new code' }).click();
    await page.getByRole('dialog').getByRole('button', { name: 'New code' }).click();
    await expect.poll(() => codeOf('Ace')).not.toBe(aceCode);
    const newCode = await codeOf('Ace');
    expect(await codeOf('Bee')).toBe(beeCode);
    await onTablet.reload();
    await expect(iAmAPilot(onTablet)).toBeVisible({ timeout: 15_000 });
    await typeCode(onTablet, aceCode);
    await expect(onTablet.getByRole('alert')).toHaveText(
      'That code is not right. Check it with the director.'
    );
    await onTablet.getByLabel('Your code').fill(newCode);
    await onTablet.getByRole('button', { name: 'Sign in' }).click();
    await expect(signedIn(onTablet)).toHaveText(/Signed in as Ace/);
    // Bee was not touched by any of that.
    await onPhone.reload();
    await expect(signedIn(onPhone)).toHaveText(/Signed in as Bee/, { timeout: 15_000 });

    // ── The director signs Bee's phone out ───────────────────────────────────────────────
    await expect(row('Bee')).toContainText('Signed in', { timeout: 15_000 });
    await row('Bee').getByRole('button', { name: 'Sign Bee out' }).click();
    await expect(row('Bee')).not.toContainText('Signed in', { timeout: 15_000 });
    await onPhone.reload();
    await expect(iAmAPilot(onPhone)).toBeVisible({ timeout: 15_000 });

    // ── The pilot signs out themselves ───────────────────────────────────────────────────
    await onTablet.getByRole('button', { name: 'Sign out' }).click();
    await expect(iAmAPilot(onTablet)).toBeVisible();
    await expect(row('Ace')).not.toContainText('Signed in', { timeout: 15_000 });

    // ── Guessing: after ten wrong codes, the device has to wait ──────────────────────────
    await iAmAPilot(onPhone).click();
    for (let n = 0; n < 10; n += 1) {
      await onPhone.getByLabel('Your code').fill('AAA222');
      await onPhone.getByRole('button', { name: 'Sign in' }).click();
      await expect(onPhone.getByRole('alert')).toHaveText(/That code is not right/);
    }
    await onPhone.getByLabel('Your code').fill(beeCode);
    await onPhone.getByRole('button', { name: 'Sign in' }).click();
    await expect(onPhone.getByRole('alert')).toHaveText(
      'Too many wrong codes from this device. Wait a minute and try again.'
    );
    await expect(signedIn(onPhone)).toHaveCount(0);
    // It can still watch.
    await expect(onPhone.getByTestId('visitor-event')).toHaveText('Friday Night Whoops');
  } finally {
    await phone.close();
    await tablet.close();
  }
});
