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

## Talking to RotorHazard: read its source, and prove the write landed

RotorHazard is a **foreign codebase we do not control**, reached over a fire-and-forget
socket.io channel. Both halves of that sentence have cost us real bugs.

**Never infer a handler, field or constant from its name — open `server.py` and read it.**
Plausible-sounding names that do not exist, or do not mean what they say, have shipped
more than once. The reverse error is just as expensive: *asserting* that a name is
wrong, four separate times, when it was fine all along. **A claim about RotorHazard's
vocabulary is itself a claim — verify it before acting on it, in either direction.**

**A socket emit has no failure signal.** RotorHazard does not ack, and a handler that
raises aborts silently — so the Director happily answers `200` for a write that never
landed. `on_set_frequency` runs `int(data['channel'])` unguarded; sending the catalog's
code `"R8"` raised `ValueError`, killed the handler, and left the gate on the old
frequency while every layer above reported success. So:

- **Every write gets a readback** — re-read the state RotorHazard reports and confirm it
  matches what we sent, or surface the mismatch. `alter_heat` (the seating write) shipped
  with no readback and silently dropped laps.
- **A value that cannot be confidently translated is omitted, never guessed.** Losing a
  cosmetic label is fine; losing the write puts a pilot on the wrong channel.
- **Verify on both 4.3.0 and 4.4.0.** The payloads differ (`cap_enter_at_btn` takes
  `node_index`, not `node`), and some handlers exist on only one.

The translation layer is `crates/adapters/src/rotorhazard/transport.rs`, and it carries
the receipts in its doc comments. Add to those comments when you learn something new
about RH's behaviour — that file is the record.

## The gates, and what they cannot see

`cargo xtask ci` = `fmt` → `lint` → `test` → `live-check` → `gen` → `barrel`. The
frontend adds `build`/`check`/`lint`/`test`/`contract`, and `cargo xtask e2e` runs the
browser suite in Microsoft's Playwright image. **CI runs all of it**, including the e2e
job (~20 min).

Each of those last few gates exists because something slipped through, and the pattern
is always the same shape — *a check that could not see the thing it was supposed to check*:

- **`live-check`** — the live-matrix targets are `#[ignore]`d, so `cargo test` never
  *compiled* them. A struct gained a field and leg 1 died 477 s into a matrix run.
- **`barrel` + `wire-shape.ts`** — a screen hand-declared a wire type instead of importing
  the generated binding. Every field name was wrong; it type-checked perfectly.
- **the e2e job** — 46 specs, 38 of them dead against a nav rename nobody noticed, because
  the suite only ever ran locally on demand.

Two rules follow:

- **A test that passes for the wrong reason is worse than no test.** Two have asserted a
  bug as expected behaviour and had to be deleted outright. When a test disagrees with
  reality, work out which one is wrong *before* editing the test to agree.
- **A type change that still type-checks can still break everything.** `node_count` went
  from "the timer's width" to "the RD's override, usually null"; four call sites read
  `?? 0` and quietly became "this timer has no seats". Renaming the field would have
  caught all four — see the pre-release section above; that is what it is for.

## Push, then watch the run finish

**Do not report a push as done until GitHub Actions is green.** Local gates are not CI:
the e2e job in particular has only ever been green on CI since #428, and a red run mails
the maintainer. `gh run list --branch devel --limit 5` — and if a merge is knowingly
riding over a failing job, **say so in the same breath as the push**, rather than letting
it arrive as a surprise email.
