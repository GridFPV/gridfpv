import { describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/svelte';
import { waitFor } from '@testing-library/dom';
import Probe from './support/SignalFeedProbe.svelte';

describe('two surfaces, one timer lease', () => {
  it('one unmounting must not stop the other stream', async () => {
    const read = vi.fn(async () => ({
      timer: 't',
      streaming: true,
      lease_ms_remaining: 5000,
      period_micros: 200000,
      sample_micros: [],
      nodes: []
    }));
    const release = vi.fn(async () => {});
    const a = render(Probe, { id: 't', read, release, pollMs: 5 });
    const b = render(Probe, { id: 't', read, release, pollMs: 5 });
    await waitFor(() => expect(read).toHaveBeenCalled());
    a.unmount();
    // B is still watching 't', so the Director's subscription must stay up. Releasing here is what
    // made the Tune page poll a subscription that had been removed under it — empty every time,
    // cured only by a manual refresh.
    await new Promise((r) => setTimeout(r, 30));
    expect(release).not.toHaveBeenCalled();

    // The last watcher out DOES give it back.
    b.unmount();
    await waitFor(() => expect(release).toHaveBeenCalledWith('t'));
  });
});
