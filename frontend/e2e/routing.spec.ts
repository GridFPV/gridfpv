/**
 * Hash-based URL routing proof (#118): a browser **refresh stays on the current page/tab**, and
 * **back/forward** move between views.
 *
 * The console's view used to live only in in-memory state, so a reload reset it (back to the active
 * event's workspace or the hub) — losing where you were. Now the view is reflected in
 * `location.hash` (e.g. `#/pilots`, `#/event/classes-roster`), so a refresh restores it, bookmarks
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

/**
 * Return to the home hub. The hash is authoritative (#118) so a bare `goto('/')` lands on the hub
 * directly; the workspace-resume fallback below is kept defensively in case a prior test left the
 * shell mid-workspace, but it should no longer be reached on a fresh load.
 */
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

/**
 * Open the **Events picker** ("Choose an event"). The worker's Director is shared, so an event may
 * already be active server-side; on a fresh load the shell hydrates `currentEvent` from it, and the
 * Events page then auto-enters that event's workspace (#118). When that happens we use "Switch
 * event" to return to the picker (it clears the local event). So this lands on the picker whether or
 * not an event was active.
 */
async function gotoPicker(page: import('@playwright/test').Page) {
  await gotoHub(page);
  await page.getByRole('button', { name: /Events/ }).click();
  const picker = page.getByRole('heading', { name: 'Choose an event' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(picker.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    // Auto-entered the active event's workspace — "Switch event" leaves it and lands on the picker.
    await page.getByRole('button', { name: /Switch event/ }).click();
  }
  await expect(picker).toBeVisible({ timeout: 15_000 });
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
  // Hub → Events picker → enter Practice → the workspace opens on the default Live tab (#/event/live).
  await gotoPicker(page);
  await page
    .getByRole('button', { name: /Practice/ })
    .first()
    .click();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/live');

  // Open the Classes & Roster tab from the sidebar → the hash becomes #/event/classes-roster.
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/classes-roster');

  // Reload — the key proof: we resume into the SAME event AND the SAME tab, not the workspace's
  // default Live tab. The active event is server state (#90); the hash restores the tab.
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  expect(new URL(page.url()).hash).toBe('#/event/classes-roster');

  // Browser BACK moves to the previous view (the Live tab) — driven by hashchange.
  await page.goBack();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/live');
  // The Classes & Roster heading is gone — we really moved off that tab.
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeHidden();

  // Browser FORWARD returns to Classes & Roster.
  await page.goForward();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/classes-roster');
});

test('on the hub with an active event, a refresh stays on the hub (no auto-resume into the event)', async ({
  page
}) => {
  // First make an event ACTIVE on the Director (server state, #90): enter Practice. After this the
  // Director's active event is set, which is exactly the condition that used to yank a hub reload
  // into the workspace. The fix (#118) makes the hash authoritative, so it must no longer happen.
  //
  // The Director is worker-scoped and reused across specs, so an event may already be active when
  // this spec runs. The Events page auto-enters the workspace when an event is active, so clicking
  // Events lands us in the workspace either way (directly if already active, or after we pick
  // Practice in the picker) — leaving the Director's active event set, which is what we need.
  await gotoHub(page);
  await page.getByRole('button', { name: /Events/ }).click();
  const picker = page.getByRole('heading', { name: 'Choose an event' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(picker.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await picker.isVisible().catch(() => false)) {
    await page
      .getByRole('button', { name: /Practice/ })
      .first()
      .click();
  }
  await expect(liveNav).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toBe('#/event/live');

  // Leave the event back to the hub via the breadcrumb Home crumb. `leaveEvent()` tears down the
  // local workspace but does NOT clear the Director's active event (it stays live for the picker's
  // Active pill), so the active-event condition persists across the reload below. Hash → canonical #/.
  await page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Home' })
    .click();
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => new URL(page.url()).hash).toMatch(/^(#\/?)?$/);

  // Reload — the key proof: with the active event STILL set on the Director, we stay on the hub
  // (hub cards visible, no workspace), rather than being auto-pulled back into the event workspace.
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible({ timeout: 15_000 });
  // The hub stayed: the hash is the canonical #/ (or empty), not #/event/<tab>.
  expect(new URL(page.url()).hash).toMatch(/^(#\/?)?$/);
  // And we are NOT in the workspace — the Live control sidebar button is absent on the hub.
  await expect(page.getByRole('button', { name: /Live control/ })).toBeHidden();
});
