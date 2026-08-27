# GridFPV RotorHazard plugin

The required in-process RotorHazard integration (decision **D16**). Design +
sliced build plan: [`docs/rotorhazard-plugin.html`](../../docs/rotorhazard-plugin.html).

A RotorHazard plugin is a directory dropped into RH's user-`plugins/` dir, with an
`__init__.py` exposing `initialize(rhapi)` and a `manifest.json` declaring
`required_rhapi_version`. RH's loader imports it and hands over the `rhapi` object.

- **Floor:** RHAPI **1.3** / RotorHazard **v4.3.0+** (declared in `manifest.json`).
- **Channel:** `gridfpv_*` events on RH's existing socket.io server (S1+).

## Installing it — where `plugins/` lives

GridFPV's console offers this plugin as a `gridfpv-plugin.zip` download. **Unzip it**,
then copy the inner `gridfpv/` folder — not the zip, and not whatever wrapper folder
your unzipper puts around it — into RotorHazard's user-`plugins/` directory, and restart
RotorHazard. You should end up with exactly:

```
<DATA_DIR>/plugins/gridfpv/__init__.py
<DATA_DIR>/plugins/gridfpv/manifest.json
```

with those files **directly** inside `gridfpv/` — no extra folder in between.

`<DATA_DIR>` is RotorHazard's *data directory*, and where it lands depends on how RH was
installed (RH resolves it through a six-step cascade: a `--data` argument, a `datapath.ini`
in the program dir, an already-existing `~/rh-data`, the program dir or CWD if `config.json`
sits there, else `~/rh-data`, created). So:

| Install | `plugins/` path |
|---|---|
| Modern default (the usual case) | `~/rh-data/plugins/` — on a Pi, `/home/pi/rh-data/plugins/` |
| Legacy in-place | `<RotorHazard>/src/server/plugins/` |
| Vendor / custom rigs (e.g. NuclearHazard) | somewhere else again |

Treat `~/rh-data/plugins/` as the **typical default install, not a guarantee**. RH v4.x
prompts users to migrate off the legacy layout, so both are in the wild. Whatever the rig,
the data dir is the folder holding RotorHazard's `config.json` / `database.db`, and RH logs
`Data path: <DATA_DIR>` at startup.

> **The `plugins/` folder may not exist yet.** RotorHazard only scans it *if present* — a
> fresh install with no user plugins has none, so there is nothing to find and you must
> create the folder yourself.

RotorHazard's own reference:
[`doc/Plugins.md`](https://github.com/RotorHazard/RotorHazard/blob/v4.3.0/doc/Plugins.md).

## Status — S3b (handshake, live dense RSSI, native passes, the Grid-owned race format)

`initialize(rhapi)` registers:

- **Handshake** — `gridfpv_hello` → `gridfpv_hello_ack` (versions, `CAPABILITIES`, node
  count) so the Director can detect the plugin and offer a guided install for a missing one.
- **Live dense RSSI** — while a race runs, a decimated loop broadcasts `gridfpv_signal`
  (per-node `current_rssi`, enter/exit levels, and the dense `history_values`/`history_times`
  window) + the race-start clock. The Director folds it into the heat's signal trace **live**,
  retiring the post-race save-then-pull.
- **Native passes** — `gridfpv_pass` per `RACE_LAP_RECORDED`, attributed by node seat. Only
  advertised (`"live_pass"`) when a load-time self-check proves this build's lap atom is
  readable; otherwise RotorHazard's own `current_laps` stays the authoritative lap source.
- **The Grid-owned race format** — a `GridFPV` format row, created once (find-or-create by
  name, so reconnects and restarts never add another) with every RH-side race decision
  neutralised, selected on `gridfpv_select_format` and acked on `gridfpv_format_ack`.

### Why the owned race format exists

RotorHazard holds opinions about when a race ends and which crossings count. So does Grid, and
Grid is the referee. Two referees is #403: a pilot flew 8 gate crossings in an open-practice
heat, RotorHazard declared a winner at lap 3, numbered the rest `-1` and marked them
late/deleted *at source*. Grid skips deleted laps — correctly — so four crossings the timer had
read perfectly were gone before Grid could see them.

The row this plugin owns therefore carries:

| field | value | why |
|---|---|---|
| `win_condition` | `WinCondition.NONE` (0) | the root cause of #403 — no winner is ever declared, so no crossing is ever marked late |
| `number_laps_win` | 0 | the lap cap behind #403 |
| `unlimited_time` | 1 | Grid owns the time limit and drives the stop |
| `race_time_sec` | 0 | moot under `unlimited_time`; pinned so no stale countdown survives |
| `lap_grace_sec` | -1 | RH's *neutral* — every grace check is guarded by `> -1`. `0` would be a zero-second grace, i.e. stricter |
| `team_racing_mode` | `INDIVIDUAL` (0) | team/co-op modes change lap aggregation and add their own late-lap rules |
| `start_behavior` | `HOLESHOT` (0) | `FIRST_LAP` makes RH do `lap_number += 1`, shifting the numbering Grid dedups and sequences on |
| staging tones / start delays | 0 | Grid owns the start procedure — its tone is the only go |

Grid **never mutates the race director's own format**. Theirs sits untouched, so handing the
timer back is `race.raceformat = <theirs>` — reversible by construction, with no snapshot to
restore and nothing left behind if Grid dies mid-race. The displaced format's id and name come
back on every ack so the Director can name it.

Verified on RH **4.3.0** (RHAPI 1.3, the floor) and **4.4.0** (RHAPI 1.4): same
`raceformat_add` signature, same `RaceFormat` columns, same coercions. Note `unlimited_time`'s
DB column is `race_mode` — a rename that going through RHAPI rather than raw columns makes a
non-event.

Confirm the plugin loads with `cargo xtask rh-mock plugin-check`; drive a live heat with
`cargo xtask rh-mock feed clean --plugin` and watch `gridfpv_signal` flow.

## Roadmap

| Slice | Adds | Status |
|-------|------|--------|
| S1 | `gridfpv_hello` handshake via `socket_listen`; capabilities/version | ✅ shipped |
| S2 | live dense RSSI broadcast (`gridfpv_signal`) — retires save-then-pull | ✅ shipped |
| S3 | per-node passes (`gridfpv_pass` from `RACE_LAP_RECORDED`) ✅; native start/stop + delete socket hacks → staged for review | ◐ partial |
| S3b | the Grid-owned `GridFPV` race format (`gridfpv_select_format`) — RH makes no race decisions | ✅ shipped |
| S4 | threshold recalculate (#3) over the stored dense trace + `gridfpv_calibrate` | |

S3 added the `"live_pass"` capability: the plugin emits each pass natively from
`RACE_LAP_RECORDED`, attributed by node seat (the Director folds it like a `current_laps`
lap, deduped on `lap_number`).

S3b added `"owned_format"` (#403/#404/#405) — see above. The Director keeps its legacy in-place
neutralisation as a loud fallback for plugin builds older than `owned_format`.

### What is actually left of "clean_control" (#423)

The audit against RotorHazard's source split the remaining half in two, and only one half is
worth building:

- **Start/stop: don't.** `on_stage_race` and `on_stop_race` *are* `race.stage()` and
  `race.stop()` — the same calls RHAPI would make, identical on 4.3.0 and 4.4.0. Routing them
  Director → plugin → RHAPI adds a hop and a capability gate to reach the same line of Python.
  Same for `save_laps` (`race.save()`), `discard_laps` (`race.clear()`), `set_current_heat`
  (`race.heat =`) and `set_race_format` — the last of which RHAPI **monkey-patches to the socket
  handler itself** (`RHAPI.race._raceformat_set = on_set_race_format`). "Newer" is not "better".
- **Seating: yes.** `db.heat_add()` and `db.pilot_add()` **return the row they create**. The
  socket dance cannot: it emits, waits up to 3 s for a broadcast, and infers the new id as "the
  highest" / "the one above the floor" — a heuristic that is wrong the moment anything else
  touches RH's heats or pilots concurrently. That is the real win, and it needs a socket fallback
  for the v0.3.0 field plugin.

Until then, the socket seating dance is kept honest by the readback added in #423: the Director
re-reads `heat_data` and checks each slot's `pilot_id` before racing the heat, because
`alter_heat` answers `emit_heat_data(noself=True)` — excluding the very socket that wrote — and a
seat that silently failed makes RH dismiss **every** crossing on that node.
