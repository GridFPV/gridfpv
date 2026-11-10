/**
 * The timer directory through the console UI.
 *
 * A person opens the Timers page from the hub and adds a timer at an address nothing listens
 * on. The Director tries to reach it by itself and fails, so the timer is listed as not
 * connected, with the Director's own sentence for why. Its Tune page opens from the row and
 * says the same, with the Director's sentence for why the settings could not be read. The
 * add-heat dialog then offers the
 * timer by name under "Timed by" but it **cannot be chosen** (it has reported no seats), and
 * the dialog stays on the Bench. Last, the timer is removed.
 *
 * Everything is the real path: real `POST /timers`, the Director's real connection attempt,
 * the page's own 2 second read of `GET /timers`. Nothing is mocked.
 *
 * The spec also checks the display rule: the timer's id is nowhere on a page.
 *
 * The worker's Director is shared, so every name here is unique and the spec deletes what it
 * made and puts the active event back.
 */
import { expect, test } from './observability.js';
import { gotoEvents, gotoHub } from './nav.js';

const STAMP = Date.now();
const TIMER = `E2E Gate ${STAMP}`;
const EVENT = `Timer Night ${STAMP}`;
/** The discard port on this machine: nothing listens there, so the connection is refused. */
const ADDRESS = '127.0.0.1:9';

interface TimerRow {
  timer: { id: string; name: string };
  status: { connected: boolean; problem?: string; seats: string[] };
}

test('RD adds a timer nothing answers at, sees why, opens its Tune page, cannot put a heat on it, and removes it', async ({
  page
}) => {
  const before = (await (await page.request.get('/active-event')).json()) as {
    event: { id: string } | null;
  };
  let eventId: string | undefined;
  const ours = async (): Promise<TimerRow | undefined> =>
    ((await (await page.request.get('/timers')).json()) as TimerRow[]).find(
      (row) => row.timer.name === TIMER
    );

  try {
    // ── Hub → Timers page ────────────────────────────────────────────────────────────────
    await gotoHub(page);
    await page.getByRole('button', { name: /Timers/ }).click();
    await expect(page).toHaveURL(/#\/timers$/);
    await expect(page.getByRole('heading', { name: 'Timers', level: 1 })).toBeVisible();

    // ── A bad address is refused in the Director's words, and the dialog stays open ──────
    await page.getByRole('button', { name: '+ Add timer' }).click();
    const form = page.getByRole('form', { name: 'Add timer' });
    await expect(form).toContainText('An OFT timer listens on port 7270.');
    await form.getByLabel('Name', { exact: true }).fill(TIMER);
    await form.getByLabel('Address', { exact: true }).fill('not an address');
    await page.getByRole('button', { name: 'Add timer', exact: true }).click();
    await expect(form.getByRole('alert')).toHaveText(
      'A timer needs an address, like 192.168.1.50:7270.'
    );
    expect(await ours()).toBeUndefined();

    // ── Add it at an address nothing listens on ──────────────────────────────────────────
    await form.getByLabel('Address', { exact: true }).fill(ADDRESS);
    await page.getByRole('button', { name: 'Add timer', exact: true }).click();
    await expect(form).toBeHidden();
    await expect(page.getByRole('form', { name: 'Control token' })).toBeHidden();

    const row = page.getByRole('list', { name: 'Timers' }).getByRole('listitem', { name: TIMER });
    await expect(row).toBeVisible();
    await expect(row).toContainText(ADDRESS);
    await expect(row.getByText('Not connected')).toBeVisible();
    await expect(row).toContainText('No seats reported yet');
    await expect(row).toContainText('Clock not measured yet');

    // The Director tries by itself; once it has failed it says why, and the page shows that
    // sentence as it is (the page reads the list again every 2 seconds).
    await expect.poll(async () => (await ours())?.status.problem, { timeout: 20_000 }).toBeTruthy();
    const made = await ours();
    expect(made).toBeDefined();
    expect(made!.status.connected).toBe(false);
    expect(made!.status.seats).toEqual([]);
    const timerId = made!.timer.id;
    await expect(row.getByTestId('timer-problem')).toHaveText(made!.status.problem!, {
      timeout: 15_000
    });
    expect(await page.locator('body').innerText()).not.toContain(timerId);

    // ── Tune: the timer's own page says it is not connected, and why ─────────────────────
    // There is no real timer here, so this goes only as far as is honest: the page opens
    // from the row, names the timer, and shows the Director's two sentences (why it is not
    // connected, and why its settings could not be read) where the settings would be.
    await row.getByRole('button', { name: 'Tune' }).click();
    await expect
      .poll(() => new URL(page.url()).hash)
      .toBe(`#/timers/${encodeURIComponent(timerId)}`);
    await expect(page.getByRole('heading', { name: TIMER, level: 1 })).toBeVisible();
    const crumbs = page.getByRole('navigation', { name: 'Breadcrumb' });
    await expect(crumbs).toContainText('Home');
    await expect(crumbs).toContainText('Timers');
    await expect(crumbs).toContainText(TIMER);
    await expect(page.getByText('Not connected', { exact: true })).toBeVisible();
    const notConnected = page.getByTestId('not-connected');
    await expect(notConnected).toContainText(`${TIMER} is not connected.`);
    await expect(notConnected).toContainText(made!.status.problem!);
    const settingsProblem = page.getByTestId('settings-problem');
    await expect(settingsProblem).toContainText("The timer's settings could not be read.");
    await expect(settingsProblem).toContainText(`Could not reach ${TIMER}`);
    // No settings to edit, and nothing to apply.
    await expect(page.getByRole('list', { name: 'Seats' })).toHaveCount(0);
    await expect(page.getByRole('button', { name: 'Apply to timer' })).toHaveCount(0);
    expect(await page.locator('body').innerText()).not.toContain(timerId);

    // A reload stays on the timer's Tune page (the hash names it).
    await page.reload();
    await expect(page.getByRole('heading', { name: TIMER, level: 1 })).toBeVisible();
    await expect(page.getByTestId('settings-problem')).toContainText(`Could not reach ${TIMER}`);
    expect(await page.locator('body').innerText()).not.toContain(timerId);

    // The Timers crumb goes back to the list.
    await crumbs.getByRole('button', { name: 'Timers' }).click();
    await expect(page).toHaveURL(/#\/timers$/);
    await expect(row).toBeVisible();

    // ── The add-heat dialog offers it by name, but it cannot be chosen ───────────────────
    await gotoEvents(page);
    await page
      .getByRole('button', { name: /New event/ })
      .first()
      .click();
    await page.getByRole('textbox', { name: 'Event name' }).fill(EVENT);
    await page.getByRole('button', { name: /Create & enter/ }).click();
    await expect(page.getByRole('heading', { name: EVENT, level: 1 })).toBeVisible();
    eventId = (
      (await (await page.request.get('/active-event')).json()) as { event: { id: string } | null }
    ).event?.id;
    expect(eventId).toBeTruthy();

    await page.getByRole('button', { name: '+ Add heat' }).click();
    const heatForm = page.getByRole('form', { name: 'Add a heat' });
    const timedBy = heatForm.getByLabel('Timed by');
    const offered = timedBy.locator('option', { hasText: TIMER });
    await expect(offered).toHaveText(`${TIMER} (not connected)`);
    await expect(offered).toBeDisabled();
    // With no timer that can be chosen, the heat is on the Bench and its eight seats.
    await expect(timedBy.locator('option:checked')).toContainText('Bench');
    await expect(heatForm).toContainText('The Bench is not a timer.');
    await expect(heatForm).toContainText('Up to 8.');
    expect(await page.locator('body').innerText()).not.toContain(timerId);
    expect(await timedBy.locator('option').allInnerTexts()).not.toContain(timerId);
    await page.getByRole('button', { name: 'Cancel' }).click();
    await expect(heatForm).toBeHidden();

    // ── Remove it: one click arms the button, the second one removes ─────────────────────
    await gotoHub(page);
    await page.getByRole('button', { name: /Timers/ }).click();
    await expect(row).toBeVisible();
    await row.getByRole('button', { name: 'Remove' }).click();
    expect(await ours()).toBeDefined();
    await row.getByRole('button', { name: 'Confirm' }).click();
    await expect(row).toHaveCount(0);
    expect(await ours()).toBeUndefined();
  } finally {
    // ── Clean up: the event, the timer if it is still there, the active event as it was ──
    if (eventId) await page.request.delete(`/events/${eventId}`);
    const left = (await (await page.request.get('/timers')).json()) as TimerRow[];
    for (const row of left.filter((r) => r.timer.name === TIMER)) {
      await page.request.delete(`/timers/${encodeURIComponent(row.timer.id)}`);
    }
    if (before.event) {
      await page.request.put('/active-event', { data: { id: before.event.id } });
    }
  }
});
