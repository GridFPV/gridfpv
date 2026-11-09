/**
 * Shared navigation helpers for the e2e specs.
 *
 * The worker's Director is shared across specs, so an event may already be the Director's active
 * event when a spec starts. These helpers land on a known page whichever way that is.
 */
import { expect, type Locator, type Page } from '@playwright/test';

/** The breadcrumb's **Events** crumb — a button only on an open event's page. */
export function eventsCrumb(page: Page): Locator {
  return page
    .getByRole('navigation', { name: 'Breadcrumb' })
    .getByRole('button', { name: 'Events' });
}

/** Load the console on the home hub. The hash is authoritative (#118), so `/` is always the hub. */
export async function gotoHub(page: Page): Promise<void> {
  await page.goto('/');
  await expect(page.getByRole('heading', { name: 'Pilots' })).toBeVisible({ timeout: 15_000 });
}

/**
 * Open the **Events page** ("Choose an event"). When an event is active on the Director, the
 * shell opens that event as soon as the Events page is reached; the breadcrumb's Events crumb
 * closes it again and lands on the list.
 */
export async function gotoEvents(page: Page): Promise<void> {
  await gotoHub(page);
  await page.getByRole('button', { name: /Events/ }).click();
  const list = page.getByRole('heading', { name: 'Choose an event' });
  const crumb = eventsCrumb(page);
  await expect(list.or(crumb).first()).toBeVisible({ timeout: 15_000 });
  if (await crumb.isVisible().catch(() => false)) await crumb.click();
  await expect(list).toBeVisible({ timeout: 15_000 });
}
