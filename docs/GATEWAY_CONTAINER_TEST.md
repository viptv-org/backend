# Isolated backend/gateway real-media acceptance

This is a local fixture, not deployment approval. It creates no public listener,
loads no developer `.env`, mounts no host directory or Docker socket, and cannot
contact a real provider: runtime networking is `none`. The test independently
asserts that only loopback exists, then adds its synthetic public-classified
address inside that namespace. Root plus `NET_ADMIN` are fixture-only requirements,
not production gateway privileges.

Build the gateway locally at the reviewed source revision, then from this repo:

```sh
sudo bash scripts/check-gateway-container.sh LOCAL_GATEWAY_IMAGE
```

The image must already exist locally. The script resolves and records its content
ID and builds from a digest-derived local alias. Backend source is a Git archive
of the committed HEAD, not the working directory: ignored credentials, databases,
cache and uncommitted runtime changes are excluded. The test executable is built
inside Rust 1.98/trixie to match the gateway image's runtime instead of assuming
a host-built binary is portable. The helper supports the local legacy Docker
builder; build/network dependency downloads are separate from runtime isolation.

The container has a read-only root, bounded `/tmp`, process/memory/CPU limits,
no published ports and only the fixture capability. A 120-second outer bound
prevents indefinite acceptance waits. Success requires the named real-media test
to execute exactly once; a successful exit with zero selected tests fails.
Cleanup removes only the new container ID returned by this run and its anonymous
test volumes. Images/aliases remain locally for reproduction; no registry push
or production migration occurs.

