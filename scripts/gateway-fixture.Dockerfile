# SPDX-License-Identifier: GPL-2.0-only
# Test-only: never use this image as a backend or gateway deployment.
ARG GATEWAY_IMAGE
FROM rust:1.98-trixie AS fixture-build
WORKDIR /src/server
COPY server/ ./
ENV CARGO_BUILD_JOBS=2
RUN cargo test --locked --lib --no-run \
    && mkdir /out \
    && executable="$(find target/debug/deps -maxdepth 1 -type f -executable -name 'viptv_server-*')" \
    && test -n "$executable" \
    && test "$(printf '%s\n' "$executable" | wc -l)" -eq 1 \
    && cp "$executable" /out/backend-fixture

FROM ${GATEWAY_IMAGE}
ARG BACKEND_REVISION
ARG GATEWAY_REVISION_IMAGE
LABEL tech.syek.viptv.fixture.backend="${BACKEND_REVISION}" \
      tech.syek.viptv.fixture.gateway-image="${GATEWAY_REVISION_IMAGE}"
COPY --from=fixture-build --chmod=0555 /out/backend-fixture /fixtures/backend-fixture
ENV VIPTV_TEST_ISOLATED_NETWORK=container \
    VIPTV_TEST_GATEWAY_BINARY=/usr/local/bin/playback-gateway \
    VIPTV_TEST_FFMPEG=/opt/ffmpeg/bin/ffmpeg \
    VIPTV_TEST_FFPROBE=/opt/ffmpeg/bin/ffprobe
ENTRYPOINT ["/fixtures/backend-fixture"]
CMD ["gateway::playback_tests::isolated_backend_gateway_real_media_lifecycle", "--ignored", "--exact", "--nocapture"]
