/**
 * Open-practice format through the console UI (open-practice Slice 2) — the deliverable proof.
 *
 * A real click-through against a **real** Director (open / no token): enter the Practice event,
 * open **Rounds & Heats**, add a round with the **open_practice** format, pick a couple of the
 * primary timer's **active channels** (the picker that swaps in for the class/seeding inputs),
 * screenshot the picker, save, **Fill** the round to mint the open practice heat, jump to **Live
 * control**, and assert the **per-channel practice board** renders one row per active channel
 * (labelled band + channel · MHz). Then exercise the **New run** reset and clean up the round.
 *
 * The Practice event's built-in Mock timer ships an 8-seat Raceband pool, so the picker / board
 * populate with no extra timer config. Screenshots land in `e2e/screenshots/` for the PR.
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

test('RD defines an open-practice round, picks active channels, and runs a per-channel board', async ({
  page
}) => {
  const LABEL = `E2E-OpenPractice-${Date.now()}`;
  await page.goto('/');
  await enterPractice(page);

  // ── Rounds & Heats → add an open-practice round ─────────────────────────────────────────────
  await openTab(page, 'Rounds & Heats');
  await expect(page.getByRole('heading', { name: 'Rounds', exact: true })).toBeVisible({
    timeout: 15_000
  });

  await page.getByRole('button', { name: '+ Add round' }).click();
  const form = page.getByRole('form', { name: 'Add round' });
  await expect(form).toBeVisible();
  await form.getByLabel('Label').fill(LABEL);

  // Picking open_practice swaps the eligible-classes picker for the active-channels picker, hides
  // the win-condition input (open practice does no scoring), and reveals the optional Time limit.
  await form.getByLabel('Format').selectOption('open_practice');
  await expect(form.getByRole('group', { name: 'Active channels' })).toBeVisible();
  await expect(form.getByLabel('Eligible Open Class')).toBeHidden();
  await expect(form.getByLabel('Win condition')).toBeHidden();
  // The time limit is now a single Minutes field (no separate hours input).
  await expect(form.getByLabel('Time limit minutes')).toBeVisible();
  await expect(form.getByLabel('Time limit hours')).toBeHidden();

  // The primary timer's Raceband seats label by band + channel · MHz (node 0 → R1 · 5658).
  const r1 = form.getByLabel('Channel Raceband R1 · 5658');
  const r2 = form.getByLabel('Channel Raceband R2 · 5695');
  await expect(r1).toBeVisible();
  await expect(r2).toBeVisible();

  // Activate two channels, set a short (1-minute) practice time limit, and screenshot the picker.
  await r1.check();
  await r2.check();
  await form.getByLabel('Time limit minutes').fill('1');
  await page.screenshot({ path: `${SHOTS}open-practice-picker.png`, fullPage: true });

  await page.getByRole('button', { name: 'Add round', exact: true }).click();
  await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

  // The round lists as an open-practice round seeded from 2 channels, showing its time limit.
  const row = page.getByRole('listitem').filter({ hasText: LABEL });
  await expect(row).toBeVisible({ timeout: 15_000 });
  await expect(row.getByText('open_practice')).toBeVisible();
  await expect(row.getByText(/Open practice · 2 channel/)).toBeVisible();
  await expect(row.getByText('1m', { exact: true })).toBeVisible();

  // ── The heat is auto-created (no manual Fill) ───────────────────────────────────────────────
  const heatSection = page.getByRole('region', { name: `Heats for ${LABEL}` });
  // An open-practice round drops the manual "Fill next heat" control — its heat is created for it.
  await expect(heatSection.getByRole('button', { name: 'Fill next heat' })).toHaveCount(0);
  // The auto-created heat lands (its node lineup shows in the heats list).
  await expect(heatSection.getByText(/node-0/).first()).toBeVisible({ timeout: 15_000 });

  // ── Live control → the per-channel practice board ───────────────────────────────────────────
  await openTab(page, 'Live control');
  const board = page.getByRole('list', { name: 'Per-channel practice board' });
  await expect(board).toBeVisible({ timeout: 15_000 });
  // One row per active channel, labelled by its timer channel.
  await expect(page.getByLabel('Channel Raceband R1 · 5658')).toBeVisible();
  await expect(page.getByLabel('Channel Raceband R2 · 5695')).toBeVisible();
  await page.screenshot({ path: `${SHOTS}open-practice-board.png`, fullPage: true });

  // ── Reset: New run re-fills the round to clear the board (a fresh practice session) ─────────
  const newRun = page.getByRole('button', { name: /New run/ });
  await expect(newRun).toBeVisible();
  await newRun.click();
  // The board still renders its per-channel rows after the reset (a fresh, cleared run).
  await expect(page.getByLabel('Channel Raceband R1 · 5658')).toBeVisible({ timeout: 15_000 });

  // ── Clean up: remove the round so the shared Director's event goes back to empty ────────────
  await openTab(page, 'Rounds & Heats');
  const cleanupRow = page.getByRole('list').getByRole('listitem').filter({ hasText: LABEL });
  await cleanupRow.getByRole('button', { name: 'Remove' }).click();
  await expect(page.getByRole('list').getByRole('listitem').filter({ hasText: LABEL })).toHaveCount(
    0,
    { timeout: 15_000 }
  );
});
