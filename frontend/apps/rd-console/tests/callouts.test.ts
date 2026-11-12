/** Lap callouts on the Run page (`lib/callouts.ts`): the words, the switch, and the caller. */
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { LapView } from '@gridfpv/types';
import {
  Caller,
  browserSpeaker,
  lapCounts,
  newLapLines,
  readCalloutsOn,
  saveCalloutsOn,
  spokenLap
} from '../src/lib/callouts.js';
import type { SeatRow } from '../src/lib/run.js';

const S = 1_000_000;

/** Laps of the given lengths in seconds, one after the other. */
const laps = (...seconds: number[]): LapView[] => {
  let at = 0;
  return seconds.map((length, index) => {
    const lap: LapView = {
      crossing: index + 1,
      started_at: at,
      ended_at: at + length * S,
      micros: length * S,
      restart: false,
      timer_restarted: false
    };
    at += length * S;
    return lap;
  });
};
/** A seat row. The seat's handle is made to look like one, so a leak of it is easy to spot. */
const row = (seat: number, who: string, ...seconds: number[]): SeatRow => ({
  adapter: 'gate-x1',
  seat: `seat-ref-${seat}`,
  seatName: `Seat ${seat + 1}`,
  bench: false,
  who,
  laps: laps(...seconds),
  onCourse: seconds.length > 0,
  falseStart: false
});

describe('spokenLap', () => {
  it('says a lap under a minute as seconds to two places', () => {
    expect(spokenLap(6_770_000)).toBe('6.77');
    expect(spokenLap(31_200_000)).toBe('31.20');
    expect(spokenLap(6_774_999)).toBe('6.77');
    expect(spokenLap(500_000)).toBe('0.50');
  });

  it('says a lap of a minute or more as minutes, then seconds', () => {
    expect(spokenLap(64_200_000)).toBe('1 minute 4.20');
    expect(spokenLap(60_000_000)).toBe('1 minute 0.00');
    expect(spokenLap(125_000_000)).toBe('2 minutes 5.00');
    expect(spokenLap(600_500_000)).toBe('10 minutes 0.50');
  });

  it('carries a lap that rounds up to a whole minute into the minutes', () => {
    // Never "60.00", and never "1 minute 60.00".
    expect(spokenLap(59_996_000)).toBe('1 minute 0.00');
    expect(spokenLap(119_996_000)).toBe('2 minutes 0.00');
    expect(spokenLap(59_994_000)).toBe('59.99');
  });
});

describe('newLapLines', () => {
  it('says just the time when the heat has one seat', () => {
    const before = lapCounts([row(0, 'Ana', 7.1)]);
    expect(newLapLines(before, [row(0, 'Ana', 7.1, 6.77)])).toEqual(['6.77']);
  });

  it('names the pilot and the lap when the heat has more than one seat', () => {
    const before = lapCounts([row(0, 'Ana', 30, 31), row(1, 'Bee', 33)]);
    const after = [row(0, 'Ana', 30, 31, 31.2), row(1, 'Bee', 33)];
    expect(newLapLines(before, after)).toEqual(['Ana, lap 3, 31.20']);
  });

  it('says a pilot the way their phonetic spelling says, where they have one', () => {
    const before = lapCounts([row(0, 'xQc', 30), row(1, 'Bee', 33)]);
    const after = [row(0, 'xQc', 30, 31.2), row(1, 'Bee', 33, 34)];
    const spoken = (seat: SeatRow) => (seat.who === 'xQc' ? 'ex cue see' : '  ');
    // A blank spelling is no spelling: the name on screen is said.
    expect(newLapLines(before, after, spoken)).toEqual([
      'ex cue see, lap 2, 31.20',
      'Bee, lap 2, 34.00'
    ]);
  });

  it('gives one line for each seat that gained a lap, in seat order', () => {
    const before = lapCounts([row(0, 'Ana', 30), row(1, 'Bee'), row(2, 'Cy', 29)]);
    const after = [row(0, 'Ana', 30, 31), row(1, 'Bee', 35.5), row(2, 'Cy', 29)];
    expect(newLapLines(before, after)).toEqual(['Ana, lap 2, 31.00', 'Bee, lap 1, 35.50']);
  });

  it('reads out only the latest lap of a seat that gained two at once', () => {
    const before = lapCounts([row(0, 'Ana', 30), row(1, 'Bee')]);
    const after = [row(0, 'Ana', 30, 31, 32.5), row(1, 'Bee')];
    expect(newLapLines(before, after)).toEqual(['Ana, lap 3, 32.50']);
  });

  it('says nothing when nothing changed, and nothing for a seat that lost a lap', () => {
    const rows = [row(0, 'Ana', 30, 31), row(1, 'Bee', 33)];
    expect(newLapLines(lapCounts(rows), rows)).toEqual([]);
    expect(newLapLines(lapCounts(rows), [row(0, 'Ana', 30), row(1, 'Bee', 33)])).toEqual([]);
    expect(newLapLines(lapCounts(rows), [])).toEqual([]);
  });

  it('counts a seat it has not seen before as having had no laps', () => {
    expect(newLapLines(new Map(), [row(0, 'Ana', 30), row(1, 'Bee')])).toEqual([
      'Ana, lap 1, 30.00'
    ]);
  });

  it("tells two timers' seats with the same handle apart", () => {
    const other: SeatRow = { ...row(0, 'Bee'), adapter: 'gate-y2' };
    const before = lapCounts([row(0, 'Ana', 30), other]);
    const after = [row(0, 'Ana', 30), { ...other, laps: laps(28) }];
    expect(newLapLines(before, after)).toEqual(['Bee, lap 1, 28.00']);
  });

  it('never speaks a seat or timer handle', () => {
    const lines = newLapLines(new Map(), [row(0, 'Ana', 30), row(1, 'Bee', 31)]);
    for (const line of lines) {
      expect(line).not.toContain('seat-ref');
      expect(line).not.toContain('gate-x1');
    }
  });
});

describe('the callouts switch', () => {
  const memory = () => {
    const held = new Map<string, string>();
    return {
      held,
      getItem: (key: string) => held.get(key) ?? null,
      setItem: (key: string, value: string) => void held.set(key, value)
    };
  };

  it('is on until switched off', () => {
    expect(readCalloutsOn(memory())).toBe(true);
    expect(readCalloutsOn(null)).toBe(true);
    expect(readCalloutsOn(undefined)).toBe(true);
  });

  it('remembers off and on again', () => {
    const storage = memory();
    saveCalloutsOn(storage, false);
    expect(readCalloutsOn(storage)).toBe(false);
    saveCalloutsOn(storage, true);
    expect(readCalloutsOn(storage)).toBe(true);
    expect([...storage.held.keys()]).toEqual(['gridfpv.lap_callouts']);
  });

  it('stays on, and does not throw, when the storage cannot be used', () => {
    const broken = {
      getItem: () => {
        throw new Error('blocked');
      },
      setItem: () => {
        throw new Error('blocked');
      }
    };
    expect(readCalloutsOn(broken)).toBe(true);
    expect(() => saveCalloutsOn(broken, false)).not.toThrow();
    expect(() => saveCalloutsOn(null, false)).not.toThrow();
  });
});

describe('Caller', () => {
  const speaker = () => {
    const said: string[] = [];
    return { said, speak: (line: string) => void said.push(line), cancel: vi.fn() };
  };

  it('does not recite the laps a race already had when it first sees it', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.update('q-1#1', [row(0, 'Ana', 30, 31), row(1, 'Bee', 33)], true);
    expect(voice.said).toEqual([]);
  });

  it('speaks each new lap after that, once', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.update('q-1#1', [row(0, 'Ana'), row(1, 'Bee')], true);
    caller.update('q-1#1', [row(0, 'Ana', 30), row(1, 'Bee')], true);
    caller.update('q-1#1', [row(0, 'Ana', 30), row(1, 'Bee')], true);
    caller.update('q-1#1', [row(0, 'Ana', 30), row(1, 'Bee', 32.25)], true);
    expect(voice.said).toEqual(['Ana, lap 1, 30.00', 'Bee, lap 1, 32.25']);
  });

  it('says nothing while switched off, and starts with the next lap when switched on', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.update('q-1#1', [row(0, 'Ana')], false);
    caller.update('q-1#1', [row(0, 'Ana', 30)], false);
    caller.update('q-1#1', [row(0, 'Ana', 30, 31)], false);
    expect(voice.said).toEqual([]);
    // Switched on with nothing new: the laps flown while it was off are not news.
    caller.update('q-1#1', [row(0, 'Ana', 30, 31)], true);
    expect(voice.said).toEqual([]);
    caller.update('q-1#1', [row(0, 'Ana', 30, 31, 29.5)], true);
    expect(voice.said).toEqual(['29.50']);
  });

  it('starts over on another race without reciting it', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.update('q-1#1', [row(0, 'Ana', 30)], true);
    caller.update('q-1#2', [row(0, 'Ana', 28, 29)], true);
    expect(voice.said).toEqual([]);
    // Fewer laps than the race before had, and it is still a new lap of this race.
    caller.update('q-1#2', [row(0, 'Ana', 28, 29, 27)], true);
    expect(voice.said).toEqual(['27.00']);
  });

  it('forgets a race once none is running, so coming back to it recites nothing', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.update('q-1#1', [row(0, 'Ana', 30)], true);
    caller.update(undefined, [], true);
    caller.update('q-1#1', [row(0, 'Ana', 30, 31)], true);
    expect(voice.said).toEqual([]);
    caller.update('q-1#1', [row(0, 'Ana', 30, 31, 32)], true);
    expect(voice.said).toEqual(['32.00']);
  });

  it('says nothing when a ruling takes a lap away, and speaks the lap that comes after', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.update('q-1#1', [row(0, 'Ana', 30, 2)], true);
    caller.update('q-1#1', [row(0, 'Ana', 30)], true);
    expect(voice.said).toEqual([]);
    caller.update('q-1#1', [row(0, 'Ana', 30, 31)], true);
    expect(voice.said).toEqual(['31.00']);
  });

  it('stops talking when the page goes away', () => {
    const voice = speaker();
    const caller = new Caller(voice);
    caller.dispose();
    expect(voice.cancel).toHaveBeenCalledTimes(1);
  });

  it('is silent, and does not throw, in a browser with no speech engine', () => {
    const caller = new Caller(undefined);
    expect(() => {
      caller.update('q-1#1', [row(0, 'Ana')], true);
      caller.update('q-1#1', [row(0, 'Ana', 30)], true);
      caller.dispose();
    }).not.toThrow();
  });
});

describe('browserSpeaker', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('is nothing where the browser has no speech engine', () => {
    vi.stubGlobal('speechSynthesis', undefined);
    vi.stubGlobal('SpeechSynthesisUtterance', undefined);
    expect(browserSpeaker()).toBeUndefined();
    // Half an engine is no engine.
    vi.stubGlobal('speechSynthesis', { speak: vi.fn(), cancel: vi.fn() });
    expect(browserSpeaker()).toBeUndefined();
  });

  it("speaks a line through the browser's engine, a little quicker than the default", () => {
    class Utterance {
      rate = 1;
      constructor(readonly text: string) {}
    }
    const engine = { speak: vi.fn(), cancel: vi.fn() };
    vi.stubGlobal('speechSynthesis', engine);
    vi.stubGlobal('SpeechSynthesisUtterance', Utterance);

    const voice = browserSpeaker();
    voice?.speak('Ana, lap 3, 31.20');
    expect(engine.speak).toHaveBeenCalledTimes(1);
    const utterance = engine.speak.mock.calls[0][0] as Utterance;
    expect(utterance.text).toBe('Ana, lap 3, 31.20');
    expect(utterance.rate).toBeGreaterThan(1);

    voice?.cancel();
    expect(engine.cancel).toHaveBeenCalledTimes(1);
  });
});
