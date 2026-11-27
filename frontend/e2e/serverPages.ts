/**
 * Getting around a remote server's page (docs/primitives.md §13): it has parts (Events,
 * Account, Club, Server), and each part has a row of pages.
 */
import type { Page } from '@playwright/test';

/** Open one of the parts of the server's page, by the word on it. */
export async function openServerPart(
  page: Page,
  part: 'Events' | 'Account' | 'Club' | 'Server'
): Promise<void> {
  await page
    .getByRole('navigation', { name: 'Parts of this server' })
    .getByRole('link', { name: part, exact: true })
    .click();
}

/** Open a page of the part that is open, by the word on it. */
export async function openServerPage(page: Page, name: string): Promise<void> {
  await page
    .getByRole('navigation', { name: 'Pages' })
    .getByRole('link', { name, exact: true })
    .click();
}
