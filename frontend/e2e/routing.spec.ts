/**
 * Hash-based URL routing proof (#118): a browser **refresh stays on the current page/tab**, and
 * **back/forward** move between views.
 *
 * The console's view used to live only in in-memory state, so a reload reset it (back to the active
 * event's workspace or the hub) — losing where you were. Now the view is reflected in
 * `location.hash` (e.g. `#/pilots`, `#/event/registration`), so a refresh restores it, bookmarks
 * work, and the browser's back/forward navigate between views.
 *
 * Why a hash and not a path: `/pilots`, `/events`, `/timers` are already Director API routes — a
 * path router would collide with the server. The `#` fragment stays client-side, so it can't.
 *
 * This spec drives real clicks in headless chromium against the worker's real Director and asserts
 * on `location.hash` plus the rendered DOM. Importing `test`/`expect` from `./observability.js`
 * means a failure carries the full-stack dump (browser console, page errors, the Director's log).
 */
import { expect, test } from './observability.js';

/** Return to the home hub regardless of whether the shell resumed into a workspace (#90). */
async function gotoHub(page: import('@playwright/test').Page) {
  await page.goto('/');
  const pilotsCard = page.getByRole('heading', { name: 'Pilots' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(pilotsCard.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page
      .getByRole('navigation', { name: 'Breadcrumb' })
      .getByRole('button', { name: 'Home' })
      .click();
  }
  await expect(pilotsCard).toBeVisible({ timeout: 15_000 });
}

test('a refresh on the Pilots page stays on Pilots (hash reflects the view)', async ({ page }) => {
  await gotoHub(page);

  // Hub → Pilots: the hash becomes #/pilots and the Pilots page renders.
  await page.getByRole('button', { name: /Pilots/ }).click();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/pilots');

  // Reload — the key proof: we stay on Pilots (not reset to the hub or a resumed workspace).
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  expect(new URL(page.url()).hash).toBe('#/pilots');
});

test('inside an event, a refresh stays on the open tab; browser back returns to the previous view', async ({
  page
}) => {
  await gotoHub(page);

  // Hub → Events → enter Practice → the workspace opens on the default Live tab (#/event/live).
  await page.getByRole('button', { name: /Events/ }).click();
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeVisible({
    timeout: 15_000
  });
  await page
    .getByRole('button', { name: /Practice/ })
    .first()
    .click();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/live');

  // Open the Registration tab from the sidebar → the hash becomes #/event/registration.
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Registration' })
    .click();
  await expect(page.getByRole('heading', { name: 'Roster for this event' })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/registration');

  // Reload — the key proof: we resume into the SAME event AND the SAME tab (Registration), not the
  // workspace's default Live tab. The active event is server state (#90); the hash restores the tab.
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Roster for this event' })).toBeVisible({
    timeout: 15_000
  });
  expect(new URL(page.url()).hash).toBe('#/event/registration');

  // Browser BACK moves to the previous view (the Live tab) — driven by hashchange.
  await page.goBack();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/live');
  // The Registration heading is gone — we really moved off that tab.
  await expect(page.getByRole('heading', { name: 'Roster for this event' })).toBeHidden();

  // Browser FORWARD returns to Registration.
  await page.goForward();
  await expect(page.getByRole('heading', { name: 'Roster for this event' })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/registration');
});
