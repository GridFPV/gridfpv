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

// ── The event-selection gate (#405) ───────────────────────────────────────────
//
// The GridFPV plugin is **required** for Grid to race a RotorHazard timer, and the gate is at
// **event timer selection** — not at connecting, restarting or probing, which are exactly how the
// RD *gets* to a working plugin. The Director enforces the same rule on
// `PUT /events/{id}/timers` (the API is the enforcement); this is the console half, which says why
// **before** the RD clicks and renders the reason rather than just greying the row out.

/** Which of the three selection problems a timer has — three problems, three different fixes. */
export type SelectionRefusalKind = 'not-connected' | 'plugin-missing' | 'plugin-incompatible';

export interface SelectionRefusal {
  kind: SelectionRefusalKind;
  /** The reason + the next action, naming the timer by its friendly name (repo display rule). */
  reason: string;
  /** The wording used when the event **already** selects this timer (a grandfathered selection). */
  alreadySelectedWarning: string;
}

/**
 * Why an event may not select `timer`, or `null` when it can (#405).
 *
 * **Mock timers are never refused** — the requirement is RotorHazard-specific, and the built-in
 * Mock is what an unconfigured Director races out of the box. A RotorHazard timer is selectable
 * only once its plugin has probed `Present`.
 *
 * `plugin: undefined` (never probed) is deliberately its **own** case: presence is only knowable
 * over a live socket, so it is the normal state of a freshly added timer and the fix is “connect
 * it”, not “install a plugin”. Saying “plugin missing” there would send the RD off to install
 * something that may already be sitting on the timer.
 */
export function selectionRefusal(timer: Timer): SelectionRefusal | null {
  if (kindTag(timer.kind) !== 'Rotorhazard') return null;

  const p = timer.plugin;
  if (typeof p === 'object' && p !== null && 'Present' in p) return null;

  if (!p) {
    return {
      kind: 'not-connected',
      reason:
        `${timer.name} hasn’t been connected yet, so Grid can’t tell whether it’s running the ` +
        'GridFPV plugin. Connect it first, then tick it for this event.',
      alreadySelectedWarning:
        `This event races ${timer.name}, but Grid hasn’t confirmed its GridFPV plugin. Connect ` +
        'it — arming a heat is refused until it answers.'
    };
  }

  if (p === 'Missing') {
    return {
      kind: 'plugin-missing',
      reason:
        `${timer.name} isn’t running the GridFPV plugin, which Grid requires to race a ` +
        'RotorHazard timer. Install it, restart RotorHazard, then tick it for this event.',
      alreadySelectedWarning:
        `This event races ${timer.name}, but its GridFPV plugin has gone away. Reinstall it and ` +
        'restart RotorHazard — arming a heat is refused until it’s back.'
    };
  }

  return {
    kind: 'plugin-incompatible',
    reason:
      `${timer.name}’s GridFPV plugin speaks a protocol this Director doesn’t. Update it, ` +
      'restart RotorHazard, then tick it for this event.',
    alreadySelectedWarning:
      `This event races ${timer.name}, but its GridFPV plugin no longer matches this Director. ` +
      'Update it and restart RotorHazard — arming a heat is refused until it does.'
  };
}
