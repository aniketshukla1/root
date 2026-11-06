# root-server, as a container.
#
# Two build stages and a small Debian runtime, for the ordinary reasons: the
# build needs a Rust toolchain and a Node one, and the thing that runs in
# production should contain neither. What ships is one binary, the built
# frontend, curl for the health check and a CA bundle — because root talks to
# HTTPS endpoints and a container with no roots cannot verify any of them, which
# is the failure that looks like "every connection is down".

# ---------------------------------------------------------------- frontend
FROM node:22-alpine AS web

WORKDIR /app
# Dependencies first, so an edit to a component does not reinstall the world.
COPY package.json package-lock.json ./
RUN npm ci

COPY tsconfig.json tsconfig.node.json vite.config.ts index.html ./
COPY public ./public
COPY src ./src
RUN npm run build

# ---------------------------------------------------------------- backend
FROM rust:1-slim-bookworm AS build

# cmake, g++, make and perl build the bundled librdkafka and its OpenSSL. curl's
# header is here because librdkafka includes it whether it uses curl or not.
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates cmake make g++ perl libcurl4-openssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY src-tauri/Cargo.toml src-tauri/Cargo.lock ./

# Build the dependency graph against a stub so the expensive layer is cached
# on everything except this crate's own source.
RUN mkdir -p src/bin \
    && echo 'fn main() {}' > src/main.rs \
    && echo 'fn main() {}' > src/bin/root-server.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --no-default-features --bin root-server 2>/dev/null || true

COPY src-tauri/src ./src
COPY src-tauri/build.rs ./build.rs
# Without the desktop shell, which would pull in a webview toolkit this image
# has no use for.
RUN touch src/lib.rs && cargo build --release --no-default-features --bin root-server

# ---------------------------------------------------------------- runtime
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /app root-server \
    && install -d -o 10001 /data

WORKDIR /app
COPY --from=build /src/target/release/root-server /usr/local/bin/root-server
COPY --from=web /app/dist /app/web

ENV ROOT_ADDR=0.0.0.0:7777 \
    ROOT_WEB_ROOT=/app/web \
    ROOT_CONFIG=/data/config.json

# Not root, despite the name. A process that polls the internet and holds
# credentials has no business owning the filesystem it runs on.
USER 10001
EXPOSE 7777

# The endpoint that answers without a credential, which is exactly what a
# health check should need.
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:7777/healthz || exit 1

ENTRYPOINT ["root-server"]
