/**
 * Collapsible sections on the data-heavy event stages (ui: collapsible sections) — the deliverable
 * proof.
 *
 * The Classes & Roster and Rounds & Heats stages get dense (20 pilots across classes; many rounds
 * each with a heats list), so the RD collapses sections to manage that density, and the choice is
 * persisted per stable section id in `localStorage` (gf.collapse.<event>.<section>) so it survives
 * navigation/reload. This spec drives the real UI: it sets up a class + a rostered member + a round
 * over the real write paths, then
 *
 *   1. **Classes & Roster** — collapses the class **placement** section, asserts its content hides,
 *      reloads, and asserts it is *still* collapsed (the persisted state stuck).
 *   2. **Rounds & Heats** — collapses a **round**, asserts its heats body hides, reloads, and asserts
 *      it stayed collapsed.
 *
 * Nothing about the collapse UI is mocked. Cleans the shared Director's event back to empty.
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

async function openTab(page: import('@playwright/test').Page, name: string) {
  await page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();
}

test('RD collapses a class placement section and a round; both persist across a reload', async ({
  page,
  director
}) => {
  const base = director.baseUrl;
  const ev = `${base}/events/practice`;
  const json = { headers: { 'Content-Type': 'application/json' } };
  const SUFFIX = Date.now();
  const ACE = `E2E-Collapse-Ace-${SUFFIX}`;
  const ROUND_LABEL = `E2E-CollapseRound-${SUFFIX}`;

  // ── Set up over the real write paths: Open Class selected, one member, a round. ───────────────
  const classes = (await (await page.request.get(`${base}/classes`)).json()) as Array<{
    id: string;
    name: string;
  }>;
  const classId = classes.find((c) => c.name === 'Open Class')!.id;
  const aceId = (await (
    await page.request.post(`${base}/pilots`, { ...json, data: { callsign: ACE } })
  ).json()) as { id: string };
  // Two classes so the per-class placement grid (each block its own Collapsible) is shown.
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [classId] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [aceId.id] } });
  await page.request.put(`${ev}/classes/${classId}/membership`, {
    ...json,
    data: { pilots: [aceId.id] }
  });
  const round = (await (
    await page.request.post(`${ev}/rounds`, {
      ...json,
      data: {
        label: ROUND_LABEL,
        classes: [classId],
        format: 'timed_qual',
        params: {},
        win_condition: 'BestLap',
        seeding: 'FromRoster',
        channel_mode: 'PerHeat'
      }
    })
  ).json()) as { id: string };

  // ── 1. Classes & Roster → collapse the Open Class placement section ────────────────────────────
  await page.goto('/');
  await enterPractice(page);
  await openTab(page, 'Classes & Roster');

  // The placement block's toggle (a button titled by the class; its accessible name also carries the
  // summary count, e.g. "Open Class 1 placed"). Match on the toggle's own class + title prefix so the
  // locator stays stable after the section collapses (the inner group goes away when hidden).
  const placeToggle = page.getByRole('button', { name: /Open Class.*placed/ });
  await expect(placeToggle).toBeVisible({ timeout: 15_000 });
  await expect(placeToggle).toHaveAttribute('aria-expanded', 'true');
  // Its content (the placed member) is visible while expanded. Scope to the placement group — the
  // callsign also appears in the roster directory list, so an unscoped match is ambiguous.
  const placedMember = page.getByRole('group', { name: 'Placement for Open Class' }).getByText(ACE);
  await expect(placedMember).toBeVisible();
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Classes and roster' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/classes-roster-expanded.png` });

  // Collapse it → the content (the placement group) hides + aria-expanded flips.
  await placeToggle.click();
  await expect(placeToggle).toHaveAttribute('aria-expanded', 'false');
  await expect(page.getByRole('group', { name: 'Placement for Open Class' })).toBeHidden();
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Classes and roster' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/classes-roster-collapsed.png` });

  // ── It persisted: a reload keeps the placement section collapsed. ─────────────────────────────
  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await openTab(page, 'Classes & Roster');
  // Collapsed: the group is hidden, so match the placement toggle by its accessible name (title + count).
  const placeToggleAfter = page.getByRole('button', { name: /Open Class.*placed/ });
  await expect(placeToggleAfter).toHaveAttribute('aria-expanded', 'false', { timeout: 15_000 });
  await expect(page.getByRole('group', { name: 'Placement for Open Class' })).toBeHidden();

  // ── 2. Rounds & Heats → collapse the round, assert its body hides + persists ───────────────────
  await openTab(page, 'Rounds & Heats');
  const roundRegion = page.getByRole('region', { name: `Heats for ${ROUND_LABEL}` });
  await expect(roundRegion).toBeVisible({ timeout: 15_000 });
  const roundToggle = roundRegion.locator('.gf-collapsible-toggle').first();
  await expect(roundToggle).toHaveAttribute('aria-expanded', 'true');
  // Its body (the "no heats yet" nudge, which lives in the collapsible region — not the header
  // "Generate heats" button) is visible while expanded.
  const roundBodyText = roundRegion.getByText(/No heats yet/).first();
  await expect(roundBodyText).toBeVisible();
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Rounds and heats' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/rounds-heats-expanded.png` });

  await roundToggle.click();
  await expect(roundToggle).toHaveAttribute('aria-expanded', 'false');
  await expect(roundBodyText).toBeHidden();
  if (process.env.GRIDFPV_SHOTS)
    await page
      .getByRole('region', { name: 'Rounds and heats' })
      .screenshot({ path: `${process.env.GRIDFPV_SHOTS}/rounds-heats-collapsed.png` });

  await page.reload();
  await expect(page.getByRole('button', { name: /Live control/ })).toBeVisible({ timeout: 15_000 });
  await openTab(page, 'Rounds & Heats');
  const roundToggleAfter = page
    .getByRole('region', { name: `Heats for ${ROUND_LABEL}` })
    .locator('.gf-collapsible-toggle')
    .first();
  await expect(roundToggleAfter).toHaveAttribute('aria-expanded', 'false', { timeout: 15_000 });

  // ── Clean up the shared Director's event back to empty. ───────────────────────────────────────
  await page.request.delete(`${ev}/rounds/${round.id}`);
  await page.request.put(`${ev}/classes/${classId}/membership`, { ...json, data: { pilots: [] } });
  await page.request.put(`${ev}/roster`, { ...json, data: { pilot_ids: [] } });
  await page.request.put(`${ev}/classes`, { ...json, data: { ids: [] } });
});
