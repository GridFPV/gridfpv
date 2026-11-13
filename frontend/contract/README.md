# Client↔server contract suite

The **strict wire contract** between the `@gridfpv/protocol-client` and the real Director.
It mocks **nothing**: every test boots the real built `gridfpv` Director (via
`../test-harness/director.ts`) and drives it with raw `fetch` **and** the real
protocol-client `dist`, asserting the actual wire behaviour at every seam.

A unit test that mocks the boundary stays green while the real wire disagrees. Here the
boundary is real, so a contract regression fails a test.

## Run it

```sh
cd frontend
npm run build       # so the protocol-client dist is current
npm run contract    # vitest run --config contract/vitest.config.ts
```

The Director binary is built on demand by the harness (`cargo build -p gridfpv-app`) if
missing, so a fresh checkout just works.

## Layout

| file                 | asserts / guards                                                                                                                                                                                                                                                                                                                                                                                   |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `events.contract.ts` | the events lifecycle (`GET/POST /events`, `DELETE /events/{id}`), the active event, the pilot and class directories (create, edit with the three-state `OptionalEdit`, delete, hide), the read-only join token, `/about`, and the real client's calls and its `RequestFailure`. Writes are RD-gated (401 without the token); reads are open; an unknown id or an unmatched API path is a typed 404 |
| `time.contract.ts`   | `GET /time` serves exactly `{ now_micros }` — microseconds, a plain JSON number                                                                                                                                                                                                                                                                                                                    |
| `wire-shape.ts`      | asserts a wire body against the **generated** `bindings/<Type>.ts` (fields, optionality, types, no undeclared keys), so a contract expectation is never a second hand-written copy of the shape                                                                                                                                                                                                    |
| `harness.ts`         | shared helpers (the event route root, a deterministic no-SPA assets dir)                                                                                                                                                                                                                                                                                                                           |

## CI-ability

The suite needs **only** the built Director binary + Node 22 — **no Docker, no browser**.
