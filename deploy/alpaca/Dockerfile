FROM rust:1.96.0-slim-bookworm@sha256:b5f842fac1e3b4ff718a652a8e0173b62d9403ec826ef4998880b9347db30684 AS builder

WORKDIR /workspace

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

RUN cargo build --locked --release -p nautilus-alpaca --features live \
    --bin alpaca-options-engine \
    --bin alpaca-operator-status \
    --bin alpaca-fleet-status \
    --bin alpaca-check-account-orders \
    --bin alpaca-compare-option-chain-scan \
    --bin alpaca-candidate-alerts \
    --bin alpaca-performance-report

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

COPY --from=builder /workspace/target/release/alpaca-options-engine /usr/local/bin/
COPY --from=builder /workspace/target/release/alpaca-operator-status /usr/local/bin/
COPY --from=builder /workspace/target/release/alpaca-fleet-status /usr/local/bin/
COPY --from=builder /workspace/target/release/alpaca-check-account-orders /usr/local/bin/
COPY --from=builder /workspace/target/release/alpaca-compare-option-chain-scan /usr/local/bin/
COPY --from=builder /workspace/target/release/alpaca-candidate-alerts /usr/local/bin/
COPY --from=builder /workspace/target/release/alpaca-performance-report /usr/local/bin/
COPY deploy/alpaca/alpaca-options-runner.sh /usr/local/bin/alpaca-options-runner
COPY deploy/alpaca/alpaca-control.sh /usr/local/bin/alpaca-control

USER nautilus
WORKDIR /workspace

ENV HOME=/home/nautilus \
    XDG_CONFIG_HOME=/config \
    XDG_STATE_HOME=/state \
    NAUTILUS_ALPACA_REPO=/workspace \
    NAUTILUS_ALPACA_RUNNER_BIN=/usr/local/bin/alpaca-options-engine \
    NAUTILUS_ALPACA_OPERATOR_BIN=/usr/local/bin/alpaca-operator-status \
    NAUTILUS_ALPACA_FLEET_BIN=/usr/local/bin/alpaca-fleet-status \
    NAUTILUS_ALPACA_COMPARE_SCAN_BIN=/usr/local/bin/alpaca-compare-option-chain-scan \
    NAUTILUS_ALPACA_CANDIDATE_ALERTS_BIN=/usr/local/bin/alpaca-candidate-alerts \
    NAUTILUS_ALPACA_PERFORMANCE_BIN=/usr/local/bin/alpaca-performance-report

CMD ["alpaca-options-engine", "--help"]
