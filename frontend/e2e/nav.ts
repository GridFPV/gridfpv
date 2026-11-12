/**
 * Shared navigation helpers for the e2e specs.
 *
 * Every console page sits under one row of pages. These helpers get to a page the way a
 * person does, by clicking it in that row, whichever page the console happened to load on.
 * (The worker's Director is shared across specs, so an event may or may not be active when
 * a spec starts.)
 */
import { expect, type Locator, type Page } from '@playwright/test';

/** The row of pages at the top of the console. */
export function pagesNav(page: Page): Locator {
  return page.getByRole('navigation', { name: 'Pages' });
}

/** Load the console and wait for the row of pages. */
export async function gotoConsole(page: Page): Promise<void> {
  await page.goto('/');
  await expect(pagesNav(page)).toBeVisible({ timeout: 15_000 });
}

/** Click a page in the row and wait for its title. `title` defaults to the link's name. */
export async function openPage(page: Page, name: string, title: string = name): Promise<void> {
  await pagesNav(page).getByRole('link', { name, exact: true }).click();
  await expect(page.getByRole('heading', { name: title, level: 1 })).toBeVisible({
    timeout: 15_000
  });
}

/** Load the console and open one of its pages. */
export async function gotoPage(page: Page, name: string, title: string = name): Promise<void> {
  await gotoConsole(page);
  await openPage(page, name, title);
}

/** A tab under a page's title (Classes beside Pilots, Timers beside Seats). */
export async function openTab(page: Page, row: string, name: string): Promise<void> {
  await page
    .getByRole('navigation', { name: row })
    .getByRole('link', { name, exact: true })
    .click();
  await expect(page.getByRole('heading', { name, level: 1 })).toBeVisible({ timeout: 15_000 });
}

/** The name of the heat the Run page has on the timer, as its Heat box shows it. */
export function heatOnTimer(page: Page): Locator {
  return page.getByRole('combobox', { name: 'Heat' }).locator('option:checked');
}

/** Open the **Events page**, where an event is opened, added or deleted. */
export async function gotoEvents(page: Page): Promise<void> {
  await gotoConsole(page);
  await page.locator('header a.event').click();
  await expect(page.getByRole('heading', { name: 'Events', level: 1 })).toBeVisible({
    timeout: 15_000
  });
}
