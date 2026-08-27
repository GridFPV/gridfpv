/**
 * Heat-lifecycle Slice 3 screenshot proof — the three new console surfaces, captured against a real
 * Director (open / no token): the **staging countdown** (a heat in Staged), the **arming state**
 * (after Start, before the runtime auto-advances to Running), and the **per-round config form**
 * (staging mm:ss + start min/max + grace).
 *
 * Screenshots are written to `GRIDFPV_SHOTS` when set (the harness passes it); the run still
 * asserts each surface is visible so this doubles as a smoke test of the Slice 3 UX.
 */
import { expect, test } from './observability.js';

async function enterPractice(page: import('@playwright/test').Page) {
  const liveNav = page.getByRole('button', { name: /Race control/ });
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

test('Slice 3 surfaces: staging countdown, arming state, and the round config form', async ({
  page,
  director
}) => {
  // ── Schedule a heat over the open control path (no round tag ⇒ default 5:00 staging) ──────────
  const ack = await page.request.post(`${director.eventRoot}/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: 's3-1', lineup: ['Ace', 'Bee', 'Cee'] } }
  });
  expect(ack.ok()).toBeTruthy();
  // Explicitly focus THIS heat as the current heat (SetCurrentHeat) over the control path. The
  // shared worker Director's `current_heat` stays pinned to whatever heat a prior spec last touched
  // (it never resets between specs), and filling a heat does NOT steal Live focus (current-heat.spec.ts)
  // — so this server-side selection keeps the run independent of spec order.
  const focused = await page.request.post(`${director.eventRoot}/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { SetCurrentHeat: { heat: 's3-1' } }
  });
  expect(focused.ok()).toBeTruthy();

  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });
  await expect(page.locator('.heat-id .value')).toHaveText('s3-1', { timeout: 15_000 });

  // ── Staging countdown ────────────────────────────────────────────────────────────────────────
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  const staging = page.getByRole('status', { name: 'Staging countdown' });
  await expect(staging).toBeVisible();
  await expect(page.getByLabel('Staging time remaining')).toBeVisible();
  await shot(staging, 'staging-countdown');

  // ── Arming state (after Start; the runtime then auto-advances to Running) ─────────────────────
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  const arming = page.getByRole('status', { name: 'Arming' });
  await expect(arming).toBeVisible();
  await expect(arming).toHaveText(/Arming… stand by/);
  await shot(arming, 'arming-state');

  // Let it auto-advance, then clean the heat up (Abort) so the shared Director is left tidy.
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Discard', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();

  // ── Round config form (staging mm:ss + start min/max + grace) ─────────────────────────────────
  // Select the Open Class so a round can be added, then open the add-round form and screenshot it.
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  const classBox = page
    .getByRole('list', { name: 'Class directory' })
    .getByRole('listitem')
    .filter({ hasText: 'Open Class' })
    .getByRole('checkbox', { name: 'Select Open Class' });
  if (!(await classBox.isChecked())) {
    const classesSaved = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await classBox.check();
    await classesSaved;
  }
  await expect(classBox).toBeChecked();

  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Rounds & Heats' })
    .click();
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill('Slice3 Config');
  await form.getByLabel('Eligible class').selectOption({ label: 'Open Class' });
  await form.getByLabel('Staging minutes').fill('3');
  await form.getByLabel('Staging seconds').fill('30');
  await form.getByLabel('Start min delay seconds').fill('1.5');
  await form.getByLabel('Start max delay seconds').fill('3.5');
  await form.getByLabel('Grace window seconds').fill('4');
  // The Start & timing fieldset is present and populated (start delays in seconds — item 3).
  await expect(form.getByLabel('Staging minutes')).toHaveValue('3');
  await expect(form.getByLabel('Start max delay seconds')).toHaveValue('3.5');
  await shot(form, 'round-config-form');

  // Clean up: cancel the form, then back on Classes & Roster deselect the class so the shared
  // Director resets to empty for the next spec.
  await form.getByRole('button', { name: 'Cancel' }).click();
  await page
    .getByRole('navigation', { name: 'Screens' })
    .getByRole('button', { name: 'Classes & Roster' })
    .click();
  await expect(page.getByRole('heading', { name: 'Present pilots' })).toBeVisible({
    timeout: 15_000
  });
  if (await classBox.isChecked()) {
    const classesCleared = page.waitForResponse(
      (r) => /\/events\/.+\/classes$/.test(r.url()) && r.request().method() === 'PUT'
    );
    await classBox.uncheck();
    await classesCleared;
  }
});
