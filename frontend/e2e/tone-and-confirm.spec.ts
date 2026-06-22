/**
 * Live-control e2e proof for two fixes:
 *
 *  A. **Audible start tone** — the race-go beep was inaudible because the `AudioContext` stays
 *     *suspended* (browser autoplay policy) when the runtime auto-advances `Armed → Running` with no
 *     click on that edge. The console now **unlocks the context on an earlier RD gesture** (any heat
 *     transition / the mute toggle). Real audio can't be asserted headless, so we inject a page-side
 *     `AudioContext` **stub** that records `resume()` + oscillator `start()` calls and assert the
 *     unlock fires on a control click — and that an oscillator actually starts at race-go.
 *
 *  B. **Prominent red Revert/Restart confirm** — the active confirm is now a solid red fill with
 *     near-black text. We arm a destructive confirm and screenshot it.
 *
 * Screenshots go to `GRIDFPV_SHOTS` when set (the harness passes it).
 */
import { expect, test } from './observability.js';

declare global {
  interface Window {
    __toneCalls: { resume: number; started: number; state: string };
  }
}

const shot = async (locator: import('@playwright/test').Locator, name: string) => {
  if (process.env.GRIDFPV_SHOTS) {
    await locator.screenshot({ path: `${process.env.GRIDFPV_SHOTS}/${name}.png` });
  }
};

// A tiny page-side AudioContext stub recording resume()/oscillator-start onto window so the spec can
// read them back. Installed via addInitScript so it's present before the app's StartTonePlayer reads
// the platform constructor.
const AUDIO_STUB = `
  window.__toneCalls = { resume: 0, started: 0, state: 'suspended' };
  class StubAudioContext {
    constructor() { this.currentTime = 0; this.state = window.__toneCalls.state; this.destination = {}; }
    createOscillator() {
      const calls = window.__toneCalls;
      return {
        type: 'square',
        frequency: { setValueAtTime() {} },
        connect() {}, start() { calls.started++; }, stop() {}
      };
    }
    createGain() { return { gain: { setValueAtTime() {}, linearRampToValueAtTime() {} }, connect() {} }; }
    async resume() { window.__toneCalls.resume++; this.state = 'running'; }
    async close() {}
  }
  window.AudioContext = StubAudioContext;
  window.webkitAudioContext = StubAudioContext;
`;

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

test('start tone unlocks the AudioContext on a control click and sounds at race-go; prominent red confirm', async ({
  page,
  director
}) => {
  await page.addInitScript(AUDIO_STUB);
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  // Schedule a heat over the open control path.
  const ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: 'tone-1', lineup: ['Ace', 'Bee', 'Cee'] } }
  });
  expect(ack.ok()).toBeTruthy();
  await expect(page.locator('.heat-id .value')).toHaveText('tone-1', { timeout: 15_000 });

  // Before any gesture the context has not been resumed.
  expect(await page.evaluate(() => window.__toneCalls.resume)).toBe(0);

  // ── A: the unlock fires on a control click (Stage is a non-destructive gesture) ───────────────
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.getByRole('status', { name: 'Staging countdown' })).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.__toneCalls.resume)).toBeGreaterThan(0);

  // Start arms the heat; the runtime then auto-advances Armed → Running (no click on that edge).
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  // An oscillator actually started at race-go (the context was unlocked by the earlier gesture).
  await expect.poll(() => page.evaluate(() => window.__toneCalls.started)).toBeGreaterThan(0);

  // Drive to a state where a destructive confirm (Restart) is available, then arm it.
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });

  // ── B: the prominent red confirm — arm Restart and screenshot the solid-red Confirm button ────
  await page.getByRole('button', { name: 'Restart', exact: true }).click();
  const confirm = page.getByRole('button', { name: 'Confirm' });
  await expect(confirm).toBeVisible();
  await expect(confirm).toHaveAttribute('data-confirm-danger', 'true');
  // Prove the prominent fill actually computed to the solid danger red (not the subtle default).
  await expect
    .poll(() => confirm.evaluate((el) => getComputedStyle(el).backgroundColor))
    .toBe('rgb(255, 107, 107)');
  await shot(page.locator('.controls'), 'revert-restart-confirm');

  // Cancel the confirm, then clean up the heat (Discard) so the shared Director is left tidy.
  await page.getByRole('button', { name: 'Cancel' }).click();
  await page.getByRole('button', { name: 'Discard', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
});
