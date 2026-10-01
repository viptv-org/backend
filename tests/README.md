# Backend checks

Use synthetic fixtures and disposable databases. No test described here
authorizes production mutation, real provider subscriptions or device installs.
The obsolete embedded-media Compose/smoke/GPU/SSE/soak hooks were removed;
historical source remains in Git and docs/RETIRED_TEST_AUDIT.md records coverage.

## Runtime and preservation

```sh
cargo fmt --manifest-path server/Cargo.toml -- --check
cargo clippy --locked --manifest-path server/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path server/Cargo.toml
python3 scripts/verify-migration.py
bash scripts/sync-provider.sh verify
```

The normal Rust suite covers auth/CSRF/profile/history/queue, Stremio catalogs,
account-owned catalogs/source admission, safe failures and guarded private
backup/export retirement. Opt-in real gateway tests remain separately run;
do not invoke every ignored fixture against a normal host.

## Public configuration and packaging

```sh
node tests/validate_deployment.cjs
python3 tests/test_host_check.py
bash -n scripts/host-check.sh scripts/container-check.sh scripts/check-gateway-container.sh
docker build --build-arg FRONTENDS=0 -t LOCAL_CANDIDATE .
```

The Node check uses Compose's loader with /dev/null instead of a real .env and
synthetic values; it validates optional keyring handling, exact origin, nonroot/
read-only security, named volume and engine-free runtime settings. It does not
talk to the daemon. Host-check unit tests use a fake Docker executable and
owner-only fixture env files. A production frontend build additionally requires
the reviewed dashboard/tv submodules initialized.

## Real gateway media

```sh
bash scripts/container-check.sh LOCAL_GATEWAY_IMAGE
```

This compatibility entry delegates to check-gateway-container.sh. It archives
tracked HEAD only and runs the named real-media fixture in a disposable
network-none container. No deployment env/data/ports are mounted. See
../docs/GATEWAY_CONTAINER_TEST.md for exact prerequisites and evidence limits.
The gateway test image may contain media tools; the backend runtime image does not.

account_smoke.py is a separate disposable HTTP account test and creates fixture
users/profiles. It is not a production diagnostic or a backend media test.
The generic benchmark-source.py HTTPS byte-range fixture is retained after
gateway-owner coordination for possible later diagnostics; it has no packaging hook.
