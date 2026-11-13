/**
 * Hash-based URL routing proof: a browser **refresh stays on the current page**, and
 * **back/forward** move between pages.
 *
 * The page is reflected in `location.hash` (e.g. `#/pilots`, `#/event`), so a refresh restores
 * it, bookmarks work, and the browser's back/forward navigate between pages.
 *
 * Why a hash and not a path: `/pilots`, `/events`, `/classes` are Director API routes — a path
 * router would collide with the server. The `#` fragment stays client-side, so it can't.
 *
 * This spec drives real clicks in headless chromium against the worker's real Director and asserts
 * on `location.hash` plus the rendered DOM. Importing `test`/`expect` from `./observability.js`
 * means a failure carries the full-stack dump (browser console, page errors, the Director's log).
 */
import { E2E_EVENT_NAME, expect, test } from './observability.js';
import {
  gotoConsole,
  gotoEvents,
  gotoPage,
  openEvent,
  openPage,
  pagesNav,
  sideLink
} from './nav.js';

const hash = (url: string): string => new URL(url).hash;

test('a refresh on the Pilots page stays on Pilots (hash reflects the page)', async ({ page }) => {
  await gotoPage(page, 'Pilots');
  await expect.poll(() => hash(page.url())).toBe('#/pilots');
  // The row marks the page that is showing.
  await expect(pagesNav(page).getByRole('link', { name: 'Pilots' })).toHaveAttribute(
    'aria-current',
    'page'
  );

  // Reload — the key proof: we stay on Pilots.
  await page.reload();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  expect(hash(page.url())).toBe('#/pilots');
});

test('an open event survives a refresh, and its name is on the switch', async ({ page }) => {
  // Events → open the worker's event: its page opens at #/event.
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  const title = page.getByRole('heading', { name: E2E_EVENT_NAME, level: 1 });
  await expect(title).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => hash(page.url())).toBe('#/event');
  await expect(openEvent(page)).toHaveText(E2E_EVENT_NAME);
  await expect(sideLink(page, 'Event')).toHaveAttribute('aria-current', 'true');

  // Reload — the key proof: we resume into the SAME event. The active event is server state;
  // the hash says to show it.
  await page.reload();
  await expect(title).toBeVisible({ timeout: 15_000 });
  expect(hash(page.url())).toBe('#/event');
});

test('browser back and forward move between pages', async ({ page }) => {
  // Pilots → Classes → Formats, each a hash change.
  await gotoPage(page, 'Pilots');
  const pilots = page.getByRole('heading', { name: 'Pilots', level: 1 });
  await openPage(page, 'Classes');
  const classes = page.getByRole('heading', { name: 'Classes', level: 1 });
  await expect.poll(() => hash(page.url())).toBe('#/classes');
  await openPage(page, 'Formats');
  await expect.poll(() => hash(page.url())).toBe('#/formats');

  // BACK twice: Formats → Classes → Pilots — driven by hashchange.
  await page.goBack();
  await expect(classes).toBeVisible({ timeout: 15_000 });
  await page.goBack();
  await expect(pilots).toBeVisible({ timeout: 15_000 });
  await expect.poll(() => hash(page.url())).toBe('#/pilots');
  await expect(classes).toBeHidden();

  // FORWARD twice returns to Formats.
  await page.goForward();
  await page.goForward();
  await expect(page.getByRole('heading', { name: 'Formats', level: 1 })).toBeVisible({
    timeout: 15_000
  });
  await expect.poll(() => hash(page.url())).toBe('#/formats');
});

test('with an event open, the Events page stays put on a refresh', async ({ page }) => {
  // First make an event ACTIVE on the Director (server state) by opening it.
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  await expect.poll(() => hash(page.url())).toBe('#/event');
  const active = (await (await page.request.get('/active-event')).json()) as {
    event: { name: string } | null;
  };
  expect(active.event?.name).toBe(E2E_EVENT_NAME);

  // To the Events page, on the Setup side. The event stays open.
  await openPage(page, 'Events');
  const events = page.getByRole('heading', { name: 'Events', level: 1 });
  await expect.poll(() => hash(page.url())).toBe('#/events');
  await expect(sideLink(page, 'Setup')).toHaveAttribute('aria-current', 'true');

  // Reload — the key proof: we stay on the Events page rather than being pulled back into
  // the open event. That is where another event is opened.
  await page.reload();
  await expect(events).toBeVisible({ timeout: 15_000 });
  expect(hash(page.url())).toBe('#/events');
  await expect(openEvent(page)).toHaveText(E2E_EVENT_NAME);
});

test('the switch moves between Setup and the event, and each side remembers its page', async ({
  page
}) => {
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  await expect.poll(() => hash(page.url())).toBe('#/event');

  // On the event side the row is the event's pages. Go to Results.
  await openPage(page, 'Results');
  await expect(pagesNav(page).getByRole('link')).toHaveText([
    'Details',
    'Phases',
    'Run',
    'Marshal',
    'Results',
    'Overlays',
    'Help'
  ]);

  // Setup shows what is kept for every event. Go to Formats.
  await sideLink(page, 'Setup').click();
  await expect(pagesNav(page).getByRole('link')).toHaveText([
    'Events',
    'Timers',
    'Seats',
    'Pilots',
    'Classes',
    'Formats',
    'Help'
  ]);
  await openPage(page, 'Formats');

  // Back to the event: the page it was on, not its first page.
  await sideLink(page, 'Event').click();
  await expect(page.getByRole('heading', { name: 'Results', level: 1 })).toBeVisible();
  await expect.poll(() => hash(page.url())).toBe('#/event/results');
  // And back to Setup: Formats again.
  await sideLink(page, 'Setup').click();
  await expect.poll(() => hash(page.url())).toBe('#/formats');

  // Help is on both rows and does not change sides.
  await openPage(page, 'Help');
  await expect(sideLink(page, 'Setup')).toHaveAttribute('aria-current', 'true');
});

test('an event page with the hash left empty is the open event', async ({ page }) => {
  await gotoEvents(page);
  await page
    .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
    .first()
    .click();
  await expect(page.getByRole('heading', { name: E2E_EVENT_NAME, level: 1 })).toBeVisible({
    timeout: 15_000
  });
  // A bare load lands on the open event, and every page of it is one click away.
  await gotoConsole(page);
  await expect(page.getByRole('heading', { name: E2E_EVENT_NAME, level: 1 })).toBeVisible({
    timeout: 15_000
  });
  await openPage(page, 'Phases');
  await expect.poll(() => hash(page.url())).toBe('#/event/phases');
  await openPage(page, 'Results');
  await expect.poll(() => hash(page.url())).toBe('#/event/results');
  await openPage(page, 'Help');
  await expect.poll(() => hash(page.url())).toBe('#/help');
});
