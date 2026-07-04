"""GridFPV **mock-node control** plugin — TEST ONLY.

A development/test companion to the real ``gridfpv`` plugin (RH plugin design D16). It exposes a
``gridfpv_mock_*`` socket namespace that drives RotorHazard's **mock nodes from the network**, so a
test (or a maintainer) can shape the emulated signal live — set a node's frequency/RSSI, inject a
lap — without rebuilding the container or remounting CSVs. It reuses S1's ``socket_listen`` channel
plumbing.

**Do not ship this to users.** It is strictly a harness aid: it reaches into the live hardware
interface (``rhapi._racecontext.interface``) — internals RHAPI deliberately does not expose — which
is fine for a test plugin (loaded only in the dev harness) but would be wrong in production. Keep it
out of the distributed ``gridfpv`` bundle. The real integration only ever uses sanctioned RHAPI.

Handlers (each replies ``gridfpv_mock_ack`` to the asking client):
- ``gridfpv_mock_tune`` ``{node, frequency}`` — set a mock node's channel (so its ``mock_data`` CSV
  is read / it goes active).
- ``gridfpv_mock_set_rssi`` ``{node, rssi}`` — force a node's ``current_rssi`` (for inspection).
- ``gridfpv_mock_lap`` ``{node}`` — inject a real lap via RH's own ``intf_simulate_lap`` (the same
  proven path RH's built-in ``simulate_lap`` uses), so the pass flows through the genuine pipeline.
- ``gridfpv_mock_pass`` ``{node, peak, baseline, width, sample_ms}`` — emulate one gate pass: append
  a smooth RSSI **bell** (baseline→peak→baseline) to the node's dense history (so the live-signal
  trace shows the pass) AND record the lap via ``intf_simulate_lap``. This is what the "mock race
  day" autopilot uses to emulate a realistic race over the wire.
- ``gridfpv_mock_reset`` ``{node?}`` — clear a node's (or all nodes') dense history + RSSI, between
  races.
- ``gridfpv_mock_state`` — reply with per-node ``{index, frequency, current_rssi}``.
"""

import logging
import math
from time import monotonic

logger = logging.getLogger(__name__)


def initialize(rhapi):
    """Register the gridfpv_mock_* control handlers on RH's socket.io server."""

    def interface():
        # Semi-private reach into the live hardware interface — acceptable for a TEST plugin only.
        return rhapi._racecontext.interface  # noqa: SLF001

    def ack(action, **fields):
        rhapi.ui.socket_send("gridfpv_mock_ack", dict(action=action, ok=True, **fields))

    def nack(action, error):
        logger.warning("gridfpv_mock %s failed: %s", action, error)
        rhapi.ui.socket_send("gridfpv_mock_ack", {"action": action, "ok": False, "error": str(error)})

    def on_tune(data=None):
        data = data or {}
        try:
            node = int(data["node"])
            freq = int(data["frequency"])
            # Set the node frequency directly. MockInterface.update only reads a node's mock_data
            # CSV while `node.frequency` is truthy, so this is what activates a node. We bypass the
            # interface's `set_frequency` (whose 4.4.0 signature wants band/channel labels) — this is
            # a test control plugin, so the raw field write is exactly the intent.
            interface().nodes[node].frequency = freq
            ack("tune", node=node, frequency=freq)
        except Exception as ex:  # noqa: BLE001 - report, never crash RH
            nack("tune", ex)

    def on_set_rssi(data=None):
        data = data or {}
        try:
            node = int(data["node"])
            rssi = int(data["rssi"])
            interface().nodes[node].current_rssi = rssi
            ack("set_rssi", node=node, rssi=rssi)
        except Exception as ex:  # noqa: BLE001
            nack("set_rssi", ex)

    def on_lap(data=None):
        data = data or {}
        try:
            node = int(data["node"])
            ms_val = int(data.get("ms_val", 0))
            # The same call RH's built-in `simulate_lap` makes: records a real lap via the
            # interface's pass_record_callback (genuine pipeline, not a faked event).
            interface().intf_simulate_lap(node, ms_val)
            ack("lap", node=node)
        except Exception as ex:  # noqa: BLE001
            nack("lap", ex)

    def on_pass(data=None):
        data = data or {}
        try:
            node = int(data["node"])
            peak = int(data.get("peak", 150))
            baseline = int(data.get("baseline", 70))
            width = max(2, int(data.get("width", 12)))
            sample_ms = float(data.get("sample_ms", 15.0))
            n = interface().nodes[node]
            # Append a smooth raised-cosine bell (baseline -> peak -> baseline) to the node's dense
            # history, so the GridFPV plugin's live-signal stream shows the gate pass as a real
            # rise/peak/fall. Timestamps are monotonic seconds (the same clock RH's history uses).
            t0 = monotonic()
            for i in range(width):
                frac = i / (width - 1)
                env = 0.5 - 0.5 * math.cos(2.0 * math.pi * frac)  # 0..1..0 over the window
                val = int(round(baseline + (peak - baseline) * env))
                n.history_values.append(val)
                n.history_times.append(t0 + i * (sample_ms / 1000.0))
            n.pass_peak_rssi = peak
            # Record the lap through RH's genuine pass pipeline (needs the race RACING).
            interface().intf_simulate_lap(node, 0)
            # Land the live RSSI back at BASELINE, not the peak: a node parked at peak reads
            # as "sitting on the gate" to RH's signal machinery at the NEXT race start, which
            # fired an instant phantom crossing on every node — doubling the injected holeshot
            # into a 4ms "lap 1" that shifted every real lap's number by one.
            n.current_rssi = baseline
            ack("pass", node=node, peak=peak)
        except Exception as ex:  # noqa: BLE001
            nack("pass", ex)

    def on_reset(data=None):
        data = data or {}
        try:
            nodes = interface().nodes
            targets = [int(data["node"])] if "node" in data else list(range(len(nodes)))
            for idx in targets:
                nd = nodes[idx]
                nd.history_values[:] = []
                nd.history_times[:] = []
                nd.current_rssi = 0
            ack("reset", nodes=targets)
        except Exception as ex:  # noqa: BLE001
            nack("reset", ex)

    def on_state(_data=None):
        try:
            nodes = [
                {
                    "index": getattr(n, "index", i),
                    "frequency": getattr(n, "frequency", 0),
                    "current_rssi": getattr(n, "current_rssi", 0),
                }
                for i, n in enumerate(interface().nodes)
            ]
            ack("state", nodes=nodes)
        except Exception as ex:  # noqa: BLE001
            nack("state", ex)

    rhapi.ui.socket_listen("gridfpv_mock_tune", on_tune)
    rhapi.ui.socket_listen("gridfpv_mock_set_rssi", on_set_rssi)
    rhapi.ui.socket_listen("gridfpv_mock_lap", on_lap)
    rhapi.ui.socket_listen("gridfpv_mock_pass", on_pass)
    rhapi.ui.socket_listen("gridfpv_mock_reset", on_reset)
    rhapi.ui.socket_listen("gridfpv_mock_state", on_state)
    logger.info("GridFPV mock-control plugin loaded (TEST ONLY) — gridfpv_mock_* handlers registered")
