/**
 * The kiosk (docs/primitives.md §11.4), through real browsers: a tablet that comes in by
 * Grid's network address, a pilot's own phone, and the director's console.
 *
 * What must hold on a real page: only the director makes a tablet a kiosk, with a code
 * that works once; nobody can act at it without typing their own code; the pilot who typed
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

const kioskRow = (page: Page, n: number) =>
  page.getByRole('row', { name: `Qualifying Heat ${n}`, exact: true });
const setupBox = (page: Page) => page.getByLabel('Kiosk code', { exact: true });
const codeBox = (page: Page) => page.getByLabel('Type your code', { exact: true });

async function typeCode(page: Page, typed: string) {
  await codeBox(page).fill(typed);
  await page.getByRole('button', { name: 'Go', exact: true }).click();
}
/** A kiosk code, made the way the Network page makes one. */
async function newKioskCode(): Promise<string> {
  return (await direct<{ codes: string[] }>('POST', '/kiosk/codes')).codes.at(-1)!;
}
/** A tablet the director has set up, on the page that waits for a pilot's code. */
async function setUpTablet(kiosk: Page) {
  await kiosk.goto(`${address}#/kiosk`);
  await setupBox(kiosk).fill(await newKioskCode());
  await kiosk.getByRole('button', { name: 'Set up', exact: true }).click();
  await expect(codeBox(kiosk)).toBeVisible({ timeout: 15_000 });
}

test('the director sets a tablet up with a code that works once, and can remove it', async ({
  page,
  browser
}) => {
  test.setTimeout(120_000);
  const tablet = await browser.newContext({ viewport: { width: 1180, height: 820 } });
  const other = await browser.newContext({ viewport: { width: 1180, height: 820 } });
  try {
    // ── The Network page: the address, and a code ────────────────────────────────────────
    await page.goto(director.baseUrl);
    await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
    await openPage(page, 'Network');
    const link = page.getByTestId('kiosk-link');
    await expect(link).toHaveText(/#\/kiosk$/);
    expect(await link.getAttribute('href')).toMatch(
      new RegExp(`^http://[0-9.]+:${new URL(address).port}/#/kiosk$`)
    );
    await expect(page.getByTestId('kiosk-code')).toHaveCount(0);
    await page.getByRole('button', { name: 'New kiosk code' }).click();
    await expect(page.getByTestId('kiosk-code')).toHaveCount(1);
    const shown = (await page.getByTestId('kiosk-code').textContent())!.trim();
    expect(shown).toMatch(/^[A-Z2-9]{4} [A-Z2-9]{4}$/);

    // ── A tablet nobody has set up: it asks for the code, and shows nothing of the race ──
    const kiosk = await tablet.newPage();
    await kiosk.goto(`${address}#/kiosk`);
    await expect(setupBox(kiosk)).toBeVisible({ timeout: 15_000 });
    await expect(kiosk.getByRole('tablist')).toHaveCount(0);
    await expect(codeBox(kiosk)).toHaveCount(0);
    // A pilot's code typed straight at the Director does nothing from here.
    const sneak = await kiosk.request.post(`${address}public/pilot/kiosk`, {
      data: { code: codes.Bee }
    });
    expect(sneak.status()).toBe(403);

    // A wrong code, then the right one typed the way it is shown.
    await setupBox(kiosk).fill('ZZZZ ZZZZ');
    await kiosk.getByRole('button', { name: 'Set up', exact: true }).click();
    await expect(kiosk.getByRole('alert')).toContainText('That kiosk code is not right');
    await setupBox(kiosk).fill(shown.toLowerCase());
    await kiosk.getByRole('button', { name: 'Set up', exact: true }).click();
    await expect(codeBox(kiosk)).toBeVisible({ timeout: 15_000 });

    // ── The console sees it by itself: the code is used up, and Kiosk 1 is listed ────────
    await expect(page.getByTestId('kiosk-code')).toHaveCount(0, { timeout: 15_000 });
    await expect(page.getByRole('list', { name: 'Kiosks set up' })).toContainText('Kiosk 1');

    // ── The director names it, and the tablet shows the name while it waits ──────────────
    await page.getByRole('button', { name: 'Name kiosk 1' }).click();
    const naming = page.getByRole('dialog');
    await naming.getByLabel('Name', { exact: true }).fill('By the gate');
    await naming.getByRole('button', { name: 'Save' }).click();
    await expect(naming).toHaveCount(0);
    await expect(page.getByTestId('kiosk-name')).toHaveText('By the gate');
    await kiosk.reload();
    await expect(kiosk.getByTestId('kiosk-name')).toHaveText('By the gate', { timeout: 15_000 });

    // ── The same code on a second tablet does nothing ────────────────────────────────────
    const second = await other.newPage();
    await second.goto(`${address}#/kiosk`);
    await setupBox(second).fill(shown);
    await second.getByRole('button', { name: 'Set up', exact: true }).click();
    await expect(second.getByRole('alert')).toContainText('or it has been used');
    await expect(codeBox(second)).toHaveCount(0);

    // ── The tablet stays a kiosk when it is loaded again, and holds only its own cookie ──
    await kiosk.reload();
    await expect(codeBox(kiosk)).toBeVisible({ timeout: 15_000 });
    const cookies = await tablet.cookies();
    expect(cookies.map((cookie) => cookie.name)).toEqual(['gridfpv_kiosk']);
    expect(cookies[0].httpOnly).toBe(true);
    expect(cookies[0].sameSite).toBe('Strict');
    expect(cookies[0].value).not.toBe(shown.replace(' ', ''));

    // ── The director removes it: the tablet goes back to asking for a kiosk code ─────────
    await page.getByRole('button', { name: 'Remove kiosk 1' }).click();
    await expect(page.getByRole('list', { name: 'Kiosks set up' })).toHaveCount(0);
    await typeCode(kiosk, codes.Bee);
    await expect(setupBox(kiosk)).toBeVisible({ timeout: 15_000 });
    await expect(kiosk.getByTestId('kiosk-pilot')).toHaveCount(0);
  } finally {
    await tablet.close();
    await other.close();
  }
});

test('pilots sign up at the kiosk one after another, and nobody is left signed in', async ({
  page,
  browser
}) => {
  test.setTimeout(180_000);
  const tablet = await browser.newContext({ viewport: { width: 1180, height: 820 } });
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
    await expect(ace.getByTestId('pilot-signed-in')).toHaveText(/Signed in as\s*Ace/, {
      timeout: 15_000
    });

    // ── The kiosk, with nobody at it: a code box and nothing else ────────────────────────
    const kiosk = await tablet.newPage();
    await kiosk.clock.install();
    await setUpTablet(kiosk);
    await expect(kiosk.getByTestId('visitor-event')).toHaveText('Friday Night Whoops');
    await expect(kiosk.getByRole('table')).toHaveCount(0);
    await expect(kiosk.getByRole('tablist')).toHaveCount(0);
    await expect(kiosk.getByRole('button')).toHaveCount(1);

    // ── A wrong code, then a slip ────────────────────────────────────────────────────────
    await typeCode(kiosk, 'ZZZ ZZZ');
    await expect(kiosk.getByRole('alert')).toHaveText(
      'That code is not right. Check it with the director.'
    );
    await expect(codeBox(kiosk)).toHaveValue('');
    await typeCode(kiosk, 'ABC');
    await expect(kiosk.getByRole('alert')).toHaveText('A code has 6 letters and numbers.');

    // ── Bee types their code, loosely, and gets the sheet as a grid ──────────────────────
    const bee = codes.Bee;
    await typeCode(kiosk, ` ${bee.slice(0, 3).toLowerCase()}-${bee.slice(3)} `);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Bee');
    const grid = kiosk.getByRole('table', { name: 'Heats of Qualifying' });
    await expect(grid.locator('tbody tr')).toHaveCount(3);
    await kiosk.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 1' }).click();
    await expect(kiosk.getByRole('button', { name: 'Leave Qualifying Heat 1' })).toBeVisible();
    await expect(kioskRow(kiosk, 1)).toContainText('Bee');
    await expect(sheetRow(1)).toContainText('Bee', { timeout: 10_000 });
    // The grid fits a tablet on its side with nothing to scroll sideways.
    expect(await kiosk.evaluate(() => document.documentElement.scrollWidth)).toBeLessThanOrEqual(
      1180
    );

    // ── Done: the next person finds a code box and nothing of Bee ────────────────────────
    await kiosk.getByRole('button', { name: 'Done', exact: true }).click();
    await expect(codeBox(kiosk)).toBeVisible();
    await expect(kiosk.getByRole('table')).toHaveCount(0);
    await expect(kiosk.getByText('Bee')).toHaveCount(0);

    // ── Ace uses the kiosk too, by a scanned link. It is Ace who acts, and Ace's phone ───
    // ── stays signed in. ─────────────────────────────────────────────────────────────────
    await typeCode(kiosk, `${address}#/join/${codes.Ace}`);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Ace');
    // Bee's seat is not Ace's to leave.
    await expect(kiosk.getByRole('button', { name: /^Leave/ })).toHaveCount(0);
    await kiosk.getByRole('button', { name: 'Take Seat 2 in Qualifying Heat 1' }).click();
    await expect(sheetRow(1)).toContainText('Ace', { timeout: 10_000 });
    await ace.reload();
    await expect(ace.getByTestId('pilot-signed-in')).toHaveText(/Signed in as\s*Ace/, {
      timeout: 15_000
    });
    await ace.getByRole('tab', { name: 'Sign up' }).click();
    await ace.getByRole('button', { name: 'Take Seat 1 in Qualifying Heat 2' }).click();
    await expect(sheetRow(2)).toContainText('Ace', { timeout: 10_000 });
    // The kiosk shows what the phone did, without being touched.
    await expect(kioskRow(kiosk, 2)).toContainText('Ace', { timeout: 10_000 });

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
    await expect(kiosk.getByRole('table')).toHaveCount(0);

    // ── A reload signs the pilot out, and the tablet is still a kiosk ────────────────────
    await typeCode(kiosk, codes.Cat);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Cat');
    expect((await tablet.cookies()).map((cookie) => cookie.name)).toEqual(['gridfpv_kiosk']);
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
    await expect(kiosk.getByRole('alert')).toHaveText(
      'That code is not right. Check it with the director.'
    );
    await typeCode(kiosk, fresh.code);
    await expect(kiosk.getByTestId('kiosk-pilot')).toContainText('Hi Cat');
    await kiosk.getByRole('button', { name: 'Take Seat 3 in Qualifying Heat 1' }).click();
    await expect(sheetRow(1)).toContainText('Cat', { timeout: 10_000 });
    // Through all of it, Ace's phone was never signed out.
    await expect(ace.getByTestId('pilot-signed-in')).toHaveText(/Signed in as\s*Ace/);
  } finally {
    await tablet.close();
    await acePhone.close();
  }
});

test("a pilot's phone cannot make itself a kiosk", async ({ browser }) => {
  const phone = await browser.newContext({ viewport: { width: 390, height: 844 } });
  try {
    const page = await phone.newPage();
    await page.goto(`${address}#/join/${codes.Bee}`);
    await expect(page.getByTestId('pilot-signed-in')).toHaveText(/Signed in as\s*Bee/, {
      timeout: 15_000
    });
    // Signed in as a pilot, the kiosk page still only asks for a code the director has.
    await page.goto(`${address}#/kiosk`);
    await expect(setupBox(page)).toBeVisible({ timeout: 15_000 });
    await expect(codeBox(page)).toHaveCount(0);
    // A pilot's own code is not a kiosk code.
    await setupBox(page).fill(`${codes.Bee}AA`);
    await page.getByRole('button', { name: 'Set up', exact: true }).click();
    await expect(page.getByRole('alert')).toContainText('That kiosk code is not right');
    // Nor can the phone ask for the director's codes.
    for (const [method, path] of [
      ['GET', 'kiosk'],
      ['POST', 'kiosk/codes']
    ] as const) {
      const answer = await page.request.fetch(`${address}${path}`, { method });
      expect([401, 403], `${method} /${path}`).toContain(answer.status());
    }
  } finally {
    await phone.close();
  }
});
