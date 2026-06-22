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

  it('is a silent no-op when Web Audio is unavailable (no factory)', () => {
    // No platform AudioContext and no injected factory → unavailable; play never throws.
    const player = new StartTonePlayer({ audioContextFactory: undefined, initialMuted: false });
    expect(player.available).toBe(false);
    expect(() => player.play()).not.toThrow();
  });
});
