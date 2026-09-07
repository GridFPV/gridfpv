/**
 * The brand block, and the Director's version line under it (#467).
 *
 * The version used to be a fixed watermark in the bottom-right corner of every screen. Ryan asked
 * for it under the "GridFPV RD CONSOLE" wordmark instead, so these pin the two halves of that:
 * the version is *in* the brand, and there is no corner overlay left behind.
 *
 * It is read from `GET /about` — the version of the Director actually answering, not a constant
 * baked into this bundle. That is the point: the failure worth catching is a stale Director serving
 * a new console, and a build-time constant agrees with itself in exactly that case.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import Brand from '../src/Brand.svelte';
import { resetDirectorVersion } from '../src/lib/buildVersion.svelte.js';

const noop = () => {};

/** Answer `/about` the way the Director does, or fail the read the way a dead one does. */
function stubAbout(answer: { ok: boolean; body?: unknown } | 'reject') {
  const fetchMock = vi.fn(async () => {
    if (answer === 'reject') throw new Error('unreachable');
    return {
      ok: answer.ok,
      json: async () => answer.body
    } as Response;
  });
  vi.stubGlobal('fetch', fetchMock);
  return fetchMock;
}

beforeEach(() => resetDirectorVersion());
afterEach(() => vi.unstubAllGlobals());

describe('Brand — the version under the wordmark (#467)', () => {
  it('renders the Director’s version as a third line in the brand block', async () => {
    stubAbout({ ok: true, body: { version: '0.4.0-alpha.1' } });
    render(Brand, { onclick: noop });

    const version = await screen.findByLabelText('Director version');
    expect(version).toHaveTextContent('v0.4.0-alpha.1');

    // Inside the brand button — under the wordmark, not floating somewhere else on the page.
    const brand = screen.getByRole('button');
    expect(brand).toContainElement(version);
    // And after the sub-line, which is the order Ryan asked for: name, RD Console, version.
    const lines = (brand.textContent ?? '').replace(/\s+/g, ' ').trim();
    expect(lines.indexOf('RD Console')).toBeLessThan(lines.indexOf('v0.4.0-alpha.1'));
  });

  it('asks the DIRECTOR, same-origin, rather than reading a build-time constant', async () => {
    const fetchMock = stubAbout({ ok: true, body: { version: '9.9.9' } });
    render(Brand, { onclick: noop });
    await screen.findByLabelText('Director version');
    expect(fetchMock).toHaveBeenCalledWith('/about');
  });

  it('reads /about ONCE however many brands are mounted', async () => {
    // Six screens mount this component; the Director's version cannot change without the page
    // reloading with it, so one fetch is the whole app's worth.
    const fetchMock = stubAbout({ ok: true, body: { version: '0.4.0' } });
    render(Brand, { onclick: noop });
    render(Brand, { onclick: noop, sub: 'Timers' });
    await waitFor(() => expect(screen.getAllByLabelText('Director version')).toHaveLength(2));
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('shows the wordmark alone when the Director does not answer', async () => {
    // A console with no version is entirely usable; a wrong one is worse than none on a support
    // call where the RD is being asked to read it out.
    stubAbout('reject');
    render(Brand, { onclick: noop });
    await waitFor(() => expect(screen.getByText('RD Console')).toBeInTheDocument());
    expect(screen.queryByLabelText('Director version')).toBeNull();
  });

  it('shows nothing rather than “vundefined” when /about answers something unexpected', async () => {
    stubAbout({ ok: true, body: { build: 'not a version' } });
    render(Brand, { onclick: noop });
    await waitFor(() => expect(screen.getByText('RD Console')).toBeInTheDocument());
    expect(screen.queryByLabelText('Director version')).toBeNull();
    expect(document.body.textContent).not.toContain('undefined');
  });

  it('shows nothing when the Director answers a non-OK status', async () => {
    stubAbout({ ok: false });
    render(Brand, { onclick: noop });
    await waitFor(() => expect(screen.getByText('RD Console')).toBeInTheDocument());
    expect(screen.queryByLabelText('Director version')).toBeNull();
  });
});
