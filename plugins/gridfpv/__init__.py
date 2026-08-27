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

**Slice 3b — the Grid-owned race format.** RotorHazard holds its own opinions about when a
race ends and which crossings count; Grid holds the same opinions and is the referee. Two
referees is how #403 happened — RH declared a winner at lap 3 of an open-practice heat and
marked the pilot's remaining four crossings late/deleted at source, so Grid never saw them.
The plugin therefore creates (find-or-create, once) a **`GridFPV` race format** with every
RH-side race decision neutralised, and selects it on request (`gridfpv_select_format` →
`gridfpv_format_ack`). The RD's own format row is never touched, so the takeover is
reversible by construction: `race.raceformat = <theirs>` puts it back. Declared via the
``"owned_format"`` capability (#404, #405).

**Slice 3c — RotorHazard's min-lap filter, neutralised.** RH carries its own minimum-lap
rule (``MinLapSec``, default **10 s**) plus a behaviour flag (``TIMING``/``MinLapBehavior``)
that can **discard** a sub-minimum crossing outright rather than merely flag it. A discarded
crossing never reaches Grid at all, so Grid's own per-round floor
(``VoidReason::UnderMinLap``, D26) never gets to run on it and #397's rejected-crossing tone
never fires — the crossing the RD most needs to hear about is precisely the one RH threw
away. The plugin reads both values, records them, and zeroes both, so every crossing reaches
Grid and Grid referees. Reported in the hello ack and re-asserted (and re-reported) on every
``gridfpv_select_format``. Declared via the ``"min_lap_neutral"`` capability (#407).

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
PLUGIN_VERSION = "0.4.0"

# Capabilities this build implements — the Director keys transport decisions off these.
#
# "clean_control" is deliberately NOT here, and #423's audit narrowed what it should ever mean.
# Native start/stop is not worth a capability: RH's `on_stage_race`/`on_stop_race`/`on_save_laps`
# ARE `race.stage()`/`race.stop()`/`race.save()`, identical on 4.3.0 and 4.4.0, so going through
# the plugin adds a hop and a version gate to reach the same call. Native *seating* is worth it —
# `db.heat_add()`/`db.pilot_add()` return the row, retiring the Director's "learn the new id from
# a broadcast" heuristic — and that is what a future capability here should cover. See README.md.
#
# BASE_CAPABILITIES are unconditional. PASS_CAPABILITY ("live_pass" — the plugin emits per-node
# passes natively from RACE_LAP_RECORDED, so the Director takes the pass atom directly rather
# than diffing the current_laps snapshot) is **earned, not assumed**: it is added to the advertised
# set only when the load-time self-check (`_self_check_live_pass`) proves this RotorHazard build's
# lap atom is readable. Advertising it makes the plugin the *authoritative* pass source on the
# Director side, so a plugin that cannot actually read a lap must not claim it — it must degrade
# to RotorHazard's own `current_laps` rather than pre-empt it (#389).
BASE_CAPABILITIES = ["hello", "live_signal", "owned_format", "min_lap_neutral"]
PASS_CAPABILITY = "live_pass"
# The Grid-owned race format (#404/#405). Unconditional, unlike `live_pass`: the Director's
# fallback here is the OLD behaviour (alter the RD's active format in place), which is
# safe-but-wrong rather than lap-destroying, and a load-time DB hiccup must not strand a timer on
# it forever. So the capability is advertised, the load-time outcome is REPORTED in the hello ack
# (`grid_format_id` / `grid_format_error`), and every `gridfpv_select_format` retries.
FORMAT_CAPABILITY = "owned_format"
# RotorHazard's own min-lap filter, neutralised (#407). Unconditional and reported rather than
# earned, for the same reason as `owned_format`: the Director's fallback is its own socket route
# (`set_min_lap` / `set_min_lap_behavior`), which is a worse place to do it but not a lossy one, and
# a load-time DB hiccup must not strand a timer forever. The Director keys off this capability to
# decide whether the plugin is even *trying* — a plugin build older than this one advertises
# nothing here, and the Director does the job itself over the socket.
MIN_LAP_CAPABILITY = "min_lap_neutral"
# Everything this build implements, for reference/tests. The *advertised* set is computed per
# server in `initialize()` and reported in the hello ack.
CAPABILITIES = BASE_CAPABILITIES + [PASS_CAPABILITY]

# Socket.io event names of the gridfpv_* namespace.
EVT_HELLO = "gridfpv_hello"
EVT_HELLO_ACK = "gridfpv_hello_ack"
EVT_SIGNAL = "gridfpv_signal"
EVT_PASS = "gridfpv_pass"
EVT_SELECT_FORMAT = "gridfpv_select_format"
EVT_FORMAT_ACK = "gridfpv_format_ack"

# ---- The Grid-owned race format (#403, #404, #405) --------------------------------------------
# The name of the format row this plugin owns. Find-or-create keys off it, so the row is created
# once EVER (not once per connect, not once per restart), and an RD reading RotorHazard's format
# list can see plainly whose it is.
GRID_FORMAT_NAME = "GridFPV"

# Every race-conduct field on RotorHazard's `RaceFormat`, set so RH makes **no race decisions at
# all** — it detects crossings, Grid referees. Verified against `Database.py::RaceFormat` and
# `RHData.py::add_format` / `alter_raceFormat` on RH 4.3.0 (RHAPI 1.3, our floor) and RH 4.4.0
# (RHAPI 1.4): the column set, the ORM attribute names and the coercions are identical on both.
#
#  win_condition = WinCondition.NONE (0)
#      THE root cause of #403. With any other value RH declares a winner, and `RHRace.py` then sets
#      `lap_data.deleted = lap_late_flag` on every later crossing — numbering it -1 and deleting it
#      at source. Grid correctly skips deleted laps, so those crossings are gone before Grid can
#      see them. WinCondition.NONE means RH never declares, never marks late, never deletes.
#  number_laps_win = 0
#      The lap cap that fired in #403 (`lap_number >= number_laps_win` -> pilot done -> late laps).
#  unlimited_time = 1
#      Grid owns the time limit and drives the stop. NOTE the DB column is `race_mode` — renamed
#      across versions — while the ORM attribute is `unlimited_time` on both 4.3.0 and 4.4.0.
#      Going through RHAPI rather than touching columns is what makes that rename a non-event.
#  race_time_sec = 0
#      Moot under unlimited_time, but pinned so the row cannot carry a stale countdown.
#  lap_grace_sec = -1
#      RH's neutral value: every grace check in `RHRace.py` is guarded by `lap_grace_sec > -1`, so
#      -1 disables the post-expiry cutoff entirely. (0 would be a ZERO-second grace — stricter, not
#      neutral. This one is a trap.)
#  team_racing_mode = RacingMode.INDIVIDUAL (0)
#      Team and co-op modes change how RH aggregates laps AND add their own late-lap rules.
#  start_behavior = StartBehavior.HOLESHOT (0)
#      FIRST_LAP makes RH do `lap_number += 1`, shifting the numbering Grid dedups and sequences
#      on. Grid assumes holeshot-first everywhere; anything else is a latent correctness bug.
#  staging_fixed_tones / staging_delay_tones / start_delay_min_ms / start_delay_max_ms = 0
#      Grid owns the start procedure — its tone is the only go. (What the Director already did to
#      the RD's format; now it lives on Grid's own row instead.)
GRID_FORMAT_FIELDS = {
    "unlimited_time": 1,
    "race_time_sec": 0,
    "lap_grace_sec": -1,
    "staging_fixed_tones": 0,
    "staging_delay_tones": 0,
    "start_delay_min_ms": 0,
    "start_delay_max_ms": 0,
    "start_behavior": 0,
    "win_condition": 0,
    "number_laps_win": 0,
    "team_racing_mode": 0,
}

# ---- RotorHazard's min-lap filter (#407) ------------------------------------------------------
# RH applies its OWN minimum-lap rule underneath Grid's, and can discard a crossing before Grid
# ever sees it. Grid already owns this decision per round (D26: `VoidReason::UnderMinLap`,
# surfaced in marshaling, reversible, enforced on every live fold by #409). Two referees is the
# #403/#405 problem again — and this one is worse than a wrong count, because a discarded crossing
# is not merely miscounted, it is *absent*: Grid's floor cannot run on it, marshaling cannot
# restore it, and #397's rejected-crossing tone — the single most useful thing the RD gets out of a
# sub-minimum pass — never fires.
#
# The two values, verified by reading the RotorHazard source on BOTH supported versions
# (`RHRace.py::pass_record_callback`, v4.3.0/RHAPI 1.3 — our floor — and v4.4.0/RHAPI 1.4):
#
#   min_lap          = rhdata.get_optionInt("MinLapSec")                     # a DB *option*
#   min_lap_behavior = serverconfig.get_item_int("TIMING", "MinLapBehavior") # a *server config* item
#   if lap_ok_flag and lap_time < (min_lap * 1000):
#       ...
#       if min_lap_behavior != 0:   # 'Discard New Short Laps'
#           lap_ok_flag = False     # <- the crossing is never recorded, never emitted, GONE
#
# Note the names. RotorHazard has NO option called `MIN_LAP_TIME` — that string is only the name of
# an *event* constant (`Evt.MIN_LAP_TIME_SET`). Writing an option by that name (which is what
# Grid's old `set_min_lap_time` transport helper did) stores a row nothing on the server ever
# reads: a no-op that looks like a success. The real keys are the two above, and they live in two
# different stores, which is why one setter cannot do it.
#
# Zeroing BOTH is deliberate belt-and-braces. Either alone is sufficient today — `MinLapSec = 0`
# makes the `lap_time < 0` test unreachable, and `MinLapBehavior = 0` ('highlight') leaves the
# crossing recorded — but they are independent settings on two independent screens, and Grid must
# not depend on the RD leaving the other one alone.
MIN_LAP_OPTION = "MinLapSec"
MIN_LAP_BEHAVIOR_SECTION = "TIMING"
MIN_LAP_BEHAVIOR_ITEM = "MinLapBehavior"
# What Grid applies. These are GRID's values, written here in Grid's own source — never derived
# from whatever the timer happened to be set to (D27: a value read from a timer is evidence about
# the timer, not an input to a decision).
MIN_LAP_NEUTRAL_SECS = 0
MIN_LAP_BEHAVIOR_HIGHLIGHT = 0


def _read_min_lap(rhapi):
    """RotorHazard's current ``(MinLapSec, TIMING/MinLapBehavior)`` as ints, ``None`` where unread.

    Read through RHAPI, not the ORM: ``db.option`` / ``config.get`` are the supported surface and
    are byte-identical on RHAPI 1.3 and 1.4. Each half is read independently so one unreadable
    value does not hide the other — half a reading is still worth reporting.
    """
    try:
        secs = rhapi.db.option(MIN_LAP_OPTION, as_int=True)
        secs = None if secs is None else int(secs)
    except Exception:  # noqa: BLE001 - reported as None; the caller says so out loud
        logger.exception("GridFPV: could not read RotorHazard's %s", MIN_LAP_OPTION)
        secs = None
    try:
        behavior = rhapi.config.get(
            MIN_LAP_BEHAVIOR_SECTION, MIN_LAP_BEHAVIOR_ITEM, as_int=True
        )
        behavior = None if behavior is None else int(behavior)
    except Exception:  # noqa: BLE001
        logger.exception(
            "GridFPV: could not read RotorHazard's %s/%s",
            MIN_LAP_BEHAVIOR_SECTION,
            MIN_LAP_BEHAVIOR_ITEM,
        )
        behavior = None
    return secs, behavior


def ensure_min_lap_neutral(rhapi, state):
    """Read RH's min-lap filter, zero it, and **confirm by re-reading**; return the wire report.

    The report is what the Director announces and records — Grid's own note of what it found on
    this timer and what it applied, never a value Grid then treats as its own config (D27).

    ``secs_was`` / ``behavior_was`` are the values seen the **first** time this plugin touched this
    server, stashed in ``state`` and never overwritten. That matters: this runs again at every
    ``gridfpv_select_format``, and a naive re-read would report Grid's own zero back as "what the
    race director had", erasing the only record of the setting Grid displaced.

    Never raises — a failure is reported, not thrown, because a timer whose filter could not be
    neutralised must still connect (so the Director can *say* so) rather than fall over.
    """
    secs_was, behavior_was = _read_min_lap(rhapi)
    if state.get("min_lap_was") is None:
        state["min_lap_was"] = {"secs": secs_was, "behavior": behavior_was}
        if secs_was or behavior_was:
            logger.info(
                "GridFPV: RotorHazard's own min-lap filter is MinLapSec=%s, %s/%s=%s — Grid is "
                "zeroing both so every crossing reaches Grid and Grid's per-round floor "
                "referees it (#407). Restore them in RotorHazard's settings to hand the timer "
                "back.",
                secs_was,
                MIN_LAP_BEHAVIOR_SECTION,
                MIN_LAP_BEHAVIOR_ITEM,
                behavior_was,
            )
    first_seen = state["min_lap_was"]

    errors = []
    if secs_was != MIN_LAP_NEUTRAL_SECS:
        try:
            rhapi.db.option_set(MIN_LAP_OPTION, MIN_LAP_NEUTRAL_SECS)
        except Exception as exc:  # noqa: BLE001
            logger.exception("GridFPV: could not set %s", MIN_LAP_OPTION)
            errors.append("{0}: {1!r}".format(MIN_LAP_OPTION, exc))
    if behavior_was != MIN_LAP_BEHAVIOR_HIGHLIGHT:
        try:
            rhapi.config.set(
                MIN_LAP_BEHAVIOR_SECTION,
                MIN_LAP_BEHAVIOR_ITEM,
                MIN_LAP_BEHAVIOR_HIGHLIGHT,
            )
        except Exception as exc:  # noqa: BLE001
            logger.exception(
                "GridFPV: could not set %s/%s",
                MIN_LAP_BEHAVIOR_SECTION,
                MIN_LAP_BEHAVIOR_ITEM,
            )
            errors.append(
                "{0}/{1}: {2!r}".format(
                    MIN_LAP_BEHAVIOR_SECTION, MIN_LAP_BEHAVIOR_ITEM, exc
                )
            )

    # Read back rather than trusting the write — the same discipline `_format_drift` applies, and
    # for the same reason: RH coerces on the way in, and a coercion that quietly kept the old value
    # would leave Grid racing on a filter it believes it removed.
    secs_now, behavior_now = _read_min_lap(rhapi)
    ok = secs_now == MIN_LAP_NEUTRAL_SECS and behavior_now == MIN_LAP_BEHAVIOR_HIGHLIGHT
    if not ok and not errors:
        errors.append(
            "still MinLapSec={0!r}, {1}/{2}={3!r} after the write".format(
                secs_now, MIN_LAP_BEHAVIOR_SECTION, MIN_LAP_BEHAVIOR_ITEM, behavior_now
            )
        )
    if not ok:
        logger.error(
            "GridFPV: RotorHazard's min-lap filter is NOT neutralised (%s) — this timer may "
            "DISCARD sub-minimum crossings before Grid ever sees them, which silently disables "
            "Grid's own min-lap ruling and its rejected-crossing tone (#397/#407)",
            "; ".join(errors),
        )
    return {
        "ok": ok,
        # What Grid found the first time it touched this server — the hand-back record.
        "secs_was": first_seen.get("secs"),
        "behavior_was": first_seen.get("behavior"),
        # What the timer reads NOW, after the write and a confirming re-read.
        "secs_now": secs_now,
        "behavior_now": behavior_now,
        "error": "; ".join(errors) if errors else None,
    }


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


def _format_drift(rhapi, format_id):
    """Which [`GRID_FORMAT_FIELDS`] the row `format_id` does NOT currently carry.

    Returns ``{field: (found, wanted)}`` — empty when the row is exactly neutral. Read back
    through ``raceformat_by_id`` rather than trusting the write, because *every* value RH stores
    here goes through a coercion (``unlimited_time`` is truthy-mapped, ``staging_delay_tones`` maps
    to 2-or-0, ``team_racing_mode`` falls back to INDIVIDUAL) and a coercion that quietly turned
    our 0 into something else is exactly the class of bug this whole change exists to kill.
    """
    row = rhapi.db.raceformat_by_id(format_id)
    if row is None:
        raise RuntimeError("race format id {0} does not exist".format(format_id))
    drift = {}
    for field, wanted in GRID_FORMAT_FIELDS.items():
        found = getattr(row, field, None)
        if found != wanted:
            drift[field] = (found, wanted)
    return drift


def _find_grid_format(rhapi):
    """The existing `GridFPV` format row, or ``None`` — the *find* half of find-or-create.

    Keyed on the name, which is what makes the whole thing idempotent across reconnects AND
    across RotorHazard restarts: the row is looked up in RH's own database, not remembered in
    plugin state. RH does not constrain format names to be unique, so if a rig somehow ends up
    with several we take the lowest id (the original) and say so rather than adding another.
    """
    owned = [
        fmt
        for fmt in (rhapi.db.raceformats or [])
        if getattr(fmt, "name", None) == GRID_FORMAT_NAME
    ]
    if not owned:
        return None
    owned.sort(key=lambda fmt: fmt.id)
    if len(owned) > 1:
        logger.warning(
            "GridFPV: %s race format rows are named '%s' (ids %s) — using the first and leaving "
            "the rest alone; delete the duplicates in RotorHazard if they bother you",
            len(owned),
            GRID_FORMAT_NAME,
            [fmt.id for fmt in owned],
        )
    return owned[0]


def ensure_grid_format(rhapi):
    """**Find-or-create** the Grid-owned `GridFPV` race format; return ``(format_id, created,
    repaired)``.

    Idempotent by construction — the row is found by name in RH's database, so a reconnect, a
    plugin reload and an RH restart all land on the same row and no `GridFPV` rows accumulate.
    ``repaired`` names the fields that had drifted (an RD editing Grid's row in RH's UI, or an RH
    upgrade changing a default) and were written back; an empty list is the normal steady state.

    Raises if the row cannot be created, or if it still is not neutral after a repair — the caller
    turns that into a failed ack, which the Director announces. A silently un-neutralised timer is
    exactly #403.
    """
    existing = _find_grid_format(rhapi)
    if existing is None:
        added = rhapi.db.raceformat_add(name=GRID_FORMAT_NAME, **GRID_FORMAT_FIELDS)
        format_id = getattr(added, "id", None)
        if format_id is None:
            raise RuntimeError(
                "raceformat_add returned {0!r}, which carries no id".format(added)
            )
        created = True
    else:
        format_id = existing.id
        created = False

    drift = _format_drift(rhapi, format_id)
    if drift:
        rhapi.db.raceformat_alter(format_id, name=GRID_FORMAT_NAME, **GRID_FORMAT_FIELDS)
        remaining = _format_drift(rhapi, format_id)
        if remaining:
            raise RuntimeError(
                "race format {0} ('{1}') is still not neutral after a repair: {2} "
                "(field: (found, wanted))".format(format_id, GRID_FORMAT_NAME, remaining)
            )
    return format_id, created, sorted(drift)


def select_grid_format(rhapi, state):
    """Ensure the `GridFPV` format exists and is **selected** as RH's current race format.

    Returns the ``gridfpv_format_ack`` payload. Selecting — rather than mutating whatever format
    the RD had selected — is the whole point (#404): their row is never touched, so handing the
    timer back is just ``race.raceformat = <theirs>``, with no snapshot/restore bookkeeping to get
    wrong and nothing left behind if Grid dies mid-race. The first format we displace is recorded
    (and reported on every ack) so that hand-back is a known id rather than a guess.

    RotorHazard refuses a format change while a race is running (`on_set_race_format` requires
    ``RaceStatus.READY``), which is why the Director asks for this at **Stage**, pre-Armed.
    """
    current = rhapi.race.raceformat
    previous_id = getattr(current, "id", None)
    previous_name = getattr(current, "name", None)

    format_id, created, repaired = ensure_grid_format(rhapi)

    if previous_id != format_id:
        # Remember the RD's own format the FIRST time we take the timer over, so the hand-back
        # target survives however many times Grid re-selects its own row afterwards.
        if state.get("displaced") is None and previous_id is not None:
            state["displaced"] = {"id": previous_id, "name": previous_name}
            logger.info(
                "GridFPV: taking the timer over from race format '%s' (%s) — that row is left "
                "untouched; select it again in RotorHazard to hand the timer back",
                previous_name,
                previous_id,
            )
        rhapi.race.raceformat = format_id
    elif repaired:
        # Already current, but the row we just repaired is the one RH is holding in memory —
        # re-select so `RaceContext.race.format` is re-read from the database.
        rhapi.race.raceformat = format_id

    selected = getattr(rhapi.race.raceformat, "id", None)
    if selected != format_id:
        raise RuntimeError(
            "RotorHazard did not select race format {0}; it is on {1!r} (a format change is "
            "refused unless the race status is READY)".format(format_id, selected)
        )

    state["format_id"] = format_id
    displaced = state.get("displaced") or {}
    # Re-assert the min-lap neutralisation at every stage, exactly as the format's own fields are
    # re-verified above: RH's filter lives on a settings screen the RD can reach mid-session, and a
    # value that drifts back between heats would take the rejected-crossing tone with it (#407).
    min_lap = ensure_min_lap_neutral(rhapi, state)
    return {
        "ok": True,
        "format_id": format_id,
        "format_name": GRID_FORMAT_NAME,
        "created": created,
        "repaired": repaired,
        "fields": dict(GRID_FORMAT_FIELDS),
        # The RD's own format, for the hand-back. Names, not just ids, so the Director can say
        # something an RD recognises.
        "previous_format_id": displaced.get("id"),
        "previous_format_name": displaced.get("name"),
        # RotorHazard's own min-lap filter as of this stage (#407) — `ok: false` means the timer
        # may still discard crossings before Grid sees them, and the Director says so.
        "min_lap": min_lap,
        "error": None,
    }


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
    # `format_id`: the Grid-owned `GridFPV` race format row (see `ensure_grid_format`).
    # `format_error`: why that row could not be readied, if it could not — reported in the hello
    # ack so the Director can announce it. `displaced`: the RD's own format we took the timer over
    # from, for the hand-back.
    state = {
        "greenlet": None,
        "acc": {},
        "passes": 0,
        "format_id": None,
        "format_error": None,
        "displaced": None,
        "min_lap_was": None,
    }

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

    # ---- S3b gate: the Grid-owned race format (#403, #404, #405) ------------------------
    # Create it ONCE, eagerly, so the row exists before the Director ever asks for it and so a
    # failure shows up in RotorHazard's own log rather than at the start of a heat. Find-or-create
    # keyed on the name: reconnects, plugin reloads and RH restarts all land on the same row.
    #
    # Hung off Evt.STARTUP, NOT done inline here: on a first boot RotorHazard loads plugins
    # *before* it creates the database (`server.py` calls `rh_program_initialize` — which loads
    # plugins — and only then `Database.create_db_all()`), so a `raceformat_add` at load time hits
    # "no such table: race_format" on exactly the rig where a clean setup matters most. STARTUP
    # fires after the schema exists, on fresh and existing databases alike.
    #
    # A failure is NOT fatal and does NOT withdraw the capability — unlike `live_pass`, whose
    # fallback would destroy laps, this one's fallback is the Director's old behaviour (mutate the
    # RD's active format), which is merely wrong rather than lossy. So we record the failure, let
    # the hello ack report it for the Director to announce, and retry on every
    # `gridfpv_select_format`.
    def ready_grid_format(_args=None):
        try:
            format_id, created, repaired = ensure_grid_format(rhapi)
            state["format_id"] = format_id
            state["format_error"] = None
            logger.info(
                "GridFPV: %s the '%s' race format (id %s)%s — RotorHazard will declare no winner, "
                "apply no lap cap, no time limit and no team aggregation while Grid drives (#403)",
                "created" if created else "reusing",
                GRID_FORMAT_NAME,
                format_id,
                "; repaired {0}".format(repaired) if repaired else "",
            )
        except Exception as exc:  # noqa: BLE001 - reported, retried at select time, never fatal
            state["format_error"] = "{0!r}".format(exc)
            logger.exception(
                "GridFPV: could NOT create/repair the '%s' race format — until it succeeds the "
                "Director falls back to altering the race director's own active format, which is "
                "what #403 was. Retried on the next %s.",
                GRID_FORMAT_NAME,
                EVT_SELECT_FORMAT,
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

    # ---- S3c: neutralise RotorHazard's own min-lap filter (#407) ------------------------
    # Done eagerly at load — like the prestage pad, and unlike the race format, because neither
    # value lives in the database RH has not created yet: `MinLapSec` is an option row and
    # `TIMING/MinLapBehavior` is a config-file item, both readable and writable before STARTUP.
    # Doing it here means the filter is already gone before the Director ever connects, so a
    # crossing detected during setup is not silently eaten. Re-asserted at every stage (see
    # `select_grid_format`) because the RD can move it back from RH's own settings screen.
    min_lap_report = ensure_min_lap_neutral(rhapi, state)

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
            # The Grid-owned race format (#404): its row id, or None with the reason it could not
            # be created. The Director announces a None through its diagnostic sink and falls back
            # to altering the RD's active format — loudly, never silently.
            "grid_format_id": state.get("format_id"),
            "grid_format_name": GRID_FORMAT_NAME,
            "grid_format_error": state.get("format_error"),
            # RotorHazard's own min-lap filter (#407): what Grid found and what it applied. A
            # missing key (an older plugin) or `ok: false` both mean the Director must neutralise
            # it itself over the socket — and say out loud that it is doing so.
            "min_lap": min_lap_report,
        }
        logger.info("GridFPV hello -> ack %s", ack)
        rhapi.ui.socket_send(EVT_HELLO_ACK, ack)

    rhapi.ui.socket_listen(EVT_HELLO, on_hello)

    # ---- S3b: select the Grid-owned race format ----------------------------------------
    def on_select_format(_data=None):
        """Ensure the `GridFPV` format exists and is RH's current format, then ack the outcome.

        The Director asks for this at each heat's **Stage** (pre-Armed) — RotorHazard refuses a
        format change during an active race. Idempotent: repeat calls re-verify the row's fields
        and re-select, which is what makes the second, pre-`stage_race` call cheap and safe.
        """
        try:
            ack = select_grid_format(rhapi, state)
            if ack["created"] or ack["repaired"]:
                logger.info("GridFPV: race format ready -> %s", ack)
            else:
                logger.debug("GridFPV: race format ready -> %s", ack)
        except Exception as exc:  # noqa: BLE001 - reported to the Director, never fatal
            logger.exception(
                "GridFPV: could not select the '%s' race format — acking the failure so the "
                "Director says so and falls back to altering the RD's format (#403/#404)",
                GRID_FORMAT_NAME,
            )
            ack = {
                "ok": False,
                "format_id": state.get("format_id"),
                "format_name": GRID_FORMAT_NAME,
                "created": False,
                "repaired": [],
                "fields": dict(GRID_FORMAT_FIELDS),
                "previous_format_id": (state.get("displaced") or {}).get("id"),
                "previous_format_name": (state.get("displaced") or {}).get("name"),
                # Best effort even on a failed format select: the two are independent, and an RD
                # whose format select failed still needs to know whether crossings are reaching
                # Grid.
                "min_lap": ensure_min_lap_neutral(rhapi, state),
                "error": "{0!r}".format(exc),
            }
        rhapi.ui.socket_send(EVT_FORMAT_ACK, ack)

    rhapi.ui.socket_listen(EVT_SELECT_FORMAT, on_select_format)

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
        # Ready the Grid-owned race format once the database exists (see `ready_grid_format`).
        rhapi.events.on(Evt.STARTUP, ready_grid_format, name="gridfpv_format")
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
        "GridFPV plugin loaded (v%s, protocol v%s) — capabilities: %s; min-lap filter: %s",
        PLUGIN_VERSION,
        PROTOCOL_VERSION,
        ", ".join(capabilities),
        (
            "neutralised (was MinLapSec={0}, {1}={2})".format(
                min_lap_report["secs_was"],
                MIN_LAP_BEHAVIOR_ITEM,
                min_lap_report["behavior_was"],
            )
            if min_lap_report["ok"]
            else "NOT NEUTRALISED ({0})".format(min_lap_report["error"])
        ),
    )


def _node_count(rhapi):
    """Best-effort node/seat count from the live interface (0 if unavailable)."""
    try:
        return len(rhapi.interface.seats)
    except Exception:  # pragma: no cover - defensive; never fail the handshake on this
        return 0
