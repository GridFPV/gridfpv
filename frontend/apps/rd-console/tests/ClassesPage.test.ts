/**
 * The Classes page: the class names kept for every event. It has no tab row, and it says
 * where a class's roster is set, since that is the other half of what a class is.
 */
import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/svelte';
import ClassesPage from '../src/screens/ClassesPage.svelte';
import { makeTestSession } from './support.js';

describe('ClassesPage', () => {
  it('says the list is only the names and points at the Details page for rosters', () => {
    const { session } = makeTestSession({ noEnter: true });
    render(ClassesPage, { session });
    expect(screen.getByRole('heading', { level: 1, name: /Classes/ })).toBeVisible();
    expect(screen.getByText(/This list is only the names/)).toBeVisible();
    expect(screen.getByRole('link', { name: 'Details page' })).toHaveAttribute('href', '#/event');
    // Its own page under Setup: no tab row shared with Pilots.
    expect(screen.queryByRole('navigation')).toBeNull();
  });
});
