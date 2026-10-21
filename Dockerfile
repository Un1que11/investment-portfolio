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

# supercronic runs the daily sync inside the `sync` container (compose.yaml, BACKLOG P36): no cron
# daemon, no root, logs to stdout. A pinned release, checked against the sha256 GitHub publishes
# for each asset; TARGETARCH is set by BuildKit (amd64 or arm64).
FROM debian:${DEBIAN_RELEASE}-slim AS supercronic
ARG TARGETARCH
ARG SUPERCRONIC_VERSION=v0.2.49
ARG SUPERCRONIC_SHA256_AMD64=a53ae236602c7338aba3fbaff40bda6300eae3b9fedb8261eb06cfe3724430c1
ARG SUPERCRONIC_SHA256_ARM64=02aa0cb229ba09050cba6638059dadb9eedc2276632ea43d6a57a2f8c1629dd5
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
RUN set -eu; \
    case "${TARGETARCH}" in \
      amd64) sha="${SUPERCRONIC_SHA256_AMD64}" ;; \
      arm64) sha="${SUPERCRONIC_SHA256_ARM64}" ;; \
      *) echo "no supercronic checksum for TARGETARCH=${TARGETARCH}" >&2; exit 1 ;; \
    esac; \
    curl -fsSL -o /supercronic \
      "https://github.com/aptible/supercronic/releases/download/${SUPERCRONIC_VERSION}/supercronic-linux-${TARGETARCH}"; \
    echo "${sha}  /supercronic" | sha256sum -c -; \
    chmod 0755 /supercronic

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
COPY --from=supercronic /supercronic /usr/local/bin/supercronic
# The daily job for compose's `sync` service: 22:30 in TZ (Europe/Warsaw), like the launchd agent.
# supercronic reads the schedule in the container's local time zone, which TZ sets.
RUN install -d -m 0755 /etc/portfolio \
    && printf '%s\n' '# portfolio-sync daily run, local time (TZ).' '30 22 * * * portfolio-sync' \
       > /etc/portfolio/crontab

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
# `docker run … portfolio portfolio-sync --date YYYY-MM-DD` runs the sync instead, and
# `… portfolio supercronic /etc/portfolio/crontab` runs it daily (compose.yaml does that).
CMD ["portfolio-web"]
