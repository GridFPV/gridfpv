/**
 * Getting around a remote server's page (docs/primitives.md §13): a pilot area (Events,
 * Account) and an admin area (Club, Server). Each part has a row of pages.
 */
import type { Page } from '@playwright/test';

/**
 * Open one of the parts of the server's page, by the word on it. Events and Account are in
 * the pilot area, Club and Server in the admin area: the area is switched first when the
 * part is in the other one.
 */
export async function openServerPart(
  page: Page,
  part: 'Events' | 'Account' | 'Club' | 'Server'
): Promise<void> {
  const link = page
    .getByRole('navigation', { name: 'Parts of this server' })
    .getByRole('link', { name: part, exact: true });
  const area = page
    .getByRole('navigation', { name: 'Area' })
    .getByRole('link', { name: part === 'Club' || part === 'Server' ? 'Admin' : 'Pilot' });
  await link.or(area).first().waitFor();
  if (!(await link.isVisible())) await area.click();
  await link.click();
}

/** Open a page of the part that is open, by the word on it. */
export async function openServerPage(page: Page, name: string): Promise<void> {
  await page
    .getByRole('navigation', { name: 'Pages' })
    .getByRole('link', { name, exact: true })
    .click();
}
