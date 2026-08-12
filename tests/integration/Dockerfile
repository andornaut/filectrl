# Integration test image: builds the real filectrl binary, then runs the
# tmux-driven TUI test suites against it.
#
#   docker build -f tests/integration/Dockerfile -t filectrl-integration .
#   docker run --rm -t filectrl-integration
#
# Or use tests/integration/run.sh, which does both.

FROM rust:1.94-slim AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release --locked

FROM debian:trixie-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends tmux && \
    rm -rf /var/lib/apt/lists/*
WORKDIR /repo
COPY fixtures fixtures
COPY tests/integration tests/integration
COPY --from=builder /src/target/release/filectrl /usr/local/bin/filectrl
ENV FILECTRL_BIN=/usr/local/bin/filectrl \
    LANG=C.UTF-8
CMD ["tests/integration/run_tests.sh"]
