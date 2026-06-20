/**
 * @gridfpv/components — shared GridFPV Svelte 5 component library.
 *
 * Race-domain widgets built once and themed per surface (RD console, spectator
 * PWA, OBS overlays), per docs/clients.html §3 ("one component, three
 * presentations") and §5 (one token set, three contexts). Every component takes
 * **typed projection data from `@gridfpv/types`** as props — never a hand-written
 * wire shape — and styles itself only through the design tokens in `tokens.css`,
 * so a surface re-themes the whole set by overriding CSS custom properties.
 *
 * The library is framework-pure: it depends on `@gridfpv/types` (types only) and
 * Svelte, never on `@gridfpv/protocol-client`. Apps wire data in.
 *
 * Design tokens ship as a stylesheet a surface imports once:
 *   import '@gridfpv/components/tokens.css';
 */

// Race-domain components
export { default as Leaderboard } from './Leaderboard.svelte';
export { default as StandingsTable } from './StandingsTable.svelte';
export { default as BracketTree } from './BracketTree.svelte';
export { default as HeatSheet } from './HeatSheet.svelte';
export { default as RaceClock } from './RaceClock.svelte';
export { default as PilotCard } from './PilotCard.svelte';

// Base UI primitives (framework-pure, token-styled) — the design-system kit the
// whole console inherits (#71, Slice 0). See ./primitives/*.
export { default as Button } from './primitives/Button.svelte';
export { default as Field } from './primitives/Field.svelte';
export { default as Input } from './primitives/Input.svelte';
export { default as Select } from './primitives/Select.svelte';
export { default as Card } from './primitives/Card.svelte';
export { default as Badge } from './primitives/Badge.svelte';
export { default as StatusPill } from './primitives/StatusPill.svelte';
export { default as Tabs } from './primitives/Tabs.svelte';
export { default as Banner } from './primitives/Banner.svelte';
export { default as Dialog } from './primitives/Dialog.svelte';
export { default as ToastHost } from './primitives/ToastHost.svelte';
export { toast, toasts, type Toast, type ToastTone } from './primitives/toast.svelte.js';

// Presentational helpers (pure, framework-agnostic)
export { formatClock, formatMicros, formatMetric, medalFor } from './format.js';

// View-model types
export type { Bracket, BracketRound, BracketMatch, BracketSlot } from './bracket.js';
