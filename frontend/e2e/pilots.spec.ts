/**
 * Pilot directory lifecycle through the console UI (#74) — the deliverable proof for the pilot
 * registration slice.
 *
 * A person opens the console, lands on the **home hub**, opens the **Pilots** page, and drives the
 * full directory CRUD against a **real** Director (open / no token, full-trust by default): **add**
 * a pilot (callsign + real name + country + color + a custom attribute) and see it listed; **edit**
 * it and **clear** the color and country, confirming they actually clear (the clear-via-`null`
 * wiring); then **remove** it. Every step is a real click/input in headless chromium on the real
 * `POST/PUT/DELETE /pilots` path — nothing mocked.
 *
 * Importing `test`/`expect` from `./observability.js` means a failure carries the full-stack dump
 * (browser console, page errors, the Director's server log). The worker's Director is shared, so
 * this spec uses a unique callsign and cleans up after itself (the final remove) to stay isolated.
 */
import { expect, test } from './observability.js';

const CALLSIGN = `E2E-Pilot-${Date.now()}`;

test('RD adds, edits (clearing color + country), and removes a directory pilot', async ({
  page
}) => {
  await page.goto('/');

  // The worker's Director may have an active event left by a prior spec (#90): if we resumed into
  // the workspace, use the breadcrumb Home to get back to the hub.
  const pilotsCard = page.getByRole('heading', { name: 'Pilots' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(pilotsCard.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page
      .getByRole('navigation', { name: 'Breadcrumb' })
      .getByRole('button', { name: 'Home' })
      .click();
  }

  // ── Hub → Pilots page ────────────────────────────────────────────────────────────────
  await page.getByRole('button', { name: /Pilots/ }).click();
  await expect(page.getByRole('heading', { name: 'Pilots', level: 1 })).toBeVisible({
    timeout: 15_000
  });

  // ── Add a pilot: callsign + real name + country + color + a custom attribute ───────────
  await page.getByRole('button', { name: '+ Add pilot' }).click();
  const addForm = page.getByRole('form', { name: 'Add pilot' });
  await expect(addForm).toBeVisible();

  await addForm.getByLabel('Callsign').fill(CALLSIGN);
  await addForm.getByLabel('Real name').fill('Ada Lovelace');
  // Color (a native color input writes #rrggbb).
  await addForm.getByLabel('Color').fill('#ff8800');

  // Country: open the searchable selector, search, pick United States.
  await addForm.getByRole('button', { name: 'Country', exact: true }).click();
  await addForm.getByRole('textbox', { name: 'Search countries' }).fill('United States');
  await addForm.getByRole('option', { name: /United States/ }).click();

  // A custom attribute row.
  await addForm.getByRole('button', { name: '+ Add attribute' }).click();
  await addForm.getByLabel('Attribute 1 key').fill('bib');
  await addForm.getByLabel('Attribute 1 value').fill('42');

  // Submit (open Director — no token prompt). `exact` so it picks the dialog's submit, not the
  // page header's "+ Add pilot".
  await page.getByRole('button', { name: 'Add pilot', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // It appears in the directory with its callsign + name, and a US flag is rendered.
  const list = page.getByRole('list', { name: 'Pilot directory' });
  const row = list.getByRole('listitem').filter({ hasText: CALLSIGN });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText('Ada Lovelace')).toBeVisible();
  // SVG flag (never emoji) is present on the row.
  await expect(row.locator('.flag svg')).toBeVisible();
  // The color swatch reflects the chosen color.
  await expect(row.locator('.color-swatch')).toBeVisible();

  // ── Edit: clear the color and the country ──────────────────────────────────────────────
  await row.getByRole('button', { name: 'Edit' }).click();
  const editForm = page.getByRole('form', { name: 'Edit pilot' });
  await expect(editForm).toBeVisible();
  // The country selector shows the previously-set United States.
  await expect(editForm.getByRole('button', { name: 'Country', exact: true })).toContainText(
    'United States'
  );

  // Clear the color (its inline Clear button) and the country (the selector's clear control).
  await editForm.getByRole('button', { name: 'Clear', exact: true }).click();
  await editForm.getByRole('button', { name: 'Clear country' }).click();
  await page.getByRole('button', { name: 'Save changes' }).click();

  // The row no longer renders a flag or a color swatch — the clear actually persisted (the edit
  // sent `null` for color/country; re-reading the directory shows them gone).
  await expect(row.locator('.flag')).toHaveCount(0, { timeout: 15_000 });
  await expect(row.locator('.color-swatch')).toHaveCount(0);
  // The callsign + name are unchanged (only color/country were cleared).
  await expect(row.getByText(CALLSIGN)).toBeVisible();
  await expect(row.getByText('Ada Lovelace')).toBeVisible();

  // ── Remove (with the confirm step) ─────────────────────────────────────────────────────
  await row.getByRole('button', { name: 'Remove' }).click();
  const confirm = page.getByRole('dialog').filter({ hasText: 'Remove pilot' });
  await expect(confirm).toBeVisible();
  await confirm.getByRole('button', { name: 'Remove' }).click();

  // The pilot is gone from the directory.
  await expect(list.getByRole('listitem').filter({ hasText: CALLSIGN })).toHaveCount(0, {
    timeout: 15_000
  });
});
