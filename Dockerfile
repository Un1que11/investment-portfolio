# syntax=docker/dockerfile:1
#
# Image with both binaries, portfolio-web and portfolio-sync (BACKLOG P35). It holds no secrets
# and no data: keys come from .env at run time, the database lives on a volume at /data, and
# config.toml is mounted at /config/config.toml. See README, "Docker".
#
#   docker build -t portfolio .                                  # native arch (arm64 on a Mac)
#   docker buildx build --platform linux/arm64,linux/amd64 .     # both arches
#
# Dependencies are built in their own layer with cargo-chef, so a change to the workspace's own
# code rebuilds only the workspace crates.

# The builder and the runtime share Debian bookworm, so the binaries link against the same glibc.
ARG RUST_VERSION=1.97
ARG DEBIAN_RELEASE=bookworm

FROM rust:${RUST_VERSION}-slim-${DEBIAN_RELEASE} AS chef
# Pinned for reproducible builds; bump together with RUST_VERSION when needed.
RUN cargo install cargo-chef --locked --version 0.1.78
WORKDIR /build

# The recipe lists only manifests and the lockfile, so it changes only when dependencies do.
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json \
    -p portfolio-web -p portfolio-sync
COPY . .
RUN cargo build --release --locked -p portfolio-web -p portfolio-sync \
    && install -D -m 0755 target/release/portfolio-web /out/portfolio-web \
    && install -D -m 0755 target/release/portfolio-sync /out/portfolio-sync

FROM debian:${DEBIAN_RELEASE}-slim AS runtime
# ca-certificates: HTTPS to NBP, Kraken, Alpha Vantage and Telegram.
# tzdata: TZ=Europe/Warsaw, so "today" and the snapshot date match the Mac's.
# tini: PID 1 that forwards SIGTERM, so `docker stop` ends the server at once.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata tini \
    && rm -rf /var/lib/apt/lists/*

# A fixed, unprivileged uid/gid, so a named volume's owner stays the same across rebuilds.
RUN groupadd --system --gid 10001 portfolio \
    && useradd --system --uid 10001 --gid portfolio --home-dir /data --no-create-home \
       --shell /usr/sbin/nologin portfolio \
    && install -d -o portfolio -g portfolio -m 0750 /data \
    && install -d -m 0755 /config

COPY --from=builder /out/portfolio-web /out/portfolio-sync /usr/local/bin/

# Inside a container the server has to listen on all interfaces to be reachable through a
# published port; publish it on the host's loopback only (-p 127.0.0.1:3000:3000).
ENV TZ=Europe/Warsaw \
    PORTFOLIO_ADDR=0.0.0.0:3000 \
    PORTFOLIO_DB_URL=sqlite:///data/portfolio.db \
    PORTFOLIO_CONFIG=/config/config.toml

USER portfolio:portfolio
WORKDIR /data
VOLUME ["/data"]
EXPOSE 3000

ENTRYPOINT ["/usr/bin/tini", "--"]
# `docker run … portfolio portfolio-sync --date YYYY-MM-DD` runs the sync instead.
CMD ["portfolio-web"]
