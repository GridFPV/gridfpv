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
- ``gridfpv_mock_state`` — reply with per-node ``{index, frequency, current_rssi}``.
"""

import logging

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
    rhapi.ui.socket_listen("gridfpv_mock_state", on_state)
    logger.info("GridFPV mock-control plugin loaded (TEST ONLY) — gridfpv_mock_* handlers registered")
