/**
 * Tests for the guided-install callout. #384: the install steps must name the **unzip** step and
 * the exact final layout, and the download must report its outcome — success or failure — instead
 * of the old silent transient-anchor click. #385: the guide must say *where* RotorHazard's
 * `plugins/` directory is, and that it may not exist yet. #386: the guide must offer a **Restart
 * timer** action that is confirmed before it fires, surfaces the Director's race-in-progress refusal
 * verbatim, and narrates the expected drop → reconnect as progress rather than a fault.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import { toasts } from '@gridfpv/components';
import type { Timer } from '@gridfpv/types';
import type { Session } from '../src/lib/session.svelte.js';
import PluginCallout from '../src/screens/PluginCallout.svelte';

const RH_MISSING: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [],
  manual_connect: false,
  calibration: [],
  disabled_nodes: [],
  plugin: 'Missing'
};

/** A minimal stand-in for the console session: only `restartTimer` is exercised here (#386). */
function fakeSession(restartTimer: (id: string) => Promise<Timer | undefined>) {
  return { restartTimer } as unknown as Session;
}

/** Collapse markup wrapping so copy assertions aren't whitespace-sensitive. */
function flat(text: string | null | undefined): string {
  return (text ?? '').replace(/\s+/g, ' ');
}

/** Open the guided-install dialog from the warning chip. */
async function openGuide(props: { timer?: Timer; session?: Session } = {}) {
  render(PluginCallout, {
    timer: props.timer ?? RH_MISSING,
    baseUrl: 'http://host:3000',
    session: props.session
  });
  await fireEvent.click(screen.getByRole('button', { name: /plugin missing/i }));
}

/** Click **Restart timer**, then its two-step **Confirm** — the action only fires on the confirm. */
async function restartAndConfirm() {
  await fireEvent.click(screen.getByRole('button', { name: 'Restart timer' }));
  await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
}

let clickSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  toasts.clear();
  // jsdom implements neither object URLs nor anchor-triggered downloads.
  URL.createObjectURL = vi.fn(() => 'blob:stub');
  URL.revokeObjectURL = vi.fn();
  clickSpy = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {});
});

afterEach(() => {
  clickSpy.mockRestore();
  vi.unstubAllGlobals();
});

describe('PluginCallout install steps', () => {
  it('tells the RD to unzip, and names the exact final layout', async () => {
    await openGuide();
    const steps = flat(document.querySelector('ol.steps')?.textContent);
    expect(steps).toMatch(/unzip/i);
    // The thing that gets copied is the inner folder — not the zip, not the wrapper.
    expect(steps).toMatch(/wrapper folder/i);
    expect(steps).toMatch(/plugins\/gridfpv\//);
    expect(steps).toContain('__init__.py');
    expect(steps).toContain('manifest.json');
  });

  it('says where RotorHazard’s plugins/ folder is, and that it may not exist (#385)', async () => {
    await openGuide();
    const where = flat(document.querySelector('details.where')?.textContent);
    // The usual location, flagged as a default rather than a guarantee.
    expect(where).toContain('~/rh-data/plugins/');
    expect(where).toMatch(/not a guarantee/i);
    // The legacy in-place install, and how to find a vendor/custom one.
    expect(where).toContain('<RotorHazard>/src/server/plugins/');
    expect(where).toContain('config.json');
    expect(where).toContain('Data path:');
    // The actual field sticking point: there may be no folder to find.
    expect(where).toMatch(/create it/i);
    // RotorHazard's own guide, rather than restating it.
    const link = document.querySelector<HTMLAnchorElement>('details.where a');
    expect(link?.href).toBe(
      'https://github.com/RotorHazard/RotorHazard/blob/v4.3.0/doc/Plugins.md'
    );
  });
});

describe('PluginCallout download feedback', () => {
  it('confirms the file name and where to find it on success', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response(new Blob(['zip']), { status: 200 }))
    );
    await openGuide();
    await fireEvent.click(screen.getByRole('button', { name: 'Download plugin' }));

    const note = await screen.findByRole('status');
    expect(note.textContent).toContain('gridfpv-plugin.zip');
    expect(note.textContent).toMatch(/downloads/i);
    expect(clickSpy).toHaveBeenCalled();
    await waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0].tone).toBe('success');
  });

  it('surfaces a real error when the bundle cannot be fetched', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('nope', { status: 500 }))
    );
    await openGuide();
    await fireEvent.click(screen.getByRole('button', { name: 'Download plugin' }));

    const note = await screen.findByRole('status');
    expect(note.textContent).toMatch(/couldn’t download/i);
    expect(note.textContent).toContain('500');
    expect(clickSpy).not.toHaveBeenCalled();
    await waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0].tone).toBe('danger');
  });
});

describe('PluginCallout restart timer (#386)', () => {
  it('confirms before firing, then restarts the named timer', async () => {
    const restartTimer = vi.fn(async () => RH_MISSING);
    await openGuide({ session: fakeSession(restartTimer) });

    // The guide points at the in-app action rather than RotorHazard's own web interface.
    const steps = flat(document.querySelector('ol.steps')?.textContent);
    expect(steps).toMatch(/Restart timer/);
    expect(steps).toMatch(/no need to open/i);

    // Arming the confirm must NOT fire the restart — that is the whole point of the two-step.
    await fireEvent.click(screen.getByRole('button', { name: 'Restart timer' }));
    expect(restartTimer).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    expect(restartTimer).toHaveBeenCalledWith('rh-1');

    // The expected drop is narrated as normal, naming the timer (never its URL or id).
    const note = await screen.findByRole('status');
    expect(note.textContent).toContain('Track RH');
    expect(note.textContent).toMatch(/reconnects on its own/i);
    expect(note.textContent).not.toContain('rh.local');
    await waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0].tone).toBe('info');
  });

  it('presents the post-restart disconnect as progress, not a fault', async () => {
    // RotorHazard re-executes, so the timer legitimately passes through Disconnected for a few
    // seconds. That window must read as "waiting for it to come back", never as an error.
    const dropped: Timer = { ...RH_MISSING, status: 'Disconnected' };
    const restartTimer = vi.fn(async () => dropped);
    await openGuide({ timer: dropped, session: fakeSession(restartTimer) });
    await restartAndConfirm();

    await waitFor(() => {
      const notes = screen.getAllByRole('status').map((n) => n.textContent ?? '');
      expect(notes.some((t) => /expected/i.test(t) && t.includes('Track RH'))).toBe(true);
    });
    // Nothing is styled as a failure while the restart is in flight.
    expect(document.querySelector('.status.bad')).toBeNull();
  });

  it('surfaces the Director’s race-in-progress refusal verbatim', async () => {
    // The gate is the Director's, on heat phase — the console just reports what it says, which names
    // the heat and the timer by their friendly names.
    const restartTimer = vi.fn(async () => {
      throw new Error(
        'Track RH is running Qualifier Heat 1 — finish or reset that heat before restarting the timer'
      );
    });
    await openGuide({ session: fakeSession(restartTimer) });
    await restartAndConfirm();

    const note = await screen.findByRole('status');
    expect(note.textContent).toContain('Qualifier Heat 1');
    expect(note.className).toContain('bad');
    await waitFor(() => expect(toasts.items).toHaveLength(1));
    expect(toasts.items[0].tone).toBe('danger');
  });

  it('offers no restart action without a session, and disables it while disconnected', async () => {
    // No session ⇒ no RD-gated call to make, so the action is absent rather than firing ungated.
    await openGuide();
    expect(screen.queryByRole('button', { name: 'Restart timer' })).toBeNull();

    // Connected is a precondition: there is no socket to emit `restart_server` on otherwise.
    document.body.innerHTML = '';
    await openGuide({
      timer: { ...RH_MISSING, status: 'Configured' },
      session: fakeSession(vi.fn(async () => RH_MISSING))
    });
    expect(screen.getByRole('button', { name: 'Restart timer' })).toBeDisabled();
  });
});
