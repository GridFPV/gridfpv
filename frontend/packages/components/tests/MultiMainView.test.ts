import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import type { MultiMain } from '../src/multiMain.js';
import MultiMainView from '../src/MultiMainView.svelte';

const VIEW: MultiMain = {
  standings: [
    { competitor: 'p1', label: 'AceOne', position: 1, tierName: 'A-Main' },
    { competitor: 'p2', label: 'Bolt', position: 2, tierName: 'A-Main' },
    { competitor: 'p3', label: 'Comet', position: 3, tierName: 'B-Main' }
  ]
};

describe('MultiMainView', () => {
  it('renders the standings with Pos | Pilot | Tier and resolved callsigns', () => {
    render(MultiMainView, { view: VIEW });
    const table = screen.getByRole('table', { name: 'Multi-main standings' });
    // The column headers.
    expect(within(table).getByText('Pos')).toBeInTheDocument();
    expect(within(table).getByText('Pilot')).toBeInTheDocument();
    expect(within(table).getByText('Tier')).toBeInTheDocument();
    // The callsigns (never the raw refs).
    expect(within(table).getByText('AceOne')).toBeInTheDocument();
    expect(within(table).getByText('Bolt')).toBeInTheDocument();
    expect(within(table).getByText('Comet')).toBeInTheDocument();
    expect(within(table).queryByText('p1')).toBeNull();
    // The tier names show.
    expect(within(table).getAllByText('A-Main').length).toBe(2);
    expect(within(table).getByText('B-Main')).toBeInTheDocument();
  });

  it('marks the medal positions (gold for 1st)', () => {
    const { container } = render(MultiMainView, { view: VIEW });
    const gold = container.querySelector("tr.medal[data-medal='gold']");
    expect(gold).not.toBeNull();
    expect(within(gold as HTMLElement).getByText('AceOne')).toBeInTheDocument();
  });

  it('shows an em dash when a row has no resolved tier', () => {
    render(MultiMainView, {
      view: { standings: [{ competitor: 'p9', label: 'Ghost', position: 1 }] }
    });
    expect(screen.getByText('—')).toBeInTheDocument();
  });

  it('shows an empty standings note when there are no rows yet', () => {
    render(MultiMainView, { view: { standings: [] } });
    expect(screen.getByText(/Standings appear as the mains are scored/i)).toBeInTheDocument();
  });
});
