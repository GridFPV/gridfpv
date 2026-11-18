/**
 * The Video page: the OBS settings (with a password that is never shown) and whether OBS is
 * connected. What was recorded is on the event's Recordings page.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/svelte';
import type { VideoSettings, VideoStatus } from '@gridfpv/types';
import VideoPage from '../src/screens/VideoPage.svelte';
import { VIDEO_OFF, makeTestSession, obsStatus, videoStatus } from './support.js';

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const ON: VideoSettings = {
  ...VIDEO_OFF,
  enabled: true,
  obs_address: '192.168.1.20:4455',
  has_password: true,
  tail_seconds: 4
};
const IDLE: VideoStatus = videoStatus();
const CONNECTED: VideoStatus = videoStatus({
  obs: obsStatus({
    connected: true,
    version: 'OBS 31.0.2 on windows',
    record_folder: 'C:\\Videos',
    scenes: ['Camera', 'Camera with overlay', 'Break'],
    source_record: true
  })
});

function open(settings: VideoSettings, status: VideoStatus, over = {}) {
  const putVideoSettingsImpl = vi.fn(async (_base: string, request: { enabled: boolean }) => ({
    ...settings,
    enabled: request.enabled
  }));
  const { session } = makeTestSession({
    noEnter: true,
    getVideoSettingsImpl: vi.fn(async () => settings),
    getVideoStatusImpl: vi.fn(async () => status),
    putVideoSettingsImpl,
    ...over
  });
  render(VideoPage, { session });
  return { putVideoSettingsImpl };
}

const form = () => screen.findByRole('form', { name: 'Video settings' });

describe('VideoPage settings', () => {
  it('shows the settings, says video is off, and has nothing to save until something changes', async () => {
    open(VIDEO_OFF, IDLE);
    const f = await form();
    expect(within(f).getByRole('checkbox', { name: 'Record every race' })).not.toBeChecked();
    expect(within(f).getByLabelText('OBS address')).toHaveValue('127.0.0.1:4455');
    expect(within(f).getByLabelText('Seconds to keep recording after the stop')).toHaveValue(3);
    // The pill is about OBS, not the switch: OBS is reached even while recording is off.
    expect(screen.getByText('Not connected')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Save' })).toBeDisabled();
  });

  it('never shows the password: it says one is stored, and sends none unless one is typed', async () => {
    const { putVideoSettingsImpl } = open(ON, CONNECTED);
    const f = await form();
    const password = within(f).getByLabelText('OBS password');
    expect(password).toHaveValue('');
    expect(password).toHaveAttribute('type', 'password');
    expect(within(f).getByText('One is stored. Type here only to change it.')).toBeVisible();

    await fireEvent.input(within(f).getByLabelText('Seconds to keep recording after the stop'), {
      target: { value: '6' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    expect(putVideoSettingsImpl).toHaveBeenCalledWith(
      'http://d.local',
      {
        enabled: true,
        obs_address: '192.168.1.20:4455',
        tail_seconds: 6,
        trash_discarded: true,
        record_from: 'Obs',
        clean_scene: '',
        overlay_scene: '',
        render_when: 'Asked',
        browser_path: '',
        ffmpeg_path: '',
        save_in: 'Obs'
      },
      'tok'
    );
  });

  it('sends a typed password, and an emptied field clears the stored one', async () => {
    const { putVideoSettingsImpl } = open(ON, CONNECTED);
    const f = await form();
    const password = within(f).getByLabelText('OBS password');
    await fireEvent.input(password, { target: { value: 'new secret' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    expect(putVideoSettingsImpl.mock.calls[0][1]).toMatchObject({ obs_password: 'new secret' });

    await fireEvent.input(password, { target: { value: 'x' } });
    await fireEvent.input(password, { target: { value: '' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(2));
    expect(putVideoSettingsImpl.mock.calls[1][1]).toMatchObject({ obs_password: '' });
  });

  it("shows the Director's sentence when it refuses, and keeps what was typed", async () => {
    open(VIDEO_OFF, IDLE, {
      putVideoSettingsImpl: vi.fn(async () => {
        throw new Error("OBS's address looks like 192.168.1.20:4455: a host, a colon, and a port.");
      })
    });
    const f = await form();
    await fireEvent.input(within(f).getByLabelText('OBS address'), { target: { value: 'obs' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await within(f).findByRole('alert')).toHaveTextContent(/a host, a colon, and a port/);
    expect(within(f).getByLabelText('OBS address')).toHaveValue('obs');
  });

  it('refuses a tail that is not a whole number of seconds, without asking the Director', async () => {
    const { putVideoSettingsImpl } = open(VIDEO_OFF, IDLE);
    const f = await form();
    await fireEvent.input(within(f).getByLabelText('Seconds to keep recording after the stop'), {
      target: { value: '2.5' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(await within(f).findByRole('alert')).toHaveTextContent(/whole number of seconds/);
    expect(putVideoSettingsImpl).not.toHaveBeenCalled();
  });
});

describe('VideoPage status', () => {
  it('says which OBS answered and where it records, once connected', async () => {
    open(ON, CONNECTED);
    await form();
    await waitFor(() => expect(screen.getByText('Connected')).toBeVisible());
    const facts = screen.getByTestId('obs-facts');
    expect(facts).toHaveTextContent('OBS 31.0.2 on windows');
    expect(screen.getByTestId('video-folder')).toHaveTextContent('Videos are saved in C:\\Videos');
    // Grid's own folder, once that is picked.
    await fireEvent.change(screen.getByLabelText('Where videos are saved'), {
      target: { value: 'Grid' }
    });
    expect(screen.getByTestId('video-folder')).toHaveTextContent(
      "Pick OBS's own folder if OBS runs on another computer."
    );
  });

  it("says why OBS is not connected, in the Director's words", async () => {
    open(
      ON,
      videoStatus({
        obs: obsStatus({
          problem:
            "OBS did not take the password. Check it against OBS's WebSocket Server Settings."
        })
      })
    );
    await form();
    expect(await screen.findByTestId('obs-problem')).toHaveTextContent(
      /OBS did not take the password/
    );
    expect(screen.getByText('Not connected')).toBeVisible();
  });
});

describe('VideoPage: recording a scene of its own', () => {
  it('offers OBS\'s scenes once "a scene of its own" is picked, and saves the choice', async () => {
    const { putVideoSettingsImpl } = open(ON, CONNECTED);
    const f = await form();
    expect(screen.queryByTestId('scenes-need-obs')).toBeNull();
    // OBS's own recording has no scenes to pick.
    expect(within(f).queryByLabelText('Clean scene')).toBeNull();
    await fireEvent.change(within(f).getByLabelText('What records each race'), {
      target: { value: 'Scenes' }
    });
    const clean = within(f).getByLabelText('Clean scene');
    expect(
      within(clean)
        .getAllByRole('option')
        .map((o) => o.textContent)
    ).toEqual(['Pick a scene', 'Camera', 'Camera with overlay', 'Break']);
    await fireEvent.change(clean, { target: { value: 'Camera' } });
    await fireEvent.change(within(f).getByLabelText('Scene with the overlay'), {
      target: { value: 'Camera with overlay' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    expect(putVideoSettingsImpl.mock.calls[0][1]).toMatchObject({
      record_from: 'Scenes',
      clean_scene: 'Camera',
      overlay_scene: 'Camera with overlay'
    });
  });

  it('Connect stores only the address and password, and leaves the rest of the form unsaved', async () => {
    const { putVideoSettingsImpl } = open(VIDEO_OFF, IDLE);
    const f = await form();
    await fireEvent.change(within(f).getByLabelText('What records each race'), {
      target: { value: 'Scenes' }
    });
    await fireEvent.input(within(f).getByLabelText('OBS password'), {
      target: { value: 'secret' }
    });
    await fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await waitFor(() => expect(putVideoSettingsImpl).toHaveBeenCalledTimes(1));
    // What is stored for the rest goes back as it was: no scene is needed to connect.
    expect(putVideoSettingsImpl.mock.calls[0]?.[1]).toMatchObject({
      enabled: false,
      obs_address: '127.0.0.1:4455',
      obs_password: 'secret',
      record_from: 'Obs',
      clean_scene: ''
    });
    // The choice made in the form is still there, and still waiting for Save.
    expect(within(f).getByLabelText('What records each race')).toHaveValue('Scenes');
    expect(screen.getByRole('button', { name: 'Save' })).toBeEnabled();
  });

  it('keeps a stored scene in the picker while OBS is closed', async () => {
    open({ ...ON, record_from: 'Scenes', clean_scene: 'Camera', overlay_scene: '' }, IDLE);
    const f = await form();
    expect(within(f).getByLabelText('Clean scene')).toHaveValue('Camera');
    expect(within(f).getByLabelText('Scene with the overlay')).toHaveValue('');
    expect(screen.getByTestId('scenes-need-obs')).toHaveTextContent(
      'The scenes show here once OBS is connected.'
    );
  });

  it('says so when OBS has no Source Record plugin', async () => {
    open(
      { ...ON, record_from: 'Scenes', clean_scene: 'Camera' },
      videoStatus({ obs: obsStatus({ connected: true, scenes: ['Camera'], source_record: false }) })
    );
    await form();
    expect(await screen.findByTestId('no-source-record')).toHaveTextContent(
      'OBS does not have the Source Record plugin.'
    );
  });

  it('says which program is missing for drawing overlays, and nothing when both are found', async () => {
    open(
      ON,
      videoStatus({ tools: { browser: '/usr/bin/chromium', can_fetch: false, fetching: false } })
    );
    await form();
    expect(await screen.findByTestId('tools-missing')).toHaveTextContent('cannot find ffmpeg');
    // Grid has no build of ffmpeg for this computer: there is nothing to press.
    expect(screen.queryByRole('button', { name: 'Get ffmpeg' })).toBeNull();
    cleanup();
    open(ON, CONNECTED);
    await form();
    expect(screen.queryByTestId('tools-missing')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Get ffmpeg' })).toBeNull();
  });

  it('gets ffmpeg when asked, shows the download, and says why one failed', async () => {
    const missing = { browser: '/usr/bin/chromium', can_fetch: true, fetching: false };
    const postVideoFfmpegImpl = vi.fn(async () => {});
    open(ON, videoStatus({ tools: missing }), { postVideoFfmpegImpl });
    await form();
    await fireEvent.click(await screen.findByRole('button', { name: 'Get ffmpeg' }));
    await waitFor(() => expect(postVideoFfmpegImpl).toHaveBeenCalledOnce());
    cleanup();

    open(ON, videoStatus({ tools: { ...missing, fetching: true } }));
    await form();
    expect(await screen.findByRole('button', { name: 'Downloading ffmpeg…' })).toBeDisabled();
    cleanup();

    open(
      ON,
      videoStatus({
        tools: { ...missing, fetch_problem: 'Could not download ffmpeg: no route to host' }
      })
    );
    await form();
    expect(await screen.findByText('ffmpeg was not downloaded.')).toBeVisible();
    expect(screen.getByText('Could not download ffmpeg: no route to host')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Get ffmpeg' })).toBeEnabled();
  });
});

describe('VideoPage: only the setup', () => {
  it("has no recordings and no runs: those are on the event's Recordings page", async () => {
    open(ON, {
      ...IDLE,
      runs: [{ event: 'evt-9x', race: 'h#1', name: 'Night Heat 1 race 1', state: 'Saved' }]
    });
    await form();
    expect(screen.queryByRole('heading', { name: 'Recordings' })).toBeNull();
    expect(screen.queryByRole('heading', { name: 'Runs' })).toBeNull();
    expect(screen.queryByRole('list', { name: 'Recorded runs' })).toBeNull();
    expect(document.body.textContent).not.toContain('Night Heat 1 race 1');
  });
});
