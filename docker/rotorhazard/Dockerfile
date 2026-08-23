# GridFPV RotorHazard dev harness — RotorHazard with mock nodes, at a version of your
# choosing (`--build-arg RH_VERSION=vX.Y.Z`, default the current stable v4.4.0).
#
# Why build from source: the only published image (cruwaller/rotorhazard:latest) is
# stale at 4.0.0-beta.4 — below the GridFPV plugin's floor of RHAPI 1.3 / RH v4.3.0+
# (decision D16, docs/rotorhazard-plugin.html). We build the current stable, v4.4.0
# (RHAPI 1.4), so the harness can exercise the real plugin loader + RHAPI surface.
#
# **One image per RH version.** `cargo xtask live` runs a RotorHazard version × plugin
# matrix, so this file is built more than once and tagged per version:
#
#   docker build -t gridfpv-rotorhazard:4.4.0 docker/rotorhazard
#   docker build -t gridfpv-rotorhazard:4.3.0 --build-arg RH_VERSION=v4.3.0 docker/rotorhazard
#
# v4.3.0 is the **declared floor** (D16) and what the field timers run, so it is a
# first-class harness target, not a curiosity. The testkit builds whichever version a run
# asks for on demand (`gridfpv_testkit::ensure_rh_image`) — nothing here is duplicated
# per version. Everything below is version-agnostic except where noted.
#
# This supersedes the ad-hoc local `rh-plugin-dev-env-rotorhazard` image (RH 4.3.1,
# source not in-repo): same layout conventions (/opt/RotorHazard, run from
# src/server, MockInterface), made reproducible and pinned to a tag.
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

# Socket.IO stack floor — needed to build **v4.3.0** at all, a no-op on v4.4.0 (which already
# pins exactly these). RH v4.3.0's requirements pair `Flask==3.1.*` with `Flask-SocketIO==5.4.*`,
# and those two are incompatible: Flask 3.1 made `RequestContext.session` a read-only property,
# so Flask-SocketIO 5.4's connect handler dies with
#   AttributeError: property 'session' of 'RequestContext' object has no setter
# on **every** socket.io connection. The server still serves HTTP and loads plugins, so it looks
# healthy — but no client can hold a socket, which is the entire GridFPV integration. (RH fixed
# this in v4.4.0 by moving to Flask-SocketIO 5.6.) Raising just the socket.io stack to the v4.4.0
# pins keeps the v4.3.0 *server* — and RHAPI 1.3, the thing we actually test against — untouched.
RUN pip install --no-cache-dir "Flask-SocketIO==5.6.*" "python-socketio==5.14.*" "python-engineio==4.12.*"

# RH v4.4.0 added GENERAL.MOCK_NODE_SIGNAL, defaulting to 0 = *no* mock signal. Set it
# to 1 so MockInterface reads mock_data_{N}.csv (the emulated-signal path the GridFPV
# harness depends on; 2 = RH's built-in random signal). Config merges this partial file
# over the defaults. Note: a node only emits signal once it ALSO has a frequency set and
# a mock_data_{N}.csv present — so with no CSV mounted the nodes are quiet (which is what
# the test harness needs; it mounts per-scenario CSVs and stages heats to set channels).
#
# Version note: MOCK_NODE_SIGNAL is a **v4.4.0+** option. On v4.3.x `MockInterface` reads
# mock_data_{N}.csv unconditionally, and the unknown key is inert there — so the same
# config.json is correct for every version this file builds (verified on v4.3.0: mock
# nodes stay quiet with no CSV, and stream once one is mounted).
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
# (Without --data, v4.4.0 chdir's to ~/rh-data and the mounts would miss; v4.3.x instead
# *prompts* on stdin when it finds a config.json in PROGRAM_DIR — which would hang a
# headless container. `--data` short-circuits both, so it is load-bearing on every version.)
CMD ["/usr/local/bin/mock-entrypoint.sh"]
