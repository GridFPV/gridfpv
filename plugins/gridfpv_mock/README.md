# GridFPV mock-control plugin — TEST ONLY

A development/test companion to the real [`gridfpv`](../gridfpv/) plugin (RH plugin design
**D16**). It drives RotorHazard's **mock nodes from the network** over a `gridfpv_mock_*`
socket namespace, so a test or maintainer can shape the emulated signal live — without
rebuilding the container or remounting CSVs. It reuses the same `socket_listen` channel
plumbing the real plugin uses (S1).

> ⚠️ **Never ship this to users.** It reaches into the live hardware interface
> (`rhapi._racecontext.interface`) — internals RHAPI deliberately doesn't expose — which is
> fine for a harness-only plugin but wrong in production. It is **not** included in the
> downloadable plugin bundle the Director serves; only [`gridfpv`](../gridfpv/) is.

## Handlers

Each replies `gridfpv_mock_ack` (`{action, ok, ...}`) to the asking client.

| Event | Payload | Effect |
|-------|---------|--------|
| `gridfpv_mock_tune` | `{node, frequency}` | Set a mock node's frequency (activates it / starts CSV reads) |
| `gridfpv_mock_set_rssi` | `{node, rssi}` | Force a node's `current_rssi` (for inspection) |
| `gridfpv_mock_lap` | `{node}` | Inject a real lap via RH's `intf_simulate_lap` (genuine pipeline) — RH must be RACING |
| `gridfpv_mock_state` | — | Reply with per-node `{index, frequency, current_rssi}` |

## Running it

```sh
# Boot RH with the mock-control plugin mounted (optionally with the real plugin too):
cargo xtask rh-mock feed clean --mock-plugin            # mock-control only
cargo xtask rh-mock feed clean --plugin --mock-plugin   # both plugins
```

Then drive it from any socket.io client connected to RH (the Director's connection, a test,
or a quick Python script): emit `gridfpv_mock_*` and read `gridfpv_mock_ack`.
