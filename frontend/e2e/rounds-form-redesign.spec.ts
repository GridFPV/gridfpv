/**
 * Rounds form redesign — the deliverable proof for the redesigned create/edit form.
 *
 * A real click-through against a **real** Director (open / no token): enter the Practice event,
 * select the built-in Open Class, open **Rounds & Heats** and open the add-round form. Then drive
 * the redesigned form for two formats and assert the **dynamic per-format field set** + the friendly
 * format names, screenshotting each:
 *
 *  - **Open Practice** — the active-channels picker + time limit show; the eligible-class dropdown,
 *    win condition, seeding and channel mode are hidden.
 *  - **Double Elimination** (a bracket format) — the eligible-class **single-select** dropdown, win
 *    condition, seeding and channel mode show; the active-channels picker is hidden.
 *
 * The format selector shows the **friendly** label ("Open Practice", "Double Elimination") while
 * storing the underlying key. Nothing is mocked. Screenshots land in `e2e/screenshots/` for the PR.
 */
import { expect, test } from './observability.js';
import type { Page } from '@playwright/test';

const SHOTS = new URL('./screenshots/', import.meta.url).pathname;

async function enterPractice(page: Page) {
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

async function openTab(page: Page, name: string) {
  await page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();
}

test('the redesigned Rounds form shows the dynamic per-format fields (open practice + a bracket)', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  await page.goto('/');
  await enterPractice(page);

  // Select the Open Class so a class round has an eligible class.
  await openTab(page, 'Classes & Roster');
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

  await openTab(page, 'Rounds & Heats');
  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill('Form Redesign Demo');

  // ── Open Practice: friendly label, channels picker + time limit, no class / win / seeding ──────
  const formatSelect = form.getByLabel('Format');
  // The selector shows the friendly label while the option value stays the key.
  await expect(formatSelect.locator('option', { hasText: 'Open Practice' })).toHaveCount(1);
  await expect(formatSelect.locator('option', { hasText: 'Double Elimination' })).toHaveCount(1);
  await formatSelect.selectOption('open_practice');
  await expect(form.getByRole('group', { name: 'Active channels' })).toBeVisible();
  await expect(form.getByLabel('Time limit minutes')).toBeVisible();
  await expect(form.getByLabel('Eligible class')).toBeHidden();
  await expect(form.getByLabel('Win condition')).toBeHidden();
  await expect(form.getByLabel('Seeding')).toBeHidden();
  await expect(form.getByLabel('Channel mode')).toBeHidden();
  await form.screenshot({ path: `${SHOTS}rounds-form-open-practice.png` });

  // ── Double Elimination (a bracket): class single-select + win + seeding + channel mode + params ─
  await formatSelect.selectOption('double_elim');
  await expect(form.getByRole('group', { name: 'Active channels' })).toBeHidden();
  // The eligible class is a single-select <select> (Rounds form redesign item 6).
  const classSelect = form.getByLabel('Eligible class');
  await expect(classSelect).toBeVisible();
  await expect(classSelect).toHaveJSProperty('tagName', 'SELECT');
  await classSelect.selectOption({ label: 'Open Class' });
  await expect(form.getByLabel('Win condition')).toBeVisible();
  await expect(form.getByLabel('Seeding')).toBeVisible();
  await expect(form.getByLabel('Channel mode')).toBeVisible();
  // double_elim declares a `bracket_reset` bool param, surfaced inline (item 4).
  await expect(form.getByLabel('Bracket reset value')).toBeVisible();
  // The start-procedure delays are entered in seconds (item 3).
  await expect(form.getByLabel('Start min delay seconds')).toBeVisible();
  // The grace defaults to 30s (item 5).
  await expect(form.getByLabel('Grace window seconds')).toHaveValue('30');
  await form.screenshot({ path: `${SHOTS}rounds-form-bracket.png` });

  // Clean up the shared Director's event back to empty (no round was saved).
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
