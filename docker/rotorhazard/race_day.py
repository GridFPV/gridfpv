"""Mock **race-day autopilot** — emulate realistic races via the GridFPV test plugin.

Run this against a RotorHazard that has the ``gridfpv`` + ``gridfpv_mock`` plugins loaded (the
``cargo xtask race-day`` harness does this for you inside the dev container). It connects to RH's
socket.io, then **watches the race state**: whenever *you* stage + start a heat in the GridFPV
Director (driving RH into RACING), the autopilot emulates that heat's race over the wire —
injecting realistic per-pilot laps + RSSI bells through ``gridfpv_mock_pass`` on the nodes the
Director seated (those tuned to a channel). You drive the application; the test plugin drives the
races.

Pick a scenario (a "race-day personality") as argv[1]. Each shapes pace, spread, and the marshaling
edge cases (missed laps, false passes, a DNF) so you can practice different situations:

    clean    smooth, steady-pace laps, strong signal (the happy path)
    varied   per-pilot pace spread + lap-to-lap jitter (a realistic leaderboard)
    messy    marshaling practice: a missed lap, a false/extra pass, and a DNF
    pack     a tight field crossing close together (close finishes)

It loops indefinitely: every heat you run gets freshly emulated. Ctrl-C (or stopping the harness)
ends it. Re-running a heat re-emulates it.
"""

import random
import sys
import threading
import time

import socketio

RH_URL = "http://localhost:5000"

# RotorHazard race_status integers (see docker/rotorhazard/README.md): 1 = racing.
RACING = 1

# ---------------------------------------------------------------------------------------------
# Scenarios — each returns a per-node schedule of passes given the list of seated node indices.
# A schedule entry is (t_seconds_from_race_start, node_index, peak_rssi). The autopilot sorts the
# merged timeline and emits a gridfpv_mock_pass at each instant.
# ---------------------------------------------------------------------------------------------

BASELINE = 70
STRONG = 165
WEAK = 110
FALSE_PEAK = 100  # a spurious low bump a marshal should void


def _laps(node, count, lap_s, jitter, peak, start=0.6, rng=random):
    """A node's clean run: a holeshot then `count` laps at ~`lap_s` pace with ±jitter, all at
    `peak`. Returns [(t, node, peak), …] including the lap-0 holeshot."""
    out = []
    t = start
    for _ in range(count + 1):  # +1 for the holeshot (lap 0)
        out.append((t, node, peak + rng.randint(-6, 6)))
        t += lap_s * (1.0 + rng.uniform(-jitter, jitter))
    return out


def scenario_clean(nodes, rng):
    out = []
    for n in nodes:
        out += _laps(n, 4, 7.0, 0.06, STRONG, rng=rng)
    return out


def scenario_varied(nodes, rng):
    out = []
    for i, n in enumerate(nodes):
        pace = 6.0 + i * 0.9  # each pilot a bit slower than the last — a clear spread
        peak = STRONG - i * 8
        out += _laps(n, 5, pace, 0.18, max(peak, WEAK), rng=rng)
    return out


def scenario_messy(nodes, rng):
    """Marshaling practice: one node misses a lap, one gets a false/extra pass, the last DNFs."""
    out = []
    for i, n in enumerate(nodes):
        if i == len(nodes) - 1 and len(nodes) > 1:
            # DNF: only a holeshot + 2 laps, then nothing.
            out += _laps(n, 2, 7.5, 0.1, WEAK, rng=rng)
            continue
        laps = _laps(n, 4, 7.0, 0.1, STRONG, rng=rng)
        if i == 0:
            # Missed crossing: drop lap 2 (a gate miss the marshal must add back).
            laps = [p for k, p in enumerate(laps) if k != 2]
        if i == 1:
            # False pass: a low-peak bump ~1.5 s after the holeshot (a bounce/reflection to void).
            t0 = laps[0][0]
            laps.append((t0 + 1.5, n, FALSE_PEAK))
        out += laps
    return out


def scenario_pack(nodes, rng):
    out = []
    base_pace = 6.5
    for n in nodes:
        # Nearly identical schedules so the field crosses close together (dedup/ordering + close
        # finishes). Tiny jitter only.
        out += _laps(n, 4, base_pace, 0.03, STRONG, start=0.6, rng=rng)
    return out


SCENARIOS = {
    "clean": scenario_clean,
    "varied": scenario_varied,
    "messy": scenario_messy,
    "pack": scenario_pack,
}


# ---------------------------------------------------------------------------------------------
# The autopilot
# ---------------------------------------------------------------------------------------------


class RaceDay:
    def __init__(self, sio, scenario, seed=0):
        self.sio = sio
        self.scenario = scenario
        self.rng = random.Random(seed)
        self.worker = None
        self.stop = threading.Event()
        self.state_nodes = None
        self.state_evt = threading.Event()

    def tuned_nodes(self, timeout=3.0):
        """Ask the mock plugin for node state; return the indices tuned to a channel (the seats the
        Director set up for this heat)."""
        self.state_nodes = None
        self.state_evt.clear()
        self.sio.emit("gridfpv_mock_state")
        self.state_evt.wait(timeout)
        nodes = self.state_nodes or []
        return [n["index"] for n in nodes if n.get("frequency")]

    def on_state_ack(self, data):
        if data.get("action") == "state":
            self.state_nodes = data.get("nodes", [])
            self.state_evt.set()

    def start_race(self):
        self.stop_race()  # cancel any prior emulation
        self.stop.clear()
        self.worker = threading.Thread(target=self._run, daemon=True)
        self.worker.start()

    def stop_race(self):
        self.stop.set()
        w = self.worker
        if w and w.is_alive():
            w.join(timeout=1.0)
        self.worker = None

    def _run(self):
        nodes = self.tuned_nodes()
        if not nodes:
            print("race-day: no tuned/seated nodes found — did the heat seat any pilots?", flush=True)
            return
        for n in nodes:
            self.sio.emit("gridfpv_mock_reset", {"node": n})
        schedule = sorted(SCENARIOS[self.scenario](nodes, self.rng))
        print(
            f"race-day [{self.scenario}]: emulating {len(schedule)} passes across nodes {nodes}",
            flush=True,
        )
        t_start = time.monotonic()
        for t, node, peak in schedule:
            # Sleep until this pass's instant, bailing promptly if the race ended.
            while True:
                if self.stop.is_set():
                    print("race-day: race ended — stopping emulation", flush=True)
                    return
                ahead = t - (time.monotonic() - t_start)
                if ahead <= 0:
                    break
                time.sleep(min(ahead, 0.1))
            self.sio.emit(
                "gridfpv_mock_pass",
                {"node": node, "peak": int(peak), "baseline": BASELINE, "width": 14},
            )
        print("race-day: scenario complete — finish the heat in the Director when ready", flush=True)


def main():
    scenario = sys.argv[1] if len(sys.argv) > 1 else "clean"
    if scenario not in SCENARIOS:
        print(f"unknown scenario '{scenario}'. choose: {', '.join(SCENARIOS)}", flush=True)
        sys.exit(2)

    sio = socketio.Client(reconnection=True)
    autopilot = RaceDay(sio, scenario)

    @sio.on("gridfpv_mock_ack")
    def _ack(data):
        autopilot.on_state_ack(data)

    @sio.on("race_status")
    def _status(data):
        if data.get("race_status") == RACING:
            autopilot.start_race()
        else:
            autopilot.stop_race()

    sio.connect(RH_URL, wait_timeout=10)
    # No lap-minimum filter, so the brisk emulated laps all record.
    sio.emit("set_option", {"option": "MIN_LAP_TIME", "value": "0"})
    print(
        f"race-day autopilot ready [{scenario}] — stage + start a heat in the GridFPV Director and "
        "watch it race. Ctrl-C to stop.",
        flush=True,
    )
    try:
        sio.wait()
    except KeyboardInterrupt:
        autopilot.stop_race()
        sio.disconnect()


if __name__ == "__main__":
    main()
