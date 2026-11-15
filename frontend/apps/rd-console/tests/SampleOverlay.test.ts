/**
 * The sample overlay (`#/overlay/race/sample`): the race panel with a made-up race under
 * way, which is what the Overlays page previews.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import type { OverlayLook } from '@gridfpv/types';
import SampleOverlay from '../src/screens/SampleOverlay.svelte';
import { OVERLAY_LOOK } from './support.js';

afterEach(() => cleanup());

function open(look: OverlayLook | Error = OVERLAY_LOOK) {
  const session = {
    getOverlayLook: vi.fn(async () => {
      if (look instanceof Error) throw look;
      return look;
    }),
    overlayLogoUrl: (side: 'left' | 'right') => `http://d.local/overlay/logo/${side}`
  };
  render(SampleOverlay, { session, theme: 'light' as const });
  return session;
}

describe('SampleOverlay', () => {
  it('shows one pilot a few laps into a timed race, with the clock running', async () => {
    open();
    const panel = screen.getByTestId('overlay-panel');
    expect(screen.getByRole('region', { name: 'Race overlay' })).toHaveAttribute(
      'data-theme',
      'light'
    );
    expect(screen.getByTestId('overlay-heat')).toHaveTextContent('Your event Track 2 race 3');
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('LIVE');
    expect(screen.getByTestId('overlay-solo')).toHaveTextContent('Your pilot');
    expect(panel.querySelector('[data-stat="laps"]')).toHaveTextContent('5');
    // 8.455 + 8.102 + 8.370: the best three in a row.
    expect(panel).toHaveTextContent('Best 3 this race');
    expect(panel.querySelector('[data-stat="best-run"]')).toHaveTextContent('0:24.927');
    const laps = within(screen.getByRole('list', { name: 'Recent laps' })).getAllByRole('listitem');
    expect(laps.map((li) => li.textContent?.replace(/\s+/g, ' ').trim())).toEqual([
      'HS 0:02.400',
      'L1 0:09.812',
      'L2 0:08.455',
      'L3 0:08.102',
      'L4 0:08.370',
      'L5 0:08.946'
    ]);
    expect(laps.map((li) => li.classList.contains('lit'))).toEqual([
      false,
      false,
      true,
      true,
      true,
      false
    ]);
    // A timed race counts down, with the time since the start beside it.
    const clock = screen.getByRole('timer');
    expect(clock.textContent).toMatch(/^1:\d\d\.\d$/);
    expect(screen.getByTestId('overlay-elapsed').textContent).toMatch(/^0:4\d\.\d$/);
    const first = clock.textContent;
    await waitFor(() => expect(clock.textContent).not.toBe(first));
  });

  it('sits where the real overlay does, with its logos', async () => {
    open({
      placement: { corner: 'BottomLeft', x: 60, y: 300, scale: 200, opacity: 20 },
      logo_left: false,
      logo_right: true
    });
    const panel = screen.getByTestId('overlay-panel');
    await waitFor(() => expect(panel).toHaveAttribute('data-corner', 'BottomLeft'));
    expect(panel.getAttribute('style')).toContain('scale(2)');
    expect(panel.getAttribute('style')).toContain('--panel-alpha: 0.2');
    expect(panel.querySelector('img')).toHaveAttribute('src', 'http://d.local/overlay/logo/right');
  });

  it('is still drawn, in the usual place, when the look cannot be read', async () => {
    const session = open(new Error('The Director did not answer.'));
    await waitFor(() => expect(session.getOverlayLook).toHaveBeenCalled());
    expect(screen.getByTestId('overlay-panel')).toHaveAttribute('data-corner', 'TopLeft');
    expect(screen.getByTestId('overlay-state')).toHaveTextContent('LIVE');
  });
});
