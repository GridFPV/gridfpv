import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/svelte';
import StatusPill from '../src/primitives/StatusPill.svelte';

/** Read the rendered pill's tone + pulse off the DOM (the color is driven by `data-tone`). */
function pill(props: Record<string, unknown>) {
  const { container } = render(StatusPill, props);
  const el = container.querySelector('.gf-pill') as HTMLElement;
  return { tone: el.getAttribute('data-tone'), pulses: el.classList.contains('pulse'), el };
}

describe('StatusPill', () => {
  it('maps read-stream connection status to its tone', () => {
    expect(pill({ status: 'live' }).tone).toBe('live');
    expect(pill({ status: 'connecting' }).tone).toBe('pending');
    expect(pill({ status: 'reconnecting' }).tone).toBe('warn');
    expect(pill({ status: 'idle' }).tone).toBe('idle');
  });

  // ── Timer connection status (#73, Slice 2b) ──────────────────────────────────
  it('maps a timer TimerStatus to a calm/healthy/alarming tone', () => {
    // Resting config reads neutral so a single Mock never alarms.
    expect(pill({ status: 'Ready' }).tone).toBe('idle');
    expect(pill({ status: 'Configured' }).tone).toBe('idle');
    // Live healthy connection is green and pulses; dialing is pending.
    expect(pill({ status: 'Connecting' }).tone).toBe('pending');
    const connected = pill({ status: 'Connected' });
    expect(connected.tone).toBe('live');
    expect(connected.pulses).toBe(true);
    // A dropped/errored timer reads as a problem.
    expect(pill({ status: 'Disconnected' }).tone).toBe('warn');
    expect(pill({ status: 'Error' }).tone).toBe('danger');
  });

  it('phase still maps to its phase tone (no regression)', () => {
    expect(pill({ phase: 'Running' }).tone).toBe('running');
    expect(pill({ phase: 'Scheduled' }).tone).toBe('scheduled');
  });
});
