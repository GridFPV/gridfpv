/**
 * The kiosk (docs/primitives.md §11.4), through real browsers: a shared tablet that comes
 * in by Grid's network address and opens `#/kiosk`, a pilot's own phone, and the
 * director's console.
 *
 * What must hold on a real page: nobody can act without typing a code; the pilot who typed
 * it is who acts; they are signed out by Done, by walking away, by a reload, and by the
 * director; and none of it signs their own phone out.
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
const codes: Record<string, string> = {};

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
const command = (body: unknown) => direct('POST', `/events/${event}/commands`, body);

test.skip(lan === undefined, 'this computer has no network address to come in by');

test.beforeAll(async () => {
  director = await startDirector({ token: false, assets: dist });
  const ids: Record<string, string> = {};
  for (const callsign of ['Ace', 'Bee', 'Cat']) {
    ids[callsign] = (await direct<{ id: string }>('POST', '/pilots', { callsign })).id;
  }
  const open = (
    await direct<{ id: string }>('POST', '/classes', { name: 'Open', source: 'Custom' })
  ).id;
  event = (await direct<{ id: string }>('POST', '/events', { name: 'Friday Night Whoops' })).id;
  await direct('PUT', '/active-event', { id: event });
  await command({ Record: { RosterSet: { class: open, pilots: Object.values(ids) } } });
  await command({
    Record: {
      PhaseDefined: {
        def: {
          id: 'quali',
          label: 'Qualifying',
          kind: { Group: { field: [{ Class: open }], combine: 'Stack' } },
          format: {
            name: 'One race each',
            race: { end: 'Open', staging_micros: 0, first_lap: 'HoleShot' },
            completion: { SavedRaces: 1 },
            ranking: { sort: ['LastFinish'] }
          }
        }
      }
    }
  });
  await command({
    Signup: {
      Set: {
        phase: 'quali',
        rules: {
          seats: [1, 2, 3].map((n) => ({ adapter: 'bench', seat: `seat-${n}` })),
          open_heats: 3,
          rest_heats: 0,
          open: true
        }
      }
    }
  });
  const access = await direct<{ pilots: { pilot: string; code: string }[] }>(
    'GET',
    `/events/${event}/access`
  );
  for (const [callsign, id] of Object.entries(ids)) {
    codes[callsign] = access.pilots.find((entry) => entry.pilot === id)!.code;
  }
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

const phoneHeat = (page: Page, n: number) =>
  page.getByRole('listitem', { name: `Qualifying Heat ${n}`, exact: true });
const codeBox = (page: Page) => page.getByLabel('Pilots: type your code to sign up');
const kioskBar = (page: Page) => page.getByRole('region', { name: 'Kiosk sign-in' });

async function typeCode(page: Page, typed: string) {
  await codeBox(page).fill(typed);
  await page.getByRole('button', { name: 'Go', exact: true }).click();
}

test('the Network page gives the kiosk address, and it opens the kiosk', async ({
  page,
  browser
}) => {
  await page.goto(director.baseUrl);
  await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
  await openPage(page, 'Network');
  const link = page.getByTestId('kiosk-link');
  await expect(link).toHaveText(/#\/kiosk$/);
  const href = await link.getAttribute('href');
  expect(href).toBe(await link.textContent());
  // Whichever of this computer's addresses it names, it is Grid's network port and the kiosk.
  expect(href).toMatch(new RegExp(`^http://[0-9.]+:${new URL(address).port}/#/kiosk$`));
  const tablet = await browser.newContext({ viewport: { width: 1024, height: 768 } });
  try {
    const kiosk = await tablet.newPage();
    await kiosk.goto(`${address}#/kiosk`);
    await expect(codeBox(kiosk)).toBeVisible({ timeout: 15_000 });
  } finally {
    await tablet.close();
  }
});

test('pilots sign up at the kiosk one after another, and nobody is left signed in', async ({
  page,
  browser
}) => {
  test.setTimeout(180_000);
  const tablet = await browser.newContext({ viewport: { width: 1024, height: 768 } });
  const acePhone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  try {
    // The director watches the sheet.
    await page.goto(director.baseUrl);
    await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
    await openPage(page, 'Phases');
    const sheet = page.getByRole('table', { name: 'Heats of Qualifying' });
    const sheetRow = (n: number) => sheet.locator('tbody tr').nth(n - 1);
    await expect(sheet.locator('tbody tr')).toHaveCount(3);

    // Ace is signed in on their own phone.
    const ace = await acePhone.newPage();
    await ace.goto(`${address}#/join/${codes.Ace}`);
    await expect(ace.getByTestId('pilot-signed-in')).toHaveText(/Signed in as Ace/, {
      timeout: 15_000
    });

    // ── The kiosk, with nobody at it ─────────────────────────────────────────────────────
    const kiosk = await tablet.newPage();
    await kiosk.clock.install();
    await kiosk.goto(`${address}#/kiosk`);
    await expect(codeBox(kiosk)).toBeVisible({ timeout: 15_000 });
    await expect(kiosk.getByTestId('visitor-event')).toHaveText('Friday Night Whoops');
    await expect(kiosk.getByRole('button', { name: 'I am a pilot' })).toHaveCount(0);
    await kiosk.getByRole('tab', { name: 'Sign up' }).click();
    await expect(phoneHeat(kiosk, 1)).toBeVisible();
    await expect(kiosk.getByRole('button', { name: /^(Take|Leave|Next free)/ })).toHaveCount(0);

    // ── A wrong code, then a slip ────────────────────────────────────────────────────────
    await typeCode(kiosk, 'ZZZ ZZZ');
    await expect(kioskBar(kiosk).getByRole('alert')).toHaveText(
      'That code is not right. Check it with the director.'
    );
    await expect(codeBox(kiosk)).toHaveValue('');
    await typeCode(kiosk, 'ABC');
    await expect(kioskBar(kiosk).getByRole('alert')).toHaveText(
      'A code has 6 letters and numbers.'
    );

    // ── Bee types their code, loosely, and signs up ──────────────────────────────────────
    const bee = codes.Bee;
    await typeCode(kiosk, ` ${bee.slice(0, 3).toLowerCase()}-${bee.slice(3)} `);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Bee');
    await expect(kiosk.getByRole('tab', { name: 'Sign up' })).toHaveAttribute(
      'aria-selected',
      'true'
    );
    await kiosk.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 1' }).click();
    await expect(kiosk.getByRole('button', { name: 'Leave Qualifying Heat 1' })).toBeVisible();
    await expect(sheetRow(1)).toContainText('Bee', { timeout: 10_000 });
    // Bee's own tab works at the kiosk too.
    await kiosk.getByRole('tab', { name: /^Me/ }).click();
    await expect(kiosk.getByRole('list', { name: 'My heats' })).toContainText('Qualifying Heat 1');

    // ── Done: the next person finds nobody signed in ─────────────────────────────────────
    await kiosk.getByRole('button', { name: 'Done', exact: true }).click();
    await expect(codeBox(kiosk)).toBeVisible();
    await expect(kiosk.getByRole('tab', { name: /^Me/ })).toHaveCount(0);
    await kiosk.getByRole('tab', { name: 'Sign up' }).click();
    await expect(phoneHeat(kiosk, 1)).toContainText('Bee');
    await expect(kiosk.getByRole('button', { name: /^(Take|Leave|Next free)/ })).toHaveCount(0);

    // ── Ace uses the kiosk too. It is Ace who acts, and Ace's phone stays signed in ──────
    await typeCode(kiosk, `${address}#/join/${codes.Ace}`);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Ace');
    // Bee's seat is not Ace's to leave.
    await expect(kiosk.getByRole('button', { name: /^Leave/ })).toHaveCount(0);
    await kiosk.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }).click();
    await expect(sheetRow(1)).toContainText('Ace', { timeout: 10_000 });
    await ace.reload();
    await expect(ace.getByTestId('pilot-signed-in')).toHaveText(/Signed in as Ace/, {
      timeout: 15_000
    });
    await ace.getByRole('tab', { name: 'Sign up' }).click();
    await ace.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 2' }).click();
    await expect(sheetRow(2)).toContainText('Ace', { timeout: 10_000 });

    // ── Ace walks away: the page counts down, then signs them out ────────────────────────
    await kiosk.clock.fastForward(31_000);
    await expect(kiosk.getByTestId('kiosk-warning')).toHaveText(
      /^Signing out in \d+ s\. Touch the screen to stay\.$/
    );
    // A touch stops the count.
    await kiosk.getByTestId('visitor-event').click();
    await kiosk.clock.fastForward(1_000);
    await expect(kiosk.getByTestId('kiosk-warning')).toHaveCount(0);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Ace');
    await kiosk.clock.fastForward(46_000);
    await expect(kiosk.getByTestId('kiosk-note')).toHaveText(
      'You were signed out. Nobody touched the screen.'
    );
    await expect(kiosk.getByTestId('kiosk-pilot')).toHaveCount(0);
    await expect(kiosk.getByRole('button', { name: /^(Take|Leave|Next free)/ })).toHaveCount(0);

    // ── A reload signs out: the pass is kept nowhere ─────────────────────────────────────
    await typeCode(kiosk, codes.Cat);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Cat');
    expect(await tablet.cookies()).toEqual([]);
    expect(await kiosk.evaluate(() => localStorage.length + sessionStorage.length)).toBe(0);
    await kiosk.reload();
    await expect(codeBox(kiosk)).toBeVisible({ timeout: 15_000 });
    await expect(kiosk.getByTestId('kiosk-pilot')).toHaveCount(0);

    // ── The director gives Cat a new code while Cat is at the kiosk ──────────────────────
    await typeCode(kiosk, codes.Cat);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Cat');
    const cat = (await direct<{ id: string; callsign: string }[]>('GET', '/pilots')).find(
      (entry) => entry.callsign === 'Cat'
    )!.id;
    const fresh = await direct<{ code: string }>(
      'POST',
      `/events/${event}/access/${cat}/reset`,
      {}
    );
    await kiosk.getByRole('button', { name: 'Take Seat 3 in Qualifying Heat 1' }).click();
    await expect(kiosk.getByTestId('kiosk-note')).toHaveText(
      'You were signed out. Type your code again.'
    );
    await expect(sheetRow(1)).not.toContainText('Cat');
    // The old code is no good, and the new one is.
    await typeCode(kiosk, codes.Cat);
    await expect(kioskBar(kiosk).getByRole('alert')).toHaveText(
      'That code is not right. Check it with the director.'
    );
    await typeCode(kiosk, fresh.code);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Cat');
    await kiosk.getByRole('button', { name: 'Take Seat 3 in Qualifying Heat 1' }).click();
    await expect(sheetRow(1)).toContainText('Cat', { timeout: 10_000 });
    // Through all of it, Ace's phone was never signed out.
    await expect(ace.getByTestId('pilot-signed-in')).toHaveText(/Signed in as Ace/);
    await expect(phoneHeat(ace, 1)).toContainText('Cat', { timeout: 10_000 });
  } finally {
    await tablet.close();
    await acePhone.close();
  }
});
