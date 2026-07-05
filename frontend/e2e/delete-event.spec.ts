/**
 * Delete-an-event-with-hard-confirm proof (the papercut fix): a created event can be **permanently
 * deleted** from the Events page through an unmissable, type-to-confirm dialog, and is then gone
 * from the list.
 *
 * The flow drives real clicks in headless chromium against the worker's real Director: open the
 * Events picker, create a uniquely-named event, open its Delete affordance, and assert the red
 * "Delete permanently" button stays disabled until the exact event name is re-typed into the
 * confirm field. Then confirm and assert the event row disappears from the list (the delete really
 * took effect server-side).
 *
 * The dialog shows the exact name in a selectable box (no copy button — the Clipboard API needs a
 * secure context and isn't available over the plain-HTTP Director). The RD selects/types it by hand;
 * the spec fills the confirm field directly.
 *
 * Importing `test`/`expect` from `./observability.js` means a failure carries the full-stack dump
 * (browser console, page errors, the Director's log).
 */
import { expect, test } from './observability.js';

/** Return to the home hub (mirrors routing.spec's helper). */
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

/** Open the Events picker ("Choose an event"), switching out of any auto-entered workspace. */
async function gotoPicker(page: import('@playwright/test').Page) {
  await gotoHub(page);
  await page.getByRole('button', { name: /Events/ }).click();
  const picker = page.getByRole('heading', { name: 'Choose an event' });
  const liveNav = page.getByRole('button', { name: /Live control/ });
  await expect(picker.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
  if (await liveNav.isVisible().catch(() => false)) {
    await page.getByRole('button', { name: /Switch event/ }).click();
  }
  await expect(picker).toBeVisible({ timeout: 15_000 });
}

test('create an event, then permanently delete it through the hard-confirm dialog', async ({
  page
}) => {
  await gotoPicker(page);

  // A unique name so the spec is independent of other specs / a reused Director.
  const name = `Doomed Event ${Date.now()}`;

  // Create it via the "New event" dialog (don't auto-open the setup wizard).
  await page
    .getByRole('button', { name: /New event/ })
    .first()
    .click();
  await page.getByRole('textbox', { name: 'Event name' }).fill(name);
  // Uncheck "Set up event" so we land back on the picker, not the wizard.
  const setupToggle = page.getByRole('checkbox', { name: /Set up event after creating/ });
  if (await setupToggle.isChecked().catch(() => false)) await setupToggle.uncheck();
  await page.getByRole('button', { name: /Create & enter/ }).click();

  // The new event entered its workspace; return to the picker to see/delete it in the list.
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await page.getByRole('button', { name: /Switch event/ }).click();
  await expect(page.getByRole('heading', { name: 'Choose an event' })).toBeVisible({
    timeout: 15_000
  });
  const row = page.getByRole('button', { name: `Delete ${name}` });
  await expect(row).toBeVisible({ timeout: 15_000 });

  // Open the hard-delete dialog.
  await row.click();
  const dialog = page.getByRole('dialog');
  await expect(dialog.getByRole('heading', { name: /Delete event permanently/ })).toBeVisible();

  // The selectable name box shows the exact name (no copy button — the Clipboard API needs a secure
  // context and isn't available over the plain-HTTP Director; the RD selects/types it by hand).
  await expect(dialog.getByLabel('Event name to copy')).toHaveText(name);
  await expect(dialog.getByRole('button', { name: /Copy event name/ })).toHaveCount(0);

  // The red confirm button is DISABLED until the exact name is typed.
  const confirm = dialog.getByRole('button', { name: 'Delete permanently' });
  await expect(confirm).toBeDisabled();

  // A wrong name keeps it disabled.
  const field = dialog.getByRole('textbox', { name: 'Confirm event name' });
  await field.fill('not the name');
  await expect(confirm).toBeDisabled();

  // Typing the exact name into the confirm field enables the gated red button.
  await field.fill(name);
  await expect(field).toHaveValue(name);
  await expect(confirm).toBeEnabled();

  // Confirm — the dialog closes and the event row is gone from the list.
  await confirm.click();
  await expect(dialog).toBeHidden({ timeout: 15_000 });
  await expect(page.getByRole('button', { name: `Delete ${name}` })).toBeHidden({
    timeout: 15_000
  });
});
