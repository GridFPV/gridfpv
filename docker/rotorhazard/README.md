# Dockerized RotorHazard (#25)

A self-hosted, disposable RotorHazard instance for capturing fixtures and live
integration testing of the RotorHazard adapter (`crates/adapters/src/rotorhazard.rs`).
It runs with 8 **simulated (mock) nodes**, and `simulate_lap` over Socket.IO
produces real pass/lap records — so we get genuine wire frames without hardware.

Per `docs/testing-strategy.html`, a dockerized RH we fully control is an
*allowed* live-test dependency; the banned dependency is shared/production
backends (e.g. the Velocidrone game servers).

## Run it

```sh
docker compose -f docker/rotorhazard/docker-compose.yml up -d
# server comes up on http://localhost:5000 (HTTP + Socket.IO)
```

## Capture frames

`capture_frames.py` connects over Socket.IO, stages a race, simulates laps on two
nodes, and dumps every captured frame (`race_status`, `current_laps`,
`pass_record`, `node_data`, `leaderboard`) as JSON:

```sh
# easiest: run inside the container's venv (has python-socketio)
docker exec gridfpv-rh /root/venv_rh/bin/python3 /tmp/capture_frames.py
# (docker cp docker/rotorhazard/capture_frames.py gridfpv-rh:/tmp/ first)
```

A trimmed real capture is checked in at
`crates/adapters/src/rotorhazard/fixtures/captured-mock-race.json` and is the
ground truth the adapter + its golden replay test are validated against.

## Real wire format (validated 2026-06 against this image)

- `race_status` carries an integer `race_status`: **3 = staging, 1 = racing,
  2 = done** (0 = ready).
- `current_laps` is a **snapshot**: `{ "current": { "node_index": [ {laps:[…]}, … ] } }`,
  one array entry per node (array index = node index). Each lap has
  `lap_index, lap_number, lap_raw (ms), lap_time ("M:SS.mmm" string),
  lap_time_stamp (cumulative ms since race start), splits, late_lap`.
  It does **not** carry `source`, `deleted`, or `peak_rssi`, and deleted laps are
  already filtered out.
- `pass_record` fires per crossing: `{ node, frequency, timestamp }` where
  `timestamp` is epoch-milliseconds.
- `node_data` carries per-node `pass_peak_rssi[]` (and node/nadir variants) —
  this is where per-pass RSSI comes from (0 under mock nodes).

## Live & emulated-signal tests

The RotorHazard adapter has a live Socket.IO transport (`crates/adapters`, feature
`live`). These are a **local-only** test class — container-dependent tests don't run
in the shared CI pipeline. Each test spins up and tears down its own disposable RH,
so `cargo xtask live` is the one command you need (Docker required):

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
the row's `lap_id` column increments. A small generator (`tests/common/mod.rs::node_csv`)
turns a scenario (lap cadence, peak RSSI, active/silent node) into the CSVs; `RhContainer`
mounts them and runs the race. This tests the full chain: emulated signal → RH
detection/recording → Socket.IO → adapter → projection, including signal context.

> **mock_data CSV gotcha:** the mock reads its file continuously from container start,
> decoupled from race start — so `lap_id` must increment **throughout** the file (a
> capped `lap_id` stops producing laps before the race begins). Exact lap timing is
> therefore approximate; assert structure (lap counts, signal magnitude, dedup), not µs.

This emulated-signal harness is also what we'll use to test timing-dependent **race
engine** features (heat loop, scoring, marshaling, advancement) end to end — see
`docs/testing-strategy.html` §5.1.

### Manual

```sh
docker compose -f docker/rotorhazard/docker-compose.yml up -d --wait
python docker/rotorhazard/capture_frames.py     # or poke the Socket.IO API yourself
docker compose -f docker/rotorhazard/docker-compose.yml down
```
