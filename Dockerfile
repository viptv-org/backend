# syntax=docker/dockerfile:1
# Pin base-image digests in your deployment for reproducible supply-chain inputs.

FROM rust:1-bookworm AS server-build
WORKDIR /src/server
COPY server/ ./
RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 viptv \
    && useradd --uid 10001 --gid viptv --no-create-home --home-dir /data --shell /usr/sbin/nologin viptv \
    && install -d -o viptv -g viptv -m 0700 /data
COPY --from=server-build /src/server/target/release/viptv-server /usr/local/bin/viptv-server
COPY --from=server-build /src/server/target/release/provider-owners /usr/local/bin/provider-owners
# The KMP viewing app is the only UI bundle; it is built separately (Gradle/wasm)
# and supplied via VIPTV_TV_DIST. There is no legacy dashboard or TV-web dist.
WORKDIR /app
ENV VIPTV_BIND=0.0.0.0:8080 \
    VIPTV_DATABASE=/data/viptv.sqlite \
    VIPTV_TV_DIST=/app/tv
USER 10001:10001
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl --fail --silent --output /dev/null http://127.0.0.1:8080/api/health \
    && curl --fail --silent --output /dev/null http://127.0.0.1:8080/ || exit 1
ENTRYPOINT ["/usr/local/bin/viptv-server"]
