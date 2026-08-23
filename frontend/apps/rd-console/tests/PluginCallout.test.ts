/**
 * Tests for the guided-install callout. #384: the install steps must name the **unzip** step and
 * the exact final layout, and the download must report its outcome — success or failure — instead
 * of the old silent transient-anchor click. #385: the guide must say *where* RotorHazard's
 * `plugins/` directory is, and that it may not exist yet.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import { toasts } from '@gridfpv/components';
import type { Timer } from '@gridfpv/types';
import PluginCallout from '../src/screens/PluginCallout.svelte';

const RH_MISSING: Timer = {
  id: 'rh-1',
  name: 'Track RH',
  kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
  status: 'Connected',
  channel_capability: 'Flexible',
  node_count: 8,
  available_channels: [],
  plugin: 'Missing'
};

/** Collapse markup wrapping so copy assertions aren't whitespace-sensitive. */
function flat(text: string | null | undefined): string {
  return (text ?? '').replace(/\s+/g, ' ');
}

/** Open the guided-install dialog from the warning chip. */
async function openGuide() {
  render(PluginCallout, { timer: RH_MISSING, baseUrl: 'http://host:3000' });
  await fireEvent.click(screen.getByRole('button', { name: /plugin missing/i }));
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
