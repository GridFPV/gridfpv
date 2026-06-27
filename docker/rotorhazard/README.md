# Dockerized RotorHazard dev harness (#25, D16)

A self-hosted, disposable RotorHazard instance we fully control — for capturing
fixtures, live integration testing of the RotorHazard adapter
(`crates/adapters/src/rotorhazard/`), and **iterating on the GridFPV RH plugin**
(decision D16; design in `docs/rotorhazard-plugin.html`).

It runs with 8 **simulated (mock) nodes**, and an emulated-signal `mock_data` CSV so
RH's *real* lap pipeline produces passes (genuine wire frames without hardware).

Per `docs/testing-strategy.html`, a dockerized RH we fully control is an *allowed*
live-test dependency; the banned dependency is shared/production backends.

## Built from source at RotorHazard v4.4.0

The image is **built from RotorHazard v4.4.0 source** (`Dockerfile`), not pulled.
This is deliberate: the only published image (`cruwaller/rotorhazard:latest`) is stale
at **4.0.0-beta.4**, below the GridFPV plugin's floor of **RHAPI 1.3 / RH v4.3.0+**
(D16). v4.4.0 (RHAPI 1.4) lets the harness exercise the real plugin loader + RHAPI
surface. (This supersedes the ad-hoc local `rh-plugin-dev-env-rotorhazard` image,
RH 4.3.1, whose source was not in-repo — same conventions, made reproducible.)

Two harness specifics, both load-bearing on v4.4.0:

- **`MOCK_NODE_SIGNAL=1`** (baked `config.json`): v4.4.0 added this option, defaulting
  to `0` = *no* mock signal. `1` makes `MockInterface` read `mock_data_{N}.csv` (the
  emulated-signal path; `2` = RH's built-in random signal). Without it, mock nodes
  report `current_rssi = 0` and the whole emulated-signal harness goes dark.
- **`--data /opt/RotorHazard/src/server`**: pins `DATA_DIR` to the server dir so
  `mock_data_{N}.csv` and the user `plugins/` dir resolve in one place and
  `plugins.<name>` imports cleanly. (Without it, v4.4.0 `chdir`s to `~/rh-data` and the
  mounts miss.) The entrypoint then tunes the CSV-backed node(s) to a channel so signal
  flows immediately on a bare run.

## Run it

```sh
docker compose -f docker/rotorhazard/docker-compose.yml up -d --build
# server comes up on http://localhost:5000 (HTTP + Socket.IO), node 1 streaming signal
docker compose -f docker/rotorhazard/docker-compose.yml down
```

The test harness (`cargo xtask live` / `rh-mock`) builds the image automatically the
first time via the testkit's `RhContainer` — no manual build needed for tests.

## GridFPV plugin (D16)

The plugin lives at `plugins/gridfpv/` (repo root). The harness mounts it into the
container's user `plugins/gridfpv` and boots it:

```sh
cargo xtask rh-mock plugin-check          # boot RH + plugin, confirm it loads cleanly
cargo xtask rh-mock feed clean --plugin   # interactive: a live timer WITH the plugin
cargo xtask live                          # every live RH boots against the plugin
```

`plugin-check` is the S0 acceptance test: it asserts RH's loader reports the plugin
**loaded** with no `load_issue`. To live-edit the plugin under `docker compose`,
uncomment the bind mount in `docker-compose.yml` and restart RH.

## Capture frames

`capture_frames.py` connects over Socket.IO, stages a race, simulates laps, and dumps
every captured frame (`race_status`, `current_laps`, `pass_record`, `node_data`,
`leaderboard`) as JSON:

```sh
docker cp docker/rotorhazard/capture_frames.py gridfpv-rh:/tmp/
docker exec gridfpv-rh python3 /tmp/capture_frames.py
```

A trimmed real capture is checked in at
`crates/adapters/src/rotorhazard/fixtures/captured-mock-race.json` and is the ground
truth the adapter + its golden replay test are validated against.

## Real wire format (validated against this image)

- `race_status` carries an integer `race_status`: **3 = staging, 1 = racing, 2 = done**
  (0 = ready).
- `current_laps` is a **snapshot**: `{ "current": { "node_index": [ {laps:[…]}, … ] } }`,
  one array entry per node (array index = node index). The lap-time/deletion shape
  **changed across RH versions**: on RH ≤ 4.0 the duration was `lap_raw` (ms) with
  `lap_time` a `"M:SS.mmm"` *string* and deleted laps pre-filtered; on **RH 4.3+/4.4**
  `lap_time` is a *numeric* ms duration (the string moved to `lap_time_formatted`) and
  laps carry `source` + `deleted` inline. Stable across both: `lap_index, lap_number,
  lap_time_stamp (cumulative ms since race start), splits, late_lap`. The adapter reads
  only `lap_number` + `lap_time_stamp`, parses `lap_time` permissively, and skips
  `deleted` laps (`crates/adapters/src/rotorhazard.rs`).
- `pass_record` fires per crossing: `{ node, frequency, timestamp }` where `timestamp`
  is epoch-milliseconds.
- `node_data` / heartbeat carry per-node RSSI (`current_rssi`, `pass_peak_rssi`, …).
  With `MOCK_NODE_SIGNAL=1` + a `mock_data` CSV these are **non-zero** (the emulated
  signal), unlike bare mock nodes which report 0.

## Live & emulated-signal tests

The RotorHazard adapter has a live Socket.IO transport (`crates/adapters`, feature
`live`). These are a **local-only** test class — container-dependent tests don't run in
the shared CI pipeline. Each test spins up and tears down its own disposable RH, so
`cargo xtask live` is the one command you need (Docker required):

```sh
cargo xtask live      # runs all live targets, one container at a time
```

There are two ways the tests drive RH, in increasing realism:

### 1. Bare-lap live (`tests/rh_live.rs`)

Drives passes via `simulate_lap` — a server-level injector that bypasses RotorHazard's
signal detection. Exercises the live Socket.IO transport + lap recording + our adapter.

### 2. Emulated-signal race (`tests/rh_signal.rs`)

Drives RH from **emulated node-output streams** so its *real* lap pipeline produces the
passes. RotorHazard's mock interface reads a per-node `mock_data_{N}.csv` — one CSV row
per tick (`RH_UPDATE_INTERVAL`, set low to speed replay) — and records a lap each time
the row's `lap_id` column increments. A small generator
(`gridfpv_testkit::node_csv`) turns a scenario (lap cadence, peak RSSI, active/silent
node) into the CSVs; `RhContainer` mounts them and runs the race. This tests the full
chain: emulated signal → RH detection/recording → Socket.IO → adapter → projection,
including signal context.

> **mock_data CSV gotcha:** the mock reads its file continuously from container start,
> decoupled from race start — so `lap_id` must increment **throughout** the file (a
> capped `lap_id` stops producing laps before the race begins). Exact lap timing is
> therefore approximate; assert structure (lap counts, signal magnitude, dedup), not µs.

This emulated-signal harness is also what we use to test timing-dependent **race
engine** features (heat loop, scoring, marshaling, advancement) end to end — see
`docs/testing-strategy.html` §5.1.

### Manual

```sh
docker compose -f docker/rotorhazard/docker-compose.yml up -d --build --wait
docker cp docker/rotorhazard/capture_frames.py gridfpv-rh:/tmp/ && \
  docker exec gridfpv-rh python3 /tmp/capture_frames.py
docker compose -f docker/rotorhazard/docker-compose.yml down
```
