import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { AutoSaver } from '../src/lib/autosave.js';

/**
 * AutoSaver is the per-target debounced saver behind the Save-less selection stages. These tests
 * pin its contract: it coalesces rapid changes per target into one trailing save, computes the
 * payload at flush time (so it captures the latest state), saves different targets independently,
 * and routes falsy results / thrown errors to the right callback.
 */
describe('AutoSaver', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('debounces rapid schedules on one target into a single trailing save', async () => {
    const save = vi.fn(async () => ({ ok: true }));
    const saver = new AutoSaver(300);

    // Three quick changes mutate a shared "working selection"; compute reads it at flush time, so
    // the single trailing save carries the LATEST value — not a stale snapshot from scheduling.
    let selection = 'a';
    const compute = () => selection;
    saver.schedule('classes', { compute, save });
    selection = 'ab';
    saver.schedule('classes', { compute, save });
    selection = 'abc';
    saver.schedule('classes', { compute, save });

    expect(save).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(300);
    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith('abc');
  });

  it('saves different targets independently', async () => {
    const save = vi.fn(async () => ({ ok: true }));
    const saver = new AutoSaver(300);

    saver.schedule('classes', { compute: () => 'c', save });
    saver.schedule('roster', { compute: () => 'r', save });

    await vi.advanceTimersByTimeAsync(300);
    expect(save).toHaveBeenCalledTimes(2);
    expect(save).toHaveBeenCalledWith('c');
    expect(save).toHaveBeenCalledWith('r');
  });

  it('routes a falsy save result to onUnsaved (not onError)', async () => {
    const onUnsaved = vi.fn();
    const onError = vi.fn();
    const saver = new AutoSaver(300);

    saver.schedule('t', { compute: () => 1, save: async () => undefined, onUnsaved, onError });
    await vi.advanceTimersByTimeAsync(300);

    expect(onUnsaved).toHaveBeenCalledTimes(1);
    expect(onError).not.toHaveBeenCalled();
  });

  it('routes a thrown save to onError', async () => {
    const onError = vi.fn();
    const saver = new AutoSaver(300);

    saver.schedule('t', {
      compute: () => 1,
      save: async () => {
        throw new Error('boom');
      },
      onError
    });
    await vi.advanceTimersByTimeAsync(300);

    expect(onError).toHaveBeenCalledTimes(1);
    expect(onError.mock.calls[0][0]).toBeInstanceOf(Error);
  });

  it('cancel drops a pending save; flush runs it immediately', async () => {
    const save = vi.fn(async () => ({ ok: true }));
    const saver = new AutoSaver(300);

    saver.schedule('a', { compute: () => 'a', save });
    saver.cancel('a');
    await vi.advanceTimersByTimeAsync(300);
    expect(save).not.toHaveBeenCalled();

    saver.schedule('b', { compute: () => 'b', save });
    saver.flush('b');
    await vi.advanceTimersByTimeAsync(0);
    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith('b');
  });
});
