/**
 * Hash-based URL routing proof (#118): a browser **refresh stays on the current page**, and
 * **back/forward** move between views.
 *
 * The view is reflected in `location.hash` (e.g. `#/pilots`, `#/event`), so a refresh restores
 * it, bookmarks work, and the browser's back/forward navigate between views.
 *
 * Why a hash and not a path: `/pilots`, `/events`, `/classes` are Director API routes — a path
 * router would collide with the server. The `#` fragment stays client-side, so it can't.
 *
 * This spec drives real clicks in headless chromium against the worker's real Director and asserts
 * on `location.hash` plus the rendered DOM. Importing `test`/`expect` from `./observability.js`
 * means a failure carries the full-stack dump (browser console, page errors, the Director's log).
 */
import { E2E_EVENT_NAME, expect, test } from './observability.js';
import { eventsCrumb, gotoEvents, gotoHub } from './nav.js';

test('a refresh on the Pilots page stays on Pilots (hash reflects the view)', async ({ page }) => {
  await gotoHub(page);

  // Hub → Pilots: the hash becomes #/pilots and the Pilots page renders.
  await page.getByRole('button', { name: /Pilots/ }).click();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/pilots');

  // Reload — the key proof: we stay on Pilots (not reset to the hub or an open event).
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  expect(new URL(page.url()).hash).toBe('#/pilots');
});

test('an open event survives a refresh', async ({ page }) => {
  // Events → open the worker's event: its page opens at #/event.
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  const title = page.getByRole('heading', { name: E2E_EVENT_NAME, level: 1 });
  await expect(title).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event');

  // Reload — the key proof: we resume into the SAME event. The active event is server state
  // (#90); the hash says to show it.
  await page.reload();
  await expect(title).toBeVisible({ timeout: 15_000 });
  expect(new URL(page.url()).hash).toBe('#/event');
});

test('browser back and forward move between views', async ({ page }) => {
  // Hub → Pilots → (Home) → Classes, each a hash change.
  await gotoHub(page);
  await page.getByRole('button', { name: /Pilots/ }).click();
  const pilots = page.getByRole('heading', { name: 'Pilots', level: 1 });
  await expect(pilots).toBeVisible({ timeout: 15_000 });
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await page.getByRole('button', { name: /Classes/ }).click();
  const classes = page.getByRole('heading', { name: 'Classes', level: 1 });
  await expect(classes).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/classes');

  // BACK twice: Classes → hub → Pilots — driven by hashchange.
  await page.goBack();
  await page.goBack();
  await expect(pilots).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/pilots');
  await expect(classes).toBeHidden();

  // FORWARD twice returns to Classes.
  await page.goForward();
  await page.goForward();
  await expect(classes).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/classes');
});

test('on the hub with an active event, a refresh stays on the hub (no auto-open of the event)', async ({
  page
}) => {
  // First make an event ACTIVE on the Director (server state, #90) by opening it.
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  await expect(eventsCrumb(page)).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event');
  const active = (await (await page.request.get('/active-event')).json()) as {
    event: { name: string } | null;
  };
  expect(active.event?.name).toBe(E2E_EVENT_NAME);

  // Leave the event for the hub via the breadcrumb Home crumb. That closes the event here but
  // does NOT clear the Director's active event, so the condition persists across the reload.
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toMatch(/^(#\/?)?$/);

  // Reload — the key proof: with the active event STILL set on the Director, we stay on the hub
  // rather than being pulled into the event.
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible({ timeout: 15_000 });
  expect(new URL(page.url()).hash).toMatch(/^(#\/?)?$/);
  await expect(eventsCrumb(page)).toBeHidden();
});
