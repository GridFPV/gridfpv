/**
 * Manual current-heat selection — the fill-no-steal + heat-picker proof, captured against a real
 * Director (open / no token):
 *
 *  1. A first heat is scheduled and Staged so it is the current heat on the timer.
 *  2. A SECOND heat is filled (a bare ScheduleHeat) — and Live control's focus must NOT jump to it
 *     (the regression this feature fixes). The current heat stays the first, staged heat.
 *  3. The RD picks the second heat in the Live heat picker → it becomes the current heat (the live
 *     state follows the explicit SetCurrentHeat selection).
 *
 * A screenshot of the Live heat picker is written to `GRIDFPV_SHOTS` when set.
 */
import { expect, test } from './observability.js';

async function enterPractice(page: import('@playwright/test').Page) {
  const liveNav = page.getByRole('button', { name: /Live control/ });
  const eventsCard = page.getByRole('button', { name: /Events/ });
  await expect(liveNav.or(eventsCard).first()).toBeVisible({ timeout: 15_000 });
  if (!(await liveNav.isVisible().catch(() => false))) {
    await eventsCard.click();
    const picker = page.getByRole('heading', { name: 'Choose an event' });
    await expect(picker.or(liveNav).first()).toBeVisible({ timeout: 15_000 });
    if (await picker.isVisible().catch(() => false)) {
      await page
        .getByRole('button', { name: /Practice/ })
        .first()
        .click();
    }
    await expect(liveNav).toBeVisible({ timeout: 15_000 });
  }
}

const shot = async (locator: import('@playwright/test').Locator, name: string) => {
  if (process.env.GRIDFPV_SHOTS) {
    await locator.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/${name}.png` });
  }
};

test('filling a heat does not steal Live focus; the heat picker selects the current heat', async ({
  page,
  director
}) => {
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  const schedule = (heat: string, lineup: string[]) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: { ScheduleHeat: { heat, lineup } }
    });

  // ── 1) First heat scheduled + staged ⇒ it is the current heat ────────────────────────────────
  expect((await schedule('ch-1', ['Ace', 'Bee'])).ok()).toBeTruthy();
  await expect(page.locator('.heat-id .value')).toHaveText('ch-1', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged', { timeout: 15_000 });

  // ── 2) Fill a SECOND heat — focus must NOT jump to it ────────────────────────────────────────
  expect((await schedule('ch-2', ['Cee', 'Dee'])).ok()).toBeTruthy();
  // The picker now lists both heats (untagged ⇒ labelled by their bare ids).
  const select = page.getByRole('combobox', { name: 'Select current heat' });
  await expect(select.locator('option', { hasText: 'ch-2' })).toHaveCount(1, { timeout: 15_000 });
  // The current heat is STILL ch-1 (filling ch-2 did not steal focus) and it is still Staged.
  await expect(page.locator('.heat-id .value')).toHaveText('ch-1');
  await expect(page.locator('.phase').first()).toHaveText('Staged');
  await shot(page.locator('.hud-heat'), 'live-heat-picker');

  // ── 3) Pick ch-2 in the picker ⇒ it becomes the current heat ─────────────────────────────────
  await select.selectOption('ch-2');
  await expect(page.locator('.heat-id .value')).toHaveText('ch-2', { timeout: 15_000 });

  // ── Clean up: discard both heats so the shared Director is left tidy ──────────────────────────
  // ch-1 is staged → abort back to scheduled, then both are harmless leftover scheduled heats; an
  // open practice Director resets per-spec, so leaving them scheduled is fine. Abort ch-1 to clear
  // its staged state via the picker + transition.
  await select.selectOption('ch-1');
  await expect(page.locator('.heat-id .value')).toHaveText('ch-1', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Abort', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
});
