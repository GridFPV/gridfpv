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

## Status — S2 (handshake + live dense RSSI)

`initialize(rhapi)` registers two things:

- **Handshake** — `gridfpv_hello` → `gridfpv_hello_ack` (versions, `CAPABILITIES`, node
  count) so the Director can detect the plugin and offer a guided install for a missing one.
- **Live dense RSSI** — while a race runs, a decimated loop broadcasts `gridfpv_signal`
  (per-node `current_rssi`, enter/exit levels, and the dense `history_values`/`history_times`
  window) + the race-start clock. The Director folds it into the heat's signal trace **live**,
  retiring the post-race save-then-pull.

Confirm the plugin loads with `cargo xtask rh-mock plugin-check`; drive a live heat with
`cargo xtask rh-mock feed clean --plugin` and watch `gridfpv_signal` flow.

## Roadmap

| Slice | Adds | Status |
|-------|------|--------|
| S1 | `gridfpv_hello` handshake via `socket_listen`; capabilities/version | ✅ shipped |
| S2 | live dense RSSI broadcast (`gridfpv_signal`) — retires save-then-pull | ✅ shipped |
| S3 | per-node passes (`gridfpv_pass` from `RACE_LAP_RECORDED`) ✅; native start/stop + delete socket hacks → staged for review | ◐ partial |
| S4 | threshold recalculate (#3) over the stored dense trace + `gridfpv_calibrate` | |

S3 added the `"live_pass"` capability: the plugin emits each pass natively from
`RACE_LAP_RECORDED`, attributed by node seat (the Director folds it like a `current_laps`
lap, deduped on `lap_number`). The "clean_control" half — native `race.stage()`/`stop()`
replacing the `alter_race_format`/seating socket workarounds — is planned next.
