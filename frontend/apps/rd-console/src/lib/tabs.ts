/**
 * The tab rows of pages that share one nav entry: Pilots with Classes, and Seats with
 * Timers. One list each, so both pages of a pair always show the same row.
 */
import type { Route } from './route.js';

export interface PageTab {
  key: string;
  label: string;
  to: Route;
}

/** The lists of people that outlive events. */
export const PEOPLE_TABS: readonly PageTab[] = [
  { key: 'pilots', label: 'Pilots', to: { kind: 'page', page: 'pilots' } },
  { key: 'classes', label: 'Classes', to: { kind: 'page', page: 'classes' } }
];

/** The timing hardware: the seats pilots fly on, and the timers that have them. */
export const TIMING_TABS: readonly PageTab[] = [
  { key: 'seats', label: 'Seats', to: { kind: 'seats' } },
  { key: 'timers', label: 'Timers', to: { kind: 'page', page: 'timers' } }
];
