/** The Run page's keyboard shortcuts (`lib/shortcuts.ts`): what a key does, and when it may. */
import { afterEach, describe, expect, it } from 'vitest';
import {
  DISCARD_ARM_MS,
  RUN_KEYS,
  runShortcut,
  shortcutFree,
  type RunCaps
} from '../src/lib/shortcuts.js';

const NOTHING: RunCaps = { start: false, stop: false, save: false, discard: false };
const EVERYTHING: RunCaps = { start: true, stop: true, save: true, discard: true };
/** What the page can do at each step of a race's life. */
const READY: RunCaps = { ...NOTHING, start: true };
const RACING: RunCaps = { ...NOTHING, stop: true };
const STOPPED: RunCaps = { ...NOTHING, save: true, discard: true };

describe('runShortcut', () => {
  it('Space does the next step of the race: Start, then Stop, then Save', () => {
    expect(runShortcut(' ', READY)).toBe('start');
    expect(runShortcut(' ', RACING)).toBe('stop');
    expect(runShortcut(' ', STOPPED)).toBe('save');
    expect(runShortcut(' ', NOTHING)).toBeNull();
  });

  it('Space takes the earliest step when the page can do more than one', () => {
    expect(runShortcut(' ', EVERYTHING)).toBe('start');
    expect(runShortcut(' ', { ...EVERYTHING, start: false })).toBe('stop');
  });

  it('Space never discards, even when discarding is all the page can do', () => {
    expect(runShortcut(' ', { ...NOTHING, discard: true })).toBeNull();
  });

  it('z starts and x stops, each only when the page can', () => {
    expect(runShortcut('z', READY)).toBe('start');
    expect(runShortcut('z', RACING)).toBeNull();
    expect(runShortcut('x', RACING)).toBe('stop');
    expect(runShortcut('x', READY)).toBeNull();
  });

  it('Caps Lock does not switch the letters off', () => {
    expect(runShortcut('Z', READY)).toBe('start');
    expect(runShortcut('X', RACING)).toBe('stop');
    expect(runShortcut('S', STOPPED)).toBe('save');
    expect(runShortcut('D', STOPPED)).toBe('discard');
    // A named key is not a letter: it is left as it is.
    expect(runShortcut('Enter', READY)).toBeNull();
  });

  it('s and c save, only when the page can', () => {
    expect(runShortcut('s', STOPPED)).toBe('save');
    expect(runShortcut('c', STOPPED)).toBe('save');
    expect(runShortcut('s', RACING)).toBeNull();
    expect(runShortcut('c', READY)).toBeNull();
  });

  it('d and v discard, only when the page can', () => {
    expect(runShortcut('d', STOPPED)).toBe('discard');
    expect(runShortcut('v', STOPPED)).toBe('discard');
    expect(runShortcut('d', RACING)).toBeNull();
    expect(runShortcut('v', NOTHING)).toBeNull();
  });

  it('a letter does only its own step, not the next one', () => {
    expect(runShortcut('z', STOPPED)).toBeNull();
    expect(runShortcut('s', READY)).toBeNull();
    expect(runShortcut('x', STOPPED)).toBeNull();
  });

  it('? shows the legend whatever the page can do', () => {
    expect(runShortcut('?', NOTHING)).toBe('legend');
    expect(runShortcut('?', EVERYTHING)).toBe('legend');
  });

  it('any other key does nothing', () => {
    for (const key of ['a', 'Enter', 'Escape', 'Tab', '', 'Spacebar', '/', '1']) {
      expect(runShortcut(key, EVERYTHING)).toBeNull();
    }
  });
});

describe('the legend', () => {
  it('lists only keys that do something', () => {
    for (const entry of RUN_KEYS) {
      expect(entry.does).not.toBe('');
      for (const key of entry.keys) {
        expect(runShortcut(key === 'Space' ? ' ' : key, EVERYTHING)).not.toBeNull();
      }
    }
  });

  it('says what each of its keys really does', () => {
    const action = (label: string) => {
      const entry = RUN_KEYS.find((candidate) => candidate.does.startsWith(label));
      if (!entry) throw new Error(`no legend line starts with "${label}"`);
      return entry.keys.map((key) => runShortcut(key, EVERYTHING));
    };
    expect(action('Start')).toEqual(['start']);
    expect(action('Stop')).toEqual(['stop']);
    expect(action('Save')).toEqual(['save', 'save']);
    // Discard is listed as the same key twice: it wants two presses.
    expect(action('Discard')).toEqual(['discard', 'discard']);
    expect(action('show or hide')).toEqual(['legend']);
  });

  it('gives the same time to press Discard twice as the page allows', () => {
    expect(DISCARD_ARM_MS).toBe(3000);
    const discard = RUN_KEYS.find((entry) => entry.does.startsWith('Discard'));
    expect(discard?.does).toContain(`within ${DISCARD_ARM_MS / 1000} seconds`);
  });
});

describe('shortcutFree', () => {
  const press = (over: Partial<Parameters<typeof shortcutFree>[0]> = {}) => ({
    key: 'z',
    ctrlKey: false,
    metaKey: false,
    altKey: false,
    repeat: false,
    defaultPrevented: false,
    ...over
  });
  const added: Element[] = [];
  const add = <T extends Element>(el: T): T => {
    document.body.append(el);
    added.push(el);
    return el;
  };

  afterEach(() => {
    for (const el of added.splice(0)) el.remove();
  });

  it('is free for a plain key with nothing focused and no dialog open', () => {
    expect(shortcutFree(press(), document)).toBe(true);
    expect(shortcutFree(press(), { activeElement: null, querySelector: () => null })).toBe(true);
  });

  it('is not free with Ctrl, the Meta key or Alt held', () => {
    expect(shortcutFree(press({ ctrlKey: true }), document)).toBe(false);
    expect(shortcutFree(press({ metaKey: true }), document)).toBe(false);
    expect(shortcutFree(press({ altKey: true }), document)).toBe(false);
  });

  it('is not free for a held key repeating, or a key something else already handled', () => {
    expect(shortcutFree(press({ repeat: true }), document)).toBe(false);
    expect(shortcutFree(press({ defaultPrevented: true }), document)).toBe(false);
  });

  it('is not free while typing in a field, a text area or a select', () => {
    for (const tag of ['input', 'textarea', 'select']) {
      const field = add(document.createElement(tag));
      field.focus();
      expect(document.activeElement).toBe(field);
      expect(shortcutFree(press(), document)).toBe(false);
    }
  });

  it('is free with a button focused: pressing a key there is not typing', () => {
    const button = add(document.createElement('button'));
    button.focus();
    expect(document.activeElement).toBe(button);
    expect(shortcutFree(press(), document)).toBe(true);
  });

  it('is not free while typing in an editable element', () => {
    const editable = { tagName: 'DIV', isContentEditable: true } as unknown as Element;
    expect(shortcutFree(press(), { activeElement: editable, querySelector: () => null })).toBe(
      false
    );
  });

  it('is not free while a dialog is open, and free again once it is closed', () => {
    const dialog = add(document.createElement('dialog'));
    expect(shortcutFree(press(), document)).toBe(true);
    dialog.setAttribute('open', '');
    expect(shortcutFree(press(), document)).toBe(false);
    dialog.removeAttribute('open');
    expect(shortcutFree(press(), document)).toBe(true);
  });
});
