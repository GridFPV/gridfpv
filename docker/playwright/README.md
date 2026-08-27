# Headless e2e in a container (#426)

The Playwright suite is **already headless** — the blocker was never the framework, it was
that a build box needs ~300 system libraries (`libatk-1.0.so.0` and friends) before Chromium
will launch. Microsoft ships an image that has them, so we borrow it instead of provisioning
the host.

```sh
cargo xtask e2e            # the whole suite, in the container
cargo xtask e2e wizard     # one spec (substring match)
```

## Why no custom image

**The host-built Director binary runs unmodified inside the image**, so the container needs no
Rust toolchain and no second `target/` dir. Verified: the image is glibc 2.39, this box builds
against 2.43, and `target/debug/gridfpv` starts and serves in the container regardless — a Rust
binary needs the *symbols it uses* to exist, not an identical libc.

That is what keeps this a three-line docker run rather than a maintained Dockerfile.

## What the runner does

1. Builds the Director on the **host** (`cargo build -p gridfpv-app`). The harness would build
   it on demand, but there is no cargo in the container — so it must already exist.
2. Runs `npx playwright test` in the image with the repo mounted at the same path.

`PLAYWRIGHT_BROWSERS_PATH=/ms-playwright` is baked into the image, so the browsers come from
the image while `@playwright/test` comes from the mounted `node_modules` — the versions must
therefore match, which is why the image tag is pinned to the installed Playwright version and
`xtask` checks it rather than letting a mismatch fail obscurely mid-run.

## The Director inside the container

Each worker boots its own Director on an **ephemeral port** (`e2e/observability.ts`), so a
container run cannot collide with a Director you have running on 8080 for bench testing.

## CI

`.github/workflows/ci.yml` runs the same suite on `ubuntu-latest` with
`npx playwright install --with-deps chromium` — no container needed there, since the runner can
install system packages. Same specs, same config, both places.
