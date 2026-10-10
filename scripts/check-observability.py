#!/usr/bin/env python3
"""Check actual backend monitoring after deployment without displaying credentials.

Run on the Docker host with permission to inspect/exec the selected container.
This proves exporter activity, not collector acceptance; verify fresh viptv-api
metrics in New Relic separately. No production data or configuration is changed.
"""
import argparse
import json
import subprocess
import time


def docker(*args):
    result = subprocess.run(["docker", *args], capture_output=True, text=True, check=True)
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("container", nargs="?", default="viptv-viptv-1")
    args = parser.parse_args()
    container = json.loads(docker("inspect", args.container))[0]
    env = dict(item.split("=", 1) for item in container["Config"]["Env"] if "=" in item)
    if env.get("OBSERVABILITY_ENABLED") not in {"true", "1"}:
        raise SystemExit("FAIL: monitoring is not enabled in the running container")
    logs = subprocess.run(
        ["docker", "logs", "--since", container["State"]["StartedAt"], args.container],
        capture_output=True, text=True, check=True,
    )
    if "observability_enabled=true" not in logs.stdout + logs.stderr:
        raise SystemExit("FAIL: the running binary has not confirmed monitoring startup")
    path = env.get("OBSERVABILITY_BUDGET_PATH", "/data/observability-budget.json")
    before = json.loads(docker("exec", args.container, "cat", path))
    interval = int(env.get("OBSERVABILITY_EXPORT_INTERVAL_SECONDS", "30"))
    if not 10 <= interval <= 300:
        raise SystemExit("FAIL: invalid exporter interval")
    deadline = time.monotonic() + interval + 8
    while time.monotonic() < deadline:
        time.sleep(2)
        after = json.loads(docker("exec", args.container, "cat", path))
        if after["bytes"] > 0 and (after["day"] > before["day"] or after["bytes"] > before["bytes"]):
            current = json.loads(docker("inspect", args.container))[0]
            if current["Id"] != container["Id"] or current["State"]["StartedAt"] != container["State"]["StartedAt"]:
                raise SystemExit("FAIL: container changed during verification")
            print("PASS: running binary confirms monitoring; persistent export ledger advances")
            print("Next: confirm fresh viptv-api metrics and export failures in New Relic")
            return
    raise SystemExit("FAIL: exporter ledger did not advance; check budget, startup, and collector diagnostics")


if __name__ == "__main__":
    main()
