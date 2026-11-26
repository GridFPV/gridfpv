# syntax=docker/dockerfile:1
#
# The Grid remote server (docs/primitives.md §13), as one image.
#
# Build it from the root of the repo:
#
#   docker build -f docker/server/Dockerfile -t gridfpv-server .
#
# The image holds the server program and the web page it shows. Its data lives in /data,
# which must be a volume or it is lost with the container. It speaks plain HTTP on port
# 8470 and is meant to sit behind something that gives it HTTPS (see docs/server-setup.md).

# ── The web page: the built console ──────────────────────────────────────────
FROM node:22-slim AS page
WORKDIR /src
# The console imports the generated wire types and the help pages from outside `frontend/`.
COPY bindings ./bindings
COPY docs/help ./docs/help
COPY frontend ./frontend
RUN cd frontend && npm ci && npm run build

# ── The server program ───────────────────────────────────────────────────────
FROM rust:1-bookworm AS program
WORKDIR /src
COPY . .
# What the server says its version is. Left empty, it is the version in Cargo.toml (or,
# when the build context has a .git folder, the commit).
ARG GRIDFPV_RELEASE_VERSION=
RUN if [ -n "$GRIDFPV_RELEASE_VERSION" ]; then export GRIDFPV_RELEASE_VERSION; else unset GRIDFPV_RELEASE_VERSION; fi \
    && cargo build --release --locked -p gridfpv-serverd --bin gridfpv-server

# ── What runs ────────────────────────────────────────────────────────────────
FROM debian:bookworm-slim
RUN useradd --system --create-home --home-dir /home/gridfpv --uid 10001 gridfpv \
    && mkdir /data && chown gridfpv:gridfpv /data
COPY --from=program /src/target/release/gridfpv-server /usr/local/bin/gridfpv-server
COPY --from=page /src/frontend/apps/rd-console/dist /srv/gridfpv/page
ENV GRIDFPV_SERVER_DATA_DIR=/data \
    GRIDFPV_SERVER_ADDR=0.0.0.0:8470 \
    GRIDFPV_SERVER_ASSETS=/srv/gridfpv/page
USER gridfpv
VOLUME /data
EXPOSE 8470
# `gridfpv-server` alone runs the server. `docker exec` it with `token new <label>` to make
# a link token for a Director.
ENTRYPOINT ["gridfpv-server"]
