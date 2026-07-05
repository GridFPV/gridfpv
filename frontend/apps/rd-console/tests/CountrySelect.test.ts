import { describe, expect, it } from 'vitest';
import { render, screen, within } from '@testing-library/svelte';
import { fireEvent } from '@testing-library/dom';
import CountrySelect from '../src/screens/CountrySelect.svelte';
import { COUNTRIES, US_CODE } from '../src/lib/countries.js';

describe('CountrySelect (#74)', () => {
  it('lists United States first, then the rest alphabetically by name', async () => {
    render(CountrySelect, { value: '' });
    await fireEvent.click(screen.getByRole('button', { name: 'Country' }));

    const listbox = screen.getByRole('listbox', { name: 'Countries' });
    const options = within(listbox).getAllByRole('option');
    // US is pinned to the very top.
    expect(options[0]).toHaveTextContent('United States');
    // The remainder are alphabetical: the second option is the first country (by name)
    // that is not the US — i.e. the head of the pre-sorted list minus US.
    const restByName = COUNTRIES.filter((c) => c.code !== US_CODE);
    expect(options[1]).toHaveTextContent(restByName[0].name);
    expect(options[2]).toHaveTextContent(restByName[1].name);
  });

  it('renders SVG flags, never emoji (Windows shows emoji as bare letters)', async () => {
    const { container } = render(CountrySelect, { value: '' });
    await fireEvent.click(screen.getByRole('button', { name: 'Country' }));
    // Each option carries an inline <svg> flag — not a text emoji.
    expect(container.querySelector('.cs-option svg')).not.toBeNull();
  });

  it('filters by name or code as the user searches', async () => {
    render(CountrySelect, { value: '' });
    await fireEvent.click(screen.getByRole('button', { name: 'Country' }));

    const search = screen.getByRole('textbox', { name: 'Search countries' });
    await fireEvent.input(search, { target: { value: 'german' } });
    const listbox = screen.getByRole('listbox', { name: 'Countries' });
    const options = within(listbox).getAllByRole('option');
    expect(options).toHaveLength(1);
    expect(options[0]).toHaveTextContent('Germany');
  });

  it('stores the alpha-2 code on selection (reflected in the trigger)', async () => {
    render(CountrySelect, { value: '' });
    await fireEvent.click(screen.getByRole('button', { name: 'Country' }));
    const search = screen.getByRole('textbox', { name: 'Search countries' });
    await fireEvent.input(search, { target: { value: 'United States' } });
    await fireEvent.click(screen.getByRole('option', { name: /United States/ }));
    // The trigger now shows the selected country's name + its stored code.
    const trigger = screen.getByRole('button', { name: 'Country' });
    expect(trigger).toHaveTextContent('United States');
    expect(within(trigger).getByText('US')).toBeInTheDocument();
  });

  it('clears the selection back to the placeholder', async () => {
    render(CountrySelect, { value: 'US', placeholder: 'Select a country…' });
    // The selected trigger shows a Clear control; clicking it returns to the placeholder.
    await fireEvent.click(screen.getByRole('button', { name: 'Clear country' }));
    expect(screen.getByText('Select a country…')).toBeInTheDocument();
  });
});
