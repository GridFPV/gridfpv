# Mock race days — drive the app, let the test plugin emulate the races

A hands-on harness for exercising GridFPV end-to-end without hardware: **you** drive the
Director (build an event, seat pilots, Stage → Start heats, marshal, advance), and the
`gridfpv_mock` **test plugin** emulates each race over the wire — realistic per-pilot laps
and live RSSI traces — reacting to the heats you start.

It's built on the RotorHazard plugin (decision D16): the **`gridfpv`** plugin streams live
signal + per-node passes; the **`gridfpv_mock`** plugin (test-only) lets the autopilot
inject emulated passes. See [`rotorhazard-plugin.html`](rotorhazard-plugin.html).

## What's running

After the overnight deploy, three things are up:

| Service | Where | What |
|---------|-------|------|
| **Director** | http://127.0.0.1:8080 | The GridFPV app (latest `devel` build), data in `~/gridfpv-data` |
| **Race RotorHazard** | http://localhost:5055 | RH v4.4.0 with the `gridfpv` + `gridfpv_mock` plugins (container `gridfpv-race-rh`) |
| (untouched) | :5099 | The maintainer's `gridfpv-demo-rh` — left alone |

The active event's timer already points at `http://localhost:5055` and reads **Connected**
with a green **plugin ✓** chip.

> If the Director or race RH aren't running, restart them — see "Restarting" at the bottom.

## Run a race day

1. **Start the autopilot** for a scenario:

   ```sh
   cargo xtask race-day clean      # or: varied | messy | pack   (cargo xtask race-day list)
   ```

   It attaches to the race RH and prints `race-day autopilot ready […]`. Leave it
   running; it watches for heats you start. Ctrl-C ends it.

2. **In the Director** (http://127.0.0.1:8080): build an event (pilots, classes, heats),
   seat pilots on nodes, then **Stage → Start** a heat.

3. **Watch it race.** The autopilot detects RACING and emulates the seated nodes per the
   scenario — laps appear, the leaderboard updates, and the marshaling trace shows the
   live RSSI bells. When the scenario's laps are in, finish the heat and advance.

4. **Run the next heat** — every heat you Start gets freshly emulated. Switch scenarios by
   Ctrl-C'ing the autopilot and starting it with a different one.

## The scenarios

| Scenario | What it exercises |
|----------|-------------------|
| `clean`  | Smooth, steady-pace laps, strong signal — the happy path |
| `varied` | Per-pilot pace spread + lap-to-lap jitter — a realistic leaderboard |
| `messy`  | **Marshaling practice**: a missed lap, a false/extra pass, and a DNF |
| `pack`   | A tight field crossing close together — close finishes |

`messy` is the one for practicing the marshaling tools (void a false pass, add a missed
lap, handle a DNF).

## Notes / gotchas

- The autopilot emulates whichever nodes are **tuned** (the Director tunes the seats it
  uses); passes for any other default-tuned nodes are ignored by the Director's lineup, so
  they're harmless.
- It injects laps through RH's genuine pass pipeline (`intf_simulate_lap`) and appends real
  RSSI history, so both the lap list **and** the live-signal/marshaling trace are populated.
- This is **test-only**: `gridfpv_mock` reaches into RH internals and is never shipped to
  users (it's not in the downloadable plugin bundle).

## Restarting

Race RH (both plugins, no signal CSV — the autopilot is the only source):

```sh
docker rm -f gridfpv-race-rh
docker run -d --name gridfpv-race-rh -p 5055:5000 \
  -v "$PWD/plugins/gridfpv:/opt/RotorHazard/src/server/plugins/gridfpv:ro" \
  -v "$PWD/plugins/gridfpv_mock:/opt/RotorHazard/src/server/plugins/gridfpv_mock:ro" \
  gridfpv-rotorhazard:4.4.0
```

(`cargo xtask race-day` will also start it automatically if it's not running.)

Director (latest build, with the live RH connector):

```sh
cd frontend && npm run build && cd ..
cargo build -p gridfpv-app --features live
GRIDFPV_ASSETS="$PWD/frontend/apps/rd-console/dist" GRIDFPV_DATA_DIR=~/gridfpv-data \
  GRIDFPV_ADDR=0.0.0.0:8080 ./target/debug/gridfpv
```
