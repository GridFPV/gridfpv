/**
 * What every console page sits under: the switch between Setup and Event, the row of pages
 * of the side that is showing, the count of races that need a marshal, and the open event.
 */
import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import AppNav from '../src/AppNav.svelte';
import type { Route } from '../src/lib/route.js';

const pages = () => screen.getByRole('navigation', { name: 'Pages' });
const sides = () => screen.getByRole('navigation', { name: 'Setup or event' });
const entry = (name: string | RegExp) => within(pages()).getByRole('link', { name });
const side = (name: string | RegExp) => within(sides()).getByRole('link', { name });
const rowOf = () =>
  within(pages())
    .getAllByRole('link')
    .map((link) => [link.textContent?.trim(), link.getAttribute('href')]);
const currentEntries = () =>
  within(pages())
    .getAllByRole('link')
    .filter((link) => link.getAttribute('aria-current') === 'page')
    .map((link) => link.textContent?.trim());
const currentSide = () =>
  within(sides())
    .getAllByRole('link')
    .filter((link) => link.getAttribute('aria-current') === 'true')
    .map((link) => link.querySelector('.side-name')?.textContent?.trim());

const EVENT_ROW = [
  ['Details', '#/event'],
  ['Phases', '#/event/phases'],
  ['Run', '#/event/run'],
  ['Marshal', '#/event/marshal'],
  ['Results', '#/event/results'],
  ['Overlays', '#/event/overlays'],
  ['Help', '#/help']
];
const SETUP_ROW = [
  ['Events', '#/events'],
  ['Timers', '#/timers'],
  ['Seats', '#/seats'],
  ['Pilots', '#/pilots'],
  ['Classes', '#/classes'],
  ['Formats', '#/formats'],
  ['Help', '#/help']
];

describe('AppNav: the two sides', () => {
  it("on an event page the row is the event's pages, in race-day order", () => {
    render(AppNav, { route: { kind: 'event', section: 'event' }, eventName: 'Club Night' });
    expect(rowOf()).toEqual(EVENT_ROW);
    expect(currentSide()).toEqual(['Event']);
  });

  it('on a setup page the row is what is kept for every event', () => {
    render(AppNav, { route: { kind: 'page', page: 'pilots' }, eventName: 'Club Night' });
    expect(rowOf()).toEqual(SETUP_ROW);
    expect(currentSide()).toEqual(['Setup']);
  });

  it.each<[string, Route, 'Setup' | 'Event', string]>([
    ['the event page', { kind: 'event', section: 'event' }, 'Event', 'Details'],
    ['the Phases page', { kind: 'event', section: 'phases' }, 'Event', 'Phases'],
    ['the Results page', { kind: 'event', section: 'results' }, 'Event', 'Results'],
    ['the Overlays page', { kind: 'event', section: 'overlays' }, 'Event', 'Overlays'],
    ['the Run page', { kind: 'run' }, 'Event', 'Run'],
    ['the Run page of one heat', { kind: 'run', heat: 'q-1' }, 'Event', 'Run'],
    ['the Marshal page', { kind: 'marshal' }, 'Event', 'Marshal'],
    ['one race in Marshal', { kind: 'marshal', race: 'q-1#1' }, 'Event', 'Marshal'],
    ['the Events page', { kind: 'page', page: 'events' }, 'Setup', 'Events'],
    ['the Timers page', { kind: 'page', page: 'timers' }, 'Setup', 'Timers'],
    ['the Seats page', { kind: 'seats' }, 'Setup', 'Seats'],
    ["one timer's seats", { kind: 'seats', timer: 'gate-x1y2' }, 'Setup', 'Seats'],
    ["a seat's Tune page", { kind: 'capture', timer: 'gate-x1y2', seat: '0' }, 'Setup', 'Seats'],
    ['the Pilots page', { kind: 'page', page: 'pilots' }, 'Setup', 'Pilots'],
    ['the Classes page', { kind: 'page', page: 'classes' }, 'Setup', 'Classes'],
    ['the Formats page', { kind: 'page', page: 'formats' }, 'Setup', 'Formats']
  ])('%s is on the %s side, under %s', (_where, route, sideName, label) => {
    render(AppNav, { route, eventName: 'Club Night' });
    expect(currentSide()).toEqual([sideName]);
    expect(currentEntries()).toEqual([label]);
    for (const link of within(pages()).getAllByRole('link')) {
      expect(link.classList.contains('active')).toBe(link.textContent?.trim() === label);
    }
  });

  it('help is on both rows and leaves the side as it was', async () => {
    const view = render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night' });
    await view.rerender({ route: { kind: 'help', slug: 'run' }, eventName: 'Club Night' });
    expect(rowOf()).toEqual(EVENT_ROW);
    expect(currentSide()).toEqual(['Event']);
    expect(currentEntries()).toEqual(['Help']);
    await view.rerender({ route: { kind: 'page', page: 'formats' }, eventName: 'Club Night' });
    await view.rerender({ route: { kind: 'help' }, eventName: 'Club Night' });
    expect(rowOf()).toEqual(SETUP_ROW);
    expect(currentSide()).toEqual(['Setup']);
  });

  it('loaded straight onto help, the side is the event when one is open, else setup', () => {
    const open = render(AppNav, { route: { kind: 'help' }, eventName: 'Club Night' });
    expect(currentSide()).toEqual(['Event']);
    open.unmount();
    render(AppNav, { route: { kind: 'help' } });
    expect(currentSide()).toEqual(['Setup']);
  });
});

describe('AppNav: the switch', () => {
  it('each side goes back to the page it was last on', async () => {
    const view = render(AppNav, { route: { kind: 'run', heat: 'q-1' }, eventName: 'Club Night' });
    // Nothing of Setup has been shown yet: it starts on the Events page.
    expect(side(/^Setup/)).toHaveAttribute('href', '#/events');
    await view.rerender({ route: { kind: 'page', page: 'formats' }, eventName: 'Club Night' });
    // Back to the event lands on the heat that was on the Run page.
    expect(side(/^Event/)).toHaveAttribute('href', '#/event/run/q-1');
    await view.rerender({ route: { kind: 'marshal' }, eventName: 'Club Night' });
    expect(side(/^Setup/)).toHaveAttribute('href', '#/formats');
    expect(side(/^Event/)).toHaveAttribute('href', '#/event/marshal');
  });

  it('the switch reads Setup and Event and nothing else', () => {
    render(AppNav, { route: { kind: 'page', page: 'pilots' }, eventName: 'Club Night' });
    expect(side(/^Setup/)).toHaveTextContent(/^Setup$/);
    expect(side(/^Event/)).toHaveTextContent(/^Event$/);
  });

  it("the open event's name is at the top right, and leads to the Events page", () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night' });
    const name = screen.getByTestId('open-event');
    expect(name).toHaveTextContent(/^Club Night$/);
    expect(name).toHaveAttribute('href', '#/events');
    // It is not part of the switch or the row of pages.
    expect(sides()).not.toContainElement(name);
    expect(pages()).not.toContainElement(name);
  });

  it('the Event side carries the open event by name', () => {
    render(AppNav, { route: { kind: 'page', page: 'pilots' }, eventName: 'Club Night' });
    const link = side('Event: Club Night');
    expect(link).toHaveAttribute('href', '#/event');
    expect(screen.getByTestId('open-event')).toHaveTextContent('Club Night');
    expect(link.classList.contains('dim')).toBe(false);
  });

  it('with no event open the Event side is dimmed and leads to the Events page', async () => {
    const view = render(AppNav, { route: { kind: 'page', page: 'pilots' } });
    const link = side('Event: none open');
    expect(link).toHaveAttribute('href', '#/events');
    expect(link.classList.contains('dim')).toBe(true);
    expect(link).toHaveAttribute('title', 'No event is open. Open or add one.');
    expect(screen.getByTestId('open-event')).toHaveTextContent(/^No event open$/);
    // Even when an event page was shown before the event went away.
    await view.rerender({ route: { kind: 'run' }, eventName: 'Club Night' });
    await view.rerender({ route: { kind: 'page', page: 'events' }, eventName: undefined });
    expect(side('Event: none open')).toHaveAttribute('href', '#/events');
  });
});

describe('AppNav: races that need a marshal', () => {
  it('shows no count when nothing needs a look', () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night', toMarshal: 0 });
    expect(screen.queryByTestId('marshal-count')).toBeNull();
    expect(entry('Marshal')).toBeVisible();
  });

  it('on the event side the count sits on Marshal, and says so in words', () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night', toMarshal: 2 });
    const link = entry('Marshal, 2 races need a look');
    expect(link).toHaveAttribute('href', '#/event/marshal');
    // Once only: not on the switch as well.
    expect(screen.getAllByTestId('marshal-count')).toHaveLength(1);
    expect(link).toContainElement(screen.getByTestId('marshal-count'));
    expect(screen.getByTestId('marshal-count')).toHaveTextContent('2');
  });

  it('says "race needs" for one', () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night', toMarshal: 1 });
    expect(entry('Marshal, 1 race needs a look')).toBeVisible();
  });

  it('on the setup side the count moves to the Event side of the switch, so it is still seen', () => {
    render(AppNav, {
      route: { kind: 'page', page: 'pilots' },
      eventName: 'Club Night',
      toMarshal: 2
    });
    const link = side('Event: Club Night, 2 races need a look');
    expect(screen.getAllByTestId('marshal-count')).toHaveLength(1);
    expect(link).toContainElement(screen.getByTestId('marshal-count'));
  });
});

describe('AppNav brand', () => {
  const brand = () => screen.getByRole('button', { name: /GridFPV/i });

  it('goes to the open event', async () => {
    location.hash = '#/pilots';
    render(AppNav, { route: { kind: 'page', page: 'pilots' }, eventName: 'Club Night' });
    brand().click();
    expect(location.hash).toBe('#/event');
  });

  it('goes to the Events page when no event is open', async () => {
    location.hash = '#/pilots';
    render(AppNav, { route: { kind: 'page', page: 'pilots' } });
    brand().click();
    expect(location.hash).toBe('#/events');
  });
});
