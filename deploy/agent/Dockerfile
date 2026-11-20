# syntax=docker/dockerfile:1
# Image of the host agent: the small Rust binary, the Docker command line it drives the
# stack with, and the tools the backup and certificate scripts need.
# Build context: the repository root (see .dockerignore).

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# The server crate compiles the e-mail texts in; the workspace does not load without them.
COPY web/src/locales ./web/src/locales
# Same caches as the platform's image: what one build compiled, the other reuses.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p teamtak-agent \
    && cp target/release/teamtak-agent /usr/local/bin/teamtak-agent

# Debian 13: its apt reads the package state of the Debian and Ubuntu hosts the agent
# reports on, and it ships the Docker command line as a package of its own.
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates docker-cli openssl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /usr/local/bin/teamtak-agent /usr/local/bin/teamtak-agent
ENV AGENT_LISTEN=0.0.0.0:9300
EXPOSE 9300
LABEL org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.source="https://github.com/Titou74/TeamTAK"
ENTRYPOINT ["/usr/local/bin/teamtak-agent"]
CMD ["serve"]
