# syntax=docker/dockerfile:1.25
ARG RUST_IMAGE
FROM koalaman/shellcheck:v0.11.0 AS shellcheck
FROM node:24.22.2 AS markdown
RUN npm install --global --ignore-scripts --no-audit --no-fund markdownlint-cli2@0.24.0
RUN mkdir /opt/cargo-deny \
    && curl -fsSL https://github.com/EmbarkStudios/cargo-deny/releases/download/0.20.2/cargo-deny-0.20.2-x86_64-unknown-linux-musl.tar.gz \
    | tar -xz --strip-components=1 -C /opt/cargo-deny
FROM ${RUST_IMAGE}
COPY --from=shellcheck /bin/shellcheck /usr/local/bin/shellcheck
RUN useradd --uid 19001 --user-group --no-create-home --home-dir /scratch test
RUN apt-get update && apt-get install --no-install-recommends --yes git ca-certificates
COPY --from=markdown /opt/cargo-deny/cargo-deny /usr/local/bin/cargo-deny
RUN rustup component add clippy rustfmt \
    && rustup target add aarch64-apple-darwin
COPY --from=markdown /usr/local/bin/node /usr/local/bin/node
COPY --from=markdown /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/markdownlint-cli2/markdownlint-cli2-bin.mjs /usr/local/bin/markdownlint-cli2
WORKDIR /opt/module
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && printf 'fn main() {}\n' > src/main.rs \
    && cargo fetch --locked \
    && chmod -R a+rX /usr/local/cargo/registry
COPY tests/container/check /opt/check/run
RUN chmod 755 /opt/check/run
