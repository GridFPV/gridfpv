/**
 * Unit tests for the two-step confirm button used by destructive heat off-ramps
 * (Revert / Restart / Abort / Discard). Covers the confirm flow *and* the prominent
 * "active confirm" styling: when armed, the Confirm button carries the `confirm-danger`
 * class that paints it a solid red (danger) fill with near-black text — an unmistakable
 * destructive affordance (vs. the restrained default danger style).
 */
import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import { createRawSnippet } from 'svelte';
import ConfirmButton from '../src/lib/ConfirmButton.svelte';

/** A bare text label snippet for the button's children (e.g. "Revert"). */
const label = (text: string) => createRawSnippet(() => ({ render: () => `<span>${text}</span>` }));

describe('ConfirmButton', () => {
  it('arms on first click and fires onconfirm only on the explicit Confirm', async () => {
    const onconfirm = vi.fn();
    render(ConfirmButton, { onconfirm, variant: 'danger', children: label('Revert') });

    // First click arms — no action yet, and a Confirm + Cancel appear.
    await fireEvent.click(screen.getByRole('button', { name: 'Revert' }));
    expect(onconfirm).not.toHaveBeenCalled();
    const confirm = screen.getByRole('button', { name: 'Confirm' });
    expect(confirm).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeInTheDocument();

    // The explicit Confirm fires the action.
    await fireEvent.click(confirm);
    expect(onconfirm).toHaveBeenCalledTimes(1);
  });

  it('the armed Confirm is the prominent danger affordance (solid red fill + dark text)', async () => {
    render(ConfirmButton, {
      onconfirm: vi.fn(),
      variant: 'danger',
      children: label('Restart')
    });

    await fireEvent.click(screen.getByRole('button', { name: 'Restart' }));
    const confirm = screen.getByRole('button', { name: 'Confirm' });
    // Inverted, high-prominence styling: the danger variant + the forwarded `data-confirm-danger`
    // hook the stylesheet repaints to a solid red fill with near-black letters. The hook is a
    // data-attribute (not a class) so the Button primitive keeps its own `gf-btn` base class.
    expect(confirm).toHaveAttribute('data-confirm-danger');
    expect(confirm).toHaveClass('gf-btn');
    expect(confirm).toHaveAttribute('data-variant', 'danger');
  });

  it('Cancel disarms without firing', async () => {
    const onconfirm = vi.fn();
    render(ConfirmButton, { onconfirm, children: label('Discard') });

    await fireEvent.click(screen.getByRole('button', { name: 'Discard' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(onconfirm).not.toHaveBeenCalled();
    // Back to the single resting button.
    expect(screen.getByRole('button', { name: 'Discard' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Confirm' })).toBeNull();
  });

  it('confirm={false} is a plain one-click button (no arming)', async () => {
    const onconfirm = vi.fn();
    render(ConfirmButton, { onconfirm, confirm: false, children: label('Start') });

    await fireEvent.click(screen.getByRole('button', { name: 'Start' }));
    expect(onconfirm).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('button', { name: 'Confirm' })).toBeNull();
  });
});
