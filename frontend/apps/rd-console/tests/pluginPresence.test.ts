/**
 * Unit tests for the GridFPV-plugin presence mapping (RH plugin design D16, Slice 1): the timer
 * row chip + guided-install prompt read these views off a timer's `plugin` field.
 */
import { describe, expect, it } from 'vitest';
import type { Timer } from '@gridfpv/types';
import { pluginBundleUrl, pluginView, selectionRefusal } from '../src/lib/pluginPresence.js';

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
    manual_connect: false,
    calibration: [],
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
      available_channels: [],
      manual_connect: false,
      calibration: []
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

/**
 * The event-selection gate (#405): the GridFPV plugin is required for Grid to race a RotorHazard
 * timer, so an RH timer is selectable only once its plugin has probed `Present`.
 */
describe('selectionRefusal', () => {
  const MOCK: Timer = {
    id: 'mock',
    name: 'Mock',
    kind: { Mock: { laps: 3, lap_ms: 30000 } },
    status: 'Ready',
    channel_capability: 'Flexible',
    node_count: 8,
    available_channels: [],
    manual_connect: false,
    calibration: []
  };

  it('never refuses a Mock timer — the requirement is RotorHazard-specific', () => {
    expect(selectionRefusal(MOCK)).toBeNull();
  });

  it('allows a RotorHazard timer whose plugin probed Present', () => {
    const ok = rhTimer({
      Present: { plugin_version: '0.1.0', rhapi_version: '1.4', capabilities: ['hello'] }
    });
    expect(selectionRefusal(ok)).toBeNull();
  });

  it('treats “never probed” as its own problem: connect it, not install it', () => {
    const r = selectionRefusal(rhTimer(undefined));
    expect(r!.kind).toBe('not-connected');
    expect(r!.reason).toContain('Track A');
    expect(r!.reason).toContain('Connect it');
    expect(r!.reason).not.toContain('Install');
  });

  it('points a Missing plugin at the install', () => {
    const r = selectionRefusal(rhTimer('Missing'));
    expect(r!.kind).toBe('plugin-missing');
    expect(r!.reason).toContain('Install it');
  });

  it('points an Incompatible plugin at the update', () => {
    const r = selectionRefusal(
      rhTimer({ Incompatible: { plugin_version: '9.9.9', protocol_version: 2, reason: 'p2' } })
    );
    expect(r!.kind).toBe('plugin-incompatible');
    expect(r!.reason).toContain('Update it');
  });

  it('gives three distinct reasons, each naming the timer and never its id', () => {
    const reasons = [
      selectionRefusal(rhTimer(undefined))!.reason,
      selectionRefusal(rhTimer('Missing'))!.reason,
      selectionRefusal(
        rhTimer({ Incompatible: { plugin_version: '1', protocol_version: 2, reason: 'p2' } })
      )!.reason
    ];
    expect(new Set(reasons).size).toBe(3);
    for (const reason of reasons) {
      expect(reason).toContain('Track A');
      // Friendly-name rule: the raw id must never reach the screen.
      expect(reason).not.toContain("'t'");
    }
  });

  it('has a distinct already-selected warning for a grandfathered selection', () => {
    // A plugin can vanish AFTER a valid selection; that row warns rather than blaming the RD's
    // choice, and points at the arm-time refusal that now applies.
    const r = selectionRefusal(rhTimer('Missing'))!;
    expect(r.alreadySelectedWarning).not.toBe(r.reason);
    expect(r.alreadySelectedWarning).toContain('Track A');
    expect(r.alreadySelectedWarning).toContain('arming a heat is refused');
  });
});
