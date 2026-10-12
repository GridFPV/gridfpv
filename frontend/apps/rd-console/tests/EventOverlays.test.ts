/**
 * EventOverlays — the in-event Overlays tab: lists the race overlay with its URL for OBS, swaps the
 * contrast into the URL, copies it, and frames a live preview of the same route.
 */
import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/svelte';
import EventOverlays from '../src/screens/EventOverlays.svelte';
import { makeTestSession } from './support.js';

describe('EventOverlays', () => {
  it('lists the race overlay with its OBS URL on the console origin, in either contrast', async () => {
    const { session } = makeTestSession({});
    render(EventOverlays, { session });
    expect(screen.getByText('Race timer')).toBeInTheDocument();
    const url = screen.getByLabelText('Race timer URL');
    expect(url).toHaveTextContent(
      new RegExp(`^${location.origin}${location.pathname}#/overlay/race$`)
    );
    await fireEvent.click(screen.getByRole('radio', { name: 'Dark text on light' }));
    expect(url).toHaveTextContent(/#\/overlay\/race\/light$/);
    await fireEvent.click(screen.getByRole('radio', { name: 'Light text on dark' }));
    expect(url).toHaveTextContent(/#\/overlay\/race$/);
  });

  it('frames the same route as a preview and links it to a new tab', async () => {
    const { session } = makeTestSession({});
    render(EventOverlays, { session });
    const frame = screen.getByTitle('Race timer preview');
    expect(frame).toHaveAttribute('src', expect.stringMatching(/#\/overlay\/race$/));
    const link = screen.getByRole('link', { name: 'Open in a new tab' });
    expect(link).toHaveAttribute('target', '_blank');
    expect(link).toHaveAttribute('href', expect.stringMatching(/#\/overlay\/race$/));
    await fireEvent.click(screen.getByRole('radio', { name: 'Dark text on light' }));
    expect(frame).toHaveAttribute('src', expect.stringMatching(/#\/overlay\/race\/light$/));
  });

  it('copies the URL to the clipboard', async () => {
    const writeText = vi.fn(async () => {});
    Object.assign(navigator, { clipboard: { writeText } });
    const { session } = makeTestSession({});
    render(EventOverlays, { session });
    await fireEvent.click(screen.getByRole('button', { name: 'Copy URL' }));
    expect(writeText).toHaveBeenCalledWith(expect.stringMatching(/#\/overlay\/race$/));
  });
});
