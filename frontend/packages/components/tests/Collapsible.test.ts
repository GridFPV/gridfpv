import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import { tick } from 'svelte';
import Collapsible from '../src/primitives/Collapsible.svelte';
import CollapsibleHarness from './CollapsibleHarness.svelte';

/** Pull the toggle button + its controlled region off a rendered Collapsible. */
function parts(container: HTMLElement) {
  const toggle = container.querySelector('.gf-collapsible-toggle') as HTMLButtonElement;
  const region = container.querySelector('.gf-collapsible-region') as HTMLElement;
  return { toggle, region };
}

describe('Collapsible', () => {
  it('renders its title and content, defaulting to open', () => {
    const { container, getByText } = render(Collapsible, { title: 'Roster' });
    const { toggle, region } = parts(container);
    expect(getByText('Roster')).toBeInTheDocument();
    // Default open: expanded, region visible.
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    expect(region.getAttribute('aria-hidden')).toBe('false');
    expect(region.hasAttribute('hidden')).toBe(false);
  });

  it('wires the toggle to the region via aria-controls', () => {
    const { container } = render(Collapsible, { title: 'Roster' });
    const { toggle, region } = parts(container);
    expect(toggle.getAttribute('aria-controls')).toBe(region.id);
    expect(region.getAttribute('role')).toBe('region');
  });

  it('self-manages open state: a click collapses, hiding the content', async () => {
    const { container } = render(Collapsible, { title: 'Roster' });
    const { toggle, region } = parts(container);
    await fireEvent.click(toggle);
    await tick();
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(region.getAttribute('aria-hidden')).toBe('true');
    expect(region.hasAttribute('hidden')).toBe(true);
    // Toggling back re-opens it.
    await fireEvent.click(toggle);
    await tick();
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    expect(region.hasAttribute('hidden')).toBe(false);
  });

  it('honors a controlled (bound) open prop, starting collapsed when told to', () => {
    const { container } = render(Collapsible, { title: 'Roster', open: false });
    const { toggle, region } = parts(container);
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(region.hasAttribute('hidden')).toBe(true);
  });

  it('keyboard-toggles via the native button (Space/Enter fire a click)', async () => {
    const { container } = render(Collapsible, { title: 'Roster' });
    const { toggle, region } = parts(container);
    // A native <button> activates on a click event (what Space/Enter dispatch).
    toggle.focus();
    await fireEvent.click(toggle);
    await tick();
    expect(region.hasAttribute('hidden')).toBe(true);
  });

  it('renders summary + actions snippets in the header', () => {
    const { getByTestId, getByText } = render(CollapsibleHarness, { title: 'Class A' });
    expect(getByText('3 placed')).toBeInTheDocument();
    expect(getByTestId('action-btn')).toBeInTheDocument();
    expect(getByTestId('body')).toBeInTheDocument();
  });

  it('a bound parent persists open state across the binding (controlled round-trip)', async () => {
    const { container, getByTestId } = render(CollapsibleHarness, { title: 'Class A', open: true });
    const { toggle } = parts(container);
    await fireEvent.click(toggle);
    await tick();
    // The harness mirrors `open` into a probe element — the collapse propagated to the parent.
    expect(getByTestId('open-probe').textContent).toBe('false');
  });
});
