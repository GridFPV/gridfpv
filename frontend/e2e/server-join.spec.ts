/**
 * Getting into a club on a remote server, through real browsers (docs/primitives.md §13.7).
 *
 * The server is the real `gridfpv-server` binary with its real page. The sign-in service is
 * a stand-in. Each person is a browser of their own.
 *
 * One story: with sign-up open, three pilots make their own accounts. One asks to join the
 * private club and is let in. One is told no. One is found by callsign and added by the
 * director. Then the club goes public, and joining is one press. A pilot leaves.
 */
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

/** Someone nobody invited makes their own account, and lands on it. */
async function signUp(browser: Browser, who: Person): Promise<Page> {
  const page = await fresh(browser);
  await page.goto(`${server.baseUrl}/#/sign-in`);
  await continueAs(page, who);
  await expect(page.getByTestId('account-name')).toHaveText(who.name);
  return page;
}

/** Who is in the club, as the server itself has it. */
async function memberNames(owner: Page): Promise<string[]> {
  const answer = await owner.request.get(`${server.baseUrl}/api/clubs/club/members`);
  return ((await answer.json()) as { name: string }[]).map((member) => member.name).sort();
}

test('pilots make their own accounts and get into a club the ways a club allows', async ({
  browser
}) => {
  test.setTimeout(120_000);
  server.run(['sign-up', 'open']);

  // ── The owner. An owner runs every club, and is a member of none. ─────────
  const setup = /http:\/\/\S+\/#\/invite\/\S+/.exec(server.run(['owner', 'invite']))![0];
  const ryan = await fresh(browser);
  await ryan.goto(setup);
  await continueAs(ryan, { sub: 'sub-ryan', name: 'Ryan' });
  await ryan.getByTestId('server-who').click();
  await openServerPart(ryan, 'Club');
  // Nobody is waiting, and a club starts private.
  await expect(ryan.getByTestId('no-join-asks')).toBeVisible();
  const clubPage = ryan.url();
  await openServerPage(ryan, 'Settings');
  const joining = ryan.getByTestId('club-join-setting');
  await expect(joining.getByTestId('club-open-state')).toHaveText('Private');

  // ── Three pilots make their own accounts. Each is in no club. ─────────────
  const pia = await signUp(browser, { sub: 'sub-pia', name: 'Pia' });
  const quin = await signUp(browser, { sub: 'sub-quin', name: 'Quin' });
  const ray = await signUp(browser, { sub: 'sub-ray', name: 'Ray' });
  for (const page of [pia, quin]) await openServerPage(page, 'Clubs');
  await expect(pia.getByText('You are not in a club yet.')).toBeVisible();
  // Ray gives himself a callsign: it is how the director will find him.
  await ray.getByTestId('pilot-profile').getByRole('button', { name: 'Fill it in' }).click();
  await ray.getByLabel('Callsign').fill('Rotorhead');
  await ray.getByTestId('pilot-profile').getByRole('button', { name: 'Save' }).click();
  await expect(ray.getByTestId('profile-callsign')).toHaveText('Rotorhead');
  await openServerPage(ray, 'Clubs');

  // ── Pia and Quin ask to join. Asking puts nobody in. ──────────────────────
  for (const page of [pia, quin]) {
    const row = page.getByTestId('join-club').filter({ hasText: 'Recon FPV' });
    await expect(row).toContainText('Private');
    await row.getByRole('button', { name: 'Ask to join Recon FPV' }).click();
    await expect(row).toContainText('You asked to join. A director of the club will answer.');
    await expect(page.getByTestId('my-club')).toHaveCount(0);
  }
  expect(await memberNames(ryan)).toEqual([]);
  // Asking gives nothing meanwhile: the club's pilots are not hers to see.
  expect((await pia.request.get(`${server.baseUrl}/api/clubs/club/pilots`)).status()).toBe(403);
  // It still says so after the page is opened again.
  await pia.reload();
  await expect(pia.getByTestId('join-club').filter({ hasText: 'Recon FPV' })).toContainText(
    'You asked to join.'
  );

  // ── The director lets Pia in, and says no to Quin. ────────────────────────
  await ryan.goto(clubPage);
  await ryan.reload();
  const asks = ryan.getByTestId('join-ask');
  await expect(asks).toHaveCount(2);
  await expect(asks.nth(0)).toContainText('Pia');
  await expect(asks.nth(1)).toContainText('Quin');
  await ryan.getByRole('button', { name: 'Let Pia in' }).click();
  await expect(ryan.getByTestId('join-ask')).toHaveCount(1);
  // It worked, and the page does not say otherwise.
  await expect(ryan.getByTestId('club-join-asks').getByRole('alert')).toHaveCount(0);
  await expect(ryan.getByTestId('club-join-asks')).not.toContainText('JSON');
  await expect(ryan.getByRole('list', { name: 'Members' })).toContainText('Pia');
  await ryan.getByRole('button', { name: 'Say no to Quin' }).click();
  await expect(ryan.getByTestId('no-join-asks')).toBeVisible();
  expect(await memberNames(ryan)).toEqual(['Pia']);

  // Pia sees she is in, as a pilot. Quin is back to where he can ask.
  await pia.reload();
  await expect(pia.getByTestId('my-club')).toContainText('Recon FPV');
  await expect(pia.getByTestId('my-club')).toContainText('Pilot');
  await expect(pia.getByTestId('join-clubs')).toHaveCount(0);
  await quin.reload();
  await expect(quin.getByRole('button', { name: 'Ask to join Recon FPV' })).toBeVisible();
  await expect(quin.getByTestId('my-club')).toHaveCount(0);

  // ── The director finds Ray by his callsign and adds him. ──────────────────
  await ryan.getByLabel('Name or callsign').fill('rotor');
  await ryan.getByTestId('club-add-someone').getByRole('button', { name: 'Find' }).click();
  const foundRay = ryan.getByTestId('found-person');
  await expect(foundRay).toHaveCount(1);
  await expect(foundRay).toContainText('Ray (Rotorhead)');
  await ryan.getByRole('button', { name: 'Add Ray (Rotorhead) to Recon FPV' }).click();
  await expect(ryan.getByTestId('club-add-someone')).not.toContainText('JSON');
  await expect(ryan.getByRole('list', { name: 'Members' })).toContainText('Ray');
  expect(await memberNames(ryan)).toEqual(['Pia', 'Ray']);
  await ray.reload();
  await expect(ray.getByTestId('my-club')).toContainText('Recon FPV');
  // Someone in the club is not offered again.
  await ryan.getByLabel('Name or callsign').fill('rotor');
  await ryan.getByTestId('club-add-someone').getByRole('button', { name: 'Find' }).click();
  await expect(ryan.getByTestId('nobody-found')).toContainText('rotor');

  // ── The club goes public. Joining is one press, from the club's own address. ──
  await openServerPage(ryan, 'Settings');
  await ryan
    .getByTestId('club-join-setting')
    .getByRole('switch', { name: 'Let anyone with an account join' })
    .click();
  await expect(ryan.getByTestId('club-join-setting').getByTestId('club-open-state')).toHaveText(
    'Public'
  );
  await quin.goto(`${server.baseUrl}/#/club/recon-fpv`);
  await quin.reload();
  await quin.getByTestId('club-join').getByRole('button', { name: 'Join Recon FPV' }).click();
  await expect(quin.getByTestId('club-join')).toContainText('You are in this club.');
  expect(await memberNames(ryan)).toEqual(['Pia', 'Quin', 'Ray']);

  // A visitor at that address is told to sign in to join.
  const visitor = await fresh(browser);
  await visitor.goto(`${server.baseUrl}/#/club/recon-fpv`);
  await expect(visitor.getByRole('link', { name: 'Sign in to join Recon FPV' })).toBeVisible();

  // ── Quin leaves, after being asked. He can join again. ────────────────────
  await quin.getByTestId('server-who').click();
  await openServerPage(quin, 'Clubs');
  await quin.getByRole('button', { name: 'Leave Recon FPV' }).click();
  const sure = quin.getByRole('dialog');
  await expect(sure).toContainText('Leave Recon FPV?');
  await sure.getByRole('button', { name: 'Leave' }).click();
  await expect(sure).toHaveCount(0);
  await expect(quin.getByTestId('my-club')).toHaveCount(0);
  await expect(quin.getByRole('button', { name: 'Join Recon FPV' })).toBeVisible();
  expect(await memberNames(ryan)).toEqual(['Pia', 'Ray']);

  // ── The only director cannot walk out of the club. ────────────────────────
  // Pia is made the director. She cannot leave while she is the only one.
  await ryan.goto(clubPage);
  await ryan.reload();
  const answer = await ryan.request.get(`${server.baseUrl}/api/clubs/club/members`);
  const piasId = ((await answer.json()) as { id: string; name: string }[]).find(
    (member) => member.name === 'Pia'
  )!.id;
  const made = await ryan.request.put(`${server.baseUrl}/api/clubs/club/members/${piasId}`, {
    data: { role: 'Director' },
    headers: { origin: server.baseUrl }
  });
  expect(made.status()).toBe(204);
  await pia.reload();
  await pia.getByRole('button', { name: 'Leave Recon FPV' }).click();
  await pia.getByRole('dialog').getByRole('button', { name: 'Leave' }).click();
  await expect(pia.getByRole('dialog')).toContainText('A club must keep at least one director.');
  await pia.getByRole('dialog').getByRole('button', { name: 'Cancel' }).click();
  await expect(pia.getByTestId('my-club')).toContainText('Recon FPV');
});
