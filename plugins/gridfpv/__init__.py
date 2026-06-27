"""GridFPV RotorHazard plugin.

The in-process RH integration described in ``docs/rotorhazard-plugin.html`` (decision
D16). It runs inside the RH server with RHAPI access and talks to the GridFPV Director
over a ``gridfpv_*`` event namespace on RH's existing socket.io server.

**Slice 1 — handshake.** This registers the ``gridfpv_hello`` handshake: the Director
emits ``gridfpv_hello`` over the socket.io connection it already holds, and the plugin
replies with ``gridfpv_hello_ack`` carrying its protocol/plugin/RHAPI versions, its
declared capabilities, and the node count. That lets the Director detect a
plugin-equipped RH (and offer a guided install for one that's missing). The live dense
RSSI, clean start/stop, and threshold recalculate arrive in later slices (S2–S4); their
capability flags are added to ``CAPABILITIES`` as they land.

Floor: RHAPI 1.3 / RotorHazard v4.3.0+ (declared in ``manifest.json``).
"""

import logging

logger = logging.getLogger(__name__)

# The gridfpv_* wire-protocol version. Bumped only on a breaking change to the
# handshake/message shapes; the Director declares the range it supports and negotiates
# against this in the hello/ack exchange.
PROTOCOL_VERSION = 1

# This plugin build's own version (independent of PROTOCOL_VERSION). Keep in step with
# manifest.json's "version".
PLUGIN_VERSION = "0.1.0"

# Capabilities this build actually implements — the Director keys transport decisions off
# these (e.g. it only prefers the plugin's live-signal path once "live_signal" appears).
# S1 ships the handshake only; later slices append "live_signal", "clean_control",
# "recalc".
CAPABILITIES = ["hello"]

# The socket.io event names of the gridfpv_* namespace (S1 subset).
EVT_HELLO = "gridfpv_hello"
EVT_HELLO_ACK = "gridfpv_hello_ack"


def initialize(rhapi):
    """RH plugin entry point — register the gridfpv_* handlers on RH's socket.io server.

    Called once by RH's loader with the ``rhapi`` object. We register a ``gridfpv_hello``
    listener that replies (to the asking client only) with ``gridfpv_hello_ack``.
    """

    def on_hello(_data=None):
        # `socket_listen` -> flask-socketio `on_event`; replying with `socket_send`
        # emits in the asking client's request context, i.e. only back to the Director
        # that sent the hello (not a broadcast).
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
    logger.info(
        "GridFPV plugin loaded (v%s, protocol v%s) — gridfpv_hello handshake registered",
        PLUGIN_VERSION,
        PROTOCOL_VERSION,
    )


def _node_count(rhapi):
    """Best-effort node/seat count from the live interface (0 if unavailable)."""
    try:
        return len(rhapi.interface.seats)
    except Exception:  # pragma: no cover - defensive; never fail the handshake on this
        return 0
