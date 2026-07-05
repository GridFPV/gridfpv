import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import RaceClock from '../src/RaceClock.svelte';

describe('RaceClock', () => {
  it('formats elapsed milliseconds as M:SS.mmm', () => {
    render(RaceClock, { elapsedMs: 83_456 });
    expect(screen.getByText('1:23.456')).toBeInTheDocument();
  });

  it('renders a countdown and marks remaining mode when given remainingMs', () => {
    const { container } = render(RaceClock, { remainingMs: 5_000 });
    expect(screen.getByText('0:05.000')).toBeInTheDocument();
    expect(container.querySelector('[data-mode="remaining"]')).not.toBeNull();
  });

  it('grades countdown urgency: ok while comfortable, closing ≤10s, over past zero', () => {
    // Comfortable: normal color (no warn tint) so the countdown isn't crying wolf all race.
    const ok = render(RaceClock, { remainingMs: 25_000 });
    expect(ok.container.querySelector('[data-urgency="ok"]')).not.toBeNull();
    ok.unmount();
    // The closing stretch: warn-colored.
    const closing = render(RaceClock, { remainingMs: 9_000 });
    expect(closing.container.querySelector('[data-urgency="closing"]')).not.toBeNull();
    closing.unmount();
    // Past zero (the grace window): a NEGATIVE, sign-prefixed readout, danger-colored.
    const over = render(RaceClock, { remainingMs: -3_250 });
    expect(screen.getByText('-0:03.250')).toBeInTheDocument();
    expect(over.container.querySelector('[data-urgency="over"]')).not.toBeNull();
  });

  it('an elapsed (count-up) clock carries no urgency grade', () => {
    const { container } = render(RaceClock, { elapsedMs: 61_000 });
    expect(container.querySelector('[data-urgency]')).toBeNull();
  });

  it('exposes an accessible timer label', () => {
    render(RaceClock, { elapsedMs: 0, label: 'Lap timer' });
    expect(screen.getByRole('timer')).toHaveAttribute('aria-label', 'Lap timer: 0:00.000');
  });
});
