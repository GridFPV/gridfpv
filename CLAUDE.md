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

- **Competitor ref → callsign / node seat:** `buildCompetitorNames`
  (`frontend/apps/rd-console/src/lib/competitorName.ts`) — hand it the sources a
  screen has (pilots, live progress, the heat, the timer, the live signal, class
  membership, the channel catalog) and consume its `name` / `channelFor` /
  `seatLabel`. **One place builds the inputs, too** (#416): the resolver was
  already shared, but three screens each assembled its inputs and answered
  `node-6` on one screen against `Node 7` on another for the same seat. Do not
  call `createCompetitorNameResolver` directly — that is the rule, not the entry
  point.
- **Heat id → "‹Round› Heat N" / main tier / custom label:** `heatNameById`
  (`frontend/apps/rd-console/src/lib/heats.ts`)
- **Pilot id → callsign**, **round id → round label**, **class id → class name**,
  **frequency/channel → band+channel label** (`channels.ts`) — same principle; use
  the existing helper, or add one.

Rules of thumb:

- Raw ids/refs are **wire handles only**. The UI layer resolves them before display.
- **Never re-derive a resolver's *inputs* inline either.** A shared resolver fed
  three different input sets is three resolvers. If a screen needs data the shared
  builder does not carry, extend the builder.
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

## Pre-release: break things freely, and do not write migrations

**GridFPV has never been released publicly.** There are no users but the maintainer,
and no deployed data anyone depends on. Until the first public release, a breaking
change is **free** — and paying for backwards compatibility is a real cost with no
benefit.

So, by default:

- **Rename badly-named things properly.** Do not keep the old name as an alias.
- **Change a wire type's shape or meaning** when the new one is right.
- **Do not write compatibility shims, dual-reads, old-key fallbacks or data
  migrations.** A stored record in the old shape may simply be lost; the maintainer
  recreates it.
- **Do not add a "legacy" branch** to keep an old client working. There are none.

The maintainer will say so when they want data preserved. Ask only when the data is
*theirs and expensive to recreate* (a tuned timer's calibration, a raced event's log)
— not for test fixtures, scratch events, or config they can re-enter in a minute.

### What this does NOT excuse

- **A stored record in an old shape must still LOAD.** Losing the field is fine;
  failing to open the event, or 500ing, is not. `#[serde(default)]` on the new field,
  and an unknown old key is ignored on read.
- **Deleting anything the maintainer owns and cannot recreate.** Preserve it or ask.
- **Silence.** If a change drops data, say so plainly in the report — "a test event
  needs recreating" is a fine outcome, discovering it in the field is not.

### When this flips

At the **first public release**. From then on: additive changes, `#[serde(default)]`
on every new field, and a real migration for anything that moves. Delete this section
when that happens — a stale "break things freely" rule is exactly the kind of note
that outlives its truth and does damage.
