# syntax=docker/dockerfile:1
# Image of the platform: the Rust API with the web UI as static files.
# Build context: the repository root (see .dockerignore).

FROM node:22-alpine AS web
WORKDIR /src/web
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store pnpm install --frozen-lockfile
COPY web/ ./
RUN pnpm build

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# E-mail texts live next to the UI's translations and are compiled into the binary.
COPY web/src/locales ./web/src/locales
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p teamtak-server \
    && cp target/release/teamtak /usr/local/bin/teamtak

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --uid 10001 --gid 0 --no-create-home --shell /usr/sbin/nologin teamtak
COPY --from=build /usr/local/bin/teamtak /usr/local/bin/teamtak
COPY --from=web /src/web/dist /app/web
USER 10001:0
# 8080: the web UI and its API, behind Caddy. 8081: the questions of the other services of
# the stack (the video server's authentication), never behind Caddy.
ENV LISTEN_ADDR=0.0.0.0:8080 \
    INTERNAL_LISTEN_ADDR=0.0.0.0:8081 \
    WEB_DIR=/app/web
EXPOSE 8080 8081
LABEL org.opencontainers.image.licenses="AGPL-3.0-only" \
      org.opencontainers.image.source="https://github.com/Titou74/TeamTAK"
ENTRYPOINT ["/usr/local/bin/teamtak"]
CMD ["serve"]
