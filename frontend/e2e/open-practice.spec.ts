/**
 * Open-practice format through the console UI (open-practice Slice 2) — the deliverable proof.
 *
 * A real click-through against a **real** Director (open / no token): enter the Practice event,
 * open **Rounds & Heats**, add a round with the **open_practice** format, pick a couple of the
 * primary timer's **active channels** (the picker that swaps in for the class/seeding inputs),
 * screenshot the picker, save (the round's one heat is auto-created), jump to **Race control**, and
 * assert the **per-channel practice board** renders one row per active channel — and that the
 * board's old "New run" control is gone (#393) — then clean up the round.
 *
 * Every seat is named through the shared resolver: **node + channel** where the channel is known
 * (`Node 1 · Raceband R1`), the **node alone** where it is genuinely unknown (`Node 1`) — which is
 * the case here, with no channel layout and nothing running. The raw `node-{i}` wire ref must never
 * reach the screen (`CLAUDE.md`), so this spec asserts its *absence*; an earlier version asserted it
 * was **visible**, pinning that leak as the expected behaviour.
 *
 * The Practice event's built-in Mock timer is 8 seats wide, so the picker / board populate with no
 * extra timer config. Screenshots land in `e2e/screenshots/` for the PR.
 */
import { expect, test } from './observability.js';
import type { Page } from '@playwright/test';

const SHOTS = new URL('./screenshots/', import.meta.url).pathname;

async function enterPractice(page: Page) {
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

async function openTab(page: Page, name: string) {
  await page.getByRole('navigation', { name: 'Screens' }).getByRole('button', { name }).click();
}

test('RD defines an open-practice round, picks active channels, and runs a per-channel board', async ({
  page,
  director
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
  // The eligible-class dropdown (Rounds form redesign item 6) is gone for open practice.
  await expect(form.getByLabel('Eligible class')).toBeHidden();
  await expect(form.getByLabel('Win condition')).toBeHidden();
  // The time limit is now a single Minutes field (no separate hours input).
  await expect(form.getByLabel('Time limit minutes')).toBeVisible();
  await expect(form.getByLabel('Time limit hours')).toBeHidden();

  // A seat is named **node + channel**, not channel alone (#416, `nodeSeatLabel`): the node number
  // is what the RD reads off the hardware and the channel is what the pilot dials in, and neither
  // alone identifies the seat. Node index 0 is the node the RD calls "Node 1".
  //
  // The `· <channel>` half is present only when the seat's channel is actually KNOWN — from the
  // heat's own assignment, from what the node reports, or from a channel layout. It is not read off
  // the timer's `available_channels` any more (#117 S3): that is a set of channels the timer may
  // use, with no per-node meaning, so indexing it by node invented the answer. On this event —
  // no layout, no running heat — every seat's channel is genuinely unknown, so the label is the
  // node alone. Hence the prefix match: the assertion is about the seat's identity, and it must not
  // break the day a layout gives these seats channels.
  const r1 = form.getByLabel(/^Channel Node 1\b/);
  const r2 = form.getByLabel(/^Channel Node 2\b/);
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
  // The friendly format name shows in the list (Rounds form redesign item 1), not the raw key. The
  // `open_practice` wire key is unchanged; only the friendly label was shortened to "Practice"
  // (#218 — see `lib/formats.ts` FORMAT_LABELS).
  await expect(row.getByText('Practice', { exact: true })).toBeVisible();
  // Seeded from 2 **node seats**, and the summary says so: `ActiveNodes { nodes }` carries node
  // indices, not frequencies (#117 S3 / the rename), so "2 nodes" is the honest count.
  await expect(row.getByText(/Open practice · 2 node/)).toBeVisible();
  await expect(row.getByText('1m', { exact: true })).toBeVisible();

  // ── The heat is auto-created (no manual Fill) ───────────────────────────────────────────────
  const heatSection = page.getByRole('region', { name: `Heats for ${LABEL}` });
  // An open-practice round drops the GENERATION control — it has no field to lay into heats, and
  // its fill emits one heat, ever. It keeps "Add heat", the only way to seat a second by hand.
  await expect(heatSection.getByRole('button', { name: 'Generate next heat' })).toHaveCount(0);
  await expect(heatSection.getByRole('button', { name: 'Generate heats' })).toHaveCount(0);
  await expect(heatSection.getByRole('button', { name: 'Add heat' })).toBeVisible();
  // The auto-created heat lands, and its lineup names the seats — `Node 1`, not the raw `node-0`
  // wire ref. This assertion used to be `getByText(/node-0/)` **visible**: it pinned the raw-ref
  // leak `CLAUDE.md` forbids as the expected behaviour, so it is inverted rather than repaired. The
  // resolver (`buildCompetitorNames`) turns an unbound `node-{i}` seat into its seat label; a raw
  // ref reaching the screen is the bug.
  await expect(heatSection.getByText('Node 1', { exact: false }).first()).toBeVisible({
    timeout: 15_000
  });
  await expect(heatSection.getByText(/node-\d/)).toHaveCount(0);
  // It displays under the friendly name "Practice Heat", not its generated heat id
  // (`OPEN_PRACTICE_HEAT_NAME` in `lib/heats.ts`).
  await expect(heatSection.getByText('Practice Heat').first()).toBeVisible();

  // ── Make this round's auto-created heat the current heat (SetCurrentHeat) ────────────────────
  // The per-channel board only renders when the open-practice heat is the current heat on the
  // timer. On the shared worker Director `current_heat` stays pinned to whatever heat a prior spec
  // last transitioned/selected (it never resets between specs), and creating a heat does NOT steal
  // Live focus (current-heat.spec.ts). The auto-created heat's id is generated, so resolve it: find
  // the round by label off /events, then its heat off the heats list (tagged with the round id), and
  // focus it over the control path — keeping the run independent of spec order.
  const events = (await (await page.request.get('/events')).json()) as Array<{
    id: string;
    rounds?: Array<{ id: string; label: string }>;
  }>;
  const roundId = events
    .find((e) => e.id === director.event)
    ?.rounds?.find((r) => r.label === LABEL)?.id;
  expect(roundId, 'the open-practice round exists').toBeTruthy();
  // Addressed through the fixture's `eventRoot`: there is no built-in `practice` event any more
  // (#414) — the worker creates one and its id is generated.
  const heats = (await (await page.request.get(`${director.eventRoot}/heats`)).json()) as Array<{
    heat: string;
    round?: string;
  }>;
  const opHeatId = heats.find((h) => h.round === roundId)?.heat;
  expect(opHeatId, 'the open-practice round has an auto-created heat').toBeTruthy();
  const focused = await page.request.post(`${director.eventRoot}/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { SetCurrentHeat: { heat: opHeatId } }
  });
  expect(focused.ok()).toBeTruthy();

  // ── Race control → the per-channel practice board ───────────────────────────────────────────
  await openTab(page, 'Race control');
  const board = page.getByRole('list', { name: 'Per-channel practice board' });
  await expect(board).toBeVisible({ timeout: 15_000 });
  // One row per active channel, named node + channel through the same shared resolver.
  await expect(page.getByLabel(/^Channel Node 1\b/)).toBeVisible();
  await expect(page.getByLabel(/^Channel Node 2\b/)).toBeVisible();
  // No raw seat ref anywhere on the board either.
  await expect(page.getByText(/node-\d/)).toHaveCount(0);
  await page.screenshot({ path: `${SHOTS}open-practice-board.png`, fullPage: true });

  // ── Going again: the board's own "New run" button is GONE (#393) ─────────────────────────────
  // It re-filled the round, on the pre-#398 theory that a fresh heat was how you cleared the laps —
  // but an open-practice round mints exactly ONE heat, ever, so once the run had ended that fill
  // scheduled nothing and still acked ok: a control that reported success and did nothing. Going
  // again is the transition row's **Run again** (the `Restart` command).
  //
  // This spec asserts the removal and stops there, deliberately. Driving a real practice run to
  // reach `Restart` (only legal once the heat is committed) would leave the round holding a RACED
  // heat, and `remove_round` correctly refuses to delete one of those — so the cleanup below could
  // no longer run and the shared Director would keep the round. The behaviour itself is covered
  // where it is cheap and deterministic: `LiveRaceControl.test.ts` ("carries no second new-run
  // control", "Run again fires the Restart command", "never strands a practice heat at Final") and
  // `transitions.test.ts`.
  await expect(page.getByRole('button', { name: /New run/ })).toHaveCount(0);

  // ── Removing the round is REFUSED while its heat is on the timer (#418) ─────────────────────
  // This used to be a cleanup step. It cannot be one any more, and that is the correct behaviour,
  // not a regression: `remove_round` refuses while any of the round's heats is *in progress*, and
  // "in progress" includes a heat that is still `Scheduled` but **loaded on the timer** — which is
  // exactly what the `SetCurrentHeat` above made this one. Pulling a round's config out from under
  // the heat the timer is holding is the thing that rule exists to prevent.
  //
  // So the step is inverted into coverage of the refusal itself: the round survives the click.
  //
  // The toast it raises is NOT asserted here, and that is deliberate. It used to read
  // "DELETE /events/{eventId}/rounds/{roundId} failed: HTTP 400" — two raw ids on screen with the
  // server's explanation discarded — and #433 fixed that at the source: every `!resp.ok` in
  // `packages/protocol-client/src/client.ts` now goes through one helper that throws the
  // Director's typed refusal verbatim ("this round has a heat in progress (Practice Heat) —
  // finalize or reset it before removing the round"), so the wording on screen is the Director's,
  // not the client's. Pinning it here would move ownership of that sentence into this spec; the
  // client's own tests assert it is surfaced verbatim and carries no raw id.
  await openTab(page, 'Rounds & Heats');
  const cleanupRow = page.getByRole('list').getByRole('listitem').filter({ hasText: LABEL });
  // Two-step confirm since #499 — WITHOUT the confirm this spec would pass vacuously (nothing
  // attempted, so of course the round is still listed) instead of proving the Director refuses.
  await cleanupRow.getByRole('button', { name: 'Remove' }).click();
  await cleanupRow.getByRole('button', { name: /Confirm/ }).click();
  // The refusal held: the round is still listed.
  await expect(cleanupRow).toHaveCount(1);
  await expect(cleanupRow).toBeVisible();
});
