# GridFPV — Project Rules

Standing rules for working in this repo. These bind every change (human or agent).

## Display: always use the friendly name, never a raw id

**Anywhere a value that has a human-friendly name is shown to a user, render the
friendly name — never the raw unique id / ref / uuid / node seat.**

This applies to *every* surface: tables, lists, dropdowns (the visible label —
the option `value` may stay the raw ref), page headers, section legends, audit /
log lines, graph and chart labels, toasts, tooltips, confirmation dialogs, and
error messages.

Go through the **shared resolvers** — never re-derive a name inline (that's how
they drift):

- **Competitor ref → callsign:** `createCompetitorNameResolver`
  (`frontend/apps/rd-console/src/lib/competitorName.ts`)
- **Heat id → "‹Round› Heat N" / main tier / custom label:** `heatNameById`
  (`frontend/apps/rd-console/src/lib/heats.ts`)
- **Pilot id → callsign**, **round id → round label**, **class id → class name**,
  **frequency/channel → band+channel label** (`channels.ts`) — same principle; use
  the existing helper, or add one.

Rules of thumb:

- Raw ids/refs are **wire handles only**. The UI layer resolves them before display.
- Resolve from a **durable source** (the entity's own record / a projection), not
  just live/current state — so **finished, non-current, or not-yet-running**
  entities still resolve (e.g. marshaling a *finished* heat must still show
  callsigns; a node-seeded heat must resolve `node-0` → the bound pilot).
- A resolver may fall back to the raw ref as a **last resort**, but any **new**
  display that shows an entity MUST go through the resolver, not print the id.
- When you add a new entity type that has an id **and** a name, add/extend a
  resolver and use it everywhere that entity is shown.

*Why this is a rule:* friendly-name leaks have been a recurring bug class —
Live control, the global header, marshaling (audit / ruling / protests / add-lap),
graph labels, and heat names have each leaked raw ids at some point. Treat a raw
id reaching the screen as a bug.
