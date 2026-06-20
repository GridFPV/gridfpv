import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import SetupWizard from '../src/screens/SetupWizard.svelte';
import { emptyConfig, type EventConfig } from '../src/lib/setup.js';

describe('SetupWizard', () => {
  it('walks the steps and commits a complete config', async () => {
    const config: EventConfig = {
      eventId: 'spring',
      eventName: 'Spring Cup',
      track: 'Main field',
      classes: [{ id: 'open', name: 'Open', format: 'timed-qual', winCondition: 'BestLap' }]
    };
    const oncommit = vi.fn();
    render(SetupWizard, { config, oncommit });

    // Jump to the Review step.
    await fireEvent.click(screen.getByRole('button', { name: '5. Review' }));
    await fireEvent.click(screen.getByRole('button', { name: 'Save configuration' }));

    expect(oncommit).toHaveBeenCalledOnce();
    expect(oncommit.mock.calls[0][0]).toMatchObject({ eventId: 'spring', eventName: 'Spring Cup' });
  });

  it('blocks finishing an incomplete config and lists the problems', async () => {
    const oncommit = vi.fn();
    render(SetupWizard, { config: emptyConfig(), oncommit });

    await fireEvent.click(screen.getByRole('button', { name: '5. Review' }));
    const save = screen.getByRole('button', { name: 'Save configuration' }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);
    expect(screen.getByText('Event needs a name.')).toBeInTheDocument();
  });
});
