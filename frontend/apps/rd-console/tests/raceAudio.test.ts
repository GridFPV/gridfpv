/**
 * Unit tests for the race-day audio player (grown out of the heat-lifecycle Slice 3 start tone).
 * A **mock `AudioContext`** is injected so we can assert — with no real audio hardware — the two
 * scopes of the tone model:
 *   • PROCEDURE tones (start tone, end-of-race countdown pip, race-end buzzer) play
 *     unconditionally — the callouts mute must NOT silence them;
 *   • the INFORMATIONAL crossing pip is mute-scoped;
 * plus each tone's pitch/length (start ≈800ms @880Hz, pip ≈150ms @880Hz, buzzer lower+longer
 * ≈600ms @440Hz, crossing higher+very short ≈60ms @1760Hz) and the autoplay-policy
 * resume-then-schedule fix.
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { RaceAudioPlayer, type ToneAudioContext } from '../src/lib/raceAudio.js';

/** A spy oscillator record: the frequency it was set to, whether it started, and its length. */
interface OscRecord {
  freq: number;
  started: boolean;
  /** `stop(when) - start(when)` in seconds — the burst length (dur + the 0.02s release pad). */
  lengthSecs: number;
}

function makeMockContext(state = 'running'): {
  ctx: ToneAudioContext;
  oscillators: OscRecord[];
  resumeSpy: ReturnType<typeof vi.fn>;
} {
  const oscillators: OscRecord[] = [];
  const resumeSpy = vi.fn(async () => {});
  const ctx: ToneAudioContext = {
    currentTime: 0,
    state,
    destination: {},
    createOscillator() {
      const rec: OscRecord = { freq: 0, started: false, lengthSecs: 0 };
      let startedAt = 0;
      oscillators.push(rec);
      return {
        type: 'sine' as OscillatorType,
        frequency: {
          setValueAtTime(value: number) {
            rec.freq = value;
          }
        },
        connect() {},
        start(when?: number) {
          startedAt = when ?? 0;
          rec.started = true;
        },
        stop(when?: number) {
          rec.lengthSecs = (when ?? 0) - startedAt;
        }
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

describe('RaceAudioPlayer', () => {
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

  describe('procedure tones (always on)', () => {
    it('plays the start tone at 880Hz for ~800ms by default (doubled from the old 400ms)', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      player.playStartTone();

      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].started).toBe(true);
      expect(oscillators[0].freq).toBe(880);
      // 0.8s burst + the 0.02s release pad.
      expect(oscillators[0].lengthSecs).toBeCloseTo(0.82, 5);
    });

    it('still honours the configured round cue (hz/ms) for the start tone', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      player.playStartTone({ hz: 440, ms: 250 });

      expect(oscillators[0].freq).toBe(440);
      expect(oscillators[0].lengthSecs).toBeCloseTo(0.27, 5);
    });

    it('the start tone plays EVEN WHILE MUTED — procedure tones ignore the callouts mute', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: true });

      player.playStartTone();

      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].started).toBe(true);
    });

    it('the countdown pip is short (~150ms) in the start-tone pitch family (880Hz), unmuted by the toggle', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: true });

      player.playCountdownBeep();

      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].freq).toBe(880);
      expect(oscillators[0].lengthSecs).toBeCloseTo(0.17, 5);
    });

    it('the race-end buzzer is LOWER (half pitch, 440Hz) and LONGER (~600ms), unmuted by the toggle', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: true });

      player.playRaceEndTone();
      player.playCountdownBeep();

      expect(oscillators[0].freq).toBe(440);
      expect(oscillators[0].lengthSecs).toBeCloseTo(0.62, 5);
      // Explicitly lower + longer than the countdown pip.
      expect(oscillators[0].freq).toBe(oscillators[1].freq / 2);
      expect(oscillators[0].lengthSecs).toBeGreaterThan(oscillators[1].lengthSecs);
    });
  });

  describe('informational layer (mute-scoped)', () => {
    it('plays the crossing pip — higher (1760Hz) + very short (~60ms) — while unmuted', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      player.playCrossingBeep();

      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].freq).toBe(1760);
      expect(oscillators[0].lengthSecs).toBeCloseTo(0.08, 5);
    });

    it('the crossing pip is a no-op while muted — the ONLY tone the mute silences', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: true });

      player.playCrossingBeep();
      expect(oscillators).toHaveLength(0);

      // …while every procedure tone still sounds.
      player.playStartTone();
      player.playCountdownBeep();
      player.playRaceEndTone();
      expect(oscillators).toHaveLength(3);
    });

    it('a mute toggled during the (suspended-path) resume suppresses the crossing pip', async () => {
      const { ctx, oscillators, resumeSpy } = makeMockContext('suspended');
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });
      resumeSpy.mockImplementation(async () => {
        player.setMuted(true); // RD hits mute during the brief resume window
      });
      player.playCrossingBeep();
      await Promise.resolve();
      await Promise.resolve();
      expect(oscillators).toHaveLength(0);
    });

    it('a mute toggled during the resume does NOT suppress a procedure tone', async () => {
      const { ctx, oscillators, resumeSpy } = makeMockContext('suspended');
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });
      resumeSpy.mockImplementation(async () => {
        player.setMuted(true);
      });
      player.playStartTone();
      await Promise.resolve();
      await Promise.resolve();
      expect(oscillators).toHaveLength(1);
    });
  });

  describe('mute preference (callouts scope)', () => {
    it('toggleMuted flips and persists under the NEW callouts key', () => {
      const { ctx } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      expect(player.toggleMuted()).toBe(true);
      expect(storage.getItem('gridfpv.callouts.muted')).toBe('true');
      expect(player.toggleMuted()).toBe(false);
      expect(storage.getItem('gridfpv.callouts.muted')).toBe('false');
    });

    it('a fresh player reads the persisted callouts pref', () => {
      storage.setItem('gridfpv.callouts.muted', 'true');
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx });
      expect(player.muted).toBe(true);
      player.playCrossingBeep();
      expect(oscillators).toHaveLength(0);
    });

    it('IGNORES the old start-tone mute key — its meaning changed (it silenced procedure tones)', () => {
      storage.setItem('gridfpv.startTone.muted', 'true');
      const { ctx } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx });
      expect(player.muted).toBe(false);
    });
  });

  describe('autoplay policy (resume-then-schedule)', () => {
    it('resumes a suspended context (autoplay-policy unlock)', async () => {
      const { ctx, resumeSpy } = makeMockContext('suspended');
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });
      await player.resume();
      expect(resumeSpy).toHaveBeenCalled();
    });

    it('on a *suspended* context, resumes BEFORE starting the oscillator (the audible-tone fix)', async () => {
      // The race-go edge is an auto transition (no click), so the context can still be suspended.
      // Scheduling against a frozen suspended clock and only resuming after means the note never
      // sounds — the original "no tone" bug. Assert playStartTone() resumes first, then emits.
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
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      player.playStartTone();
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
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });
      player.playStartTone();
      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].started).toBe(true);
      expect(resumeSpy).not.toHaveBeenCalled();
    });
  });

  it('is a silent no-op when Web Audio is unavailable (no factory)', () => {
    // No platform AudioContext and no injected factory → unavailable; nothing throws.
    const player = new RaceAudioPlayer({ audioContextFactory: undefined, initialMuted: false });
    expect(player.available).toBe(false);
    expect(() => player.playStartTone()).not.toThrow();
    expect(() => player.playCountdownBeep()).not.toThrow();
    expect(() => player.playRaceEndTone()).not.toThrow();
    expect(() => player.playCrossingBeep()).not.toThrow();
  });
});
