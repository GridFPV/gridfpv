/**
 * Sending the Director's events to a club on a server, for the specs (docs/primitives.md
 * §13): the Director keeps a list of clubs, and each event goes to one or to none.
 */
import type { Page } from '@playwright/test';

import { expect } from './observability.js';

async function send<T>(base: string, method: string, path: string, body?: unknown): Promise<T> {
  const answer = await fetch(`${base}${path}`, {
    method,
    headers: { 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body)
  });
  if (!answer.ok) {
    throw new Error(`${method} ${path} answered ${answer.status}: ${await answer.text()}`);
  }
  return (await answer.json()) as T;
}

interface Link {
  clubs: { id: string }[];
}

/**
 * Forget every saved club, so the Director sends nothing. The Director is shared by every
 * spec in the worker: each spec leaves it this way.
 */
export async function unlink(directorUrl: string): Promise<void> {
  const link = await send<Link>(directorUrl, 'GET', '/server-link');
  for (const club of link.clubs) {
    await send(directorUrl, 'DELETE', `/server-link/clubs/${club.id}`);
  }
}

/**
 * The director opens an event. The Director connects only to the club of the open event,
 * so a spec that sends an event opens it first.
 */
export async function openEvent(directorUrl: string, event: string): Promise<void> {
  await send(directorUrl, 'PUT', '/active-event', { id: event });
}

/** Save a club on the Director, over its API. Returns the saved club's id. */
export async function saveClub(directorUrl: string, url: string, token: string): Promise<string> {
  const link = await send<Link>(directorUrl, 'POST', '/server-link/clubs', { url, token });
  return link.clubs[link.clubs.length - 1].id;
}

/** Say over the Director's API which saved club an event goes to, or `null` for none. */
export async function sendEventTo(
  directorUrl: string,
  event: string,
  club: string | null
): Promise<void> {
  await send(directorUrl, 'PUT', `/server-link/events/${event}/club`, { club });
}

/**
 * Save a club and send an event to it, over the Director's API. The Director connects
 * once that event is the open one.
 */
export async function linkTo(
  directorUrl: string,
  url: string,
  token: string,
  event: string
): Promise<string> {
  const club = await saveClub(directorUrl, url, token);
  await sendEventTo(directorUrl, event, club);
  return club;
}

/** The box on the Server page that says where an event is sent. */
export function whereSent(page: Page, eventName: string) {
  return page.getByRole('combobox', { name: `Where ${eventName} is sent` });
}

/** On the Server page, add a club the way a director does. The form closes when it is saved. */
export async function addClubOnPage(page: Page, url: string, token: string): Promise<void> {
  const before = await page.getByTestId('link-club').count();
  await page.getByRole('button', { name: 'Add a club' }).click();
  await page.getByLabel('Address').fill(url);
  await page.getByLabel('Link token').fill(token);
  await page.getByRole('button', { name: 'Save' }).click();
  await expect(page.getByTestId('link-club')).toHaveCount(before + 1);
  await expect(page.getByTestId('club-form')).toHaveCount(0);
}

/**
 * On the Server page, add a club and send an event to it. The club just added is the
 * last one in the event's box.
 */
export async function linkOnPage(
  page: Page,
  url: string,
  token: string,
  eventName: string
): Promise<void> {
  await addClubOnPage(page, url, token);
  const box = whereSent(page, eventName);
  const last = (await box.locator('option').count()) - 1;
  await box.selectOption({ index: last });
}
