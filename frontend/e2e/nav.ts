/**
 * Shared navigation helpers for the e2e specs.
 *
 * The console has two sides, **Setup** and **Event**, with a switch between them at the top.
 * The row beside the switch holds the pages of the side that is showing. These helpers get
 * to a page the way a person does: switch sides if the page is on the other one, then click
 * it in the row. (The worker's Director is shared across specs, so an event may or may not
 * be active when a spec starts.)
 */
import { expect, type Locator, type Page } from '@playwright/test';

/** The pages kept for every event. Every other page belongs to the open event. */
const SETUP_PAGES = ['Events', 'Timers', 'Seats', 'Pilots', 'Classes', 'Formats'];

/** The row of pages of the side that is showing. */
export function pagesNav(page: Page): Locator {
  return page.getByRole('navigation', { name: 'Pages' });
}

/** One half of the switch between the two sides. */
export function sideLink(page: Page, side: 'Setup' | 'Event'): Locator {
  return page
    .getByRole('navigation', { name: 'Setup or event' })
    .getByRole('link', { name: new RegExp(`^${side}`) });
}

/** The open event's name, as the Event side of the switch shows it ("None open" with none). */
export function openEvent(page: Page): Locator {
  return page.getByTestId('open-event');
}

/** Load the console and wait for the row of pages. */
export async function gotoConsole(page: Page): Promise<void> {
  await page.goto('/');
  await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
}

/**
 * Open a page: switch to its side if the other one is showing, click it in the row, and
 * wait for its title. `title` defaults to the page's name in the row.
 */
export async function openPage(page: Page, name: string, title: string = name): Promise<void> {
  const link = pagesNav(page).getByRole('link', { name, exact: true });
  if ((await link.count()) === 0) {
    await sideLink(page, SETUP_PAGES.includes(name) ? 'Setup' : 'Event').click();
  }
  await link.click();
  await expect(page.getByRole('heading', { name: title, level: 1 })).toBeVisible({
    timeout: 15_000
  });
}

/** Load the console and open one of its pages. */
export async function gotoPage(page: Page, name: string, title: string = name): Promise<void> {
  await gotoConsole(page);
  await openPage(page, name, title);
}

/** The name of the heat the Run page has on the timer, as its Heat box shows it. */
export function heatOnTimer(page: Page): Locator {
  return page.getByRole('combobox', { name: 'Heat' }).locator('option:checked');
}

/** Open the **Events page**, where an event is opened, added or deleted. */
export async function gotoEvents(page: Page): Promise<void> {
  await gotoPage(page, 'Events');
}
