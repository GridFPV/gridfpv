/**
 * @gridfpv/components — shared GridFPV Svelte 5 component library.
 *
 * The design-system kit the console is built from: token-styled primitives (buttons, fields,
 * cards, dialogs, pills, toasts) plus a few pure display pieces. Every component styles itself
 * only through the design tokens in `tokens.css`, so a surface re-themes the whole set by
 * overriding CSS custom properties.
 *
 * The library is framework-pure: it depends only on Svelte, never on
 * `@gridfpv/protocol-client`. Apps wire data in.
 *
 * Design tokens ship as a stylesheet a surface imports once:
 *   import '@gridfpv/components/tokens.css';
 */

// Display components
export { default as RaceClock } from './RaceClock.svelte';

// Base UI primitives (framework-pure, token-styled) — the design-system kit the
// whole console inherits (#71, Slice 0). See ./primitives/*.
export { default as Button } from './primitives/Button.svelte';
export { default as Field } from './primitives/Field.svelte';
export { default as Input } from './primitives/Input.svelte';
export { default as Select } from './primitives/Select.svelte';
export { default as Card } from './primitives/Card.svelte';
export { default as Collapsible } from './primitives/Collapsible.svelte';
export { default as Badge } from './primitives/Badge.svelte';
export { default as InfoTip } from './primitives/InfoTip.svelte';
export { default as StatusPill } from './primitives/StatusPill.svelte';
export { default as Tabs } from './primitives/Tabs.svelte';
export { default as Banner } from './primitives/Banner.svelte';
export { default as Dialog } from './primitives/Dialog.svelte';
export { default as ToastHost } from './primitives/ToastHost.svelte';
export { toast, toasts, type Toast, type ToastTone } from './primitives/toast.svelte.js';

// Presentational helpers (pure, framework-agnostic)
export { formatClock, formatMicros, medalFor } from './format.js';
