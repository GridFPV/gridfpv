/**
 * The kiosk's helpers: its address, the code in what was typed, and the countdown.
 */
import { describe, expect, it } from 'vitest';
import {
  isKioskHash,
  KIOSK_IDLE_MS,
  KIOSK_WARN_MS,
  kioskLeft,
  kioskLink,
  kioskWarning,
  typedCode
} from '../src/lib/kiosk.js';
import { codeInHash, codeProblem } from '../src/lib/pilotAccess.js';

describe('isKioskHash', () => {
  it('knows the kiosk address and nothing that only looks like it', () => {
    for (const hash of ['#/kiosk', '#/kiosk/', '#/kiosk?big=1']) {
      expect(isKioskHash(hash), hash).toBe(true);
    }
    for (const hash of [
      '',
      '#',
      '#/',
      '#/kiosks',
      '#/kiosk2',
      '#kiosk',
      '#/join/ABC234',
      '#/Kiosk'
    ]) {
      expect(isKioskHash(hash), hash).toBe(false);
    }
  });

  it('a kiosk address is never read as a sign-in link', () => {
    expect(codeInHash('#/kiosk')).toBeUndefined();
  });
});

describe('kioskLink', () => {
  it('adds the kiosk page to the address other devices use', () => {
    expect(kioskLink('http://192.168.1.20:8081/')).toBe('http://192.168.1.20:8081/#/kiosk');
    expect(kioskLink('http://192.168.1.20:8081/#/join/ABC234')).toBe(
      'http://192.168.1.20:8081/#/kiosk'
    );
    expect(isKioskHash(new URL(kioskLink('http://grid.local:8081/')).hash)).toBe(true);
  });
});

describe('typedCode', () => {
  it('tidies a code the way the sign-in page does', () => {
    expect(typedCode(' abc-234 ')).toBe('ABC234');
    expect(typedCode('ABC 234')).toBe('ABC234');
  });

  it('takes the code out of a scanned sign-in link', () => {
    expect(typedCode('http://192.168.1.20:8081/#/join/abc234')).toBe('ABC234');
    expect(typedCode('#/join/ABC-234')).toBe('ABC234');
    // The last one counts.
    expect(typedCode('http://x/join/ZZZZZZ/#/join/ABC234')).toBe('ABC234');
  });

  it('leaves what is not a code for the check to refuse', () => {
    expect(codeProblem(typedCode('http://x/#/join/'))).toBe('Type your code.');
    expect(codeProblem(typedCode('http://x/#/join/ABC234/more'))).toBeDefined();
    expect(codeProblem(typedCode('http://x/#/other/ABC234'))).toBeDefined();
    expect(codeProblem(typedCode('http://x/#/join/ABC234'))).toBeUndefined();
  });
});

describe('kioskLeft and kioskWarning', () => {
  it('counts down from the last touch and stops at nothing', () => {
    expect(kioskLeft(1000, 1000)).toBe(KIOSK_IDLE_MS);
    expect(kioskLeft(1000, 11_000)).toBe(KIOSK_IDLE_MS - 10_000);
    expect(kioskLeft(1000, 1000 + KIOSK_IDLE_MS)).toBe(0);
    expect(kioskLeft(1000, 1000 + KIOSK_IDLE_MS * 10)).toBe(0);
  });

  it('a clock that jumps back never gives more than the full time', () => {
    expect(kioskLeft(50_000, 1000)).toBe(KIOSK_IDLE_MS);
  });

  it('says nothing while there is plenty of time, then counts whole seconds', () => {
    expect(kioskWarning(KIOSK_IDLE_MS)).toBeUndefined();
    expect(kioskWarning(KIOSK_WARN_MS + 1)).toBeUndefined();
    expect(kioskWarning(KIOSK_WARN_MS)).toBe('Signing out in 15 s. Touch the screen to stay.');
    expect(kioskWarning(14_001)).toBe('Signing out in 15 s. Touch the screen to stay.');
    expect(kioskWarning(900)).toBe('Signing out in 1 s. Touch the screen to stay.');
    expect(kioskWarning(0)).toBe('Signing out in 0 s. Touch the screen to stay.');
  });

  it('the warning starts before the end, and the server keeps a pass longer than the page', () => {
    expect(KIOSK_WARN_MS).toBeLessThan(KIOSK_IDLE_MS);
    // The Director's backstop is 120 s (KIOSK_IDLE in pilot_access.rs).
    expect(KIOSK_IDLE_MS).toBeLessThan(120_000);
  });
});
