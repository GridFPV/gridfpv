import { describe, expect, it } from 'vitest';
import {
  adjustLapCommand,
  applyPenaltyCommand,
  DISQUALIFY,
  insertLapCommand,
  reverseRulingCommand,
  secondsToSourceTime,
  splitLapCommand,
  timeAddedPenalty,
  voidDetectionCommand,
  voidHeatCommand
} from '../src/lib/marshaling.js';

describe('marshaling command builders', () => {
  it('voidDetectionCommand targets a log offset', () => {
    expect(voidDetectionCommand(7)).toEqual({ VoidDetection: { target: 7 } });
  });

  it('insertLapCommand carries adapter, competitor, and source time', () => {
    expect(insertLapCommand('rh-1', 'ALICE', 1_000_000)).toEqual({
      InsertLap: { adapter: 'rh-1', competitor: 'ALICE', at: 1_000_000 }
    });
  });

  it('adjustLapCommand re-times a logged pass', () => {
    expect(adjustLapCommand(3, 2_000_000)).toEqual({ AdjustLap: { target: 3, at: 2_000_000 } });
  });

  it('splitLapCommand splits the lap ending at a logged pass', () => {
    expect(splitLapCommand(5, 4_000_000)).toEqual({ SplitLap: { target: 5, at: 4_000_000 } });
  });

  it('reverseRulingCommand reverses a prior ruling by its offset', () => {
    expect(reverseRulingCommand(9)).toEqual({ ReverseRuling: { target: 9 } });
  });

  it('voidHeatCommand voids the whole heat', () => {
    expect(voidHeatCommand('heat-1')).toEqual({ VoidHeat: { heat: 'heat-1' } });
  });

  it('applyPenaltyCommand carries a TimeAdded penalty in micros', () => {
    expect(applyPenaltyCommand('heat-1', 'BOB', timeAddedPenalty(2))).toEqual({
      ApplyPenalty: {
        heat: 'heat-1',
        competitor: 'BOB',
        penalty: { TimeAdded: { micros: 2_000_000 } }
      }
    });
  });

  it('applyPenaltyCommand carries a Disqualify penalty', () => {
    expect(applyPenaltyCommand('heat-1', 'BOB', DISQUALIFY)).toEqual({
      ApplyPenalty: { heat: 'heat-1', competitor: 'BOB', penalty: 'Disqualify' }
    });
  });

  it('converts whole seconds to microsecond SourceTime', () => {
    expect(secondsToSourceTime(1.5)).toBe(1_500_000);
  });
});
