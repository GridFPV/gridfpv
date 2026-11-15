/**
 * The Overlays card on the event page: it names the overlay, gives its URL for each
 * contrast, copies it, opens it in a new tab, and shows the live preview on demand.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, waitFor } from '@testing-library/dom';
import type { OverlayLook, OverlayPlacement } from '@gridfpv/types';
import EventOverlays from '../src/screens/EventOverlays.svelte';
import { OVERLAY_LOOK } from './support.js';

/** The card with a Director behind it that keeps the look it is given. */
function open(start: OverlayLook = OVERLAY_LOOK) {
  let look = start;
  const session = {
    getOverlayLook: vi.fn(async () => look),
    putOverlayLook: vi.fn(async (placement: OverlayPlacement) => (look = { ...look, placement })),
    overlayLogoUrl: (side: 'left' | 'right') => `http://d.local/overlay/logo/${side}`,
    putOverlayLogo: vi.fn(
      async (side: 'left' | 'right', _file: Blob) => (look = { ...look, [`logo_${side}`]: true })
    ),
    deleteOverlayLogo: vi.fn(
      async (side: 'left' | 'right') => (look = { ...look, [`logo_${side}`]: false })
    )
  };
  render(EventOverlays, { session });
  return session;
}

const BASE = `${location.origin}${location.pathname}`;

afterEach(() => vi.unstubAllGlobals());

describe('EventOverlays', () => {
  it('lists the race overlay with what it shows and its URL', () => {
    open();
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
    open();
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

  it('the preview is the overlay with a made-up race, in a frame, opened on demand', async () => {
    open();
    expect(screen.queryByTitle('Race overlay preview')).toBeNull();
    await fireEvent.click(screen.getByRole('button', { name: 'Show preview' }));
    const frame = screen.getByTitle('Race overlay preview');
    // The sample page, never the URL that goes into OBS.
    expect(frame).toHaveAttribute('src', `${BASE}#/overlay/race/sample`);
    expect(screen.getByLabelText('Race overlay URL')).toHaveTextContent(`${BASE}#/overlay/race`);
    await fireEvent.click(screen.getByRole('radio', { name: 'Dark text, for bright video' }));
    expect(screen.getByTitle('Race overlay preview')).toHaveAttribute(
      'src',
      `${BASE}#/overlay/race/light/sample`
    );
    expect(frame).toHaveAttribute('width', '1920');
    expect(frame).toHaveAttribute('height', '1080');

    await fireEvent.click(screen.getByRole('button', { name: 'Hide preview' }));
    expect(screen.queryByTitle('Race overlay preview')).toBeNull();
  });

  it('moves the overlay: each change is saved as it is made', async () => {
    const session = open();
    const placement = await screen.findByRole('group', { name: 'Placement' });
    expect(screen.getByRole('radio', { name: 'Top left' })).toHaveAttribute('aria-checked', 'true');
    expect(screen.getByLabelText('Size')).toHaveValue(100);
    expect(screen.getByLabelText('Background')).toHaveValue(90);

    await fireEvent.click(screen.getByRole('radio', { name: 'Bottom right' }));
    await waitFor(() =>
      expect(screen.getByRole('radio', { name: 'Bottom right' })).toHaveAttribute(
        'aria-checked',
        'true'
      )
    );
    expect(session.putOverlayLook).toHaveBeenLastCalledWith({
      corner: 'BottomRight',
      x: 24,
      y: 24,
      scale: 100,
      opacity: 90
    });

    const side = screen.getByLabelText('In from the side');
    await fireEvent.change(side, { target: { value: '60' } });
    await waitFor(() => expect(session.putOverlayLook).toHaveBeenCalledTimes(2));
    expect(session.putOverlayLook).toHaveBeenLastCalledWith(
      expect.objectContaining({ corner: 'BottomRight', x: 60 })
    );
    await waitFor(() => expect(screen.getByLabelText('Size slider')).toBeEnabled());
    await fireEvent.change(screen.getByLabelText('Size slider'), { target: { value: '150' } });
    await waitFor(() => expect(screen.getByLabelText('Size')).toHaveValue(150));
    await fireEvent.change(screen.getByLabelText('Background slider'), { target: { value: '0' } });
    await waitFor(() =>
      expect(session.putOverlayLook).toHaveBeenLastCalledWith(
        expect.objectContaining({ scale: 150, opacity: 0 })
      )
    );
    expect(placement).toBeVisible();
  });

  it('refuses a size out of range in words, and saves nothing', async () => {
    const session = open();
    await screen.findByRole('group', { name: 'Placement' });
    await fireEvent.change(screen.getByLabelText('Size'), { target: { value: '10' } });
    expect(await screen.findByRole('alert')).toHaveTextContent('The size is 25 to 300 percent.');
    expect(session.putOverlayLook).not.toHaveBeenCalled();
  });

  it('keeps a logo for each end of the title line, and takes one away', async () => {
    const session = open();
    await screen.findByRole('group', { name: 'Logos' });
    expect(screen.getByTestId('no-logo-left')).toHaveTextContent('The GridFPV name shows.');
    const file = new File(['png'], 'racegow.png', { type: 'image/png' });
    await fireEvent.change(screen.getByLabelText('Left logo picture'), {
      target: { files: [file] }
    });
    await waitFor(() => expect(session.putOverlayLogo).toHaveBeenCalledWith('left', file));
    const shown = await screen.findByRole('img', { name: 'Left logo now' });
    expect(shown.getAttribute('src')).toMatch(/^http:\/\/d\.local\/overlay\/logo\/left\?v=\d+$/);
    expect(screen.getByTestId('no-logo-right')).toBeVisible();

    await fireEvent.click(screen.getByRole('button', { name: 'Remove the left logo' }));
    await waitFor(() => expect(session.deleteOverlayLogo).toHaveBeenCalledWith('left'));
    expect(await screen.findByTestId('no-logo-left')).toBeVisible();
  });
});
