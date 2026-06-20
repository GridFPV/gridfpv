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

## Live integration test

The RotorHazard adapter has a live Socket.IO transport (`crates/adapters`, feature
`live`) and an integration test (`crates/adapters/tests/rh_live.rs`) that drives a
real race on this container and asserts the adapter produces correct laps.

It's a **local-only** test class — container-dependent tests don't run in the
shared CI pipeline. The one-command entry point brings the container up, runs the
test, and tears it down:

```sh
cargo xtask live
```

Or manually, against an already-running container:

```sh
docker compose -f docker/rotorhazard/docker-compose.yml up -d --wait
cargo test -p gridfpv-adapters --features live --test rh_live -- --ignored --nocapture
```

> Note: the live test drives passes via `simulate_lap` (a server-level injector),
> which bypasses RotorHazard's RSSI detection. Driving the **real detection** from
> emulated RSSI signals (`mock_data_*.csv`) is tracked separately (issue #27).
