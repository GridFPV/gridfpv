/**
 * A sign-up phase for the screen tests: three seats on the Gate timer, Ace, Bee and Cat in
 * the phase, three heats open.
 */
import type {
  EventView,
  Pilot,
  SeatChannel,
  SignupHeatView,
  SignupPilotView,
  SignupRules,
  SignupView,
  TimerView
} from '@gridfpv/types';
import { buildNames, type Names } from '../src/lib/names.js';
import type { SignupPhase } from '../src/lib/signup.js';
import { eventView, group, heat, timerView } from './competition.js';

export const GATE: TimerView = timerView('gate', 'Gate', 3);
export const SIGNUP_PILOTS: Pilot[] = [
  { id: 'p-ace', callsign: 'Ace', vtx_types: [] },
  { id: 'p-bee', callsign: 'Bee', vtx_types: [] },
  { id: 'p-cat', callsign: 'Cat', vtx_types: [] }
];

export const SIGNUP_RULES: SignupRules = {
  seats: [
    { adapter: 'gate', seat: '0' },
    { adapter: 'gate', seat: '1' },
    { adapter: 'gate', seat: '2' }
  ],
  open_heats: 3,
  rest_heats: 0,
  open: true
};

export function signupHeat(
  number: number,
  seats: (string | null)[],
  state: SignupHeatView['state'] = 'Open'
): SignupHeatView {
  return {
    heat: `quali/heat-${number}`,
    number,
    seats,
    state,
    full: seats.every((on) => on !== null)
  };
}

export function signupRuns(pilot: string, over: Partial<SignupPilotView> = {}): SignupPilotView {
  return { pilot, used: 0, held: 0, extra: 0, ...over };
}

/** Ace is on seat 1 of heat 1. Everything else is free. */
export function signupSheet(over: Partial<SignupView> = {}): SignupView {
  return {
    rules: SIGNUP_RULES,
    heats: [
      signupHeat(1, ['p-ace', null, null]),
      signupHeat(2, [null, null, null]),
      signupHeat(3, [null, null, null])
    ],
    pilots: [signupRuns('p-ace', { held: 1 }), signupRuns('p-bee'), signupRuns('p-cat')],
    ...over
  };
}

/** An event whose Qualifying phase has that sheet, with its heats as plain phases. */
export function signupEvent(sheet: SignupView = signupSheet(), over: Partial<EventView> = {}) {
  return eventView({
    phases: [
      group('quali', 'Qualifying', undefined, { signup: sheet }),
      ...sheet.heats.map((entry) => ({
        ...heat(entry.heat, `Qualifying Heat ${entry.number}`, [], { parent: 'quali' }),
        lineup: entry.seats.flatMap((on, index) =>
          on === null ? [] : [{ adapter: 'gate', seat: String(index), who: { Pilot: on } as const }]
        )
      }))
    ],
    ...over
  });
}

export function signupNames(view: EventView, channels: SeatChannel[] = []): Names {
  return buildNames(view.phases, SIGNUP_PILOTS, [GATE], [], channels);
}

export function signupPhase(sheet: SignupView = signupSheet(), locked = false): SignupPhase {
  return { id: 'quali', name: 'Qualifying', locked, signup: sheet };
}
