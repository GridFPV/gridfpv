import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent, within } from '@testing-library/dom';
import Registration from '../src/screens/Registration.svelte';
import { makeTestSession } from './support.js';
import { liveRunning } from './fixtures.js';

describe('Registration', () => {
  it('lists the seen competitor refs from the live lineup', () => {
    const { session } = makeTestSession({ live: liveRunning });
    render(Registration, { session });
    expect(screen.getByText('ALICE')).toBeInTheDocument();
    expect(screen.getByText('BOB')).toBeInTheDocument();
  });

  it('emits a Register command binding the ref to the typed pilot id', async () => {
    const { session, sendSpy } = makeTestSession({ live: liveRunning });
    render(Registration, { session, adapter: 'rh-1' });

    const input = screen.getByLabelText('Pilot for ALICE') as HTMLInputElement;
    await fireEvent.input(input, { target: { value: 'pilot-alice' } });
    // Scope to ALICE's row (each row has its own Register button).
    const row = input.closest('tr')!;
    await fireEvent.click(within(row).getByRole('button', { name: 'Register' }));

    expect(sendSpy).toHaveBeenCalledWith({
      Register: { adapter: 'rh-1', competitor: 'ALICE', pilot: 'pilot-alice' }
    });
  });

  it('shows an empty state when no competitors are seen yet', () => {
    const { session } = makeTestSession({ live: { phase: 'Scheduled' } });
    render(Registration, { session });
    expect(screen.getByText(/No competitors seen yet/i)).toBeInTheDocument();
  });
});
