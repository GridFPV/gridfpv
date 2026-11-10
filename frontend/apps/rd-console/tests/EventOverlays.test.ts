/**
 * The Overlays card on the event page: it names the overlay, gives its URL for each
 * contrast, copies it, opens it in a new tab, and shows the live preview on demand.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import EventOverlays from '../src/screens/EventOverlays.svelte';

const BASE = `${location.origin}${location.pathname}`;

afterEach(() => vi.unstubAllGlobals());

describe('EventOverlays', () => {
  it('lists the race overlay with what it shows and its URL', () => {
    render(EventOverlays);
    const card = screen.getByRole('region', { name: 'Race overlay' });
    expect(card).toHaveTextContent('The heat being raced');
    expect(screen.getByLabelText('Race overlay URL')).toHaveTextContent(`${BASE}#/overlay/race`);
    const link = screen.getByRole('link', { name: 'Open in a new tab' });
    expect(link).toHaveAttribute('href', `${BASE}#/overlay/race`);
    expect(link).toHaveAttribute('target', '_blank');
  });

  it('the contrast choice changes the URL, the link and what is copied', async () => {
    const writeText = vi.fn(async () => {});
    vi.stubGlobal('navigator', { ...navigator, clipboard: { writeText } });
    render(EventOverlays);
    const dark = screen.getByRole('radio', { name: 'Light text, for dark video' });
    const light = screen.getByRole('radio', { name: 'Dark text, for bright video' });
    expect(dark).toHaveAttribute('aria-checked', 'true');

    await fireEvent.click(light);
    expect(light).toHaveAttribute('aria-checked', 'true');
    expect(dark).toHaveAttribute('aria-checked', 'false');
    expect(screen.getByLabelText('Race overlay URL')).toHaveTextContent(
      `${BASE}#/overlay/race/light`
    );
    expect(screen.getByRole('link', { name: 'Open in a new tab' })).toHaveAttribute(
      'href',
      `${BASE}#/overlay/race/light`
    );

    await fireEvent.click(screen.getByRole('button', { name: 'Copy URL' }));
    await waitFor(() => expect(writeText).toHaveBeenCalledWith(`${BASE}#/overlay/race/light`));
  });

  it('the preview is the same route in a frame, opened on demand', async () => {
    // The preview measures its box to set the scale; jsdom has nothing to measure with.
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe() {}
        unobserve() {}
        disconnect() {}
      }
    );
    render(EventOverlays);
    expect(screen.queryByTitle('Race overlay preview')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Show preview' }));
    const frame = screen.getByTitle('Race overlay preview');
    expect(frame).toHaveAttribute('src', `${BASE}#/overlay/race`);
    expect(frame).toHaveAttribute('width', '1920');
    expect(frame).toHaveAttribute('height', '1080');

    await fireEvent.click(screen.getByRole('button', { name: 'Hide preview' }));
    expect(screen.queryByTitle('Race overlay preview')).toBeNull();
  });
});
