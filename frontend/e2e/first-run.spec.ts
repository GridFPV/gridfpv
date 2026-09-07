/**
 * First run on a brand-new Director (#414) — the empty picker leads somewhere.
 *
 * The built-in in-memory Practice event is gone, so a Director that has never had an event
 * created lists **nothing**. That is the honest first-run state, and the Events page must make
 * the next step obvious rather than dropping the RD at an empty list: a "Create your first
 * event" call to action that opens the same create dialog, whose "Set up event" default hands
 * straight off to the existing setup wizard (#97). There is deliberately no second wizard.
 *
 * This spec boots its **own** Director (the worker fixture creates an event, so its picker is
 * never empty) and drives real clicks in headless chromium — nothing mocked.
 */
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { expect, test } from '@playwright/test';

import { type Director, startDirector } from '../test-harness/director.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const dist = resolve(here, '..', 'apps', 'rd-console', 'dist');

let director: Director;

test.beforeAll(async () => {
  // A Director with no data dir and no events ever created — the true first-run registry.
  director = await startDirector({ token: false, assets: dist });
});

test.afterAll(async () => {
  await director?.stop();
});

test('a fresh Director lists no events and the picker offers the create path', async ({ page }) => {
  // The API itself says so first: an empty list is the first-run state, not an error.
  const listed = await (await fetch(`${director.baseUrl}/events`)).json();
  expect(listed).toEqual([]);
  // And nothing is active — so the console lands on the hub, not a dangling workspace.
  const active = await (await fetch(`${director.baseUrl}/active-event`)).json();
  expect(active.event).toBeNull();

  await page.goto(director.baseUrl);

  // The hub loads (no event to auto-enter), and the Events page is reachable from it.
  await expect(page.getByRole('heading', { name: 'Events' })).toBeVisible({ timeout: 15_000 });
  await page.getByRole('heading', { name: 'Events' }).click();
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeVisible({
    timeout: 15_000
  });

  // The empty picker is a call to action, not an empty list.
  const cta = page.getByRole('button', { name: 'Create your first event' });
  await expect(page.getByRole('heading', { name: 'Create your first event' })).toBeVisible();
  await expect(cta).toBeVisible();

  // It opens the SAME create dialog the header button does — one create path, not two.
  await cta.click();
  const newForm = page.getByRole('form', { name: 'New event' });
  await expect(newForm).toBeVisible();

  // "Set up event" is ticked by default, so creating hands off to the existing wizard (#97).
  const setupBox = page.getByRole('checkbox', { name: 'Set up event after creating' });
  await expect(setupBox).toBeChecked();

  const name = `First Event ${Date.now()}`;
  await newForm.getByLabel('Event name').fill(name);
  await page.getByRole('button', { name: 'Create & enter' }).click();

  // The wizard opens on the freshly-created event — the empty picker led somewhere.
  const wizard = page.getByRole('dialog', { name: 'Event setup wizard' });
  await expect(wizard).toBeVisible({ timeout: 15_000 });
  await expect(wizard.getByText(`Set up · ${name}`)).toBeVisible();

  // And the event really exists server-side, created through the ordinary path.
  const after = (await (await fetch(`${director.baseUrl}/events`)).json()) as { name: string }[];
  expect(after.map((e) => e.name)).toEqual([name]);
});
