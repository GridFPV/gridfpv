/**
 * The nav row every console page sits under: which entry is the current one, the entries
 * that need an open event, the count of races that need a marshal, and the open event.
 */
import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import AppNav from '../src/AppNav.svelte';
import type { Route } from '../src/lib/route.js';

const pages = () => screen.getByRole('navigation', { name: 'Pages' });
const entry = (name: string | RegExp) => within(pages()).getByRole('link', { name });
const currentEntries = () =>
  within(pages())
    .getAllByRole('link')
    .filter((link) => link.getAttribute('aria-current') === 'page')
    .map((link) => link.textContent?.trim());

describe('AppNav', () => {
  it('lists the pages in race-day order, each a plain link to its own URL', () => {
    render(AppNav, { route: { kind: 'event', section: 'event' }, eventName: 'Club Night' });
    const links = within(pages()).getAllByRole('link');
    expect(links.map((link) => [link.textContent?.trim(), link.getAttribute('href')])).toEqual([
      ['Event', '#/event'],
      ['Seats', '#/seats'],
      ['Pilots', '#/pilots'],
      ['Phases', '#/event/phases'],
      ['Formats', '#/formats'],
      ['Run', '#/event/run'],
      ['Marshal', '#/event/marshal'],
      ['Results', '#/event/results'],
      ['Overlays', '#/event/overlays'],
      ['Help', '#/help']
    ]);
  });

  it.each<[string, Route, string | undefined]>([
    ['the event page', { kind: 'event', section: 'event' }, 'Event'],
    ['the Phases page', { kind: 'event', section: 'phases' }, 'Phases'],
    ['the Results page', { kind: 'event', section: 'results' }, 'Results'],
    ['the Overlays page', { kind: 'event', section: 'overlays' }, 'Overlays'],
    ['the Run page', { kind: 'run' }, 'Run'],
    ['the Run page of one heat', { kind: 'run', heat: 'q-1' }, 'Run'],
    ['the Marshal page', { kind: 'marshal' }, 'Marshal'],
    ['one race in Marshal', { kind: 'marshal', race: 'q-1#1' }, 'Marshal'],
    ['the Seats page', { kind: 'seats' }, 'Seats'],
    ["one timer's seats", { kind: 'seats', timer: 'gate-x1y2' }, 'Seats'],
    ["a seat's Tune page", { kind: 'capture', timer: 'gate-x1y2', seat: '0' }, 'Seats'],
    ['the Timers page, which sits behind Seats', { kind: 'page', page: 'timers' }, 'Seats'],
    ['the Pilots page', { kind: 'page', page: 'pilots' }, 'Pilots'],
    ['the Classes page, which sits beside Pilots', { kind: 'page', page: 'classes' }, 'Pilots'],
    ['the Formats page', { kind: 'page', page: 'formats' }, 'Formats'],
    ['the help index', { kind: 'help' }, 'Help'],
    ['one help page', { kind: 'help', slug: 'run' }, 'Help'],
    ['the Events page, which belongs under no entry', { kind: 'page', page: 'events' }, undefined]
  ])('on %s the current entry is %s', (_where, route, label) => {
    render(AppNav, { route, eventName: 'Club Night' });
    expect(currentEntries()).toEqual(label === undefined ? [] : [label]);
    for (const link of within(pages()).getAllByRole('link')) {
      expect(link.classList.contains('active')).toBe(link.textContent?.trim() === label);
    }
  });

  it('dims the pages that need an event while none is open, and no others', () => {
    render(AppNav, { route: { kind: 'page', page: 'pilots' } });
    const dimmed = within(pages())
      .getAllByRole('link')
      .filter((link) => link.classList.contains('dim'))
      .map((link) => link.textContent?.trim());
    expect(dimmed).toEqual(['Event', 'Phases', 'Run', 'Marshal', 'Results', 'Overlays']);
    // Dimmed, but still a link: following one lands on the Events page.
    expect(entry('Run')).toHaveAttribute('href', '#/event/run');
  });

  it('dims nothing once an event is open', () => {
    render(AppNav, { route: { kind: 'page', page: 'pilots' }, eventName: 'Club Night' });
    expect(pages().querySelectorAll('.dim')).toHaveLength(0);
  });

  it('shows no count on Marshal when nothing needs a look', () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night', toMarshal: 0 });
    expect(screen.queryByTestId('marshal-count')).toBeNull();
    expect(entry('Marshal')).toBeVisible();
  });

  it('counts the races that need a look on Marshal, and says so in words', () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night', toMarshal: 2 });
    expect(screen.getByTestId('marshal-count')).toHaveTextContent('2');
    const link = entry('Marshal, 2 races need a look');
    expect(link).toHaveAttribute('href', '#/event/marshal');
    // The count sits on the Marshal entry and nowhere else.
    expect(link).toContainElement(screen.getByTestId('marshal-count'));
  });

  it('says "race needs" for one', () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night', toMarshal: 1 });
    expect(entry('Marshal, 1 race needs a look')).toBeVisible();
  });

  it("shows the open event's name as a link to the Events page", () => {
    render(AppNav, { route: { kind: 'run' }, eventName: 'Club Night' });
    const link = screen.getByRole('link', { name: /Club Night/ });
    expect(link).toHaveAttribute('href', '#/events');
    expect(link.classList.contains('active')).toBe(false);
  });

  it('says none is open when there is no event, and marks that link on the Events page', () => {
    render(AppNav, { route: { kind: 'page', page: 'events' } });
    const link = screen.getByRole('link', { name: /None open/ });
    expect(link).toHaveAttribute('href', '#/events');
    expect(link.classList.contains('active')).toBe(true);
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
