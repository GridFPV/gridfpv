import { afterEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import InfoTip from '../src/primitives/InfoTip.svelte';
import Card from '../src/primitives/Card.svelte';

/**
 * InfoTip — the console's one tooltip (#466).
 *
 * The point of adding a component rather than reaching for `title=` was reachability: the console's
 * standing help was being moved OFF the page, so whatever replaced it had to stay available to a
 * keyboard and a screen reader. These tests pin exactly that, because it is the property that
 * justifies the change — a tooltip nobody can open is worse than the paragraph it replaced.
 */

describe('InfoTip', () => {
  it('exposes a real focusable control, named for what it explains', () => {
    render(InfoTip, { text: 'How layouts work.', label: 'About channel layouts' });
    const trigger = screen.getByRole('button', { name: 'About channel layouts' });
    expect(trigger).toBeInTheDocument();
    expect(trigger.tagName).toBe('BUTTON');
  });

  it('keeps the text in the accessibility tree at all times via aria-describedby', () => {
    // Deliberately NOT an `{#if}`: the description has to resolve on focus whether or not the
    // bubble is visually shown, which is what makes this readable without a mouse.
    render(InfoTip, { text: 'Higher is cleaner and 100 is the ceiling.', label: 'About IMD' });
    const trigger = screen.getByRole('button', { name: 'About IMD' });
    const described = document.getElementById(trigger.getAttribute('aria-describedby')!);
    expect(described).not.toBeNull();
    expect(described).toHaveTextContent('Higher is cleaner and 100 is the ceiling.');
  });

  it('toggles open on click and reports it with aria-expanded (the touch path)', async () => {
    render(InfoTip, { text: 'Body.', label: 'About it' });
    const trigger = screen.getByRole('button', { name: 'About it' });
    expect(trigger).toHaveAttribute('aria-expanded', 'false');

    await fireEvent.click(trigger);
    expect(trigger).toHaveAttribute('aria-expanded', 'true');
    await fireEvent.click(trigger);
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
  });

  it('closes on Escape when open, and does not swallow Escape when shut', async () => {
    render(InfoTip, { text: 'Body.', label: 'About it' });
    const trigger = screen.getByRole('button', { name: 'About it' });
    await fireEvent.click(trigger);
    await fireEvent.keyDown(trigger, { key: 'Escape' });
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
    // Shut already: Escape must keep bubbling so it still closes the dialog around it.
    await fireEvent.keyDown(trigger, { key: 'Escape' });
    expect(trigger).toHaveAttribute('aria-expanded', 'false');
  });

  it('gives each tip its own id, so two on one screen describe themselves', () => {
    render(InfoTip, { text: 'One.', label: 'First' });
    render(InfoTip, { text: 'Two.', label: 'Second' });
    const a = screen.getByRole('button', { name: 'First' }).getAttribute('aria-describedby');
    const b = screen.getByRole('button', { name: 'Second' }).getAttribute('aria-describedby');
    expect(a).not.toBe(b);
    expect(document.getElementById(a!)).toHaveTextContent('One.');
    expect(document.getElementById(b!)).toHaveTextContent('Two.');
  });
});

describe('where the bubble sits', () => {
  /** A window of this size, a trigger at `by`, and a bubble of this size. */
  function lay(
    by: { left: number; top: number },
    bubble = { width: 300, height: 80 },
    win = { width: 1000, height: 600 }
  ) {
    vi.spyOn(document.documentElement, 'clientWidth', 'get').mockReturnValue(win.width);
    vi.spyOn(document.documentElement, 'clientHeight', 'get').mockReturnValue(win.height);
    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (
      this: HTMLElement
    ) {
      const box =
        this.getAttribute('role') === 'tooltip'
          ? { left: 0, top: 0, ...bubble }
          : { ...by, width: 18, height: 18 };
      return {
        ...box,
        x: box.left,
        y: box.top,
        right: box.left + box.width,
        bottom: box.top + box.height,
        toJSON: () => ({})
      };
    });
  }
  afterEach(() => vi.restoreAllMocks());

  async function shown(label: string): Promise<HTMLElement> {
    await fireEvent.mouseEnter(screen.getByRole('button', { name: label }).parentElement!);
    return screen.getByRole('tooltip');
  }

  it('is fixed to the window, under its trigger, so a card cannot cut it off', async () => {
    lay({ left: 200, top: 100 });
    render(InfoTip, { text: 'Help.', label: 'About it' });
    const bubble = await shown('About it');
    expect(bubble.style.left).toBe('200px');
    // Under the trigger (which ends at 118), with the gap.
    expect(bubble.style.top).toBe('126px');
  });

  it('hangs from the other edge when asked to', async () => {
    lay({ left: 600, top: 100 });
    render(InfoTip, { text: 'Help.', label: 'About it', align: 'end' });
    const bubble = await shown('About it');
    // Its right edge is the trigger's right edge: 618 - 300.
    expect(bubble.style.left).toBe('318px');
  });

  it('is pushed back in from the sides of the window', async () => {
    lay({ left: 900, top: 100 });
    render(InfoTip, { text: 'Help.', label: 'Right' });
    expect((await shown('Right')).style.left).toBe('692px');
    vi.restoreAllMocks();
    lay({ left: 100, top: 100 });
    render(InfoTip, { text: 'Help.', label: 'Left', align: 'end' });
    await fireEvent.mouseEnter(screen.getByRole('button', { name: 'Left' }).parentElement!);
    expect(screen.getAllByRole('tooltip')[1].style.left).toBe('8px');
  });

  it('goes above its trigger when there is no room below', async () => {
    lay({ left: 200, top: 560 });
    render(InfoTip, { text: 'Help.', label: 'About it' });
    // Above the trigger, with the gap: 560 - 8 - 80.
    expect((await shown('About it')).style.top).toBe('472px');
  });

  it('is placed when opened by a press or by the keyboard too', async () => {
    lay({ left: 200, top: 100 });
    render(InfoTip, { text: 'Help.', label: 'Pressed' });
    await fireEvent.click(screen.getByRole('button', { name: 'Pressed' }));
    expect(screen.getByRole('tooltip').style.top).toBe('126px');
    render(InfoTip, { text: 'Help.', label: 'Focused' });
    await fireEvent.focusIn(screen.getByRole('button', { name: 'Focused' }));
    expect(screen.getAllByRole('tooltip')[1].style.top).toBe('126px');
  });
});

describe('Card help', () => {
  it('renders the tip beside the heading, leaving the heading’s name clean', () => {
    // The regression this pins: nesting the `?` inside the <h3> makes it part of the heading's
    // accessible name ("Rounds, More information"), which breaks by-role queries and screen-reader
    // navigation alike.
    render(Card, {
      title: 'Rounds',
      help: 'Define this event’s rounds.',
      children: (() => {}) as never
    });
    expect(screen.getByRole('heading', { name: 'Rounds' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'About Rounds' })).toBeInTheDocument();
  });

  it('renders no tip when a card supplies no help', () => {
    render(Card, { title: 'Rounds', children: (() => {}) as never });
    expect(screen.getByRole('heading', { name: 'Rounds' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^About / })).toBeNull();
  });
});
