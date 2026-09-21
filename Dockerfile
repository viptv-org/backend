# syntax=docker/dockerfile:1
# Pin base-image digests in your deployment for reproducible supply-chain inputs.
ARG FRONTENDS=1

FROM rust:1-bookworm AS server-build
WORKDIR /src/server
COPY server/ ./
RUN cargo build --release --locked

FROM node:22-bookworm-slim AS dashboard-build
ARG FRONTENDS
WORKDIR /src/dashboard
COPY dashboard/ ./
# Credential-free CI builds pass FRONTENDS=0: the stage stays empty and the
# server-only image answers / with its identity instead of a dashboard.
RUN if [ "$FRONTENDS" = "0" ]; then mkdir -p dist; exit 0; fi \
    && npm ci --no-audit --no-fund \
    && npm test \
    && npm run build

FROM node:22-bookworm-slim AS tv-build
ARG FRONTENDS
WORKDIR /src/tv
# tv-web validates its design/core/video pins offline before bundling.
COPY tv/ ./
RUN if [ "$FRONTENDS" = "0" ]; then mkdir -p dist; exit 0; fi \
    && npm ci --no-audit --no-fund \
    && npm run build

FROM debian:bookworm-slim AS runtime
ARG FRONTENDS
RUN sed -i 's/Components: main$/Components: main non-free/' /etc/apt/sources.list.d/debian.sources \
    && apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg curl \
    && if [ "$(dpkg --print-architecture)" = amd64 ]; then apt-get install -y --no-install-recommends intel-media-va-driver-non-free; fi \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 viptv \
    && useradd --uid 10001 --gid viptv --no-create-home --home-dir /data --shell /usr/sbin/nologin viptv \
    && install -d -o viptv -g viptv -m 0700 /data /cache \
    && install -d -m 0755 /app/dashboard /app/tv
COPY --from=server-build /src/server/target/release/viptv-server /usr/local/bin/viptv-server
COPY --from=dashboard-build /src/dashboard/dist/ /app/dashboard/
COPY --from=tv-build /src/tv/dist/ /app/tv/
# Build artifacts can inherit a restrictive host/build umask. These are public files.
RUN chmod -R a+rX /app/dashboard /app/tv
WORKDIR /app
ENV VIPTV_BIND=0.0.0.0:8080 \
    VIPTV_DATABASE=/data/viptv.sqlite \
    VIPTV_MEDIA_DIR=/cache/hls \
    VIPTV_DASHBOARD_DIST=/app/dashboard \
    VIPTV_TV_DIST=/app/tv
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl --fail --silent --output /dev/null http://127.0.0.1:8080/api/health \
    && curl --fail --silent --output /dev/null http://127.0.0.1:8080/ || exit 1
ENTRYPOINT ["/usr/local/bin/viptv-server"]
