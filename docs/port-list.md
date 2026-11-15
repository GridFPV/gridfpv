# Port list

On 2026-11-09 the Rust code was cut down to a skeleton, so Grid can be rebuilt from the
primitives in `docs/primitives.md`. Most of what was removed will be written again in a new
shape. This page lists the parts that are **worth bringing back from `devel`** instead of
writing from scratch.

Nothing is lost. All of the old code is on the `devel` branch. To read a file from it:

```
git show devel:crates/server/src/ws.rs
```

Each entry below gives the path on `devel` and one line on why it is worth keeping. When you
port something, change it to fit the new design. Do not copy the old round and heat model
back in with it.

## Live updates to the console

- **Stream, snapshot and resume** — `crates/server/src/ws.rs`, `stream.rs`, `snapshot.rs`,
  `scope.rs`, with tests in `crates/server/tests/ws_stream.rs`. A client gets a snapshot,
  then a stream of changes, and can pick up where it left off after a dropped connection.
  The cursor rules and the "too old, start over" answer took real work to get right. The
  projections it served are gone, so the payloads need new shapes, but the plumbing stands.
- **Typed error for a bad request body** — `JsonBody` in
  `crates/server/src/control_handler.rs`. It turns a broken or missing JSON body into the
  normal `ProtocolError` instead of plain text. Small, and every write route should use it.

## Turning passes into laps

- **Corrected-passes folds** — `crates/projection/src/lib.rs`. The minimum lap time floor,
  the bounce window (two crossings too close together count as one), and the removal record
  (a removed lap stays in the log with who removed it and why). `docs/primitives.md` §8 says
  to keep these.
- **Min-lap floor tests** — `crates/server/tests/min_lap_floor_conformance.rs`. They pin
  how the floor must behave at the edges. Good cases to reuse against the new folds.
- **Signal trace folding** — `signal_trace` in `crates/projection/src/lib.rs`. It joins
  signal chunks into one trace per seat, and lets a full trace replace a rough live one. The
  `base` offset rule on `SignalHistory` (still in `crates/events`) only makes sense with
  this fold.
- **Golden replay tests** — `crates/app/tests/replay.rs` and `adapter_replay.rs`, with the
  recorded sessions in `crates/app/tests/fixtures/`. Feed a saved session in, compare the
  result to a saved answer. The idea and the fixtures are worth keeping for the new folds.

## Channels

- **Channel interference maths** — `crates/engine/src/imd.rs`. It rates a set of video
  channels for interference and names the worst pair. It was checked against IMDTabler. It
  does not depend on the old model at all.
- **Channel catalog** — `crates/server/src/channels.rs`. The table of bands, channels and
  frequencies, and the code that turns a frequency into a label like `R8`.

## Timers

- **RotorHazard transport facts** — already saved in this repo at
  `docs/reference/rotorhazard-transport-2026-10.rs.txt` (from
  `crates/adapters/src/rotorhazard/transport.rs`). Every fact in it was checked against
  RotorHazard's own source or a real timer: handler names, payload shapes, the differences
  between 4.3.0 and 4.4.0, and which writes need a readback. Read it before the rewrite.
- **RotorHazard translator** — `crates/adapters/src/rotorhazard.rs`. How RotorHazard's
  messages became passes and signal events, including lap renumbering after a delete.
- **Timer registry and connection lifecycle** — `crates/server/src/timers.rs` and
  `crates/app/src/source/rh_connections.rs`. Connect, disconnect, reconnect with back-off,
  status reporting, and the queue of pending writes with one drain point. It returns
  redesigned around seats and profiles, but the lifecycle lessons carry over.
- **Failover between two timers on one gate** — `crates/app/src/source/failover.rs`. Small
  and self-contained: use the primary while it is healthy, switch to the spare, switch back.
- **RotorHazard plugin** — `plugins/gridfpv/__init__.py`. It is being rewritten, but it
  shows which RotorHazard hooks give a live pass and a live signal trace.
- **Velocidrone translator** — `crates/adapters/src/velocidrone.rs`. Never tested against
  the real sim. Newer work is on branch `vd-core-484`; start from that branch, not this file.

## The race clock

- **Server-time race clock** — `crates/server/src/live_state.rs`. The server stamps the
  start and the end of a race, and every screen counts from those stamps instead of its own
  clock. The `/time` route that consoles use to measure their clock offset is still here.
- **Start tone timing** — `StartProcedure` and `StartTone` in
  `crates/server/src/events.rs`, and how `live_state.rs` tells the console when to play
  the tone. The console plays the tone; the server says when.

## Test rigs

- **`xtask` rh-mock and race-day** — `xtask/src/rh_mock.rs`, `xtask/src/race_day.rs`,
  `docker/rotorhazard/`, `plugins/gridfpv_mock/`. A RotorHazard in Docker with fake signal
  data, and an autopilot that runs races on it while you drive the console.
- **Fake signal generator** — `crates/testkit/src/lib.rs`. Builds the fake node data for
  that rig: steady laps, uneven pace, a missed crossing, a pilot who stops, weak and noisy
  signals. Also starts and stops the Docker container for a test.
- **The live matrix** — `live()` in `xtask/src/main.rs`. It ran the timer tests against
  two RotorHazard versions, with and without the plugin. Running only one setup once let a
  real bug reach the field with every test green, so the matrix idea should come back with
  the new adapter.

## Rules worth reading before rebuilding

These will be rewritten, not ported. Read them first so the same lessons are not paid for
twice.

- **Scoring and tie-breaks** — `crates/engine/src/scoring.rs`. Best lap, best consecutive
  laps, first to N laps, and what happens on a tie or with a pilot who never flew.
- **Round fill and seeding** — `crates/server/src/round_engine.rs`. How heats were filled
  from a ranking, and the many edge cases found along the way.
- **Marshal commands** — `crates/server/src/control_handler.rs`. Add a lap, remove a lap,
  penalties and protests, and the checks that stop a change to a locked result.
- **Audit trail** — `event_audit_log` in `crates/server/src/app.rs`. One readable line per
  ruling, newest first, with friendly names and no raw ids.

## Frontend

On the same day the console was cut down to a skeleton too: the home hub, the Events, Pilots
and Classes pages, and a plain page for an open event (since rebuilt: it now lists the
phases and opens a Run page for a heat). Everything below was removed from
`frontend/` and is on `devel`. Paths are under `frontend/` unless they say otherwise.

### Talking to the Director

- **Stream client with resume** — `packages/protocol-client/src/client.ts` (the `connect`
  function), with tests in `client.test.ts`. It fetches a snapshot, opens the stream, drops
  repeats, spots a gap, and starts over from a fresh snapshot when the server says its cursor
  is too old. It pairs with the server stream at the top of this page.
- **Session stream plumbing** — `apps/rd-console/src/lib/session.svelte.ts`. How the live
  state reached every screen, and how one screen could ask for a single heat's data. It also
  holds the server clock offset (`syncServerClock`), measured from `/time`.
- **Command sender** — `apps/rd-console/src/lib/control.ts`. Sends one command and turns any
  failure into the same answer shape, so a screen has one thing to check.

### Names on screen

- **Shared competitor-name resolver** — `apps/rd-console/src/lib/competitorName.ts`. One
  place turns a raw competitor into a callsign, a seat label and a channel. One place builds
  its inputs too. Screens that built their own inputs showed different names for the same
  seat. Keep the idea, even though the inputs will change.
- **Heat names** — `apps/rd-console/src/lib/heats.ts`. Turns a heat id into a name people
  can read. Every screen used the same helper, so the name never drifted.

### Signal and tuning

- **RSSI signal graph** — `apps/rd-console/src/lib/RssiGraph.svelte` and `rssiGraph.ts`,
  with four test files in `apps/rd-console/tests/`. Draws a seat's signal with its
  thresholds and lap marks, live or after the race. The smoothing and the scaling took many
  tries to get right.
- **Re-detection** — `apps/rd-console/src/lib/redetect.ts`. Works out where the laps would
  fall if the thresholds were different, from a saved trace. This is what makes tuning after
  a race possible.
- **Tuning maths and the Tune page** — `apps/rd-console/src/lib/tuning.ts`, `applyTune.ts`
  and `screens/TunePage.svelte`. Record a seat, draw the trace, drag the thresholds, see the
  laps change. `docs/primitives.md` §3.2 says tuning should work this way.
- **Live signal strip and feed** — `apps/rd-console/src/lib/GateSignalStrip.svelte`,
  `gateSignal.ts` and `signalFeed.svelte.ts`. A small live bar per seat, and a poller that
  stops asking when no screen is looking.

### Race audio and clocks

- **Race audio** — `apps/rd-console/src/lib/raceAudio.ts` and `raceDayAudio.svelte.ts`.
  Plays the start tone at the moment the server names, on every page, not only the race
  page. Browsers block sound until the first click, and this code handles that.
- **Lap callouts** — `apps/rd-console/src/lib/lapCallouts.svelte.ts` and `callouts.ts`.
  Speaks each lap time once, in order, and does not repeat old laps after a reconnect.
- **End tones** — `apps/rd-console/src/lib/endTones.svelte.ts`. The countdown beeps before
  a timed race ends.
- **Clocks** — `apps/rd-console/src/lib/raceClock.svelte.ts`, `armingClock.svelte.ts`,
  `stagingClock.svelte.ts`, `protestClock.svelte.ts` and `raceWindow.ts`. Every clock counts
  from the server's time stamps, so two screens always agree.

### Screens

- **Marshaling UI** — `apps/rd-console/src/screens/Marshaling.svelte`, `lib/marshaling.ts`
  and `tests/MarshalingScreen.test.ts`. Review a race lap by lap against the signal trace:
  add a lap, remove a lap, penalties, protests, and the audit trail beside it.
- **Live race control** — `apps/rd-console/src/screens/LiveRaceControl.svelte` and
  `lib/transitions.ts`. The start, stop and restart buttons, and which of them is allowed
  in each state. The states will change; the layout and the two-step confirm are worth
  keeping.
- **Results and standings** — `apps/rd-console/src/screens/Results.svelte`, `lib/results.ts`,
  and the `Leaderboard`, `StandingsTable`, `HeatSheet` and `PilotCard` components in
  `packages/components/src/`. Tables with medals and tie marks, built on the design tokens.
- **Audit page** — `apps/rd-console/src/screens/EventAudit.svelte` and `lib/auditRender.ts`.
  One readable line per ruling, with friendly names and a filter.
- **Timers page** — `apps/rd-console/src/screens/TimerManager.svelte`, `TimersPage.svelte`
  and `TimerNodesDialog.svelte`. Add, connect and watch a timer. `docs/primitives.md` §3.4
  wants this page to list timers found on the network.
- **Event workspace shell** — `apps/rd-console/src/App.svelte` and `ContextHeader.svelte`.
  The side menu with keyboard shortcuts, and the top bar that shows the current heat, the
  clock and each timer's status on every page of an event.
- **Roster and class picking in an event** — `apps/rd-console/src/screens/EventClassesRoster.svelte`.
  Tick boxes that save on every change, using `lib/autosave.ts`.
- **Setup wizard** — `apps/rd-console/src/screens/EventSetupWizard.svelte`. Walks a new event
  through its first setup by reusing the real pages, not copies of them.

### Channels

- **IMD and channel helpers** — `apps/rd-console/src/lib/channels.ts` and
  `channelLayouts.ts`. Channel labels like `R8`, and the warning shown when a set of channels
  will interfere. The layout model is gone, but the labels and the warning will be needed.

### Overlay

- **Race overlay** — not on `devel`. It is on the `racegow` branch:
  `apps/rd-console/src/screens/RaceOverlay.svelte`, `EventOverlays.svelte` and
  `lib/overlay.ts`. A page for OBS that follows the active event and shows the clock and a
  row per pilot. Read it with `git show racegow:frontend/apps/rd-console/src/lib/overlay.ts`.

### Tests and rigs

- **Contract tests for the removed routes** — `contract/stream.contract.ts`,
  `snapshot.contract.ts`, `control.contract.ts` and `race.contract.ts`. They run the real
  Director and check the real wire. The stream ones caught bugs that mocked tests missed.
- **Browser tests** — `e2e/race.spec.ts`, `rounds.spec.ts` and `marshaling.spec.ts`. Full
  click-through runs of a race day. The steps will change, but they show what to cover.
- **Small helpers** — `apps/rd-console/src/lib/collapse.svelte.ts` (remembers which
  sections are folded) and `lib/autosave.ts` (saves a moment after the last click).
