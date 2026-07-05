# Client↔server contract suite

The **strict wire contract** between the `@gridfpv/protocol-client` and the real Director.
It mocks **nothing**: every test boots the real built `gridfpv` Director (via
`../test-harness/director.ts`) and drives it with raw `fetch`/`WebSocket` **and** the real
protocol-client `dist`, asserting the actual wire behaviour at every seam.

This is the durable guard for the bug class that hit us in v0.4 — all five of those bugs
lived at a _mocked_ boundary, so a unit test that mocked the seam stayed green while the real
wire disagreed. Here the boundary is real, so a contract regression fails a test.

## Run it

```sh
cd frontend
npm run build       # so the protocol-client dist + Director binary are current
npm run contract    # vitest run --config contract/vitest.config.ts
```

The Director binary is built on demand by the harness (`cargo build -p gridfpv-app`) if
missing, so a fresh checkout just works.

## Layout (one file per seam group)

| file                   | seams   | asserts / guards                                                                                                                                                                                                                                                                                                                                                                                           |
| ---------------------- | ------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `snapshot.contract.ts` | 1, 4    | path-scoped snapshot routes + the right `ProjectionBody` variant + numeric `cursor`; a wrong route is the SPA fallback, never a silent `Snapshot` (guards the path-vs-`?scope=` bug); every integer field is a JSON `number` (guards the i64/bigint class)                                                                                                                                                 |
| `stream.contract.ts`   | 2, 3, 7 | `/stream` frames are externally-tagged `StreamMessage` `{ Change }` / `{ ReSnapshotRequired }` (guards the StreamMessage-unwrap bug); the per-stream `sequence` axis (starts at 1) is distinct from the snapshot `cursor` axis and the real client converges rather than dropping early frames as duplicates (guards the cursor/sequence freeze); an out-of-band `contract_version` gets `VersionMismatch` |
| `control.contract.ts`  | 5, 6    | each `Command` acks `{ ok: true }` and appends; missing `Content-Type` is rejected; an illegal transition is `{ ok: false, error: BadRequest }` (HTTP 200, not an HTTP error); the resulting change reaches a `/stream` subscriber; control needs an RD bearer token (401 without), reads are open                                                                                                         |
| `race.contract.ts`     | 8       | a full sim race driven through the real control path, asserting the real protocol-client's _exposed converged state_ (current heat, climbing per-pilot laps, the scored result) — the end-to-end client↔server contract in one test                                                                                                                                                                        |
| `harness.ts`           | —       | shared helpers (control POST, raw sockets, wait predicates, a deterministic no-SPA assets dir)                                                                                                                                                                                                                                                                                                             |

## CI-ability

The suite needs **only** the built Director binary + Node 22 — **no Docker, no browser**. It
is therefore CI-able as-is (build `gridfpv-app`, `npm ci && npm run build`, `npm run contract`)
and could later join a CI runner. It is **not** yet wired into `cargo xtask ci` (which is
Rust-only) — adding it is a separate, deliberate step.

## Recorded gap

The server has **no wire endpoint to mint a read-only join token** (`issue_join_token` exists
in `crates/server/src/auth.rs` but is unreachable over HTTP; only `GRIDFPV_RD_TOKEN` is
pinned). So the "a read-only join-token is rejected on control" case is asserted at the unit
level (`auth::tests::join_token_is_read_only_and_rejected_on_control`); over the wire we assert
the reachable equivalent — an unknown/revoked token is `401`. See the note in
`control.contract.ts`.
