"""GridFPV RotorHazard plugin — S0 placeholder.

This is the in-process RH integration described in ``docs/rotorhazard-plugin.html``
(decision D16). At S0 it is an **empty, load-only skeleton**: RH's plugin loader
imports this module and calls :func:`initialize`, which currently does nothing. Its
sole job at S0 is to prove the loader path end-to-end inside the RH v4.4.0 dev
harness — RH logs the plugin as loaded with no ``load_issue``.

The real handshake (``gridfpv_hello`` over ``rhapi.ui.socket_listen``), live dense
RSSI, clean start/stop, and the threshold recalculate all arrive in later slices
(S1–S4). Nothing here taps RHAPI yet.

Floor: RHAPI 1.3 / RotorHazard v4.3.0+ (declared in ``manifest.json``).
"""

import logging

logger = logging.getLogger(__name__)


def initialize(rhapi):
    """RH plugin entry point — called once by the loader with the ``rhapi`` object.

    S0 placeholder: log that we loaded, then return. No event hooks, no socket
    handlers, no RHAPI calls yet (those land in S1+). ``rhapi`` is accepted and
    intentionally unused so the signature matches what the loader invokes.
    """
    logger.info("GridFPV plugin loaded (S0 placeholder — no handlers registered yet)")
