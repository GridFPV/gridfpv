# Grid as the portable race manager — November 2026

> **Old, kept for history (2026-11-09).** This proposal, with Ryan's notes and answers below,
> led to [Grid primitives](primitives.md), which is the current design. Its advice to adopt
> grid-rh's vocabulary (stages, formats, seats) was rejected. Its OFT adapter design (section 4.3)
> is still the working plan for that adapter.

**Status:** architecture review and proposal. Nothing here is built and nothing is decided;
Ryan decides. Written 2026-11-08 from the OFT spec v0.3, the `oftd` reference Timer, the
`rh-oft` RotorHazard plugin, Grid's design docs and crates, and `gridfpv-rh` at plugin 0.11.2.

**Why:** RaceGOW should run with **a laptop, an OFT Timer gate and a PoE switch** — no
RotorHazard Pi. `gridfpv-rh` is very solid now; Grid proper should absorb what it learned. This
doc says what that means for Grid's structure and definitions, in what order, and how the OFT
adapter should be built.

**The one-line recommendation:** keep Grid's spine (append-only log, pure adapters, pure
folds, server-time race clock) exactly as it is; replace the *vocabulary* and the *UI shape*
above it with `gridfpv-rh`'s (stages, seats, saved races, one Run page, Marshal as a to-do list);
add OFT as the first timer that is an instrument and nothing more, with the in-process
`oft-core` host for the laptop-only bench case and the network adapter for the gate box.

//NOTE: I dont know how I feel about the vocab changes. I an fine with RH vocab in RH, but think its not that accurate. We should talk more about vocabulary.

---

## 1. The portable race day, as requirements

The scenario is Ryan in the house or at a field with one laptop, one gate, one switch. Everything
below is a requirement on Grid, not a feature idea.

//NOTE: We are forgetting that we will also have 1 timer I think, we need 1 OFT Timer.

### 1.1 Setup

| Requirement | Target | Why |
|---|---|---|
| Time from power-on to "the gate is reading" | under 2 minutes, no typing an IP | A Pi-less day has no second screen to debug on |
| Timer discovery | mDNS `_oft._tcp` browse, list by `id`/`fw`/`seats`/`level` TXT (OFT §6.1); manual URL as the fallback | DHCP on a dumb switch gives the Timer an address nobody knows |
| Pilots | the persistent roster; one click to put yourself on seat 0 | Re-adding pilots between events was the standing complaint on RH (pivot note, 2026-10-20) |
| Channel | set on the seat; written to `/config` `seats[].freq_mhz` and read back | Same shape as grid-rh Seats; the Timer is *applied to*, never consulted (D27) |
| Thresholds | `auto` mode by default (`snr_enter` 15 dB, `hysteresis` 5 dB per OFT §8); `manual` only after a Tune capture | OFT's noise-floor tracking removes the Enter/Exit calibration ritual that cost every RH session |
| Event shape | one event, one stage, one heat, unlimited rounds, ranked by best 3 consecutive | How Ryan actually times RaceGOW today on recon-nh |

//NOTE: I think the MDNS is good when we can rely on DHCP. 

### 1.2 Offline, one laptop

- **No internet, no Pi, no second process.** The Director binary is the race manager, the
  server, the UI and (for a USB-dongle Node) the Timer too. mDNS browsing must not need a
  daemon the laptop may not have.
- **Power loss is normal.** Grid's log-on-disk already survives this (Architecture §3). The
  Timer's `boot_id` changes on power loss; the adapter must treat that as "sequence and clock
  start over", not as an error (`rh_oft/interface.py::_connect`).
- **One browser window.** Run, Marshal, Results reachable without a nav tour. grid-rh's "Mode"
  idea (Full / Simple / RaceGOW) is the right lever; the RaceGOW mode's *vocabulary*
  (seasons/tracks/runs) is not — see §2.5.

//NOTE: We also need to count that we will have an OFT Timer.

### 1.3 Timing

- **Laps from Timer timestamps only.** `t_mid` is the lap instant (OFT §8). Arrival time
  affects latency, never a lap time. Grid already states this (Timer Adapters §4); the OFT
  adapter must honour it and never substitute `recorded_at`.
- **Catch-up after a drop.** `/crossings?since=<seq>` or the WebSocket `sub.crossings.since`
  replays up to 256 crossings (`oftd::timer::CROSSING_REPLAY`). A Wi-Fi laptop that sleeps for
  10 s loses nothing.
- **Marshaling from Timer sample history.** OFT L3 keeps full-rate samples (`oftd` keeps
  600 s per seat, `HISTORY_S`). Grid must be able to fetch `/samples?seat=&from=&to=` for a
  saved run and re-detect laps against it, the way grid-rh Marshal recalculates against RH's
  `history_values`.
- **Hole shot and short laps are Grid's rules.** `min_lap_ms` on the Timer is a dedupe guard,
  not scoring. Grid's D26 floor and #517 bounce window stay the enforcement point.

### 1.4 Overlays and video (if RaceGOW uses them)

- The solo race overlay (`/grid/live/race` in grid-rh; `#/overlay/race` in Grid) must follow
  the active heat from the served lap list, never accumulate client-side (racegow memory).
- Replay-by-race-time (`?heat=&round=&t=`) and the video companion's render hook
  (`window.gridfpvReplay.seek(ms)`) are grid-rh features Grid does not have. They are needed
  only if RaceGOW video stays on the overlay-render path; they are not timing requirements.
- Overlay placement is server-held state, not browser-local (grid-rh Overlays help). Grid's
  overlay has a contrast picker only.

//NOTE: Overlays have been in the plan. I want simple integration at first, but in the future, we should provide a cool modular system that people can use to pull infor for overlays to build custom ones.

---

## 2. Grid's current structure and where it lags how races are actually run

### 2.1 The spine is right; keep it

Nothing below touches these, and they are the reason to come back to Grid rather than keep
stacking on RotorHazard:

- **Append-only log, projections, replay** — `crates/events`, `crates/projection`,
  `crates/storage` (Architecture §3). grid-rh has *none* of this: a saved race is a mutable
  RotorHazard row, deletes are a snapshot into a plugin option (`plugin/gridfpv/races.py`),
  and the audit trail is whatever RH logs.
- **Pure adapters with capabilities** — `crates/adapters/src/lib.rs` (`Adapter::translate`,
  `Capabilities`). OFT's `capabilities` object in `/info` maps onto it one for one (§4.3).
- **Source time for intervals, session time for ordering** — `crates/adapters/src/clock.rs`.
  OFT's clock model (§7 of the spec) is exactly this; the RotorHazard adapter never got to use
  it properly because RH's lap stamps are race-relative milliseconds.
- **Server-time race clock** from `recorded_at` on `Armed → Running` (Live State §2).
- **Timers are instruments** — D26 and D27. OFT was written to this principle (open question 9
  of the spec, "always emit and let the Race Manager filter").

### 2.2 The heat lifecycle: six states where race day uses three

Grid: `Scheduled → Staged → Armed → Running → Unofficial → Final`, plus `Abort`, `Restart`,
`Discard`, `Revert`, `Advance` (`crates/engine/src/heat.rs`, Heat Lifecycle §1–2). grid-rh's
Run page, after five tiers of race-day rehearsal: **Start, Stop, Save, Discard**, with Space
cycling Start → Stop → Save (`docs/help/run.md`).

What the field taught:

- `Staged` as a separate RD step buys nothing for one laptop and one gate. Staging in grid-rh is
  the countdown *inside* Start. In Grid it exists so IRL can assign frequencies; with D27 and
  OFT, channels are applied to the Timer when the heat is **picked**, not when it is staged.
- `Unofficial → Final` is a real idea (defensible results, Vision §7) but it has never been
  exercised at an event: RaceGOW has one pilot and no protests. grid-rh's "Save" is the commit;
  "Needs marshal" plus a note (#109) is the provisional marker, and saving laps from Marshal
  clears it. That is the lifecycle a race director actually wants: **saved, flagged, fixed**.
- `Abort`/`Restart`/`Discard` all land on `Scheduled` (D2). grid-rh has one: Discard (asks
  first when there are laps). Nobody has needed the distinction between a Restart and an Abort.

**Lag, concretely:** `LiveRaceControl.svelte` exposes Stage/Start/Stop/Finalize/Revert/
Discard/Advance; `Run.svelte` in grid-rh exposes Start/Stop/Save/Discard and is done with the
night. Grid's FSM is correct and replayable; its *surface* is the wrong shape.

### 2.3 Rounds and heats: Grid's generator model versus "a stage is a class with heats"

Grid: event → class-tagged **rounds** (`RoundDef` with format key, win condition, seeding rule,
channel mode, grace window, protest window) → heats emitted by a generator
(`crates/engine/src/{format,timed_qual,head_to_head}.rs`, Race Engine §3, Format Model). The
console shows this as `EventRounds.svelte` with an Add-round picker of Practice / Time Trial /
Head-to-Head.

grid-rh: event → **stages** (one RotorHazard class each) → **heats** on a board of rounds, with
rounds either *per heat* or *grouped*, a template picker (practice, qualifying, bracket,
Chase the Ace, blank), seats that hold a pilot *or* a seeding rule, and `Seed now`
(`docs/help/stages.md`, `frontend/.../lib/stages.ts`, `heatplan.ts`, `bracket.ts`).

//NOTE: I dont like the RH structure of stages, heats, rounds. Its just what we had to do for grid-rh

Where Grid lags:

- **The board.** The single most-used race-day view in grid-rh is rows = heats, columns =
  rounds, cell = winner or "next". Grid has no equivalent; its heats list (`GET /heats`,
  `HeatSummary[]`) is flat.

//NOTE: This is probably a lie, I like that feature, but I have only used grid-rh for racegow which I do not use that feature at all.

- **Open-ended rounds.** RaceGOW is "fly this heat again until I'm done". grid-rh models it as
  one heat, unlimited rounds, advance = stay. Grid models it as Time Trial with `rounds = 0`
  and needs a race-time ceiling to be allowed a ranking-only round (racegow memory: "the
  Director refuses a ranking-only round with no race time"). That refusal is wrong for the
  portable case.

//NOTE: I think we need to go back to primitives

- **Seeding is a seat property in grid-rh, a round property in Grid.** grid-rh: a seat in a
  heat holds a pilot or "from heat X position N / from class ranking N"; Grid: `RoundDef.seeding`
  is one rule for the whole round, frozen at first fill (D8 update, `RoundFieldDrawn`). The
  seat-level model is what RDs understand and what RH's heat plan shows; Grid's is more
  principled and less usable.
- **Chase the Ace** is a shipped stage type in grid-rh (`lib/ace.ts`). In Grid it was carved to
  `tournaments-snapshot` (D17 update). Same for brackets.
- **Formats.** grid-rh has a Formats page: a named thing with clock, win condition, staging
  tones, points table, reused across stages. Grid spreads the same knobs across the round
  (`win_condition`, `start_procedure`, `grace_window`, `time_limit_secs`, scoring table). A
  reusable, named **Format** entity is missing and is the thing Ryan asked for first when he
  named "event/class/heat/round management" as the real pain (direction note, 2026-10-26).


  //NOTE: Key thing for grid, we build the base minimal primitives, everything else is build on top of those primitives. RH does not follow that philosophy in my opinion.

### 2.4 Seats, channels and calibration

Grid's channel model went through three revisions (`channel-model.html` and its #117 S1–S3
header): a global allowed set on the timer, per-event **layouts**, per-heat layout binds,
`HeatLayoutSet`, `HeatSeatingOverridden`, `Timer::seat_nodes`. It is correct and nobody can
hold it in their head. The five-bug trail at the top of that page ("empty means unrestricted,
never none") is the cost.

grid-rh's model: a **seat** has a channel and two thresholds inside a **profile**; a **preset**
is channels-on-seats; a heat seats pilots on seats; a pilot on an `Off` seat is flagged on Run.
That is RotorHazard's model, and Ryan's verdict on it was "RH's tuning, seats and marshalling
actually work well" (direction note). Grid should adopt it.

//NOTE: I do think the RH model here makes sense. Though I am not sure I like the seat vocabulary, it could stick around. 

Calibration: Grid's `TunePage.svelte` and the capture path were built against RH's socket
(`cap_enter_at_btn` traps in `rotorhazard-plugin.html` §2). grid-rh's Tune page records a seat
for as long as the director wants, draws the trace, and nudges enter/exit against it
(`plugin/gridfpv/capture.py`, `docs/help/seats.md`). OFT's `/samples` makes grid-rh's shape
trivial to reproduce and RH's 3-second capture window irrelevant.

//NOTE: The grid-rh tuning tis the way to go, I really like that.

### 2.5 RaceGOW: Grid dropped the mode; grid-rh re-grew one

Grid removed its RaceGOW preset on 2026-10-12 (`20b58e2`, racegow memory: "a normal event is
better, keep the overlay"). grid-rh then shipped **RaceGOW mode** (#95): seasons = databases,
tracks = classes, runs = rounds, a seat card on Run, hidden pages, a name template that also
names the video file (`plugin/gridfpv/racegow.py`, `lib/racegow.ts`, `docs/help/racegow.md`).

These two are not actually in conflict, and the difference is instructive. Grid's mode failed
because it was a *different event kind* with its own screens. grid-rh's mode works because it is
a **vocabulary and a nav filter** over the ordinary model (a class with one heat, unlimited
rounds), plus one real feature: the run **name template** that ties the timer, the overlay and
the recording together. The memory rule "do NOT reintroduce a RaceGOW preset or per-competition
event kind" stands; what Grid should take is the **mode-as-filter** and the **name template**,
not a kind.

//NOTE: I think grid we need to go back to the basics of build the primitives and then build everything else on top of that. In that sense. I do think we could/should have customizable simplified UI modes. Maybe we start with non-customizable, but a basic set, and then in the future allow customization for custom modes.

### 2.6 Marshaling: Grid's is deeper, grid-rh's is more usable

Grid: re-detection over the captured trace, void/insert/adjust/split rulings, the shared
removal record (D25), min-lap floor (D26), protests, audit (`Marshaling.svelte`,
`crates/projection::corrected_and_voided_passes_with_floor`). grid-rh: a race picker that is a
**to-do list** (dots for "Needs marshal"), drag enter/exit, laps re-found from the recording,
delete/restore/manual lap/clean, save, reassign (`docs/help/marshal.md`).

Grid's fidelity caution (Marshaling §4: the trace must match what the detector saw) was never
satisfiable on RotorHazard, whose streamed history is peaks and nadirs (`capture.py` docstring
explains why the trace "zigzags"). OFT L3 history **is** the detector's input, so the caution
is satisfied by construction. That alone justifies putting Grid's marshaling model back in
front of a real timer.

//NOTE: we have to make sure grid can work across all timers,

What Grid lacks is the *workflow*: a flag at save time, a note, a picker that shows what still
needs doing, and "Load from seat / Use these thresholds on the seat" to close the loop back to
calibration.

### 2.7 Results, persistence, roster

- grid-rh's Results page is RH's boards (stage summary, heat summary, event totals) and matched
  RH cell for cell (T4). Grid's `Results.svelte` serves round ranking and class standings and has
  never been checked against anything.
- Roster: Grid has an app-level pilot directory (`GET /pilots`) that survives events — exactly
  the thing RH lacks and grid-rh had to bolt on (`roster.py`, "Add from roster"). This is a Grid
  strength to keep.
- Teams exist in grid-rh (`teams.py`) and not in Grid.

### 2.8 Small race-day rules grid-rh has and Grid does not

These are cheap and each cost a live session to learn:

- **Ignore crossings before** (RH's "minimum first crossing"): a crossing within N seconds of
  the start is not the hole shot. Grid has the min-lap floor (D26) and the bounce window
  (#517) but no start-side rule. It belongs beside them in the corrected-passes fold, as a
  Format field.
- **Restart point** (rh#106): on a small track a pilot who clips a gate flies back through the
  start gate. That crossing is a *restart*, not a lap, and the best-3-consecutive window must
  not span it. RotorHazard has no state for it; Grid should: a marshal ruling
  `RestartMarked { target }` that the consecutive-window scorer treats as a window boundary.
  This is exactly the kind of fact the append-only model is for, and it is a RaceGOW rule.
- **Needs marshal flag and note** on a saved race (rh#109), cleared by saving laps from Marshal.
- **Scheduled start** and **broadcast messages** on Run (#71), and the keyboard set
  (Space / z / x / s / d-twice).
- **Audio settings as a dialog on Run**, exported and imported as a file.
- **Adaptive calibration and start-of-race lowering** were RotorHazard features grid-rh
  exposed. With OFT `auto` thresholds they are unnecessary; with `manual` they are Grid's to
  own under D27. Do not port them until a manual-threshold session asks for them.

### 2.9 Definitions that should change

| Today in Grid | Should be | Why |
|---|---|---|
| **Round** (class-tagged, format key, seeding, channel mode, lifecycle knobs) | **Stage** (format reference, rounds mode, ranking) holding **heats**; **Round** becomes the ordinal of a race within a heat | grid-rh's words; the board needs rows = heats, columns = rounds |
| Format knobs spread on `RoundDef` | **Format**: a named, reusable entity (win condition, race time, staging/start procedure, grace, points) | The first thing RDs build and reuse; grid-rh Formats page |
| **Competitor ref** `node-N` + **CompetitorRegistered** binding | **Seat** (index, channel, thresholds) as a first-class entity on the timer; heat seats a pilot on a seat | RH model that "works well"; OFT `seat` maps to it directly |
| **Channel layout** + heat layout bind + seating override (three events) | **Profile** (seat → channel + thresholds) + **Preset** (channels only) | Fewer concepts; what grid-rh ships |
| `Unofficial` / `Final` / `Revert` as the RD's lifecycle | **Saved** + **Needs marshal** flag + note; `Final` stays as a lock reachable from Results, not from Run | What race day used |
| `Staged` as an RD step | Staging countdown inside **Start**; channels applied on heat pick | One button fewer, D27-consistent |
| "Timer" = `Mock` \| `Rotorhazard{url}` | `Mock` \| `Rotorhazard{url}` \| `Oft{url}` \| `OftLocal{serial}` | §4 |

The FSM underneath can keep all six states. The change is in what the console names and
offers, and in adding the two entities (Format, Seat) the log vocabulary lacks.

//NOTE: I think we need to go deeper on this. I want to deep dive into what are the primitives we need/want in grid that we can ubuild all other event types from. We talked about this before.

---

## 3. grid-rh lessons to carry over

### 3.1 UX flows that are proven

1. **Run is race day in one screen** (`run.md`): heat picker at the top, race settings strip
   underneath (profile · min lap · format, each a link), Start/Stop/Save/Discard with keys,
   seat cards with `+ Lap` and clickable lap chips, leaderboard, Last race card with "Next: …"
   and the Needs-marshal flag. Reload mid-race resumes cues without replaying them (#73).
2. **The Stages board** (`stages.md`): fold finished stages, click a raced cell for result /
   re-run / delete (kept copy), the next-round cell loads the heat, "next" marked.
3. **Marshal as a to-do list** (`marshal.md`): dots in the picker, note under it, Save clears.
4. **Seats page**: profile bar, calibration panel, Channels row with presets and an IMD badge
   (`lib/imd.ts`), one card per seat with a live bar and enter/exit marks, Tune per seat, and
   **refusals with the reason** while a race is on (#87). Every write is confirmed by readback.
5. **Help ships inside the app and works offline**, one `?` per page.
6. **Mode** as a nav filter, one setting for the whole timer.
7. **Sound owned by the race manager**, not the timer: staging tones, start tone, lap callouts,
   expiry, all from the page (#38, #72). Grid already decided this (D4); grid-rh proved the
   settings UI it needs.

### 3.2 Data model decisions worth keeping

- **A saved race is the unit of results, and it can be deleted with a kept copy and
  restored** (`races.py`). In Grid this is `Discard` + the log, which is strictly better, but
  the *surface* ("Deleted races", Restore) is what the board needs.
- **The run name template** `{season} {track} Round {round}` lives with the event and names
  the overlay title and the recording. Keep it as an event-level setting in Grid, not a mode.
- **Teams** and **roster** are separate from the event's pilots.
- **Seats, not pilots, are the timing-side identity.** Passes land on seats; the heat decides
  who was on the seat. Grid's `CompetitorRef("node-N")` already is this; stop treating the
  binding as an exceptional registration step and make the heat lineup the binding.

### 3.3 Testing approach

grid-rh's gate per tier is: lib unit tests (vitest) + plugin unit tests (unittest) + a browser
e2e against the rig **that ends by asking RotorHazard whether the write landed**
(`frontend/e2e/check-agrees.mjs` enforces it), + a rehearsal checklist run twice (rig, then
live) + a live log issue per real session. Three things transfer to Grid:

1. **"The instrument agrees" as the e2e exit criterion.** For OFT: every e2e that writes
   config ends with `GET /oft/v1/config` and asserts the Timer holds it.
2. **Replay-testable adapters** (Grid already has this, Testing Strategy §2–3) plus a **fake
   Timer**: `cargo run -p oftd -- --fake` is the OFT counterpart to the dockerized RH, far
   cheaper, and it is deterministic (`oft-sim` scenes). The adapter's golden cases should be
   recorded against it, and the Grid e2e rig should launch it.
3. **A rehearsal checklist per area with a "without opening the other tool" pass line**, and
   a live log per session with nothing fixed mid-session except blockers (`live-log.md`).

What not to carry: the socket-emit audit discipline (`rotorhazard-plugin.html` §4b) exists
because RH has no acks. OFT's `PUT /config` returns the applied config; the WebSocket `sub`
gets an `ack`. The readback rule still applies, but it is one HTTP GET, not a forensic exercise.

---

## 4. Proposal

### 4.1 What to change in Grid, in order

Each step is a vertical slice with its own gate, as the direction note demands. Order is by
what RaceGOW needs first.

**Step 0 — OFT adapter, network transport, Mock-parity (2 weeks).**
`crates/adapters/src/oft/` as a pure translator over decoded OFT JSON (`protocol.rs` shapes),
plus `crates/app/src/source/oft.rs` as the live driver. Capabilities L1 → `LivePasses`,
`FrequencyMgmt`, `Calibration`; L2 adds `SignalContext`; L3 adds `SignalRecovery`. Timer kind
`Oft { url }` in `crates/server/src/timers.rs`. Gate: a heat runs end to end against
`oftd --fake` from the existing `LiveRaceControl`, laps have Timer timestamps, a kill and
restart of `oftd` mid-heat loses no crossing. No UI change yet. Design in §4.3.

**Step 1 — Seats and profiles replace layouts (1–2 weeks).**
New entities `Seat` (index, channel MHz, enter/exit or `auto`) and `Profile` on the timer;
`Preset` as channels-only. Delete `HeatLayoutSet`, `HeatSeatingOverridden`, channel layouts and
`available_channels` (pre-1.0, no migration, per CLAUDE.md). A heat's lineup is `(seat, pilot)`
pairs and is the binding; `CompetitorRegistered` goes. Seats page ported from grid-rh
(`Seats.svelte`, `SeatTune.svelte`, `lib/seats.ts`, `lib/tuning.ts`, `lib/imd.ts`). Gate: the
T5 checklist from `gridfpv-rh/docs/rehearsal.md` passes against `oftd --fake` with "the Timer's
`/config` agrees" replacing "RotorHazard agrees".

**Step 2 — Format and Stage replace RoundDef; the board (2–3 weeks).**
`Format` entity (win condition, race time, staging secs, start procedure, grace, points table,
min lap). `Stage` = format ref + rounds mode (per heat / grouped) + ranking + heats. `Round` is
the ordinal of a race in a heat. Seat-level seeding (pilot or "from stage/heat/position").
Port `Stages.svelte`, `Formats.svelte`, `lib/stages.ts`, `heatplan.ts`, `rounds.ts`,
`ace.ts`, `bracket.ts`. Chase the Ace and single-elim come back as stage templates over
Head-to-Head, which is the D17 tenet made concrete. Gate: the T2 checklist.

**Step 3 — Run page, race-day complete (1–2 weeks).**
Start/Stop/Save/Discard with keys; staging inside Start; "Needs marshal" flag and note on
save; Last race "Next: …"; audio settings dialog. The FSM keeps its states; `Save` =
`Finalize`, `Discard` = `Discard`, `Revert` is reachable from Marshal. Gate: the T3 checklist,
with the OFT fake emitting the laps.

**Step 4 — Marshal workflow over Grid's marshaling (1 week).**
Picker-as-to-do, note, "Load from seat / Use on seat", `/samples` fetch for a saved run into
`SignalHistory`, re-detection using **OFT's `edge_mid` rule** (§4.3.5), not RH's plateau rule.
Gate: the T4 checklist.

**Step 5 — Mode, help, overlay placement, name template (1 week).**
Mode as a nav filter (Full / Simple); in-app help pages ported from `docs/help/*.md`; overlay
placement as server state; event-level run name template. Gate: a RaceGOW session timed on
the B1 gate box with nothing but the laptop, logged per `live-log.md`.

**Step 6 — In-process OFT host (`OftLocal`) (1 week, can slide).**
`oft-core::NodeHost` driven from a serial port inside the Director for the USB dongle (B0).
Same adapter, no network. Only after the network adapter is proven.

Explicitly **not** in this plan: Cloud, PWA, MultiGP push, Velocidrone changes, tournament
structures beyond Chase the Ace and single-elim. RotorHazard support stays as it is; grid-rh
keeps running the club nights on recon-nh until Grid+OFT has done a RaceGOW season.

### 4.2 What to delete

Pre-1.0 rules apply. Candidates, all replaced by §4.1:

- `channel-model.html` and the three-scope layout machinery (`HeatLayoutSet`,
  `HeatSeatingOverridden`, `ChannelLayout`, `available_channels`, `Timer::seat_nodes`).
- `CompetitorRegistered` as a separate event (the heat lineup carries the binding).
- The `Staged` RD button and `SkipCountdown` (already wire-only).
- `ProtestWindow` / `HeatFinalizing` from the Run surface (keep in the engine and audit; expose
  from Results if ever needed).
- `EventSetupWizard.svelte` (the Stages template picker replaces it).

### 4.3 OFT adapter design

**GridNode is dropped.** `timer-hardware-research-2026-11.md` §6 sketched a Grid-specific
"GridNode" protocol (mDNS `_gridnode._tcp`, TCP length-prefixed CBOR). OFT v0.3 (HTTP + JSON +
one WebSocket, mDNS `_oft._tcp`, capabilities in `/info`) supersedes it; Grid gets an `oft`
adapter and no `gridnode` adapter. That doc's §6 and §8 step 4 should be marked superseded.

#### 4.3.1 Shape

```
oftd / oft-hub  ──HTTP+WS──▶  source/oft.rs (driver)  ──Raw──▶  adapters/oft (translate)  ──Event──▶  log
                                 │ /time every 1 s                 │ ClockAlign per boot_id
                                 │ /info, /config, /status         │ seat → CompetitorRef("seat-N")
                                 │ WS sub {crossings, status}      │ crossing → Pass
                                 │ /crossings?since on reconnect   │ samples → SignalChunk / SignalHistory
```

The driver owns I/O and reconnect; the translator is pure and replay-testable from recorded
JSON, exactly like `rotorhazard.rs`. `Raw` is an enum over the decoded shapes in
`oftd/src/protocol.rs`: `Hello`, `Crossing(CrossingJson)`, `Samples(SamplesJson)`,
`Status(StatusJson)`, `TimeObs { t_send, t_recv, time_us, boot_id }`, `Info(serde_json::Value)`,
`Disconnected`.

#### 4.3.2 Identity

- **Seat → competitor.** `CompetitorRef(format!("seat-{seat}"))`. Seats are 0-based on the wire
  (`put_config` rejects `seat >= 8`); the UI shows "Seat 1..8" through the shared resolver.
- **Adapter id** = the Timer's `id` from `/info` (`oft-3fa2`), so two Timers at two gates are two
  adapters and dedup keys never collide (`dedup.rs` keys on `(adapter, competitor, seq)`).
- **Pass identity** = `(adapter, competitor, Seq(seq))` with `PassIdentity::Sequence`. OFT's
  `seq` is a true monotonic counter per `boot_id`, not RH's renumbering display ordinal
  (`dedup.rs` "When the sequence is not an identity"), so the default keying is correct here.
  On a `boot_id` change the driver emits `AdapterDisconnected` + `AdapterConnected` and resets
  the dedup window and clock.

#### 4.3.3 Crossing → Pass

```
Pass {
  adapter, competitor: "seat-N",
  at: SourceTime(t_mid),          // µs on the Timer clock, as-is
  sequence: Some(seq),
  gate: GateIndex::LAP,            // one Timer = one gate (split timing later, §4.3.7)
  signal: Some(SignalContext { rssi_peak: Some(peak as f32 / 10.0) }),
  heat: None,                      // the bridge stamps it (D20)
}
```

- `t_mid` is the lap instant. `t_enter`/`t_exit` are kept for marshaling (the pass window)
  and should land on `SignalContext`; extend it with `enter_at`, `exit_at`, `snr`, `flags`
  (all `Option`, additive). `flags` matter: `low_snr` and `truncated` are exactly what a
  marshal wants to see on the removal record.
- Power is tenths of a dB (`peak`, `snr`, `p`, `nf`). Keep the integer tenths in a new field and
  stop pretending RSSI is a 0–255 count: `SignalChunk.rssi: Vec<u16>` and `SignalThresholds
  {enter, exit}: u16` assume RH units. Change them to `i16` tenths of a dB with a per-adapter
  unit tag (`SignalUnit::RhCounts | DeciDb`), which is one `enum` and a rename, not a
  migration (pre-1.0). The RH adapter keeps emitting counts.
- Thresholds: `SignalThresholds` gains `mode: Auto { snr_enter, hysteresis } | Manual { enter,
  exit }` mirroring `ThresholdConfig`. In `auto` mode the effective enter level moves with the
  noise floor, so the marshal view should draw `floor + snr_enter` from `/status`
  `seats[].floor/enter/exit`, not a constant.

#### 4.3.4 Clock alignment

Port `rh_oft/clock.py::ClockAlign` to Rust in `adapters/src/clock.rs` beside `ClockAlignment`:

- One `/time` exchange per second; midpoint assumption; keep an observation only if its RTT is
  within `2 × best + 2 ms`; fit the offset over the fastest quarter of the last 64.
- The alignment maps Timer µs → the Director's `SessionTime`. It is used **only** to order
  passes from two sources and to place the race-go on the Timer's axis. Lap intervals are
  `SourceTime` differences on the Timer clock (`source_interval_micros`), never aligned values.
- Drop a crossing with a log line if the clock is not yet synced? **No.** `rh_oft` drops it
  because RH needs a monotonic stamp; Grid does not. Append the Pass with its Timer `at`
  regardless and let the session alignment catch up. Nothing in the fold needs the alignment
  to be present.
- **The start tone and the Timer's timestamps are on different clocks**, and the console may
  be on a third machine. Grid's answer today is server-time anchoring (Live State §2) for the
  display clock only. With OFT the alignment closes the loop: the console cues the tone from
  `HeatStarting`, the Director records `Running` at `recorded_at`, and the driver maps that
  instant onto the Timer clock (below). A crossing's position relative to the tone is then a
  Timer-clock comparison with about 1 ms of alignment error on a wired LAN (spec §7), which is
  what "crossings before the start are dropped" needs.
- The **race-go on the Timer clock** is what the engine needs for "crossings before the start
  are dropped" and for the Timed window. Record it as a fact when the heat goes `Running`:
  `HeatStarting` already logs `delay_ms`; add `RaceGoAt { heat, adapter, at: SourceTime }`
  emitted by the driver from `align.to_source(recorded_at)` at the `Running` transition. Pure
  folds then compare `Pass.at` to it on one clock.

#### 4.3.5 Reconnect and replay

- Connect: `GET /info` → `hello` on the WebSocket → `sub { crossings: { since: last_seq },
  samples: { seats: "all", decimate: 10 }, status: true }`. The Timer replays missed
  crossings before the `ack` (`api.rs::handle_ws`).
- On drop: backoff like `rh_connections.rs`; status `Disconnected`; on reconnect, `since =
  last_seq` again. If `/info.boot_id` changed, reset `last_seq = 0`, clock and dedup.
- Belt and braces: once a minute while idle, `GET /crossings?since=last_seq` and feed
  anything new through the same translate path. Dedup makes it harmless.
- Replay buffer is 256 on `oftd`; a drop longer than 256 crossings (hours at RaceGOW rates)
  surfaces as a gap in `seq`, and the adapter emits a diagnostic the Run page can show
  ("Timer replay window exceeded; check marshaling").

#### 4.3.6 Samples and marshaling

- **Live** (L2): `samples` frames at `decimate: 10` (100 Hz per seat) → `SignalChunk { from:
  t0, period_micros: dt_us, rssi: p }`. This is already the shape; the fidelity bound in the
  `SignalChunk` doc comment goes away because the Timer owns the full-rate history.
- **Saved run** (L3): at `Finished`, the driver fetches `/samples?seat=N&from=race_go&to=
  t_end` per seated seat and appends one `SignalHistory { base: 0 }` per seat. Full rate,
  bounded by `HISTORY_S` (600 s on `oftd`; MCU Timers keep less, §9.4 of the spec), so fetch
  promptly.
- **Re-detection** must run OFT's `edge_mid` rule, not RH's plateau midpoint. There are
  already two RH-rule detectors in TypeScript (Grid's `redetect.ts`, grid-rh's
  `lib/crossings.ts`); do not write a third. Options, in order of preference: (a) the Director
  runs `oft-core::Detector` over the stored `SignalHistory` behind
  `POST /events/{e}/redetect` and the console previews the result, so the marshal's
  recalculation is byte-identical to the Timer's and works offline on a saved event;
  (b) ask the Timer to recompute, which needs a new OFT endpoint and a live Timer at marshal
  time; (c) compile `oft-core` to WASM for the console later if the round trip is felt. Keep
  `redetect.ts` for RotorHazard timers, gated by adapter.
- `min_lap_ms` on the Timer stays at its default (2000 ms) as a hardware dedupe; Grid's floor
  and bounce window do the scoring-grade filtering (D26, #517). Do not push Grid's min lap to
  the Timer; that is the D27 trap RH taught (#407).

#### 4.3.7 Config writes

- Channel on a seat → `PUT /config` with the whole `Config` (seats, `seats_per_node`,
  `threshold`, `min_lap_ms`). The response is the applied config; compare and surface drift.
- `threshold` is **per Timer**, not per seat, in v0.3 (`rh_oft` works around it by taking seat
  0's). For RaceGOW (one seat) this is fine. For a multi-pilot gate it is a spec gap; flag to
  the OFT side as **per-seat thresholds** before B2/B3.
- Never read `/config` back as truth (D27). On connect, apply Grid's seat config and warn if the
  Timer reported something else beforehand.

#### 4.3.8 Multi-Timer split timing (later)

Two Timers = two adapters with two clocks. Grid already models `GateIndex` and
`Capability::GatesSplits`. For splits: the event names ordered gates, each bound to a Timer
id; passes from Timer k get `GateIndex(k)`; per-Timer `ClockAlign` puts them on one session
axis to about 1 ms (spec §7). Laps still come from the lap-gate Timer's own clock. Nothing in
§4.3.1–4.3.7 has to change; this is why the adapter id is the Timer id from day one.

#### 4.3.9 Discovery

- `mdns-sd` crate in the driver: browse `_oft._tcp.local.`, list by TXT `id`, `seats`, `level`,
  `fw`; the Timers page offers "Add this Timer" per result and falls back to a URL field.
- The console must work when the laptop's firewall blocks mDNS: typed URL, and remember it.

#### 4.3.10 In-process host (`OftLocal`)

`oft-core::NodeHost::feed(msg, t_timer, out)` already yields `Event::Crossing` and samples from
decoded Node-link frames. The Director's driver can own a `serialport` reader, run
`NodeHost`, and hand the same `Raw::Crossing` into the same translator. Clock alignment is the
identity (the Node clock is mapped onto the Director's own monotonic clock by `ClockMap`), and
`/samples` becomes an in-memory ring. This is the B0 bench dongle and the "laptop parked by the
gate" build; it also makes `cargo test` able to run a whole heat with no process but the
Director.

---

## 5. Open questions for Ryan

1. **Scope of the vocabulary change.** §2.9 renames Round → Stage/Heat/Round and adds Format
   and Seat. It is a big break (pre-1.0 allows it). Do it in one branch or let grid-rh keep
   being the proving ground for another season first?

//ANSWER: I am still not sure on the vocab changes, I think we need to do a focused dive back on the develop minimal primitives then build all the other stuff on top of it. I think RH has the wording of heat/round wrong when we look at the larger context of racing in general. RH is not always the right answer.

2. **RaceGOW first, or club night first?** The order in §4.1 optimises for one pilot, one
   seat. If a club night on OFT (B3, eight seats) is the real target, Step 1 needs per-seat
   thresholds in the OFT spec before Grid can rely on them.

//ANSWER: I think we should always build Grid optimised for club racing, single pilot racing mode should be build on top of all the primitives we build.

3. **Per-seat thresholds in OFT v0.4.** `/config.threshold` is Timer-wide. Grid's seat model
   assumes per-seat. Change the spec now, before any firmware ships it?

//ANSWER: Yea, I do like the RH model that seats/profiles are sepaerate from events. I would agree with a switch to that.

4. **Re-detection in Rust or TypeScript?** §4.3.6 recommends a Director endpoint running
   `oft-core`'s detector. It is the only way to guarantee parity with the Timer but it adds a
   server round trip to a drag. The TS port is faster to build and will drift.

//ANSWER: I think this needs more thought and re-detection is a great feature but also I found that its actually fairly limited use. Is there a way to bring along oft-core detector as a library with wrappings? If not I think TS port is going to be fine.

5. **Signal units.** Change `SignalChunk`/`SignalThresholds` to tenths-of-dB with a unit tag
   (recommended) or keep RH counts and convert OFT at the adapter (`rh_oft::ddb_to_rh` does
   this for RH's sake, losing resolution)?

//ANSWER: As a guiding principle we should use the most granular data we have and never down convert

6. **What happens to grid-rh?** Recommendation: it stays the RotorHazard front end and keeps
   running club nights on recon-nh; its Svelte libs are the source Grid ports from, not a
   shared package. A shared `packages/components` already exists; sharing the pure libs
   (`stages.ts`, `imd.ts`, `tuning.ts`) is possible but couples two release trains.

//ANSWER: Keep it separate, once Grid proper is where I want it, I will not use grid-rh

7. **`Unofficial`/`Final`/protests on Run.** Hide them (recommended) or remove them from the
   engine? Hiding keeps the defensible-results story available for a club event without
   putting it in the way at home.

//ANSWER: For single pilot mode (probably what we should call it rather than RaceGOW mode), yes hide protest features.

8. **Video companion.** It talks to RotorHazard's race events today. If RaceGOW video stays on
   the companion path, it needs a Grid event source. In scope for Step 5 or not?

//ANSWER: I think we should eventually tie grid-rh video fully into grid, I would absolutely use those features and if I am already running grid on my laptop what I am running OBS on, it should be in the same UI

9. **Restart point as a ruling.** §2.8 proposes `RestartMarked` as a marshal ruling that
   bounds the consecutive window. Is "fly-back = restart" automatic (a crossing in the wrong
   direction, once OFT reports `direction`) or always a marshal call?

//ANSWER: Restart point as ruling make sense. I dont understand the rest of the question

10. **Open OFT spec question 9 ("does a Timer need race start/stop?").** Grid's answer is no:
   always emit, Grid filters by `RaceGoAt`. Confirm so the spec can close it.

// ANSWER: I would say no, we have to use RH in that way, but I think OFT should always listen and report so we never need to tell it we are starting/stopping/etc. We just update its configs and get teh crossing and other data from it.

---

## 6. Things found that contradict or sharpen the brief

- **"Grid's OFT adapter should do the same as `rh_oft` (poll `/crossings`)."** It should use
  the WebSocket with `since` for live and `/crossings?since` as the catch-up, not a 100 ms poll.
  `rh_oft` polls because RotorHazard's gevent loop makes a WebSocket client awkward; Grid has
  tokio. The clock alignment, `boot_id` handling and fastest-RTT fit transfer unchanged.
- **"Drop crossings until the clock is synced"** (`rh_oft/interface.py::_poll_crossings`) is a
  RotorHazard constraint, not a Grid one. Grid stores `SourceTime`; the alignment is for
  display. Never drop a crossing.
- **Per-seat thresholds are missing from OFT v0.3**, and `rh_oft` papers over it. Grid's seat
  model cannot.
- **The spec's L1 → "LivePasses, FrequencyMgmt, Calibration" mapping** is right, but
  `Calibration` in Grid means "enter/exit calibration", which in `auto` mode the Timer does
  itself. Grid should treat `auto` as "no calibration needed" and show Tune only for `manual`.
- **grid-rh's RaceGOW mode and Grid's dropped RaceGOW mode are not the same thing** (§2.5).
  The brief's "racegow-specific modes" should be read as "a nav filter and a name template",
  not an event kind.
- **`oftd` keeps 600 s of history, the spec says 3600 s** for `samples.history.full_rate_s` in
  the `hello` example. The adapter must read the advertised value, not assume either.
- **`timer-hardware-research-2026-11.md` still proposes GridNode** and a `gridnode` adapter
  (§6, §8 step 4). OFT v0.3 supersedes both; that doc needs a superseded banner.
- **Grid already has an RH-rule detector in the console** (`redetect.ts`), so the OFT marshal
  path is a question of which detector runs, not whether re-detection exists (§4.3.6).
- **Grid's Timer registry has no mDNS code** (`grep -rn mdns crates` is empty). Discovery is new
  work, not a port.
