#!/bin/sh
# GridFPV RH dev-harness entrypoint.
#
# Default (test harness): just run RotorHazard. Mock nodes stay quiet (frequency 0,
# no mock_data CSV mounted) until something tunes them — exactly like a fresh RH. This
# is what the `cargo xtask live` / testkit flows need: a node only emits signal once a
# heat is staged (which sets its frequency) AND a mock_data_{N}.csv is mounted for it.
# Anything that streams signal *before* the test stages its heat corrupts lap detection
# (spurious/again pre-race crossings), so the harness must boot a quiet server.
#
# Demo mode (GRIDFPV_RH_DEMO=1, set by docker-compose): after boot, tune every mock
# node that has a mock_data_{N}.csv to a channel, so a bare `docker compose up` shows a
# live, non-zero RSSI feed without anyone staging a heat. Never set in the test harness.
set -eu

SERVER_DIR=/opt/RotorHazard/src/server
cd "$SERVER_DIR"

if [ "${GRIDFPV_RH_DEMO:-0}" != "1" ]; then
    # Test-harness / plain path: hand PID 1 to RotorHazard (clean signals, no extras).
    exec python3 server.py --data "$SERVER_DIR"
fi

# --- Demo mode only below ---
python3 server.py --data "$SERVER_DIR" &
RH_PID=$!
trap 'kill -TERM "$RH_PID" 2>/dev/null || true; wait "$RH_PID" 2>/dev/null || true; exit 0' TERM INT

# One-shot tuner: wait for the socket.io server, then set a channel on each CSV-backed
# node. Failures are non-fatal — the rig still runs, just without the head-start signal.
python3 - <<'PY' || echo "mock-entrypoint: demo tuner skipped (RH up regardless)"
import glob, os, time
import socketio  # ships with RotorHazard's requirements

# Raceband R1..R8 — one channel per node; only CSV-backed nodes actually emit signal.
RACEBAND = [5658, 5695, 5732, 5769, 5806, 5843, 5880, 5917]
nodes = sorted(int(os.path.basename(p).split("_")[2].split(".")[0]) - 1
               for p in glob.glob("mock_data_*.csv"))
if not nodes:
    raise SystemExit(0)

sio = socketio.Client(reconnection=True, reconnection_attempts=30)
for _ in range(60):
    try:
        sio.connect("http://localhost:5000", wait_timeout=5)
        break
    except Exception:
        time.sleep(1)
else:
    raise SystemExit("could not reach RH socket.io to tune mock nodes")

for n in nodes:
    sio.emit("set_frequency", {"node": n, "frequency": RACEBAND[n % len(RACEBAND)]})
    time.sleep(0.1)
time.sleep(0.5)
sio.disconnect()
print("mock-entrypoint: demo-tuned nodes %s to raceband channels" % [n + 1 for n in nodes])
PY

wait "$RH_PID"
