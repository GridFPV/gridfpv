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

  it('exposes an accessible timer label', () => {
    render(RaceClock, { elapsedMs: 0, label: 'Lap timer' });
    expect(screen.getByRole('timer')).toHaveAttribute('aria-label', 'Lap timer: 0:00.000');
  });
});
