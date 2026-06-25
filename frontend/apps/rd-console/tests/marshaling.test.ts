import { describe, expect, it } from 'vitest';
import {
  adjustLapCommand,
  applyPenaltyCommand,
  deductPointsCommand,
  disqualifyPenalty,
  DISQUALIFY,
  fileProtestCommand,
  insertLapCommand,
  pointsDeductedPenalty,
  resolveProtestCommand,
  reverseRulingCommand,
  secondsToSourceTime,
  splitLapCommand,
  throwOutLapCommand,
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

  it('applyPenaltyCommand carries a reason-less Disqualify penalty', () => {
    expect(applyPenaltyCommand('heat-1', 'BOB', DISQUALIFY)).toEqual({
      ApplyPenalty: { heat: 'heat-1', competitor: 'BOB', penalty: { Disqualify: {} } }
    });
  });

  it('disqualifyPenalty carries an optional reason', () => {
    expect(disqualifyPenalty()).toEqual({ Disqualify: {} });
    expect(disqualifyPenalty('  cut the course ')).toEqual({
      Disqualify: { reason: 'cut the course' }
    });
  });

  it('pointsDeductedPenalty builds a standings points penalty', () => {
    expect(pointsDeductedPenalty(5)).toEqual({ PointsDeducted: { points: 5 } });
    // Clamps to a non-negative integer.
    expect(pointsDeductedPenalty(-2.6)).toEqual({ PointsDeducted: { points: 0 } });
  });

  it('deductPointsCommand is sugar over a points penalty', () => {
    expect(deductPointsCommand('heat-1', 'BOB', 3)).toEqual({
      DeductPoints: { heat: 'heat-1', competitor: 'BOB', points: 3 }
    });
  });

  it('throwOutLapCommand targets the lap end-pass offset', () => {
    expect(throwOutLapCommand(11)).toEqual({ ThrowOutLap: { target: 11 } });
  });

  it('fileProtestCommand and resolveProtestCommand build the protest pair', () => {
    expect(fileProtestCommand('heat-1', 'BOB', 'contact')).toEqual({
      FileProtest: { heat: 'heat-1', competitor: 'BOB', note: 'contact' }
    });
    expect(resolveProtestCommand(12, 'Upheld')).toEqual({
      ResolveProtest: { target: 12, outcome: 'Upheld' }
    });
  });

  it('converts whole seconds to microsecond SourceTime', () => {
    expect(secondsToSourceTime(1.5)).toBe(1_500_000);
  });
});
