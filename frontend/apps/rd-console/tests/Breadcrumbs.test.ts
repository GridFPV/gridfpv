import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import Breadcrumbs from '../src/Breadcrumbs.svelte';

describe('Breadcrumbs (#118)', () => {
  it('renders intermediate crumbs as buttons and the last as the current page', () => {
    const onhome = vi.fn();
    render(Breadcrumbs, {
      crumbs: [{ label: 'Home', onclick: onhome }, { label: 'Events' }, { label: 'Friday' }]
    });

    // Home is a navigable button; the leaf is aria-current, not a button.
    expect(screen.getByRole('button', { name: 'Home' })).toBeInTheDocument();
    const leaf = screen.getByText('Friday');
    expect(leaf).toHaveAttribute('aria-current', 'page');
    expect(screen.queryByRole('button', { name: 'Friday' })).toBeNull();
  });

  it('invokes a crumb onclick when navigated', async () => {
    const onhome = vi.fn();
    const onevents = vi.fn();
    render(Breadcrumbs, {
      crumbs: [
        { label: 'Home', onclick: onhome },
        { label: 'Events', onclick: onevents },
        { label: 'Friday' }
      ]
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Home' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Events' }));
    expect(onhome).toHaveBeenCalledTimes(1);
    expect(onevents).toHaveBeenCalledTimes(1);
  });
});
