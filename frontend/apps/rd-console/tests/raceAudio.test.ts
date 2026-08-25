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
 *
 * Since #397 the crossing pip fires per gate CROSSING rather than per recorded lap, so a whole
 * heat can pip inside one frame — hence the stagger cases here: bursts are spread on the audio
 * clock (80ms apart) instead of stacking into one blip, a pile-up past the 600ms lookahead is
 * dropped rather than trailing the race, and a procedure tone never queues behind the pips.
 */
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { RaceAudioPlayer, type ToneAudioContext } from '../src/lib/raceAudio.js';

/** A spy oscillator record: the frequency it was set to, whether it started, and its length. */
interface OscRecord {
  freq: number;
  started: boolean;
  /** `stop(when) - start(when)` in seconds — the burst length (dur + the 0.02s release pad). */
  lengthSecs: number;
  /** The `start(when)` instant on the context clock — what the crossing-pip stagger moves. */
  startAt: number;
}

function makeMockContext(state = 'running'): {
  ctx: ToneAudioContext;
  oscillators: OscRecord[];
  resumeSpy: ReturnType<typeof vi.fn>;
  /** Advance the mock's `currentTime` (seconds) — the pip stagger is scheduled against it. */
  advance: (secs: number) => void;
} {
  const oscillators: OscRecord[] = [];
  const resumeSpy = vi.fn(async () => {});
  let clock = 0;
  const ctx: ToneAudioContext = {
    get currentTime() {
      return clock;
    },
    state,
    destination: {},
    createOscillator() {
      const rec: OscRecord = { freq: 0, started: false, lengthSecs: 0, startAt: 0 };
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
          rec.startAt = startedAt;
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
  return {
    ctx,
    oscillators,
    resumeSpy,
    advance: (secs: number) => {
      clock += secs;
    }
  };
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

    it('STAGGERS a pack of pips 80ms apart — eight seats must read as eight pips, not one blip', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      // A whole heat crosses within the same frame. Scheduled at the SAME instant they would sum
      // into one louder burst (and toward clipping) rather than eight countable pips.
      for (let i = 0; i < 8; i += 1) player.playCrossingBeep();

      expect(oscillators).toHaveLength(8);
      expect(oscillators.map((o) => o.startAt)).toEqual(
        [0, 0.08, 0.16, 0.24, 0.32, 0.4, 0.48, 0.56].map((t) => expect.closeTo(t, 5))
      );
      // Each 60ms burst finishes before the next starts, so silence separates them.
      for (let i = 1; i < oscillators.length; i += 1) {
        expect(oscillators[i].startAt - oscillators[i - 1].startAt).toBeGreaterThan(
          oscillators[i - 1].lengthSecs - 0.02
        );
      }
    });

    it('DROPS a pip that would land past the lookahead — a pile-up must not machine-gun', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      // Far more crossings than a heat can produce at once (a stuck gate re-triggering).
      for (let i = 0; i < 40; i += 1) player.playCrossingBeep();

      // Only the pips inside the 600ms lookahead sound; the rest are dropped, not queued.
      expect(oscillators).toHaveLength(8);
      expect(oscillators[oscillators.length - 1].startAt).toBeLessThanOrEqual(0.6);
    });

    it('a later pip plays at NOW once the clock has passed the queue (never scheduled in the past)', () => {
      const { ctx, oscillators, advance } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      player.playCrossingBeep();
      player.playCrossingBeep();
      expect(oscillators[1].startAt).toBeCloseTo(0.08, 5);

      // A quiet stretch: the next lap's pip must fire immediately, not at a stale slot.
      advance(30);
      player.playCrossingBeep();
      expect(oscillators[2].startAt).toBeCloseTo(30, 5);
    });

    it('a procedure tone is never delayed behind the pip queue — the race signal comes first', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: false });

      for (let i = 0; i < 4; i += 1) player.playCrossingBeep();
      player.playRaceEndTone();

      // The buzzer sounds NOW (0), while the pips ahead of it hold slots out to 0.24.
      expect(oscillators[4].freq).toBe(440);
      expect(oscillators[4].startAt).toBeCloseTo(0, 5);
      expect(oscillators[3].startAt).toBeCloseTo(0.24, 5);
    });

    it('the stagger never leaks past the mute — a muted pack claims no slots at all', () => {
      const { ctx, oscillators } = makeMockContext();
      const player = new RaceAudioPlayer({ audioContextFactory: () => ctx, initialMuted: true });

      for (let i = 0; i < 8; i += 1) player.playCrossingBeep();
      expect(oscillators).toHaveLength(0);

      // Unmuting starts from a clean slot, not from where a muted burst would have left it.
      player.setMuted(false);
      player.playCrossingBeep();
      expect(oscillators).toHaveLength(1);
      expect(oscillators[0].startAt).toBeCloseTo(0, 5);
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
