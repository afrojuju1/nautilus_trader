# syntax=docker/dockerfile:1.7

FROM rust:1.96.0-slim-bookworm@sha256:b5f842fac1e3b4ff718a652a8e0173b62d9403ec826ef4998880b9347db30684 AS builder

WORKDIR /workspace

ARG CARGO_BUILD_JOBS=1
ARG CARGO_PROFILE=release-rollout
ARG ALPACA_BUILD_TOOLS=false
ENV CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS}

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        capnproto \
        clang \
        git \
        libcapnp-dev \
        lld \
        pkg-config \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates ./crates
COPY examples/tutorials ./examples/tutorials
COPY patches ./patches

RUN --mount=type=cache,id=nautilus-alpaca-cargo-registry,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,id=nautilus-alpaca-cargo-git,target=/usr/local/cargo/git,sharing=locked \
    --mount=type=cache,id=nautilus-alpaca-target,target=/workspace/target,sharing=locked \
    set -eux; \
    core_bins="alpaca-options-engine alpaca-operator-status alpaca-check-account-orders alpaca-option-chain-scan-live-node"; \
    tool_bins="alpaca-fleet-status alpaca-compare-option-chain-scan alpaca-candidate-alerts alpaca-performance-report"; \
    bins="$core_bins"; \
    if [ "$ALPACA_BUILD_TOOLS" = "true" ]; then bins="$bins $tool_bins"; fi; \
    cargo_args=""; \
    for bin in $bins; do cargo_args="$cargo_args --bin $bin"; done; \
    cargo build --locked --profile "$CARGO_PROFILE" -p nautilus-alpaca --features live,warehouse-clickhouse $cargo_args; \
    case "$CARGO_PROFILE" in release) profile_dir=release ;; dev) profile_dir=debug ;; *) profile_dir="$CARGO_PROFILE" ;; esac; \
    mkdir -p /artifacts; \
    for bin in $bins; do cp "/workspace/target/$profile_dir/$bin" "/artifacts/$bin"; done

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        bash \
        ca-certificates \
        jq \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 --shell /usr/sbin/nologin nautilus \
    && mkdir -p /config /state /workspace \
    && chown -R nautilus:nautilus /config /state /workspace

COPY --from=builder /artifacts/ /usr/local/bin/
COPY deploy/alpaca/alpaca-options-runner.sh /usr/local/bin/alpaca-options-runner
COPY deploy/alpaca/alpaca-control.sh /usr/local/bin/alpaca-control

USER nautilus
WORKDIR /workspace

ENV HOME=/home/nautilus \
    XDG_CONFIG_HOME=/config \
    XDG_STATE_HOME=/state \
    NAUTILUS_ALPACA_REPO=/workspace \
    NAUTILUS_ALPACA_RUNNER_BIN=/usr/local/bin/alpaca-option-chain-scan-live-node \
    NAUTILUS_ALPACA_OPERATOR_BIN=/usr/local/bin/alpaca-operator-status \
    NAUTILUS_ALPACA_FLEET_BIN=/usr/local/bin/alpaca-fleet-status \
    NAUTILUS_ALPACA_OPTION_CHAIN_LIVE_BIN=/usr/local/bin/alpaca-option-chain-scan-live-node \
    NAUTILUS_ALPACA_COMPARE_SCAN_BIN=/usr/local/bin/alpaca-compare-option-chain-scan \
    NAUTILUS_ALPACA_CANDIDATE_ALERTS_BIN=/usr/local/bin/alpaca-candidate-alerts \
    NAUTILUS_ALPACA_PERFORMANCE_BIN=/usr/local/bin/alpaca-performance-report

CMD ["alpaca-option-chain-scan-live-node", "--help"]
