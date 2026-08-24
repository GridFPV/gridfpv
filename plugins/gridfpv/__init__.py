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

**Slice 3 — native passes.** While a race runs, the plugin broadcasts ``gridfpv_pass``
per recorded lap (``RACE_LAP_RECORDED``), attributed by node seat. Declared via the
``"live_pass"`` capability — but only when it is **earned**: ``initialize()`` self-checks
that this RotorHazard build's lap atom is actually readable, and declines the capability
otherwise, leaving RotorHazard's own ``current_laps`` snapshot as the authoritative lap
source (#389). A pass whose required fields cannot be read is **never** broadcast — a
partial/zero-filled atom would pre-empt the working ``current_laps`` value and silently
destroy laps, which is precisely the failure #389 spent a day bisecting.

Clean start/stop and threshold recalculate arrive in S4.

Floor: RHAPI 1.3 / RotorHazard v4.3.0+ (declared in ``manifest.json``).
"""

import bisect
import logging
import sys

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
PLUGIN_VERSION = "0.2.0"

# Capabilities this build implements — the Director keys transport decisions off these.
# Native start/stop control ("clean_control") lands in a later step.
#
# BASE_CAPABILITIES are unconditional. PASS_CAPABILITY ("live_pass" — the plugin emits per-node
# passes natively from RACE_LAP_RECORDED, so the Director takes the pass atom directly rather
# than diffing the current_laps snapshot) is **earned, not assumed**: it is added to the advertised
# set only when the load-time self-check (`_self_check_live_pass`) proves this RotorHazard build's
# lap atom is readable. Advertising it makes the plugin the *authoritative* pass source on the
# Director side, so a plugin that cannot actually read a lap must not claim it — it must degrade
# to RotorHazard's own `current_laps` rather than pre-empt it (#389).
BASE_CAPABILITIES = ["hello", "live_signal"]
PASS_CAPABILITY = "live_pass"
# Everything this build implements, for reference/tests. The *advertised* set is computed per
# server in `initialize()` and reported in the hello ack.
CAPABILITIES = BASE_CAPABILITIES + [PASS_CAPABILITY]

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


def _lap_fields(lap):
    """Read ``(lap_number, lap_time_stamp)`` off a RotorHazard lap atom (``RHRace.Crossing``).

    Deliberately **no** ``getattr(..., default)`` fallbacks: a field this RH build does not expose
    must raise so the caller broadcasts *nothing*. Silent zero-fill is what made #389
    undiagnosable — a structurally valid pass carrying ``0 / 0.0`` looks fine on the wire, claims
    the Director's dedup key for ``(seat, lap_number)``, and destroys the correct ``current_laps``
    value.

    Returns ``(int, float)``; raises if the atom cannot be read.
    """
    lap_number = lap.lap_number
    lap_time_stamp = lap.lap_time_stamp
    if lap_number is None or lap_time_stamp is None:
        raise ValueError(
            "RH lap atom carries lap_number={0!r}, lap_time_stamp={1!r}".format(
                lap_number, lap_time_stamp
            )
        )
    return int(lap_number), float(lap_time_stamp)


def _self_check_live_pass(rhapi):
    """Prove at load time that this RH build can actually produce a pass atom (#389).

    ``live_pass`` makes the plugin the Director's **authoritative** lap source, so it must be
    earned: we exercise the exact read path ``on_lap_recorded`` uses (`_lap_fields`) against a real
    instance of this server's lap type, and confirm the broadcast entry point exists. Anything
    unreadable => decline the capability and let RotorHazard's ``current_laps`` stay authoritative.

    Returns ``(ok, detail)`` — ``detail`` is logged either way, so the field can see *why*.
    """
    if Evt is None or not getattr(Evt, "RACE_LAP_RECORDED", None):
        return False, "eventmanager.Evt.RACE_LAP_RECORDED is unavailable"

    broadcast = getattr(getattr(rhapi, "ui", None), "socket_broadcast", None)
    if not callable(broadcast):
        return False, "rhapi.ui.socket_broadcast is missing or not callable"

    # RH's lap atom: the object handed to RACE_LAP_RECORDED as args["lap"]. Prefer the module the
    # running server ALREADY imported — a fresh `import RHRace` re-enters RH's RHRace/RHUI import
    # cycle and would fail for a reason that has nothing to do with the lap shape.
    rhrace = sys.modules.get("RHRace")
    if rhrace is None:  # pragma: no cover - the server imports RHRace long before plugins load
        try:
            import RHRace as rhrace
        except Exception as exc:  # noqa: BLE001 - an RH build we do not recognise
            return False, "RH module RHRace is not importable: {0!r}".format(exc)

    crossing = getattr(rhrace, "Crossing", None)
    if crossing is None:
        return False, "RHRace exposes no Crossing lap type on this build"

    try:
        probe = crossing()
        probe.lap_number = 7
        probe.lap_time_stamp = 12345.5
        lap_number, lap_time_stamp = _lap_fields(probe)
    except Exception as exc:  # noqa: BLE001 - this build's lap shape is not the one we read
        return False, "RH lap atom is not readable: {0!r}".format(exc)

    if lap_number != 7 or lap_time_stamp != 12345.5:
        return False, "RH lap atom read back {0!r}/{1!r}, expected 7/12345.5".format(
            lap_number, lap_time_stamp
        )

    return True, "RHRace.Crossing exposes readable lap_number/lap_time_stamp"


def initialize(rhapi):
    """RH plugin entry point — register the gridfpv_* handlers + the live-signal loop."""

    # The running broadcast greenlet (None when idle). A dict so the inner handlers can
    # rebind it without `nonlocal` gymnastics.
    # `greenlet`: the running broadcast loop. `acc`: per-race, per-node append-only dense buffer
    # {index: {"t": [secs], "v": [rssi], "sent": int}} — the source of the incremental slices.
    # `passes`: per-race count of gridfpv_pass broadcasts, reported at race end (field diagnostics
    # for #389 — "did the plugin actually deliver?" is otherwise unanswerable on a shipped build).
    state = {"greenlet": None, "acc": {}, "passes": 0}

    # ---- S3 gate: earn `live_pass` before advertising it (#389) -------------------------
    live_pass_ok, live_pass_detail = _self_check_live_pass(rhapi)
    capabilities = list(BASE_CAPABILITIES)
    if live_pass_ok:
        capabilities.append(PASS_CAPABILITY)
        logger.info(
            "GridFPV: '%s' self-check passed (%s) — advertising it; this plugin is the "
            "authoritative pass source for the Director",
            PASS_CAPABILITY,
            live_pass_detail,
        )
    else:
        logger.error(
            "GridFPV: '%s' self-check FAILED (%s) — NOT advertising it and NOT registering the "
            "%s handler. RotorHazard's own current_laps snapshot stays the authoritative lap "
            "source, which is the safe degrade (#389).",
            PASS_CAPABILITY,
            live_pass_detail,
            EVT_PASS,
        )

    # ---- Grid owns start timing --------------------------------------------------------
    # RotorHazard adds a fixed pre-stage pad (GENERAL/RACE_START_DELAY_EXTRA_SECS, default 0.9s)
    # AFTER `stage_race` before it reaches RACING — separate from the race format's staging the
    # Director already zeroes. So the race actually starts ~0.9s after the Director's start
    # countdown hits zero. The Director owns race timing, so zero the pad here: RACING is then
    # reached immediately on stage and the start lines up with the countdown. (Lap times are
    # measured from RACING either way, so this only removes the offset.) Prior value reported in
    # the hello ack for transparency.
    def zero_prestage_pad():
        try:
            was = rhapi.config.get("GENERAL", "RACE_START_DELAY_EXTRA_SECS")
            if was is not None and float(was) != 0.0:
                rhapi.config.set("GENERAL", "RACE_START_DELAY_EXTRA_SECS", 0)
                logger.info(
                    "GridFPV: zeroed RACE_START_DELAY_EXTRA_SECS (was %s) — Grid owns start timing",
                    was,
                )
            return was
        except Exception:  # noqa: BLE001 - never fail load on an optional timing tweak
            logger.exception("GridFPV: could not read/zero RACE_START_DELAY_EXTRA_SECS")
            return None

    prestage_secs_was = zero_prestage_pad()

    # ---- S1: handshake -----------------------------------------------------------------
    def on_hello(_data=None):
        ack = {
            "protocol_version": PROTOCOL_VERSION,
            "plugin_version": PLUGIN_VERSION,
            "rhapi_version": "{0}.{1}".format(
                getattr(rhapi, "API_VERSION_MAJOR", 1),
                getattr(rhapi, "API_VERSION_MINOR", 0),
            ),
            # The EARNED set (see the self-check above) — never the implemented set.
            "capabilities": capabilities,
            "node_count": _node_count(rhapi),
            # The RACE_START_DELAY_EXTRA_SECS we found before zeroing it (None if unreadable).
            "prestage_secs_was": prestage_secs_was,
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
        state["passes"] = 0
        state["greenlet"] = gevent.spawn(signal_loop)
        logger.info("GridFPV live signal: streaming %s every %ss", EVT_SIGNAL, SIGNAL_INTERVAL)

    def stop_signal(_args=None):
        g = state.get("greenlet")
        if g is None:
            return
        g.kill(block=False)
        state["greenlet"] = None
        # Field diagnostics (#389): say plainly how many passes this race actually broadcast. A
        # zero here against a race RH clearly recorded is the single line that names the fault.
        if PASS_CAPABILITY in capabilities:
            logger.info(
                "GridFPV: race ended — broadcast %s %s message(s) this race",
                state.get("passes", 0),
                EVT_PASS,
            )
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
        unit current_laps carries), so the Director folds it identically (and dedups on lap_number).

        **All or nothing (#389).** Because the Director treats an advertised `live_pass` plugin as
        the authoritative pass source, a pass it cannot build correctly must not be sent at all: a
        partial/zero-filled atom would claim the lap and suppress RotorHazard's correct
        `current_laps` value. Anything unreadable is logged loudly and dropped, which lets the
        Director's liveness fallback notice the gap and take over.
        """
        args = args or {}
        lap = args.get("lap")
        node_index = args.get("node_index")
        if lap is None or node_index is None:
            logger.error(
                "GridFPV: %s not broadcast — RACE_LAP_RECORDED carried lap=%r, node_index=%r. "
                "Sending nothing so RotorHazard's current_laps stays authoritative (#389).",
                EVT_PASS,
                lap,
                node_index,
            )
            return

        try:
            lap_number, lap_time_stamp = _lap_fields(lap)
        except Exception:  # noqa: BLE001 - an RH build whose lap atom we cannot read
            logger.exception(
                "GridFPV: %s not broadcast for node index %r — this RotorHazard's lap atom (%s) is "
                "unreadable. Sending nothing rather than a partial pass, so current_laps stays "
                "authoritative (#389).",
                EVT_PASS,
                node_index,
                type(lap).__name__,
            )
            return

        payload = {
            "node_index": node_index,
            "lap_number": lap_number,
            "lap_time_stamp": lap_time_stamp,
            # Optional signal context only — its absence does not make the pass partial.
            "peak_rssi": args.get("peak_rssi"),
        }
        try:
            rhapi.ui.socket_broadcast(EVT_PASS, payload)
        except Exception:  # noqa: BLE001 - never let a pass broadcast take down RH
            logger.exception(
                "GridFPV: %s broadcast FAILED for node index %r lap %s — the Director will fall "
                "back to current_laps for this lap (#389)",
                EVT_PASS,
                node_index,
                lap_number,
            )
            return

        state["passes"] = state.get("passes", 0) + 1
        logger.debug("GridFPV %s -> %s", EVT_PASS, payload)

    if Evt is not None:
        rhapi.events.on(Evt.RACE_START, start_signal, name="gridfpv_signal_start")
        rhapi.events.on(Evt.RACE_STOP, stop_signal, name="gridfpv_signal_stop")
        rhapi.events.on(Evt.RACE_FINISH, stop_signal, name="gridfpv_signal_finish")
        # Only hook the lap event when `live_pass` was earned: an un-advertised plugin that still
        # broadcast passes would be a pass source the Director never chose (#389).
        if live_pass_ok:
            rhapi.events.on(Evt.RACE_LAP_RECORDED, on_lap_recorded, name="gridfpv_pass")
    else:  # pragma: no cover
        logger.warning("GridFPV: eventmanager.Evt unavailable; live signal/passes disabled")

    logger.info(
        "GridFPV plugin loaded (v%s, protocol v%s) — capabilities: %s",
        PLUGIN_VERSION,
        PROTOCOL_VERSION,
        ", ".join(capabilities),
    )


def _node_count(rhapi):
    """Best-effort node/seat count from the live interface (0 if unavailable)."""
    try:
        return len(rhapi.interface.seats)
    except Exception:  # pragma: no cover - defensive; never fail the handshake on this
        return 0
