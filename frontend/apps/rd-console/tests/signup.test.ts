/**
 * The sign-up helpers: which phases have it, what a pilot can do with a seat, and the
 * rules form.
 */
import { describe, expect, it } from 'vitest';
import type { SignupHeatView, SignupPilotView, SignupRules, SignupView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import { seatSources } from '../src/lib/seats.js';
import {
  canHaveSignup,
  hasNextFree,
  heatStateWords,
  pullTarget,
  rulesForm,
  rulesFrom,
  rulesProblem,
  runsLeft,
  runsOf,
  runsWords,
  seatAction,
  seatHeadings,
  seatOf,
  signupPhases
} from '../src/lib/signup.js';
import { eventView, group, heat, timerView } from './competition.js';

const RULES: SignupRules = {
  seats: [
    { adapter: 'gate', seat: '0' },
    { adapter: 'gate', seat: '1' },
    { adapter: 'gate', seat: '2' }
  ],
  open_heats: 3,
  rest_heats: 0,
  open: true
};
const h = (
  number: number,
  seats: (string | null)[],
  state: SignupHeatView['state'] = 'Open'
): SignupHeatView => ({
  heat: `quali/heat-${number}`,
  number,
  seats,
  state,
  full: seats.every((on) => on !== null)
});
const runs = (pilot: string, over: Partial<SignupPilotView> = {}): SignupPilotView => ({
  pilot,
  used: 0,
  held: 0,
  extra: 0,
  ...over
});
const sheet = (over: Partial<SignupView> = {}): SignupView => ({
  rules: RULES,
  heats: [h(1, ['p-ace', null, null]), h(2, [null, null, null]), h(3, [null, null, null])],
  pilots: [runs('p-ace', { held: 1 }), runs('p-bee')],
  ...over
});

describe('which phases have sign-up', () => {
  it('lists them in event order with their sheets, and nothing else', () => {
    const view = eventView({
      phases: [
        group('practice', 'Practice'),
        group('quali', 'Qualifying', undefined, { signup: sheet() }),
        heat('h1', 'Heat 1', []),
        group('mains', 'Mains', undefined, { signup: sheet(), locked: true })
      ]
    });
    expect(signupPhases(view).map((p) => [p.id, p.name, p.locked])).toEqual([
      ['quali', 'Qualifying', false],
      ['mains', 'Mains', true]
    ]);
    expect(signupPhases(undefined)).toEqual([]);
    expect(signupPhases(eventView())).toEqual([]);
  });

  it('can be turned on for a phase that holds heats and is not locked', () => {
    expect(canHaveSignup(group('quali', 'Qualifying'))).toBe(true);
    expect(canHaveSignup(group('quali', 'Qualifying', undefined, { locked: true }))).toBe(false);
    expect(canHaveSignup(heat('h1', 'Heat 1', []))).toBe(false);
  });
});

describe('the seats, named', () => {
  it('gives each its name and, when the timer has said it, its channel', () => {
    const names = buildNames([], [], [timerView('gate', 'Gate', 3)]);
    const headings = seatHeadings(RULES, names, [{ adapter: 'gate', seat: '1', freq_mhz: 5658 }]);
    expect(headings.map((s) => [s.name, s.channel])).toEqual([
      ['Seat 1', undefined],
      ['Seat 2', 'R1 · 5658 MHz'],
      ['Seat 3', undefined]
    ]);
  });
});

describe("a pilot's runs", () => {
  it('finds the pilot on the sheet, or nobody', () => {
    expect(runsOf(sheet(), 'p-ace')?.held).toBe(1);
    expect(runsOf(sheet(), 'p-stranger')).toBeUndefined();
  });

  it('counts what is left under a limit, never below none', () => {
    expect(runsLeft(runs('p', { allowed: 3, used: 1, held: 1 }))).toBe(1);
    expect(runsLeft(runs('p', { allowed: 2, used: 1, held: 1 }))).toBe(0);
    expect(runsLeft(runs('p', { allowed: 1, used: 2, held: 1 }))).toBe(0);
    expect(runsLeft(runs('p', { allowed: 0 }))).toBe(0);
    expect(runsLeft(runs('p'))).toBeUndefined();
    expect(runsLeft(undefined)).toBeUndefined();
  });

  it('says them in a sentence', () => {
    expect(runsWords(runs('p', { allowed: 3, used: 1, held: 2 }))).toBe(
      '1 flown · 2 held · 0 left of 3'
    );
    expect(runsWords(runs('p', { used: 4, held: 1 }))).toBe('4 flown · 1 held');
    expect(runsWords(undefined)).toBe('You are not in this phase.');
  });
});

describe('what a heat says about itself', () => {
  it('says nothing for an open heat with room, and the reason otherwise', () => {
    expect(heatStateWords(h(1, [null, null, null]))).toBeUndefined();
    expect(heatStateWords(h(1, ['a', 'b', 'c']))).toBe('Full');
    expect(heatStateWords(h(1, [null, null, null], 'Closed'))).toBe('Closed');
    expect(heatStateWords(h(1, ['a', null, null], 'Racing'))).toBe('On now');
    expect(heatStateWords(h(1, ['a', null, null], 'Flown'))).toBe('Flown');
  });
});

describe('what a pilot can do with a seat', () => {
  const action = (signup: SignupView, heatNo: number, seat: number, pilot: string) =>
    seatAction(signup, signup.heats[heatNo - 1], seat, pilot);

  it('takes a free seat', () => {
    expect(action(sheet(), 2, 0, 'p-bee')).toEqual({ kind: 'take' });
    expect(action(sheet(), 1, 1, 'p-bee')).toEqual({ kind: 'take' });
  });

  it('leaves their own, and sees a taken one as taken', () => {
    expect(action(sheet(), 1, 0, 'p-ace')).toEqual({ kind: 'leave' });
    expect(action(sheet(), 1, 0, 'p-bee')).toEqual({ kind: 'taken' });
  });

  it('cannot take a second seat in a heat they are in', () => {
    expect(action(sheet(), 1, 1, 'p-ace')).toEqual({
      kind: 'blocked',
      why: 'You are in this heat'
    });
  });

  it('cannot join or leave a heat that is closed, on, or flown', () => {
    for (const [state, why] of [
      ['Closed', 'Closed'],
      ['Racing', 'On now'],
      ['Flown', 'Flown']
    ] as const) {
      const signup = sheet({ heats: [h(1, ['p-ace', null, null], state)] });
      expect(action(signup, 1, 1, 'p-bee'), state).toEqual({ kind: 'blocked', why });
      // Their own seat is shown as theirs, with nothing to press.
      expect(action(signup, 1, 0, 'p-ace'), state).toEqual({ kind: 'mine' });
    }
  });

  it('cannot take a seat while sign-up is closed, but can still leave', () => {
    const closed = sheet({ rules: { ...RULES, open: false } });
    expect(action(closed, 2, 0, 'p-bee')).toEqual({ kind: 'blocked', why: 'Sign-up is closed' });
    expect(action(closed, 1, 0, 'p-ace')).toEqual({ kind: 'leave' });
  });

  it('a pilot who is not in the phase cannot take a seat', () => {
    expect(action(sheet(), 2, 0, 'p-stranger')).toEqual({
      kind: 'blocked',
      why: 'You are not in this phase'
    });
  });

  it('is stopped at the most spots held at once', () => {
    const signup = sheet({
      rules: { ...RULES, held_limit: 1 },
      pilots: [runs('p-ace', { held: 1 }), runs('p-bee')]
    });
    expect(action(signup, 2, 0, 'p-ace')).toEqual({
      kind: 'blocked',
      why: 'You hold 1, the most at once'
    });
    expect(action(signup, 2, 0, 'p-bee')).toEqual({ kind: 'take' });
  });

  it('is stopped when no runs are left', () => {
    const signup = sheet({
      rules: { ...RULES, run_limit: 2 },
      pilots: [runs('p-ace', { held: 1, used: 1, allowed: 2 }), runs('p-bee', { allowed: 2 })]
    });
    expect(action(signup, 2, 0, 'p-ace')).toEqual({ kind: 'blocked', why: 'No runs left' });
    expect(action(signup, 2, 0, 'p-bee')).toEqual({ kind: 'take' });
  });

  it('finds which seat a pilot is on', () => {
    expect(seatOf(h(1, [null, 'p-ace', null]), 'p-ace')).toBe(1);
    expect(seatOf(h(1, [null, 'p-ace', null]), 'p-bee')).toBe(-1);
    expect(seatOf(h(1, [null, null, null]), undefined)).toBe(-1);
  });
});

describe('next free on a seat', () => {
  it('is on offer when some heat would take the pilot on that seat', () => {
    expect(hasNextFree(sheet(), 0, 'p-bee')).toBe(true);
    // Seat 0 is taken in heat 1 by Ace, who is then offered heats 2 and 3.
    expect(hasNextFree(sheet(), 0, 'p-ace')).toBe(true);
  });

  it('is not on offer when the seat is taken everywhere, or the pilot may not', () => {
    const taken = sheet({
      heats: [h(1, ['a', null, null]), h(2, ['b', null, null]), h(3, ['c', null, null])]
    });
    expect(hasNextFree(taken, 0, 'p-bee')).toBe(false);
    expect(hasNextFree(taken, 1, 'p-bee')).toBe(true);
    expect(hasNextFree(sheet({ rules: { ...RULES, open: false } }), 0, 'p-bee')).toBe(false);
    expect(hasNextFree(sheet(), 0, 'p-stranger')).toBe(false);
  });
});

describe('pulling a pilot forward', () => {
  it('finds the earliest earlier heat with the same seat free', () => {
    const signup = sheet({
      heats: [h(1, ['p-ace', null, null]), h(2, [null, null, null]), h(3, [null, 'p-bee', null])]
    });
    expect(pullTarget(signup, signup.heats[2], 1)?.number).toBe(1);
  });

  it('skips a heat where the seat is taken, the pilot already is, or it has flown', () => {
    const signup = sheet({
      heats: [
        h(1, [null, 'p-cat', null]),
        h(2, ['p-bee', null, null]),
        h(3, [null, null, null], 'Flown'),
        h(4, [null, null, null], 'Closed'),
        h(5, [null, 'p-bee', null])
      ]
    });
    // Heat 1: seat taken. Heat 2: Bee is in it. Heat 3: flown. Heat 4: closed, but the
    // director can still change a closed heat.
    expect(pullTarget(signup, signup.heats[4], 1)?.number).toBe(4);
  });

  it('is nothing for an empty seat, the first heat, or a heat that has started', () => {
    const signup = sheet({
      heats: [h(1, ['p-ace', null, null]), h(2, [null, 'p-bee', null], 'Racing')]
    });
    expect(pullTarget(signup, signup.heats[0], 0)).toBeUndefined();
    expect(pullTarget(signup, signup.heats[0], 1)).toBeUndefined();
    expect(pullTarget(signup, signup.heats[1], 1)).toBeUndefined();
  });
});

describe('the rules form', () => {
  const sources = seatSources([timerView('gate', 'Gate', 4)]);

  it('starts a new one from the first timer, all its seats, three heats, no limits, closed', () => {
    expect(rulesForm(undefined, sources)).toEqual({
      source: 'gate',
      seats: ['0', '1', '2', '3'],
      openHeats: '3',
      heldLimit: '',
      runLimit: '',
      rest: '0',
      open: false
    });
  });

  it('falls back to the bench when no timer has seats', () => {
    const none = seatSources([timerView('gate', 'Gate', 0)]);
    expect(rulesForm(undefined, none).source).toBe('bench');
    expect(rulesForm(undefined, none).seats.length).toBeGreaterThan(0);
  });

  it('shows rules that exist as they are', () => {
    const form = rulesForm({ ...RULES, held_limit: 2, run_limit: 5, rest_heats: 1 }, sources);
    expect(form).toEqual({
      source: 'gate',
      seats: ['0', '1', '2'],
      openHeats: '3',
      heldLimit: '2',
      runLimit: '5',
      rest: '1',
      open: true
    });
  });

  it("turns the form back into rules, in the timer's seat order, leaving out empty limits", () => {
    const form = { ...rulesForm(undefined, sources), seats: ['2', '0'], open: true };
    expect(rulesFrom(form, sources)).toEqual({
      seats: [
        { adapter: 'gate', seat: '0' },
        { adapter: 'gate', seat: '2' }
      ],
      open_heats: 3,
      rest_heats: 0,
      open: true
    });
    const limited = { ...form, heldLimit: ' 2 ', runLimit: '5', rest: '1', openHeats: '4' };
    expect(rulesFrom(limited, sources)).toMatchObject({
      held_limit: 2,
      run_limit: 5,
      rest_heats: 1,
      open_heats: 4
    });
  });

  it('round-trips rules through the form', () => {
    const rules: SignupRules = { ...RULES, held_limit: 2, run_limit: 5, rest_heats: 1 };
    expect(rulesFrom(rulesForm(rules, sources), sources)).toEqual(rules);
    expect(rulesFrom(rulesForm(RULES, sources), sources)).toEqual(RULES);
  });

  it('says what is wrong before anything is sent', () => {
    const good = rulesForm(undefined, sources);
    expect(rulesProblem(good)).toBeUndefined();
    expect(rulesProblem({ ...good, seats: [] })).toBe('Pick at least one seat.');
    for (const openHeats of ['', '0', '21', 'x', '2.5', '-1']) {
      expect(rulesProblem({ ...good, openHeats }), openHeats).toBe(
        'Heats open at once: a number from 1 to 20.'
      );
    }
    for (const heldLimit of ['0', 'x', '-1', '1.5']) {
      expect(rulesProblem({ ...good, heldLimit }), heldLimit).toBe(
        'Spots held at once: a number from 1 up, or leave it empty for no limit.'
      );
    }
    expect(rulesProblem({ ...good, runLimit: '0' })).toBe(
      'Runs for each pilot: a number from 1 up, or leave it empty for no limit.'
    );
    for (const rest of ['', '21', 'x']) {
      expect(rulesProblem({ ...good, rest }), rest).toBe(
        'Heats to sit out: a number from 0 to 20.'
      );
    }
    // The edges that are fine.
    expect(
      rulesProblem({ ...good, openHeats: '1', heldLimit: '1', runLimit: '1', rest: '20' })
    ).toBeUndefined();
    expect(
      rulesProblem({ ...good, openHeats: '20', heldLimit: ' ', runLimit: '' })
    ).toBeUndefined();
  });
});
