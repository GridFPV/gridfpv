/**
 * Live-control e2e proof for the start-tone fixes + the prominent confirm:
 *
 *  A. **Audible start tone (stubbed observability)** — the race-go beep was inaudible because the
 *     `AudioContext` stays *suspended* (browser autoplay policy) when the runtime auto-advances into
 *     `Running` with no click on that edge. The console unlocks the context on an earlier RD gesture
 *     and now fires the tone on the heat **entering Running** (robust to a fast/batched transition
 *     where the Armed snapshot is skipped). We inject a page-side `AudioContext` **stub** recording
 *     `resume()`/oscillator `start()` to assert the unlock fires on a control click and an oscillator
 *     actually starts when the heat goes Running.
 *
 *  B. **Real AudioContext** — the stub proves the wiring, but not that the *real* Web Audio path runs
 *     in Chromium. A second test wraps the **real** `AudioContext` (no stub) so the synth graph is
 *     genuinely built, and asserts the **Enable sound** button resumes it to `state === 'running'`
 *     and that the oscillator graph is constructed on race-go.
 *
 *  C. **Prominent red Revert/Restart confirm** — the active confirm is a solid red fill with
 *     near-black text. We arm a destructive confirm and screenshot it (and the live toolbar).
 *
 * Screenshots go to `GRIDFPV_SHOTS` when set (the harness passes it).
 */
import { expect, test } from './observability.js';

declare global {
  interface Window {
    __toneCalls: { resume: number; started: number; state: string };
    __realAudio: { contexts: number; oscillators: number; lastState: string };
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

  // ── A1: the explicit "Enable sound / Test tone" button is the reliable unlock + audible test ──
  // A definite user gesture that resumes the context AND plays a confirmation beep, with a locked →
  // enabled indicator the RD reads at a glance.
  const enableBtn = page.getByRole('button', { name: /Enable sound/ });
  await expect(enableBtn).toBeVisible();
  await enableBtn.click();
  await expect.poll(() => page.evaluate(() => window.__toneCalls.resume)).toBeGreaterThan(0);
  // The confirmation beep emitted an oscillator, and the indicator flipped to "Test tone" (enabled).
  await expect.poll(() => page.evaluate(() => window.__toneCalls.started)).toBeGreaterThan(0);
  await expect(page.getByRole('button', { name: /Test tone/ })).toBeVisible();

  // Note the oscillator count after the test beep so we can prove race-go adds another.
  const afterEnable = await page.evaluate(() => window.__toneCalls.started);

  // Stage the heat (a non-destructive forward gesture), then Start arms it; the runtime then
  // auto-advances into Running (no click on that edge). The per-heat trigger fires the tone on
  // entering Running even if the Armed snapshot is skipped.
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.getByRole('status', { name: 'Staging countdown' })).toBeVisible();
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  // Another oscillator started at race-go (on top of the enable-test beep).
  await expect
    .poll(() => page.evaluate(() => window.__toneCalls.started))
    .toBeGreaterThan(afterEnable);

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

  // Screenshot the live toolbar (HUD) showing the Enable/Test-tone button + its enabled indicator.
  await shot(page.locator('.hud'), 'live-toolbar-enable-tone');
});

// A page-side wrapper around the **real** AudioContext that records context creation, oscillator
// builds, and the latest state — so we can prove the genuine Web Audio synth path runs in Chromium
// (not just the injected stub). Installed before the app reads the platform constructor.
const REAL_AUDIO_HOOK = `
  window.__realAudio = { contexts: 0, oscillators: 0, lastState: 'none' };
  const Real = window.AudioContext || window.webkitAudioContext;
  if (Real) {
    class HookedAudioContext extends Real {
      constructor(...args) {
        super(...args);
        window.__realAudio.contexts++;
        window.__realAudio.lastState = this.state;
      }
      createOscillator() {
        window.__realAudio.oscillators++;
        window.__realAudio.lastState = this.state;
        return super.createOscillator();
      }
      async resume() {
        const r = await super.resume();
        window.__realAudio.lastState = this.state;
        return r;
      }
    }
    window.AudioContext = HookedAudioContext;
    window.webkitAudioContext = HookedAudioContext;
  }
`;

test('REAL AudioContext: Enable button resumes to running and the synth graph is built on race-go', async ({
  page,
  director
}) => {
  await page.addInitScript(REAL_AUDIO_HOOK);
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  const ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: 'tone-real-1', lineup: ['Ace', 'Bee', 'Cee'] } }
  });
  expect(ack.ok()).toBeTruthy();
  await expect(page.locator('.heat-id .value')).toHaveText('tone-real-1', { timeout: 15_000 });

  // ── Enable sound: a real user gesture must resume the REAL context to 'running' and emit a beep ─
  const enableBtn = page.getByRole('button', { name: /Enable sound/ });
  await expect(enableBtn).toBeVisible();
  await enableBtn.click();
  // The real AudioContext exists and was resumed to 'running' (the autoplay unlock genuinely works).
  await expect.poll(() => page.evaluate(() => window.__realAudio.contexts)).toBeGreaterThan(0);
  await expect.poll(() => page.evaluate(() => window.__realAudio.lastState)).toBe('running');
  // The confirmation beep built a real oscillator graph; the indicator flips to enabled.
  await expect.poll(() => page.evaluate(() => window.__realAudio.oscillators)).toBeGreaterThan(0);
  await expect(page.getByRole('button', { name: /Test tone/ })).toBeVisible();

  const oscAfterEnable = await page.evaluate(() => window.__realAudio.oscillators);

  // ── Race-go: a real oscillator graph is built when the heat enters Running ─────────────────────
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.getByRole('status', { name: 'Staging countdown' })).toBeVisible();
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  await expect
    .poll(() => page.evaluate(() => window.__realAudio.oscillators))
    .toBeGreaterThan(oscAfterEnable);

  // Clean up the heat so the shared Director is left tidy for the next spec.
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Discard', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
});
