import { describe, expect, it } from 'vitest';
import { registerCommand } from '../src/lib/registration.js';

describe('registerCommand', () => {
  it('binds a source-local competitor to an event-scoped pilot', () => {
    expect(registerCommand('rh-1', 'seat-2', 'pilot-alice')).toEqual({
      Register: { adapter: 'rh-1', competitor: 'seat-2', pilot: 'pilot-alice' }
    });
  });
});
