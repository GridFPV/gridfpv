/**
 * The Video page through the console UI, as far as it goes with no OBS.
 *
 * The e2e Director has no OBS to talk to, so this is the honest part: the page is under
 * Setup, video starts switched off, switching it on at an address nothing listens on says
 * so in the Director's words, the Run page then warns before Start that a race would not be
 * recorded, and switching it off again makes both go quiet. The password that was typed is
 * never sent back.
 *
 * Recording itself is proved against a real OBS by `crates/app/tests/obs_live.rs`.
 */
import { gotoEvents, gotoPage, openPage } from './nav.js';
import { E2E_EVENT_NAME, expect, test } from './observability.js';

/** A port on this machine nothing listens on. */
const NOWHERE = '127.0.0.1:9';

test('RD switches video on with no OBS there, is told so on the Video and Run pages, and switches it off', async ({
  page
}) => {
  try {
    await gotoPage(page, 'Video');
    await expect(page).toHaveURL(/#\/video$/);
    const form = page.getByRole('form', { name: 'Video settings' });
    const record = form.getByRole('checkbox', { name: 'Record every race' });
    await expect(record).not.toBeChecked();
    // The setup only: what was recorded is on the event's Recordings page.
    await expect(page.getByRole('heading', { name: 'Runs' })).toHaveCount(0);
    const save = page.getByRole('button', { name: 'Save', exact: true });
    await expect(save).toBeDisabled();

    // ── A bad address is refused in the Director's words ─────────────────────────────────
    await record.check();
    await form.getByLabel('OBS address').fill('obs');
    await save.click();
    await expect(form.getByRole('alert')).toContainText('a host, a colon, and a port');

    // ── On, at an address nothing answers at ─────────────────────────────────────────────
    await form.getByLabel('OBS address').fill(NOWHERE);
    await form.getByLabel('OBS password').fill('e2e-secret');
    await save.click();
    await expect(save).toBeDisabled({ timeout: 15_000 });
    await expect(page.getByTestId('obs-problem')).toContainText(
      `Could not reach OBS at ${NOWHERE}`,
      {
        timeout: 20_000
      }
    );
    // The password is stored and never comes back.
    const settings = (await (await page.request.get('/video/settings')).json()) as {
      enabled: boolean;
      has_password: boolean;
    };
    expect(settings).toMatchObject({ enabled: true, has_password: true });
    expect(JSON.stringify(settings)).not.toContain('e2e-secret');
    await expect(form.getByLabel('OBS password')).toHaveValue('');
    await page.reload();
    await expect(
      page.getByRole('form', { name: 'Video settings' }).getByLabel('OBS password')
    ).toHaveValue('');
    expect(await page.locator('body').innerText()).not.toContain('e2e-secret');

    // ── The Run page warns before Start ──────────────────────────────────────────────────
    await gotoEvents(page);
    await page
      .getByRole('button', { name: new RegExp(E2E_EVENT_NAME) })
      .first()
      .click();
    await openPage(page, 'Run');
    await expect(page.getByTestId('video-warning')).toContainText(
      'OBS is not connected. A race started now is not recorded.',
      { timeout: 15_000 }
    );

    // ── Off again: the Run page goes quiet. The Video page still says OBS is not there, ──
    // because Grid stays connected to OBS with recording off (the scenes are picked first).
    await openPage(page, 'Video');
    const again = page.getByRole('form', { name: 'Video settings' });
    await again.getByRole('checkbox', { name: 'Record every race' }).uncheck();
    const saveAgain = page.getByRole('button', { name: 'Save', exact: true });
    await saveAgain.click();
    await expect(saveAgain).toBeDisabled({ timeout: 15_000 });
    await expect(page.getByTestId('obs-problem')).toContainText(
      `Could not reach OBS at ${NOWHERE}`
    );
    await openPage(page, 'Run');
    await expect(page.getByTestId('video-warning')).toHaveCount(0, { timeout: 15_000 });
  } finally {
    // Leave the shared Director with video off and no password.
    await page.request.put('/video/settings', {
      data: {
        enabled: false,
        obs_address: '127.0.0.1:4455',
        obs_password: '',
        tail_seconds: 3,
        trash_discarded: true,
        record_from: 'Obs',
        clean_scene: '',
        overlay_scene: '',
        render_when: 'Asked',
        browser_path: '',
        ffmpeg_path: '',
        save_in: 'Obs'
      }
    });
  }
});
