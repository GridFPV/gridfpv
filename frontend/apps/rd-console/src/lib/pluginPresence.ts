// Interpret a timer's GridFPV-plugin presence for the console UI (RH plugin design D16, Slice 1).
//
// The backend probes a connected RotorHazard timer for the GridFPV plugin (the `gridfpv_hello`
// handshake) and reports a `PluginPresence` on the `Timer`. This turns that wire enum into the
// small view the timer row + guided-install prompt render. Kept here (not inline in the component)
// so the mapping is one place and unit-testable. Friendly-name rule: copy never names a URL.

import type { Timer } from '@gridfpv/types';
import { kindTag } from './timers.js';

/** The UI's coarse plugin state for a timer. */
export type PluginViewKind = 'healthy' | 'missing' | 'incompatible';

export interface PluginView {
  kind: PluginViewKind;
  /** Badge tone for the row chip. */
  tone: 'success' | 'warn' | 'danger';
  /** Short chip label. */
  label: string;
  /** Whether the guided one-step install should be offered (missing/incompatible). */
  needsInstall: boolean;
  /** Modal heading. */
  title: string;
  /** Longer explanation (version / mismatch reason). */
  detail?: string;
}

/**
 * Interpret a timer's plugin presence. Returns `null` for non-RotorHazard timers and for the
 * not-yet-probed `Unknown` state — no chip, no noise before the timer has connected.
 */
export function pluginView(timer: Timer): PluginView | null {
  // Only RotorHazard timers have a plugin; the Mock never does.
  if (kindTag(timer.kind) !== 'Rotorhazard') return null;

  const p = timer.plugin;
  // Not probed yet (pre-connect / Mock): show nothing until we actually know.
  if (!p) return null;

  if (p === 'Missing') {
    return {
      kind: 'missing',
      tone: 'warn',
      label: 'plugin missing',
      needsInstall: true,
      title: 'GridFPV plugin needed',
      detail:
        'This RotorHazard timer isn’t running the GridFPV plugin. Installing it unlocks live ' +
        'signal and clean start/stop. If RotorHazard is older than v4.3.0, update it first.'
    };
  }

  if (typeof p === 'object' && 'Present' in p) {
    return {
      kind: 'healthy',
      tone: 'success',
      label: 'plugin ✓',
      needsInstall: false,
      title: 'GridFPV plugin active',
      detail: `Plugin v${p.Present.plugin_version} · RHAPI ${p.Present.rhapi_version}`
    };
  }

  // Incompatible.
  return {
    kind: 'incompatible',
    tone: 'danger',
    label: 'plugin update',
    needsInstall: true,
    title: 'GridFPV plugin update needed',
    detail: p.Incompatible.reason
  };
}

/** The URL the guided install downloads the plugin bundle from (served by the Director). */
export function pluginBundleUrl(baseUrl: string): string {
  return `${baseUrl.replace(/\/$/, '')}/plugin/gridfpv.zip`;
}
