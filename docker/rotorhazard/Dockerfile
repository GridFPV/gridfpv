# GridFPV RotorHazard dev harness — RotorHazard v4.4.0 with mock nodes.
#
# Why build from source: the only published image (cruwaller/rotorhazard:latest) is
# stale at 4.0.0-beta.4 — below the GridFPV plugin's floor of RHAPI 1.3 / RH v4.3.0+
# (decision D16, docs/rotorhazard-plugin.html). We build the current stable, v4.4.0
# (RHAPI 1.4), so the harness can exercise the real plugin loader + RHAPI surface.
#
# This supersedes the ad-hoc local `rh-plugin-dev-env-rotorhazard` image (RH 4.3.1,
# source not in-repo): same layout conventions (/opt/RotorHazard, run from
# src/server, MockInterface), made reproducible and pinned to v4.4.0.
#
# Mock nodes: RH auto-selects MockInterface when no timing hardware/serial nodes are
# present (always true in this container). MockInterface reads an emulated per-node
# RSSI stream from mock_data_{N}.csv in its working dir and loops it at EOF, so node
# RSSI/history is non-zero and laps record through RH's genuine detection pipeline.
# A default node-1 stream is baked in; the test harness (crates/testkit RhContainer,
# cargo xtask live/rh-mock) bind-mounts its own per-scenario CSVs over it.
FROM python:3.11-slim

ARG RH_VERSION=v4.4.0

# git to fetch the pinned source; build-essential/libffi as an sdist fallback for the
# native deps (gevent/greenlet/cffi normally resolve to manylinux wheels, but a wheel
# gap on some arches would otherwise fail the build).
RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates build-essential libffi-dev \
    && rm -rf /var/lib/apt/lists/*

RUN git clone --depth 1 --branch "${RH_VERSION}" \
        https://github.com/RotorHazard/RotorHazard.git /opt/RotorHazard

WORKDIR /opt/RotorHazard/src/server

# Install the server requirements, minus the Raspberry-Pi-only hardware packages
# (RPi.GPIO / rpi-ws281x / pi-ina219 / RPi.bme280) — they don't build or run on the
# x86 dev host and are irrelevant to mock nodes. The pure-python smbus2 is kept.
RUN grep -viE '^(RPi\.GPIO|rpi-ws281x|pi-ina219|RPi\.bme280)' requirements.txt > /tmp/requirements.harness.txt \
    && pip install --no-cache-dir -r /tmp/requirements.harness.txt

# RH v4.4.0 added GENERAL.MOCK_NODE_SIGNAL, defaulting to 0 = *no* mock signal. Set it
# to 1 so MockInterface reads mock_data_{N}.csv (the emulated-signal path the GridFPV
# harness depends on; 2 = RH's built-in random signal). Config merges this partial file
# over the defaults. Note: a node only emits signal once it ALSO has a frequency set and
# a mock_data_{N}.csv present — so with no CSV mounted the nodes are quiet (which is what
# the test harness needs; it mounts per-scenario CSVs and stages heats to set channels).
COPY config.json /opt/RotorHazard/src/server/config.json

# Entrypoint: by default just runs RH (quiet mock nodes — the test-harness contract). In
# demo mode (GRIDFPV_RH_DEMO=1, set by docker-compose) it also tunes any CSV-backed node
# so a bare `docker compose up` shows live signal. No mock_data CSV is baked into the
# image — the demo mounts one via compose; tests mount their own. See the script.
COPY mock-entrypoint.sh /usr/local/bin/mock-entrypoint.sh
RUN chmod +x /usr/local/bin/mock-entrypoint.sh

# 8 mock nodes; MockInterface's per-tick wall-clock (RH_UPDATE_INTERVAL) — ~67 Hz so a
# lap spans dozens of dense samples. The harness overrides RH_UPDATE_INTERVAL per run.
ENV RH_NODES=8 \
    RH_UPDATE_INTERVAL=0.015

EXPOSE 5000

HEALTHCHECK --interval=10s --timeout=8s --retries=6 --start-period=20s \
    CMD python3 -c "import urllib.request,sys; sys.exit(0 if urllib.request.urlopen('http://localhost:5000/',timeout=5).status==200 else 1)"

# The entrypoint runs `server.py --data /opt/RotorHazard/src/server`: pinning DATA_DIR to
# the server dir (= PROGRAM_DIR) makes RH chdir there, so mock_data_{N}.csv and the user
# plugins/ dir both resolve in one place and `plugins.<name>` imports cleanly off sys.path.
# (Without --data, v4.4.0 chdir's to ~/rh-data and the mounts would miss.)
CMD ["/usr/local/bin/mock-entrypoint.sh"]
