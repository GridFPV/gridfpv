# GridFPV RotorHazard plugin

The required in-process RotorHazard integration (decision **D16**). Design +
sliced build plan: [`docs/rotorhazard-plugin.html`](../../docs/rotorhazard-plugin.html).

A RotorHazard plugin is a directory dropped into RH's user-`plugins/` dir, with an
`__init__.py` exposing `initialize(rhapi)` and a `manifest.json` declaring
`required_rhapi_version`. RH's loader imports it and hands over the `rhapi` object.

- **Floor:** RHAPI **1.3** / RotorHazard **v4.3.0+** (declared in `manifest.json`).
- **Channel:** `gridfpv_*` events on RH's existing socket.io server (S1+).

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
| S3 | clean start/stop (`race.stage()`/`stop()`) + per-node passes | next |
| S4 | threshold recalculate (#3) over the stored dense trace + `frequencyset_alter` | |
