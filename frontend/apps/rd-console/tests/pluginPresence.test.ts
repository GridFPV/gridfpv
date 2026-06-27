/**
 * Unit tests for the GridFPV-plugin presence mapping (RH plugin design D16, Slice 1): the timer
 * row chip + guided-install prompt read these views off a timer's `plugin` field.
 */
import { describe, expect, it } from 'vitest';
import type { Timer } from '@gridfpv/types';
import { pluginBundleUrl, pluginView } from '../src/lib/pluginPresence.js';

/** A RotorHazard timer with the given plugin presence (the field under test). */
function rhTimer(plugin: Timer['plugin']): Timer {
  return {
    id: 't',
    name: 'Track A',
    kind: { Rotorhazard: { url: 'http://rh.local:5000' } },
    status: 'Connected',
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    plugin
  };
}

describe('pluginView', () => {
  it('returns null for a Mock timer (no plugin concept)', () => {
    const mock: Timer = {
      id: 'mock',
      name: 'Mock',
      kind: { Mock: { laps: 3, lap_ms: 30000 } },
      status: 'Ready',
      channel_capability: 'Flexible',
      node_count: 8,
      available_channels: []
    };
    expect(pluginView(mock)).toBeNull();
  });

  it('returns null when not yet probed (undefined)', () => {
    expect(pluginView(rhTimer(undefined))).toBeNull();
  });

  it('flags Missing as a warn chip that needs install', () => {
    const v = pluginView(rhTimer('Missing'));
    expect(v).not.toBeNull();
    expect(v!.kind).toBe('missing');
    expect(v!.tone).toBe('warn');
    expect(v!.needsInstall).toBe(true);
  });

  it('shows Present as a healthy chip with version detail, no install', () => {
    const v = pluginView(
      rhTimer({
        Present: { plugin_version: '0.1.0', rhapi_version: '1.4', capabilities: ['hello'] }
      })
    );
    expect(v!.kind).toBe('healthy');
    expect(v!.tone).toBe('success');
    expect(v!.needsInstall).toBe(false);
    expect(v!.detail).toContain('0.1.0');
    expect(v!.detail).toContain('1.4');
  });

  it('flags Incompatible as a danger chip that needs install, surfacing the reason', () => {
    const v = pluginView(
      rhTimer({
        Incompatible: { plugin_version: '9.9.9', protocol_version: 2, reason: 'protocol v2' }
      })
    );
    expect(v!.kind).toBe('incompatible');
    expect(v!.tone).toBe('danger');
    expect(v!.needsInstall).toBe(true);
    expect(v!.detail).toBe('protocol v2');
  });
});

describe('pluginBundleUrl', () => {
  it('joins the base URL with the bundle path, tolerating a trailing slash', () => {
    expect(pluginBundleUrl('http://host:3000')).toBe('http://host:3000/plugin/gridfpv.zip');
    expect(pluginBundleUrl('http://host:3000/')).toBe('http://host:3000/plugin/gridfpv.zip');
  });
});
