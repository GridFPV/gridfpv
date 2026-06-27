# GridFPV RotorHazard plugin

The required in-process RotorHazard integration (decision **D16**). Design +
sliced build plan: [`docs/rotorhazard-plugin.html`](../../docs/rotorhazard-plugin.html).

A RotorHazard plugin is a directory dropped into RH's user-`plugins/` dir, with an
`__init__.py` exposing `initialize(rhapi)` and a `manifest.json` declaring
`required_rhapi_version`. RH's loader imports it and hands over the `rhapi` object.

- **Floor:** RHAPI **1.3** / RotorHazard **v4.3.0+** (declared in `manifest.json`).
- **Channel:** `gridfpv_*` events on RH's existing socket.io server (S1+).

## Status — S0 (placeholder)

`__init__.py` is an empty, load-only skeleton: `initialize(rhapi)` logs and returns.
S0's job is the **dev harness**, not plugin logic — see
[`docker/rotorhazard/README.md`](../../docker/rotorhazard/README.md). The harness
mounts this folder into the RH v4.4.0 container's `plugins/gridfpv/` and boots it;
RH must log the plugin as loaded with no `load_issue`. Confirm with:

```sh
cargo xtask rh-mock plugin-check
```

## Roadmap

| Slice | Adds |
|-------|------|
| S1 | `gridfpv_hello` handshake via `socket_listen`; capabilities/version |
| S2 | live dense RSSI broadcast (`gridfpv_signal`) — retires save-then-pull |
| S3 | clean start/stop (`race.stage()`/`stop()`) + per-node passes |
| S4 | threshold recalculate (#3) over the stored dense trace + `frequencyset_alter` |
