ARG RUNTIME_IMAGE
FROM rust:1-bookworm AS fixture-vault
WORKDIR /qualification
COPY scripts/populated-vault-helper/ scripts/populated-vault-helper/
COPY server/src/secret_store.rs server/src/secret_store.rs
RUN cargo build --release --locked --manifest-path scripts/populated-vault-helper/Cargo.toml

FROM ${RUNTIME_IMAGE}
USER 0:0
RUN apt-get update && apt-get install -y --no-install-recommends python3 \
    && rm -rf /var/lib/apt/lists/*
COPY scripts/populated-seed.py /qualification/populated-seed.py
COPY --from=fixture-vault /qualification/scripts/populated-vault-helper/target/release/populated-vault-helper /qualification/vault-helper
USER 10001:10001
ENTRYPOINT ["python3", "/qualification/populated-seed.py"]
