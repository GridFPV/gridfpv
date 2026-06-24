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

  // ── 3) Abort ch-1 back to Scheduled, then pick ch-2 ⇒ it becomes the current heat ─────────────
  // The picker is locked while ch-1 is Staged (you're committed to that race), so first abort it
  // back to Scheduled — then the switch to ch-2 is allowed.
  await expect(select).toBeDisabled();
  await page.getByRole('button', { name: 'Abort', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(page.locator('.phase').first()).toHaveText('Scheduled', { timeout: 15_000 });
  await expect(select).toBeEnabled();
  await select.selectOption('ch-2');
  await expect(page.locator('.heat-id .value')).toHaveText('ch-2', { timeout: 15_000 });
});

test('the heat picker is locked while a heat is staged, and re-enabled after an abort back to Scheduled', async ({
  page,
  director
}) => {
  // The lock: once the current heat is Staged/Armed/Running you're committed to that race — the
  // picker is disabled and a hint says so. Aborting it back to Scheduled re-enables the picker.
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  const schedule = (heat: string, lineup: string[]) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: { ScheduleHeat: { heat, lineup } }
    });

  const select = page.getByRole('combobox', { name: 'Select current heat' });

  // ── 1) Two heats; pin lk-1 as the current heat via the picker (deterministic, no leftover dep) ──
  expect((await schedule('lk-1', ['Ace', 'Bee'])).ok()).toBeTruthy();
  expect((await schedule('lk-2', ['Cee', 'Dee'])).ok()).toBeTruthy();
  await expect(select.locator('option', { hasText: 'lk-1' })).toHaveCount(1, { timeout: 15_000 });
  await select.selectOption('lk-1');
  await expect(page.locator('.heat-id .value')).toHaveText('lk-1', { timeout: 15_000 });
  // While Scheduled the picker is enabled.
  await expect(select).toBeEnabled();

  // ── 2) Stage lk-1 ⇒ the picker locks (disabled) and the lock hint appears ───────────────────────
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Staged', { timeout: 15_000 });
  await expect(select).toBeDisabled();
  await expect(page.getByTestId('heat-pick-lock-hint')).toBeVisible();

  // ── 3) Abort lk-1 back to Scheduled ⇒ the picker re-enables, hint gone ──────────────────────────
  await page.getByRole('button', { name: 'Abort', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
  await expect(page.locator('.phase').first()).toHaveText('Scheduled', { timeout: 15_000 });
  await expect(select).toBeEnabled();
  await expect(page.getByTestId('heat-pick-lock-hint')).toHaveCount(0);

  // Now the switch works again.
  await select.selectOption('lk-2');
  await expect(page.locator('.heat-id .value')).toHaveText('lk-2', { timeout: 15_000 });
});

test('scheduling a heat that does not move on-deck still appears in the picker without a transition', async ({
  page,
  director
}) => {
  // Regression for the stale-list bug: filling a heat that changes neither `current_heat`
  // (fill-no-steal) NOR `on_deck` (the earliest still-scheduled heat) leaves the whole
  // `LiveRaceState` body unchanged — so the picker (and the Rounds & Heats list), which re-read
  // `/heats` on a stream update, used to stay stale until the next transition. The backend now
  // force-emits a stream envelope on a schedule, so the new heat must appear immediately.
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  const schedule = (heat: string, lineup: string[]) =>
    page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: { ScheduleHeat: { heat, lineup } }
    });

  // The Director is shared across specs, so a prior spec may have left a current heat / on-deck.
  // We don't depend on which heat is current — we drive the current heat explicitly via the picker
  // (a SetCurrentHeat) so the assertions hold regardless of leftover ordering.
  const select = page.getByRole('combobox', { name: 'Select current heat' });

  // 1) Schedule two heats and pin the first as the current heat via the picker. Picking it records
  //    a SetCurrentHeat, so `current_heat` is now sc-1 deterministically (no leftover dependence).
  expect((await schedule('sc-1', ['Ace', 'Bee'])).ok()).toBeTruthy();
  expect((await schedule('sc-2', ['Cee', 'Dee'])).ok()).toBeTruthy();
  await expect(select.locator('option', { hasText: 'sc-1' })).toHaveCount(1, { timeout: 15_000 });
  await select.selectOption('sc-1');
  await expect(page.locator('.heat-id .value')).toHaveText('sc-1', { timeout: 15_000 });

  // 2) The bug repro: a THIRD heat scheduled now. With sc-1 current and an on-deck already present,
  //    appending sc-3 moves neither `current_heat` (fill-no-steal) nor the on-deck (the earliest
  //    still-scheduled heat), so the live `LiveRaceState` body is unchanged. Before the fix the
  //    picker stayed stale until a transition; now the backend wakes the stream on a schedule, so
  //    sc-3 must appear in the picker IMMEDIATELY — no transition driven.
  expect((await schedule('sc-3', ['Eff', 'Gee'])).ok()).toBeTruthy();
  await expect(select.locator('option', { hasText: 'sc-3' })).toHaveCount(1, { timeout: 15_000 });

  // 3) The schedule did NOT steal focus — the current heat is still sc-1.
  await expect(page.locator('.heat-id .value')).toHaveText('sc-1');
});
