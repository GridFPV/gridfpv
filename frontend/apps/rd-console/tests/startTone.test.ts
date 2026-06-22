/**
 * Unit tests for the start-tone player (heat-lifecycle Slice 3). A **mock `AudioContext`** is
 * injected so we can assert — with no real audio hardware — that the tone plays at race-go, that
 * mute suppresses it, and that the configured/default frequency is used.
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { StartTonePlayer, type ToneAudioContext } from '../src/lib/startTone.js';

/** A spy oscillator capturing the frequency it was set to and whether it was started. */
function makeMockContext(state = 'running'): {
  ctx: ToneAudioContext;
  oscillators: Array<{ freq: number; started: boolean }>;
  resumeSpy: ReturnType<typeof vi.fn>;
} {
  const oscillators: Array<{ freq: number; started: boolean }> = [];
  const resumeSpy = vi.fn(async () => {});
  const ctx: ToneAudioContext = {
    currentTime: 0,
    state,
    destination: {},
    createOscillator() {
      const rec = { freq: 0, started: false };
      oscillators.push(rec);
      return {
        type: 'sine' as OscillatorType,
        frequency: {
          setValueAtTime(value: number) {
            rec.freq = value;
          }
        },
        connect() {},
        start() {
          rec.started = true;
        },
        stop() {}
      };
    },
    createGain() {
      return {
        gain: { setValueAtTime() {}, linearRampToValueAtTime() {} },
        connect() {}
      };
    },
    resume: resumeSpy,
    close: async () => {}
  };
  return { ctx, oscillators, resumeSpy };
}

/** A minimal in-memory Storage (this jsdom config provides no real localStorage). */
function makeMemoryStorage(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k: string) => map.get(k) ?? null,
    key: (i: number) => [...map.keys()][i] ?? null,
    removeItem: (k: string) => void map.delete(k),
    setItem: (k: string, v: string) => void map.set(k, v)
  };
}

describe('StartTonePlayer', () => {
  let storage: Storage;
  beforeEach(() => {
    // A fresh in-memory storage per test so the persisted mute pref doesn't bleed across cases.
    storage = makeMemoryStorage();
    vi.stubGlobal('localStorage', storage);
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('plays an oscillator burst at race-go (default tone) when unmuted', () => {
    const { ctx, oscillators } = makeMockContext();
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });

    player.play();

    expect(oscillators).toHaveLength(1);
    expect(oscillators[0].started).toBe(true);
    // The default tone is 880Hz when no cue is supplied.
    expect(oscillators[0].freq).toBe(880);
  });

  it('uses the configured tone frequency from the round cue', () => {
    const { ctx, oscillators } = makeMockContext();
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });

    player.play({ hz: 440, ms: 250 });

    expect(oscillators[0].freq).toBe(440);
  });

  it('is a no-op while muted — no oscillator is created', () => {
    const { ctx, oscillators } = makeMockContext();
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: true });

    player.play();

    expect(oscillators).toHaveLength(0);
  });

  it('toggleMuted flips and persists the preference, gating the next play', () => {
    const { ctx, oscillators } = makeMockContext();
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });

    expect(player.toggleMuted()).toBe(true); // now muted
    player.play();
    expect(oscillators).toHaveLength(0);
    // The preference persisted to storage.
    expect(storage.getItem('gridfpv.startTone.muted')).toBe('true');

    expect(player.toggleMuted()).toBe(false); // unmuted again
    player.play();
    expect(oscillators).toHaveLength(1);
  });

  it('a fresh player reads the persisted mute preference', () => {
    storage.setItem('gridfpv.startTone.muted', 'true');
    const { ctx, oscillators } = makeMockContext();
    const player = new StartTonePlayer({ audioContextFactory: () => ctx });
    expect(player.muted).toBe(true);
    player.play();
    expect(oscillators).toHaveLength(0);
  });

  it('resumes a suspended context (autoplay-policy unlock)', async () => {
    const { ctx, resumeSpy } = makeMockContext('suspended');
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });
    await player.resume();
    expect(resumeSpy).toHaveBeenCalled();
  });

  it('on a *suspended* context, resumes BEFORE starting the oscillator (the audible-tone fix)', async () => {
    // The race-go edge is an auto transition (no click), so the context can still be suspended.
    // Scheduling against a frozen suspended clock and only resuming after means the note never
    // sounds — the original "no tone" bug. Assert play() resumes first, then emits an oscillator.
    const { ctx, oscillators, resumeSpy } = makeMockContext('suspended');
    const order: string[] = [];
    resumeSpy.mockImplementation(async () => {
      order.push('resume');
    });
    const origCreate = ctx.createOscillator.bind(ctx);
    ctx.createOscillator = () => {
      order.push('oscillator');
      return origCreate();
    };
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });

    player.play();
    // Let the resume() promise + its .then() emit chain settle.
    await Promise.resolve();
    await Promise.resolve();

    expect(resumeSpy).toHaveBeenCalled();
    expect(oscillators).toHaveLength(1);
    expect(oscillators[0].started).toBe(true);
    // Resume must precede the oscillator so the note is scheduled on the *resumed* clock.
    expect(order).toEqual(['resume', 'oscillator']);
  });

  it('on a *running* context, plays synchronously without an extra resume', () => {
    const { ctx, oscillators, resumeSpy } = makeMockContext('running');
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });
    player.play();
    // Already running → fire immediately; no resume needed on this path.
    expect(oscillators).toHaveLength(1);
    expect(oscillators[0].started).toBe(true);
    expect(resumeSpy).not.toHaveBeenCalled();
  });

  it('a mute toggled during the resume suppresses the (suspended-path) tone', async () => {
    const { ctx, oscillators, resumeSpy } = makeMockContext('suspended');
    const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });
    resumeSpy.mockImplementation(async () => {
      player.setMuted(true); // RD hits mute during the brief resume window
    });
    player.play();
    await Promise.resolve();
    await Promise.resolve();
    expect(oscillators).toHaveLength(0);
  });

  it('is a silent no-op when Web Audio is unavailable (no factory)', () => {
    // No platform AudioContext and no injected factory → unavailable; play never throws.
    const player = new StartTonePlayer({ audioContextFactory: undefined, initialMuted: false });
    expect(player.available).toBe(false);
    expect(() => player.play()).not.toThrow();
  });

  // A mock whose resume() actually flips `state` to 'running' (a real browser does this on a user
  // gesture). The base makeMockContext keeps `state` fixed; these enable()/locked cases need it to
  // transition so they mirror the real autoplay-unlock behaviour.
  function makeUnlockingContext(): {
    ctx: ToneAudioContext;
    oscillators: Array<{ freq: number; started: boolean }>;
  } {
    const oscillators: Array<{ freq: number; started: boolean }> = [];
    const ctx = {
      currentTime: 0,
      state: 'suspended',
      destination: {},
      createOscillator() {
        const rec = { freq: 0, started: false };
        oscillators.push(rec);
        return {
          type: 'sine' as OscillatorType,
          frequency: {
            setValueAtTime(value: number) {
              rec.freq = value;
            }
          },
          connect() {},
          start() {
            rec.started = true;
          },
          stop() {}
        };
      },
      createGain() {
        return {
          gain: { setValueAtTime() {}, linearRampToValueAtTime() {} },
          connect() {}
        };
      },
      async resume() {
        ctx.state = 'running';
      },
      close: async () => {}
    } as ToneAudioContext & { state: string };
    return { ctx, oscillators };
  }

  describe('enable() — the Enable/Test-tone affordance', () => {
    it('resumes a suspended context AND plays one confirmation beep', async () => {
      const { ctx, oscillators } = makeUnlockingContext();
      const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });

      const running = await player.enable();

      // resume() flipped the context to 'running', so the beep emits.
      expect(running).toBe(true);
      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].started).toBe(true);
    });

    it('plays the confirmation beep even while muted (the RD asked to test it)', async () => {
      // Mute suppresses the *automatic* race-go tone, not the explicit Enable/Test gesture.
      const { ctx, oscillators } = makeMockContext('running');
      const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: true });

      const running = await player.enable();

      expect(running).toBe(true);
      expect(oscillators).toHaveLength(1);
    });

    it('reports the context could not be unlocked (still locked) and does not throw', async () => {
      // A context whose resume() never reaches 'running' (no real gesture) stays locked.
      const { ctx, oscillators } = makeMockContext('suspended');
      // Override resume to NOT flip the state (still suspended after the await).
      (ctx as { resume: () => Promise<void> }).resume = async () => {};
      const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });

      const running = await player.enable();
      expect(running).toBe(false);
      expect(oscillators).toHaveLength(0);
    });

    it('is a silent no-op (returns false) when Web Audio is unavailable', async () => {
      const player = new StartTonePlayer({ audioContextFactory: undefined });
      await expect(player.enable()).resolves.toBe(false);
    });
  });

  describe('locked — the enabled/locked indicator', () => {
    it('reads false when Web Audio is unavailable (nothing to unlock)', () => {
      const player = new StartTonePlayer({ audioContextFactory: undefined });
      expect(player.locked).toBe(false);
    });

    it('reads locked until the context is resumed, then enabled', async () => {
      const { ctx } = makeUnlockingContext();
      const player = new StartTonePlayer({ audioContextFactory: () => ctx, initialMuted: false });
      // Before any use the context isn't even created → locked.
      expect(player.locked).toBe(true);
      await player.resume();
      // resume() flips the context to 'running' → no longer locked.
      expect(player.locked).toBe(false);
    });
  });
});
