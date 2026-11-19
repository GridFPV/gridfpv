/**
 * A pilot's sign-in code: how it is shown, tidied and checked before it is sent.
 */
import { describe, expect, it } from 'vitest';
import { codeInHash, codeProblem, spellCode, tidyCode } from '../src/lib/pilotAccess.js';

describe('showing a code', () => {
  it('writes it in two halves', () => {
    expect(spellCode('ABC234')).toBe('ABC 234');
    expect(spellCode('ABCD2345')).toBe('ABCD 2345');
    expect(spellCode('ABCDE')).toBe('ABC DE');
    expect(spellCode('A')).toBe('A');
    expect(spellCode('')).toBe('');
  });
});

describe('tidying a typed code', () => {
  it('takes out spaces and dashes and makes capitals', () => {
    for (const typed of ['ABC234', 'abc234', ' abc 234 ', 'ABC-234', 'a-b-c-2-3-4', 'abc\t234\n']) {
      expect(tidyCode(typed), JSON.stringify(typed)).toBe('ABC234');
    }
    expect(tidyCode('')).toBe('');
    expect(tidyCode(' - ')).toBe('');
  });
});

describe('checking a typed code before it is sent', () => {
  it('lets a whole code through, however it was typed', () => {
    for (const typed of ['ABC234', 'abc234', 'ABC 234', 'abc-234', '  ZZZ999  ']) {
      expect(codeProblem(typed), typed).toBeUndefined();
    }
  });

  it('asks for a code when there is none', () => {
    for (const typed of ['', ' ', '-', '\n']) {
      expect(codeProblem(typed), JSON.stringify(typed)).toBe('Type your code.');
    }
  });

  it('says how long a code is when it is too short or too long', () => {
    expect(codeProblem('ABC23')).toBe('A code has 6 letters and numbers.');
    expect(codeProblem('A')).toBe('A code has 6 letters and numbers.');
    expect(codeProblem('ABC2345')).toBe('A code has only 6 letters and numbers.');
    expect(codeProblem('ABC234ABC234')).toBe('A code has only 6 letters and numbers.');
  });

  it('names the characters no code has', () => {
    const words = 'A code has capital letters and numbers, and never 0, O, 1 or I.';
    for (const typed of [
      'ABC230',
      'ABC23O',
      'ABC231',
      'ABC23I',
      'ABC23!',
      'ABC_23',
      'ABC23é',
      'ＡＢＣ２３４'
    ]) {
      expect(codeProblem(typed), typed).toBe(words);
    }
  });
});

describe('a code in the page address', () => {
  it('is read from the link a QR code holds', () => {
    expect(codeInHash('#/join/ABC234')).toBe('ABC234');
    expect(codeInHash('#/join/abc234')).toBe('ABC234');
    expect(codeInHash('#/join/ABC-234')).toBe('ABC234');
    expect(codeInHash('#/join/ABC%20234')).toBe('ABC234');
  });

  it('is nothing for any other address', () => {
    for (const hash of [
      '',
      '#',
      '#/',
      '#/join',
      '#/join/',
      '#/join/ABC23',
      '#/join/ABC2345',
      '#/join/ABC23O',
      '#/join/ABC234/extra',
      '#/join/ABC234?x=1',
      '#/joined/ABC234',
      '#/events',
      '/join/ABC234',
      '#/join/%E0%A4%A',
      '#/join/<script>'
    ]) {
      expect(codeInHash(hash), hash).toBeUndefined();
    }
  });
});

describe('the printed sheet of codes', () => {
  it('offers four or six cards to a page', async () => {
    const { PER_PAGE_CHOICES } = await import('../src/lib/pilotAccess.js');
    expect([...PER_PAGE_CHOICES]).toEqual([4, 6]);
  });

  it('fills each page in order and puts what is left on the last', async () => {
    const { sheetPages } = await import('../src/lib/pilotAccess.js');
    const rows = Array.from({ length: 10 }, (_, n) => n + 1);
    expect(sheetPages(rows, 4)).toEqual([
      [1, 2, 3, 4],
      [5, 6, 7, 8],
      [9, 10]
    ]);
    expect(sheetPages(rows, 6)).toEqual([
      [1, 2, 3, 4, 5, 6],
      [7, 8, 9, 10]
    ]);
    expect(sheetPages(rows.slice(0, 6), 6)).toHaveLength(1);
    expect(sheetPages(rows.slice(0, 4), 4)).toHaveLength(1);
    expect(sheetPages([], 6)).toEqual([]);
  });
});
