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

import bisect
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

# Capabilities this build implements — the Director keys transport decisions off these.
# "live_pass": the plugin emits per-node passes natively from RACE_LAP_RECORDED (S3), so the
# Director gets the pass atom directly rather than diffing the current_laps snapshot. Native
# start/stop control ("clean_control") lands in a later step.
CAPABILITIES = ["hello", "live_signal", "live_pass"]

# Socket.io event names of the gridfpv_* namespace.
EVT_HELLO = "gridfpv_hello"
EVT_HELLO_ACK = "gridfpv_hello_ack"
EVT_SIGNAL = "gridfpv_signal"
EVT_PASS = "gridfpv_pass"

# Live-signal broadcast cadence (seconds) — decimated so the stream stays cheap on a Pi
# (design risk #5). 0.5 s = 2 Hz. Each tick sends only the NEW dense samples since the last
# (incremental), so the per-tick payload is tiny regardless of heat length.
SIGNAL_INTERVAL = 0.5
# Safety cap on the per-race accumulator length (samples) so a pathological run can't grow
# memory unbounded — a real heat's peak/nadir history is far smaller. When exceeded, the
# oldest samples are dropped (the Director keeps what it already folded).
SIGNAL_WINDOW = 20000


def initialize(rhapi):
    """RH plugin entry point — register the gridfpv_* handlers + the live-signal loop."""

    # The running broadcast greenlet (None when idle). A dict so the inner handlers can
    # rebind it without `nonlocal` gymnastics.
    # `greenlet`: the running broadcast loop. `acc`: per-race, per-node append-only dense buffer
    # {index: {"t": [secs], "v": [rssi], "sent": int}} — the source of the incremental slices.
    state = {"greenlet": None, "acc": {}}

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

    # ---- S2: live dense RSSI (incremental) ---------------------------------------------
    def reconcile(seat):
        """Merge a seat's current RH history into our per-race append-only accumulator; return
        ``(index, acc)``.

        RH prunes its own node history to ~60 s and reports peak/nadir entries (which can repeat a
        timestamp), so we keep the full per-race trace ourselves and append only samples newer than
        the last we hold — found by timestamp (monotonic), which is robust to RH front-pruning.
        """
        idx = getattr(seat, "index", 0)
        acc = state["acc"].setdefault(idx, {"t": [], "v": [], "sent": 0})
        ht = list(getattr(seat, "history_times", None) or [])
        hv = list(getattr(seat, "history_values", None) or [])
        n = min(len(ht), len(hv))
        if n:
            # First index in RH's (possibly front-pruned) buffer past what we already hold.
            start = bisect.bisect_right(ht, acc["t"][-1], hi=n) if acc["t"] else 0
            for i in range(start, n):
                acc["t"].append(ht[i])
                acc["v"].append(hv[i])
            if len(acc["t"]) > SIGNAL_WINDOW:  # safety: drop oldest if pathologically long
                drop = len(acc["t"]) - SIGNAL_WINDOW
                del acc["t"][:drop]
                del acc["v"][:drop]
                acc["sent"] = max(0, acc["sent"] - drop)
        return idx, acc

    def broadcast_signal_once(final=False):
        """Broadcast each seat's NEW dense samples since the last tick (incremental).

        Each node carries ``base`` — the accumulator index this slice starts at — so the Director
        can append at ``base`` (or REPLACE when ``base == 0``). ``final`` sends the full accumulated
        trace (base 0) so the Director's end state is complete even if it missed ticks.
        """
        seats = getattr(rhapi.interface, "seats", []) or []
        nodes = []
        for seat in seats:
            idx, acc = reconcile(seat)
            base = 0 if final else acc["sent"]
            nodes.append(
                {
                    "index": idx,
                    "frequency": getattr(seat, "frequency", 0),
                    "current_rssi": getattr(seat, "current_rssi", 0),
                    "enter_at": getattr(seat, "enter_at_level", 0),
                    "exit_at": getattr(seat, "exit_at_level", 0),
                    "base": base,
                    "history_values": acc["v"][base:],
                    "history_times": acc["t"][base:],
                }
            )
            if not final:
                acc["sent"] = len(acc["t"])
        payload = {
            # The race-start origin (RH's monotonic seconds) so the Director can make the dense
            # `history_times` race-relative — the same anchor the marshal-data path uses.
            "race_start": getattr(rhapi.race, "start_time_internal", None),
            "final": final,
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
        # Cancel any stale loop, reset the per-race accumulator, then start a fresh loop.
        g = state.get("greenlet")
        if g is not None:
            g.kill(block=False)
        state["acc"] = {}
        state["greenlet"] = gevent.spawn(signal_loop)
        logger.info("GridFPV live signal: streaming %s every %ss", EVT_SIGNAL, SIGNAL_INTERVAL)

    def stop_signal(_args=None):
        g = state.get("greenlet")
        if g is None:
            return
        g.kill(block=False)
        state["greenlet"] = None
        # Final full snapshot (base 0 = replace) so the Director has the complete trace for the heat
        # even if it missed ticks (e.g. a mid-heat reconnect) — the live equivalent of the old pull.
        try:
            broadcast_signal_once(final=True)
        except Exception:  # noqa: BLE001
            logger.exception("GridFPV final signal flush error")

    # ---- S3: per-node passes -----------------------------------------------------------
    def on_lap_recorded(args=None):
        """Broadcast the pass atom natively as RH records each lap (RACE_LAP_RECORDED), attributed by
        node seat — so the Director gets passes directly instead of diffing the current_laps snapshot.
        `lap` is RH's Crossing object; `lap_time_stamp` is cumulative ms since race start (the same
        unit current_laps carries), so the Director folds it identically (and dedups on lap_number)."""
        args = args or {}
        try:
            lap = args.get("lap")
            node_index = args.get("node_index")
            if lap is None or node_index is None:
                return
            rhapi.ui.socket_broadcast(
                EVT_PASS,
                {
                    "node_index": node_index,
                    "lap_number": getattr(lap, "lap_number", 0),
                    "lap_time_stamp": getattr(lap, "lap_time_stamp", 0.0),
                    "peak_rssi": args.get("peak_rssi"),
                },
            )
        except Exception:  # noqa: BLE001 - never let a pass broadcast take down RH
            logger.exception("GridFPV pass broadcast error")

    if Evt is not None:
        rhapi.events.on(Evt.RACE_START, start_signal, name="gridfpv_signal_start")
        rhapi.events.on(Evt.RACE_STOP, stop_signal, name="gridfpv_signal_stop")
        rhapi.events.on(Evt.RACE_FINISH, stop_signal, name="gridfpv_signal_finish")
        rhapi.events.on(Evt.RACE_LAP_RECORDED, on_lap_recorded, name="gridfpv_pass")
    else:  # pragma: no cover
        logger.warning("GridFPV: eventmanager.Evt unavailable; live signal/passes disabled")

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
