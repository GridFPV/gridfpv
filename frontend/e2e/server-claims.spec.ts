/**
 * How race results reach an account on a remote server, through real browsers
 * (docs/primitives.md §13.7).
 *
 * The server is the real `gridfpv-server` binary with its real page. A Director is stood
 * in for by plain requests with the club's link token: it sends the club's pilots and one
 * event they are in. Each person is a browser of their own.
 *
 * One story: a pilot gets their results from an invite made for them; another finds
 * themselves in the club's list and asks; a claims approver says yes; a director takes a
 * pilot back. At every step, only the account that holds a pilot sees its results.
 */
import { createHash } from 'node:crypto';
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

/** What a Director with the club's link token sends. */
async function push(method: 'PUT' | 'POST', path: string, body: string): Promise<void> {
  const answer = await fetch(`${server.baseUrl}${path}`, {
    method,
    headers: { authorization: `Bearer ${server.token}`, 'content-type': 'application/json' },
    body
  });
  expect(answer.ok, `${method} ${path}: ${answer.status} ${await answer.text()}`).toBe(true);
}

/** Send the club's pilots, and an event with Cat and Dog in its Open class. */
async function sendRaceDay(): Promise<void> {
  const pilot = (id: string, callsign: string) => ({ id, callsign, vtx_types: [] });
  await push(
    'PUT',
    '/push/pilots',
    JSON.stringify([
      { id: 'cat-1', callsign: 'Cat' },
      { id: 'dog-2', callsign: 'Dog' },
      { id: 'eel-3', callsign: 'Eel' }
    ])
  );
  await push(
    'PUT',
    '/push/events/spring',
    JSON.stringify({
      meta: { id: 'spring', name: 'Spring Race', created_at: 1, persistent: true },
      pilots: [pilot('cat-1', 'Cat'), pilot('dog-2', 'Dog')],
      classes: [{ id: 'open', name: 'Open', source: 'Custom' }]
    })
  );
  // The log starts from the value every log's chain starts from.
  const start = createHash('sha256').update('gridfpv log chain v1').digest('hex');
  const entry = JSON.stringify({ RosterSet: { class: 'open', pilots: ['cat-1', 'dog-2'] } });
  await push(
    'POST',
    '/push/events/spring/log',
    `{"from":0,"before":"${start}","entries":[{"event":${entry}}]}`
  );
}

/** On a club's page: make an invite for one person, and give back its link. */
async function invite(page: Page, label: string, role: string, pilot?: string): Promise<string> {
  await openServerPage(page, 'Invites');
  await page.getByLabel('Who it is for').fill(label);
  await page.getByLabel('Joins as').selectOption(role);
  if (pilot) await page.getByLabel('Their pilot').selectOption({ label: pilot });
  await page.getByRole('button', { name: 'Make the invite' }).click();
  const link = (await page.getByTestId('invite-link').innerText()).trim();
  await page.getByRole('button', { name: 'Done' }).click();
  return link;
}

async function resultsOf(page: Page): Promise<string[]> {
  await page.goto(`${server.baseUrl}/#/results`);
  // Going to the address the page is already at reads nothing again.
  await page.reload();
  await expect(page.getByTestId('server-results')).toBeVisible();
  await expect(page.getByRole('status')).toHaveCount(0);
  return page.getByTestId('result').allInnerTexts();
}

test('results reach the account that holds the pilot: by invite, and by asking', async ({
  browser
}) => {
  await sendRaceDay();

  // ── The owner, from the setup link. ───────────────────────────────────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await continueAs(ryan, { sub: 'sub-ryan', name: 'Ryan' });
  await ryan.getByTestId('server-who').click();
  await openServerPart(ryan, 'Club');
  await openServerPage(ryan, 'Pilots');

  // The club's pilots are there, by callsign, and nobody holds one.
  await expect(ryan.getByTestId('club-pilot')).toHaveCount(3);
  await expect(ryan.getByTestId('pilot-holder')).toHaveText([
    'Nobody yet',
    'Nobody yet',
    'Nobody yet'
  ]);

  // ── An invite made for Cat gives Cat's results in the same step. ──────────
  const catLink = await invite(ryan, 'Cara', 'Pilot', 'Cat');
  const cara = await fresh(browser);
  await cara.goto(catLink);
  await expect(cara.getByTestId('invite-pilot')).toContainText('This invite is for the pilot Cat');
  await continueAs(cara, { sub: 'sub-cara', name: 'Cara' });
  await expect(cara.getByTestId('server-who')).toHaveText('Cara');
  await cara.getByTestId('server-who').click();
  await expect(cara.getByTestId('my-pilot')).toContainText('Cat');
  const caras = await resultsOf(cara);
  expect(caras).toHaveLength(1);
  expect(caras[0]).toContain('Spring Race');
  expect(caras[0]).toContain('Flew as Cat');
  await cara.getByRole('link', { name: 'Spring Race' }).click();
  await expect(cara.getByTestId('server-event')).toHaveText('Spring Race');

  // ── Don joins with a plain invite, and holds nothing. ─────────────────────
  const don = await fresh(browser);
  await don.goto(await invite(ryan, 'Don', 'Pilot'));
  await continueAs(don, { sub: 'sub-don', name: 'Don' });
  await don.getByTestId('server-who').click();
  await expect(don.getByTestId('my-pilots')).toContainText('None yet.');
  expect(await resultsOf(don)).toEqual([]);

  // He finds himself. Cat is taken, so it is not offered, and he is not told by whom.
  await don.goto(`${server.baseUrl}/#/account`);
  await don.getByRole('button', { name: 'Find yourself in Recon FPV' }).click();
  const offered = don.getByRole('list', { name: 'Pilots of Recon FPV' });
  await expect(offered.getByRole('listitem')).toHaveCount(2);
  await expect(offered).not.toContainText('Cat');
  await expect(don.getByRole('dialog')).not.toContainText('Cara');
  await don.getByRole('button', { name: 'I am Dog' }).click();
  await expect(don.getByTestId('my-pilot')).toContainText('Waiting for the club to say yes');
  // Asking gives nothing, on the page or from the server.
  expect(await resultsOf(don)).toEqual([]);

  // He cannot say yes to himself: the server refuses.
  const me = (await (await don.request.get(`${server.baseUrl}/api/me`)).json()) as {
    account: { id: string };
  };
  const selfServe = await don.request.put(`${server.baseUrl}/api/clubs/club/pilots/dog-2/holder`, {
    data: { account: me.account.id }
  });
  expect(selfServe.status()).toBe(403);
  // Nor ask for a pilot that is taken, though he knows its id.
  const taken = await don.request.post(`${server.baseUrl}/api/clubs/club/pilots/cat-1/claim`);
  expect(taken.status()).toBe(409);

  // ── A claims approver says yes. ───────────────────────────────────────────
  const dana = await fresh(browser);
  await dana.goto(await invite(ryan, 'Dana', 'Approver'));
  await continueAs(dana, { sub: 'sub-dana', name: 'Dana' });
  await dana.getByTestId('server-who').click();
  // An approver's Club part is the club's pilots.
  await openServerPart(dana, 'Club');
  await expect(dana.getByTestId('pilot-ask')).toContainText('Don says they are Dog');
  // An approver sees the pilots, and nothing of members or invites.
  await expect(dana.getByTestId('club-pilot')).toHaveCount(3);
  await expect(dana.getByTestId('club-member')).toHaveCount(0);
  await expect(dana.getByLabel('Give Eel to')).toHaveCount(0);
  await dana.getByRole('button', { name: 'Yes, Don is Dog' }).click();
  await expect(dana.getByTestId('pilot-ask')).toHaveCount(0);
  await expect(dana.getByTestId('pilot-holder')).toHaveText(['Cara', 'Don', 'Nobody yet']);

  const dons = await resultsOf(don);
  expect(dons).toHaveLength(1);
  expect(dons[0]).toContain('Flew as Dog');
  // Each sees their own and not the other's.
  expect(dons[0]).not.toContain('Cat');
  expect((await resultsOf(cara))[0]).not.toContain('Dog');

  // ── The owner takes Dog back, and gives Eel by hand. ──────────────────────
  await ryan.reload();
  await openServerPage(ryan, 'Pilots');
  await ryan.getByRole('button', { name: 'Take Dog back from Don' }).click();
  await ryan.getByRole('dialog').getByRole('button', { name: 'Take back' }).click();
  await expect(ryan.getByTestId('pilot-holder')).toHaveText(['Cara', 'Nobody yet', 'Nobody yet']);
  expect(await resultsOf(don)).toEqual([]);
  await ryan.getByLabel('Give Eel to').selectOption({ label: 'Don' });
  await expect(ryan.getByTestId('pilot-holder')).toHaveText(['Cara', 'Nobody yet', 'Don']);
  // Eel flew in nothing, so Don holds a pilot and still has no results.
  await don.goto(`${server.baseUrl}/#/account`);
  await expect(don.getByTestId('my-pilot')).toContainText('Eel');
  expect(await resultsOf(don)).toEqual([]);

  // ── Someone with no account sees none of it. ──────────────────────────────
  const stranger = await fresh(browser);
  for (const path of ['/api/clubs/club/pilots', '/api/me/results']) {
    expect((await stranger.request.get(`${server.baseUrl}${path}`)).status()).toBe(401);
  }
});
