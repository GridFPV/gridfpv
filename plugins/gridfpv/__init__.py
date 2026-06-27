"""GridFPV RotorHazard plugin.

The in-process RH integration described in ``docs/rotorhazard-plugin.html`` (decision
D16). It runs inside the RH server with RHAPI access and talks to the GridFPV Director
over a ``gridfpv_*`` event namespace on RH's existing socket.io server.

**Slice 1 — handshake.** ``gridfpv_hello`` → ``gridfpv_hello_ack`` so the Director can
detect a plugin-equipped RH (versions, capabilities, node count) and offer a guided
install for one that's missing.

**Slice 2 — live dense RSSI.** While a race runs, the plugin broadcasts ``gridfpv_signal``
(decimated): per-node ``current_rssi`` plus the dense ``history_values``/``history_times``
window read live from ``rhapi.interface.seats``, plus the enter/exit detection levels and
the race-start clock origin. The Director folds this into the heat's signal trace **live**,
retiring the post-race save-then-pull. Declared via the ``"live_signal"`` capability.

Clean start/stop and threshold recalculate arrive in S3–S4.

Floor: RHAPI 1.3 / RotorHazard v4.3.0+ (declared in ``manifest.json``).
"""

import logging

import gevent  # RH's runtime; used for the decimated broadcast greenlet.

try:
    # RH's event constants — the lifecycle hooks we attach the signal loop to.
    from eventmanager import Evt
except Exception:  # pragma: no cover - only importable inside the RH server
    Evt = None

logger = logging.getLogger(__name__)

# The gridfpv_* wire-protocol version. Bumped only on a breaking change to the
# handshake/message shapes; the Director declares the range it supports and negotiates
# against this in the hello/ack exchange.
PROTOCOL_VERSION = 1

# This plugin build's own version (independent of PROTOCOL_VERSION). Keep in step with
# manifest.json's "version".
PLUGIN_VERSION = "0.1.0"

# Capabilities this build implements — the Director keys transport decisions off these
# (e.g. it prefers the plugin's live-signal path, and skips the post-race pull, once it
# sees "live_signal"). Later slices append "clean_control", "recalc".
CAPABILITIES = ["hello", "live_signal"]

# Socket.io event names of the gridfpv_* namespace.
EVT_HELLO = "gridfpv_hello"
EVT_HELLO_ACK = "gridfpv_hello_ack"
EVT_SIGNAL = "gridfpv_signal"

# Live-signal broadcast cadence (seconds) — decimated so the stream stays cheap on a Pi
# (design risk #5). 0.5 s = 2 Hz; the Director gets a fresh dense trace twice a second.
SIGNAL_INTERVAL = 0.5
# Cap on the per-node dense window sent each broadcast (most-recent samples). RH already
# prunes node history to ~60 s; this bounds a pathological burst. A normal heat fits well
# under this. (A future optimization streams only the incremental slice; see the doc.)
SIGNAL_WINDOW = 2000


def initialize(rhapi):
    """RH plugin entry point — register the gridfpv_* handlers + the live-signal loop."""

    # The running broadcast greenlet (None when idle). A dict so the inner handlers can
    # rebind it without `nonlocal` gymnastics.
    state = {"greenlet": None}

    # ---- S1: handshake -----------------------------------------------------------------
    def on_hello(_data=None):
        ack = {
            "protocol_version": PROTOCOL_VERSION,
            "plugin_version": PLUGIN_VERSION,
            "rhapi_version": "{0}.{1}".format(
                getattr(rhapi, "API_VERSION_MAJOR", 1),
                getattr(rhapi, "API_VERSION_MINOR", 0),
            ),
            "capabilities": CAPABILITIES,
            "node_count": _node_count(rhapi),
        }
        logger.info("GridFPV hello -> ack %s", ack)
        rhapi.ui.socket_send(EVT_HELLO_ACK, ack)

    rhapi.ui.socket_listen(EVT_HELLO, on_hello)

    # ---- S2: live dense RSSI -----------------------------------------------------------
    def broadcast_signal_once():
        """Read the live per-node signal off the interface and broadcast one snapshot."""
        seats = getattr(rhapi.interface, "seats", []) or []
        nodes = []
        for n in seats:
            hv = list(getattr(n, "history_values", None) or [])
            ht = list(getattr(n, "history_times", None) or [])
            if len(hv) > SIGNAL_WINDOW:
                hv = hv[-SIGNAL_WINDOW:]
                ht = ht[-SIGNAL_WINDOW:]
            nodes.append(
                {
                    "index": getattr(n, "index", 0),
                    "frequency": getattr(n, "frequency", 0),
                    "current_rssi": getattr(n, "current_rssi", 0),
                    "enter_at": getattr(n, "enter_at_level", 0),
                    "exit_at": getattr(n, "exit_at_level", 0),
                    "history_values": hv,
                    "history_times": ht,
                }
            )
        payload = {
            # The race-start origin (RH's monotonic seconds) so the Director can make the
            # dense `history_times` race-relative — the same anchor the marshal-data path uses.
            "race_start": getattr(rhapi.race, "start_time_internal", None),
            "nodes": nodes,
        }
        rhapi.ui.socket_broadcast(EVT_SIGNAL, payload)

    def signal_loop():
        try:
            while True:
                broadcast_signal_once()
                gevent.sleep(SIGNAL_INTERVAL)
        except gevent.GreenletExit:  # killed on race stop — normal
            pass
        except Exception:  # noqa: BLE001 - never let the loop crash take down RH
            logger.exception("GridFPV signal loop error")

    def start_signal(_args=None):
        # Cancel any stale loop, then start a fresh one for this race.
        g = state.get("greenlet")
        if g is not None:
            g.kill(block=False)
        state["greenlet"] = gevent.spawn(signal_loop)
        logger.info("GridFPV live signal: streaming %s every %ss", EVT_SIGNAL, SIGNAL_INTERVAL)

    def stop_signal(_args=None):
        g = state.get("greenlet")
        if g is None:
            return
        g.kill(block=False)
        state["greenlet"] = None
        # Final full snapshot so the Director has the complete trace for the heat even if
        # it missed the last tick — the live equivalent of the old post-race pull.
        try:
            broadcast_signal_once()
        except Exception:  # noqa: BLE001
            logger.exception("GridFPV final signal flush error")

    if Evt is not None:
        rhapi.events.on(Evt.RACE_START, start_signal, name="gridfpv_signal_start")
        rhapi.events.on(Evt.RACE_STOP, stop_signal, name="gridfpv_signal_stop")
        rhapi.events.on(Evt.RACE_FINISH, stop_signal, name="gridfpv_signal_finish")
    else:  # pragma: no cover
        logger.warning("GridFPV: eventmanager.Evt unavailable; live signal disabled")

    logger.info(
        "GridFPV plugin loaded (v%s, protocol v%s) — handshake + live signal registered",
        PLUGIN_VERSION,
        PROTOCOL_VERSION,
    )


def _node_count(rhapi):
    """Best-effort node/seat count from the live interface (0 if unavailable)."""
    try:
        return len(rhapi.interface.seats)
    except Exception:  # pragma: no cover - defensive; never fail the handshake on this
        return 0
