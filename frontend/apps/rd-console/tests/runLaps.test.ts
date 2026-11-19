/**
 * A seat card's laps and the All laps table (docs/primitives.md §12.3): the card lists the
 * latest laps and always the best, and every lap of the race is in one table.
 */
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup, fireEvent, render, screen, within } from '@testing-library/svelte';
import type { LapView } from '@gridfpv/types';
import { buildNames } from '../src/lib/names.js';
import {
  CARD_LAPS,
  allLapRows,
  cardLaps,
  filterLapRows,
  raceSeatRows,
  runLapsLabel
} from '../src/lib/run.js';
import RunAllLaps from '../src/screens/RunAllLaps.svelte';
import RunSeatCard from '../src/screens/RunSeatCard.svelte';
import { heat, lap, race, raceSeat } from './competition.js';

afterEach(cleanup);

const names = buildNames(
  [heat('q', 'Qualifying', ['p1', 'p2'])],
  [
    { id: 'p1', callsign: 'Ace', vtx_types: [] },
    { id: 'p2', callsign: 'Bee', vtx_types: [] },
    { id: 'p3', callsign: 'Cat', vtx_types: [] }
  ]
);
/** `count` laps, each `seconds[i]` long (or 10 s), with crossings numbered from `first`. */
const laps = (count: number, seconds: Record<number, number> = {}, first = 1): LapView[] =>
  Array.from({ length: count }, (_, i) => lap(first + i, seconds[i] ?? 10));
const numbers = (listed: ReturnType<typeof cardLaps>) => listed.laps.map((l) => l.number);

describe('the laps a seat card lists', () => {
  it('lists nine laps', () => {
    expect(CARD_LAPS).toBe(9);
  });

  it('with nine laps or fewer lists them all and hides none', () => {
    for (const count of [0, 1, 8, 9]) {
      const listed = cardLaps(laps(count), 10);
      expect(numbers(listed)).toEqual(Array.from({ length: count }, (_, i) => i + 1));
      expect(listed.hidden).toBe(0);
    }
  });

  it('past nine, lists the latest nine in order and says how many are not on the card', () => {
    const listed = cardLaps(laps(30, { 25: 7 }), 7 * 1_000_000);
    expect(numbers(listed)).toEqual([22, 23, 24, 25, 26, 27, 28, 29, 30]);
    expect(listed.hidden).toBe(21);
    expect(listed.laps.filter((l) => l.best).map((l) => l.number)).toEqual([26]);
    expect(listed.bestNumber).toBe(26);
  });

  it('never lists more than nine: an older best lap is named by number, not kept', () => {
    const listed = cardLaps(laps(30, { 3: 7 }), 7 * 1_000_000);
    expect(numbers(listed)).toEqual([22, 23, 24, 25, 26, 27, 28, 29, 30]);
    expect(listed.laps.some((l) => l.best)).toBe(false);
    expect(listed.bestNumber).toBe(4);
    expect(listed.hidden).toBe(21);
  });

  it('names one lap as best when two tie, the first', () => {
    const listed = cardLaps(laps(12, { 1: 7, 10: 7 }), 7 * 1_000_000);
    expect(listed.bestNumber).toBe(2);
    expect(listed.laps.some((l) => l.best)).toBe(false);
    expect(numbers(listed)).toEqual([4, 5, 6, 7, 8, 9, 10, 11, 12]);
  });

  it('does not keep the best run of N either, and marks the part of it still listed', () => {
    const old = cardLaps(laps(30, { 1: 6, 4: 8, 5: 8, 6: 8 }), 6 * 1_000_000, {
      start: 4,
      length: 3
    });
    expect(numbers(old)).toEqual([22, 23, 24, 25, 26, 27, 28, 29, 30]);
    expect(old.laps.some((l) => l.inRun)).toBe(false);
    // A run that straddles the window: only its laps among the nine are marked.
    const straddling = cardLaps(laps(30), undefined, { start: 20, length: 3 });
    expect(numbers(straddling)).toEqual([22, 23, 24, 25, 26, 27, 28, 29, 30]);
    expect(straddling.laps.filter((l) => l.inRun).map((l) => l.number)).toEqual([22, 23]);
  });

  it('with no best lap known names none', () => {
    const listed = cardLaps(laps(20), undefined);
    expect(numbers(listed)).toEqual([12, 13, 14, 15, 16, 17, 18, 19, 20]);
    expect(listed.hidden).toBe(11);
    expect(listed.bestNumber).toBeUndefined();
  });

  it('says which laps a run is', () => {
    expect(runLapsLabel({ start: 4, length: 3 })).toBe('laps 5 to 7');
    expect(runLapsLabel({ start: 0, length: 1 })).toBe('lap 1');
  });
});

describe('RunSeatCard in a long race', () => {
  function card(count: number, seconds: Record<number, number>, best: number) {
    const on = race('q', 1, 'Running', {
      seats: [raceSeat(0, 'p1', { laps: laps(count, seconds), best_lap_micros: best * 1_000_000 })]
    });
    const [row] = raceSeatRows(on, names);
    render(RunSeatCard, { row, racing: true, oncrossing: () => {} });
    return within(screen.getByRole('list', { name: 'Laps for Seat 1' })).getAllByRole('listitem');
  }

  it('shows every lap of a short race, and says which lap was the best', () => {
    const items = card(5, { 2: 8 }, 8);
    expect(items.map((item) => item.dataset.lap)).toEqual(['1', '2', '3', '4', '5']);
    expect(items[2]).toHaveClass('best');
    expect(document.querySelector('[data-stat="best"]')).toHaveTextContent('8.000 (lap 3)');
  });

  it('shows only the latest nine however many were flown, with the best lap named by number', () => {
    const items = card(40, { 5: 7.25 }, 7.25);
    expect(items.map((item) => item.dataset.lap)).toEqual([
      '32',
      '33',
      '34',
      '35',
      '36',
      '37',
      '38',
      '39',
      '40'
    ]);
    expect(items.some((item) => item.classList.contains('best'))).toBe(false);
    expect(document.querySelector('[data-stat="best"]')).toHaveTextContent('7.250 (lap 6)');
    // The stats are about the whole race, not the nine.
    expect(document.querySelector('[data-stat="laps"]')).toHaveTextContent('40');
  });

  it('says nothing in brackets before there is a lap', () => {
    const on = race('q', 1, 'Running', { seats: [raceSeat(0, 'p1', { laps: [] })] });
    const [row] = raceSeatRows(on, names);
    render(RunSeatCard, { row, racing: true, oncrossing: () => {} });
    expect(document.querySelector('[data-stat="best"]')?.textContent).not.toContain('(');
  });
});

describe('every lap of a race', () => {
  // Seat 1 is Ace's. Seat 2 is open: Bee held it, then nobody, then Cat, then Bee again.
  const on = race('q', 1, 'Running', {
    seats: [
      raceSeat(0, 'p1', {
        laps: [lap(1, 10), lap(2, 8), lap(3, 9, { restart: true }), lap(4, 11)],
        best_lap_micros: 8_000_000
      }),
      raceSeat(1, undefined, {
        laps: [lap(20, 12), lap(21, 13), lap(22, 30), lap(23, 9), lap(24, 14), lap(25, 11)],
        holder: 'p2',
        stints: [
          {
            pilot: 'p2',
            open: false,
            laps: [lap(20, 12), lap(21, 13)],
            best_lap_micros: 12_000_000
          },
          { pilot: 'p3', open: false, laps: [lap(23, 9), lap(24, 14)], best_lap_micros: 9_000_000 },
          { pilot: 'p2', open: true, laps: [lap(25, 11)], best_lap_micros: 11_000_000 }
        ]
      })
    ]
  });
  const rows = allLapRows(on, names);
  const say = (list: typeof rows) =>
    list.map((row) => `${row.seatName} ${row.who} ${row.number} ${row.micros / 1_000_000}`);

  it('lists each seat in turn, a named pilot’s counted laps numbered from 1', () => {
    expect(say(rows).slice(0, 3)).toEqual([
      'Seat 1 Ace 1 10',
      'Seat 1 Ace 2 8',
      // The lap marked as a restart is not a lap. The next is the pilot's third.
      'Seat 1 Ace 3 11'
    ]);
  });

  it('gives each lap on an open seat to whoever held the seat, and to nobody when nobody did', () => {
    expect(say(rows).slice(3)).toEqual([
      // Bee's two stints are one run of laps, numbered through.
      'Seat 2 Bee 1 12',
      'Seat 2 Bee 2 13',
      'Seat 2 Bee 3 11',
      'Seat 2 Cat 1 9',
      'Seat 2 Cat 2 14',
      // Flown with the seat open: it counts for nobody, and is not hidden.
      'Seat 2 Nobody 1 30'
    ]);
    const nobody = rows.find((row) => row.who === 'Nobody')!;
    expect(nobody.pilot).toBeUndefined();
    expect(nobody.best).toBe(false);
  });

  it('marks one fastest lap for each pilot on each seat', () => {
    expect(rows.filter((row) => row.best).map((row) => `${row.who} ${row.micros / 1e6}`)).toEqual([
      'Ace 8',
      'Bee 11',
      'Cat 9'
    ]);
  });

  it('has a key of its own for every row, and no id in what is shown', () => {
    expect(new Set(rows.map((row) => row.key)).size).toBe(rows.length);
    expect(say(rows).join(' ')).not.toMatch(/p1|p2|p3|bench|seat-/);
  });

  it('narrows to a seat, to a pilot, or both, and orders fastest first without losing ties’ order', () => {
    expect(filterLapRows(rows, {})).toEqual(rows);
    expect(say(filterLapRows(rows, { seat: 'bench/seat-1' }))).toHaveLength(3);
    expect(say(filterLapRows(rows, { pilot: 'p2' }))).toEqual([
      'Seat 2 Bee 1 12',
      'Seat 2 Bee 2 13',
      'Seat 2 Bee 3 11'
    ]);
    // A pilot picked leaves out the laps that were nobody's.
    expect(filterLapRows(rows, { seat: 'bench/seat-2', pilot: 'p3' })).toHaveLength(2);
    expect(filterLapRows(rows, { seat: 'bench/seat-1', pilot: 'p3' })).toEqual([]);
    expect(say(filterLapRows(rows, { order: 'fastest' })).slice(0, 4)).toEqual([
      'Seat 1 Ace 2 8',
      'Seat 2 Cat 1 9',
      'Seat 1 Ace 1 10',
      // Two laps of 11 seconds: in flying order, seat 1 first.
      'Seat 1 Ace 3 11'
    ]);
  });

  describe('RunAllLaps', () => {
    const table = () => screen.queryByRole('table', { name: 'All laps' });
    const shown = () =>
      within(table()!)
        .getAllByRole('row')
        .slice(1)
        .map((row) =>
          [...row.querySelectorAll('td')].map((cell) => cell.textContent?.trim()).join(' | ')
        );

    it('is shut until opened, and says how many laps it holds', async () => {
      render(RunAllLaps, { race: on, names });
      const toggle = screen.getByRole('button', { name: /^All laps/ });
      expect(toggle).toHaveAttribute('aria-expanded', 'false');
      expect(screen.getByTestId('all-laps-count')).toHaveTextContent('9');
      expect(table()).toBeNull();
      await fireEvent.click(toggle);
      expect(toggle).toHaveAttribute('aria-expanded', 'true');
      expect(shown()).toHaveLength(9);
      expect(shown()[0]).toBe('Seat 1 | Ace | 1 | 10.000');
    });

    it('is not there at all for a race with no lap yet', () => {
      render(RunAllLaps, { race: race('q', 1, 'Running', { seats: [raceSeat(0, 'p1')] }), names });
      expect(screen.queryByRole('button', { name: /^All laps/ })).toBeNull();
    });

    it('is narrowed by seat and by pilot, offers only those that have a lap, and can be put fastest first', async () => {
      render(RunAllLaps, { race: on, names });
      await fireEvent.click(screen.getByRole('button', { name: /^All laps/ }));
      const options = (label: string) =>
        within(screen.getByLabelText(label))
          .getAllByRole('option')
          .map((option) => option.textContent);
      expect(options('Seat')).toEqual(['Every seat', 'Seat 1', 'Seat 2']);
      // By callsign, and "Nobody" is not a pilot to pick.
      expect(options('Pilot')).toEqual(['Every pilot', 'Ace', 'Bee', 'Cat']);

      await fireEvent.change(screen.getByLabelText('Pilot'), { target: { value: 'p3' } });
      expect(shown()).toEqual(['Seat 2 | Cat | 1 | 9.000', 'Seat 2 | Cat | 2 | 14.000']);
      await fireEvent.change(screen.getByLabelText('Seat'), {
        target: { value: 'bench/seat-1' }
      });
      expect(table()).toBeNull();
      expect(screen.getByText('No lap matches.')).toBeVisible();
      await fireEvent.change(screen.getByLabelText('Pilot'), { target: { value: '' } });
      await fireEvent.change(screen.getByLabelText('Order'), { target: { value: 'fastest' } });
      expect(shown()).toEqual([
        'Seat 1 | Ace | 2 | 8.000',
        'Seat 1 | Ace | 1 | 10.000',
        'Seat 1 | Ace | 3 | 11.000'
      ]);
      expect(document.body.textContent).not.toMatch(/p1|p2|p3|bench/);
    });
  });
});
