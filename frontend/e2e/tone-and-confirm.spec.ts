/**
 * Live-control e2e proof for the start-tone behaviour + the prominent confirm:
 *
 *  A. **No buzz on a late join** — the start tone is a race-go cue for the RD *watching* the heat go
 *     live. Navigating to the Live page of an **already-running** heat (a late join) must NOT play
 *     the tone: the first phase the console observes for that heat is `Running`, with no pre-Running
 *     phase seen. We schedule + drive a heat to Running over the control API *before* the page lands
 *     on it, then assert no oscillator is built when the Live page renders.
 *
 *  B. **Tone on a genuine race-go** — a heat the RD *watches* cross into Running (Stage → Start → …
 *     → Running, the runtime auto-advancing the Armed → Running edge with no click) DOES play. The
 *     console unlocks the suspended `AudioContext` on the earlier control click (autoplay policy) and
 *     fires the tone when the heat enters Running (robust to a fast/batched transition where the
 *     Armed snapshot is skipped). We inject a page-side `AudioContext` **stub** recording
 *     `resume()`/oscillator `start()` to assert the unlock fires on a control click and an oscillator
 *     actually starts at race-go.
 *
 *  C. **Real AudioContext** — the stub proves the wiring, not that the *real* Web Audio path runs in
 *     Chromium. A second test wraps the **real** `AudioContext` (no stub) so the synth graph is
 *     genuinely built, and asserts a control click resumes it to `state === 'running'` and the
 *     oscillator graph is constructed on race-go.
 *
 *  D. **Prominent red Revert/Restart confirm** — the active confirm is a solid red fill with
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

test('no start tone on landing on an already-running heat (late join); tone on a watched race-go; prominent red confirm', async ({
  page,
  director
}) => {
  await page.addInitScript(AUDIO_STUB);

  // ── A: late join — drive a heat all the way to Running over the control API BEFORE the page lands
  // on the Live screen, then assert the Live page renders with NO tone (no oscillator built). ──────
  let ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: 'late-join-1', lineup: ['Ace', 'Bee', 'Cee'] } }
  });
  expect(ack.ok()).toBeTruthy();
  for (const cmd of ['Stage', 'Start'] as const) {
    ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
      headers: { 'Content-Type': 'application/json' },
      data: { [cmd]: { heat: 'late-join-1' } }
    });
    expect(ack.ok()).toBeTruthy();
  }
  // The runtime holds in Armed for a hidden random delay before auto-advancing to Running. Poll the
  // **event-scoped** snapshot — the exact projection the page's live scope folds — until it reads
  // Running, so when the page connects its first folded live state for this heat is already Running:
  // a real late join (not an Armed → Running transition the page would witness). Using the same
  // event projection the client reads (not the heat-scoped one) avoids a replication-lag race where
  // the heat projection is Running but the event-live fold the page consumes is briefly still Armed.
  await expect
    .poll(
      async () => {
        const snap = await page.request.get(
          `${director.baseUrl}/events/practice/snapshot/event/practice`
        );
        const body = (await snap.json()) as { body?: { LiveRaceState?: { phase?: string } } };
        return body.body?.LiveRaceState?.phase;
      },
      { timeout: 20_000, intervals: [250] }
    )
    .toBe('Running');

  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });
  // The late-join heat's own Stage/Start transitions above already moved the Director's
  // `current_heat` onto it (a transition is exactly what moves focus), so the page lands on it
  // regardless of what a prior spec left current.
  // The heat is already Running when the page lands on it (a late join).
  await expect(page.locator('.heat-id .value')).toHaveText('late-join-1', { timeout: 15_000 });
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  // No oscillator was built — the late join does NOT buzz. Give the effect a beat to (not) fire.
  await page.waitForTimeout(500);
  expect(await page.evaluate(() => window.__toneCalls.started)).toBe(0);

  // Clean up the late-join heat so the watched-race-go heat below starts clean.
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Discard', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();

  // ── B: a watched race-go — schedule a fresh heat and drive it from the UI so the console observes
  // the pre-Running phases first, then fires the tone on entering Running. The late-join part A above
  // left NO tone (asserted), but its cleanup clicks did unlock the context — so part B reasons over
  // the oscillator-start count *delta* from a baseline, not an absolute zero. ──────────────────────
  const startedBeforeRaceGo = await page.evaluate(() => window.__toneCalls.started);
  ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: 'tone-1', lineup: ['Ace', 'Bee', 'Cee'] } }
  });
  expect(ack.ok()).toBeTruthy();
  // Filling tone-1 does NOT steal Live focus (fill-no-steal), and discarding the late-join heat
  // above did not move focus off it either — so explicitly focus tone-1 as the current heat over
  // the control path (server-side, independent of the Live picker's fetch timing).
  ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { SetCurrentHeat: { heat: 'tone-1' } }
  });
  expect(ack.ok()).toBeTruthy();
  await expect(page.locator('.heat-id .value')).toHaveText('tone-1', { timeout: 15_000 });
  await expect(page.locator('.phase').first()).toHaveText('Scheduled', { timeout: 15_000 });

  // Stage (a forward gesture) unlocks the suspended AudioContext (autoplay policy); still pre-Running
  // so no new tone yet (the count holds at the baseline).
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.getByRole('status', { name: 'Staging countdown' })).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.__toneCalls.resume)).toBeGreaterThan(0);
  expect(await page.evaluate(() => window.__toneCalls.started)).toBe(startedBeforeRaceGo);

  // Start arms it; the runtime auto-advances into Running (no click on that edge). The tone fires on
  // entering Running because the console watched the pre-Running phases — a NEW oscillator starts.
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  await expect
    .poll(() => page.evaluate(() => window.__toneCalls.started))
    .toBeGreaterThan(startedBeforeRaceGo);

  // Drive to a state where a destructive confirm (Restart) is available, then arm it.
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });

  // ── D: the prominent red confirm — arm Restart and screenshot the solid-red Confirm button ──────
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

  // Screenshot the live toolbar (HUD) — the inline test-tone button is gone; only the mute toggle.
  await shot(page.locator('.hud'), 'live-toolbar-tone');
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

test('REAL AudioContext: a control click resumes to running and the synth graph is built on race-go', async ({
  page,
  director
}) => {
  await page.addInitScript(REAL_AUDIO_HOOK);
  await page.goto('/');
  await enterPractice(page);
  await expect(page.locator('.conn-label')).toHaveText('live', { timeout: 15_000 });

  let ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { ScheduleHeat: { heat: 'tone-real-1', lineup: ['Ace', 'Bee', 'Cee'] } }
  });
  expect(ack.ok()).toBeTruthy();
  // Explicitly focus THIS heat as the current heat (SetCurrentHeat) over the control path; filling
  // does not steal Live focus, so this keeps the test independent of any heat a prior spec left
  // current (server-side, independent of the Live picker's fetch timing).
  ack = await page.request.post(`${director.baseUrl}/events/practice/control`, {
    headers: { 'Content-Type': 'application/json' },
    data: { SetCurrentHeat: { heat: 'tone-real-1' } }
  });
  expect(ack.ok()).toBeTruthy();
  await expect(page.locator('.heat-id .value')).toHaveText('tone-real-1', { timeout: 15_000 });

  // ── A control click (Stage) must resume the REAL context to 'running' (the autoplay unlock works) ─
  await page.getByRole('button', { name: 'Stage', exact: true }).click();
  await expect(page.getByRole('status', { name: 'Staging countdown' })).toBeVisible();
  await expect.poll(() => page.evaluate(() => window.__realAudio.contexts)).toBeGreaterThan(0);
  await expect.poll(() => page.evaluate(() => window.__realAudio.lastState)).toBe('running');

  const oscBeforeRaceGo = await page.evaluate(() => window.__realAudio.oscillators);

  // ── Race-go: a real oscillator graph is built when the watched heat enters Running ──────────────
  await page.getByRole('button', { name: 'Start', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Running', { timeout: 15_000 });
  await expect
    .poll(() => page.evaluate(() => window.__realAudio.oscillators))
    .toBeGreaterThan(oscBeforeRaceGo);

  // Clean up the heat so the shared Director is left tidy for the next spec.
  await page.getByRole('button', { name: 'ForceEnd', exact: true }).click();
  await expect(page.locator('.phase').first()).toHaveText('Unofficial', { timeout: 15_000 });
  await page.getByRole('button', { name: 'Discard', exact: true }).click();
  await page.getByRole('button', { name: 'Confirm' }).click();
});
