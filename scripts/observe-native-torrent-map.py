#!/usr/bin/env python3
"""Read-only proof for the owned synthetic gateway's current native output.

Raw argv stays in the private qualification directory. Identical silent tracks
cannot identify audible language; this proves the engine's selected input map.
"""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import time
from urllib.parse import urlsplit


def docker(*args):
    return subprocess.run(["docker", *args], check=True, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, timeout=10).stdout


def output_job(delivery, expected):
    value = delivery.get("delivery", {})
    if delivery.get("status") != "ready":
        return None
    selected = value.get("selected_audio", {})
    if selected.get("input_index") != expected:
        return None
    path = urlsplit(value["url"]).path.split("/")
    try:
        job = path[path.index("media") + 1]
    except (ValueError, IndexError):
        return None
    return job if re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", job) else None


def matching_maps(raw, job):
    result = []
    for record in raw.split(b"\0\0\0"):
        argv = record.split(b"\0")
        if len(argv) < 3 or not argv[0].isdigit():
            continue
        argv = [os.fsdecode(arg) for arg in argv[1:] if arg]
        if not argv or Path(argv[0]).name != "ffmpeg":
            continue
        if not any(arg.endswith(f"/{job}/index.m3u8") for arg in argv):
            continue
        result.extend(argv[index + 1] for index, arg in enumerate(argv[:-1]) if arg == "-map")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("selector", type=int)
    parser.add_argument("--timeout", type=int, default=20)
    args = parser.parse_args()
    if not 0 <= args.selector <= 65535 or not 1 <= args.timeout <= 30:
        parser.error("selector or bounded timeout out of range")
    directory = args.directory.resolve(strict=True)
    if directory.stat().st_mode & 0o077:
        parser.error("qualification directory must be private")
    candidates = docker("ps", "--quiet").decode().splitlines()
    owned = []
    for container in candidates:
        mounts = json.loads(docker("inspect", "--format", "{{json .Mounts}}", container))
        if any(mount.get("Destination") == "/qualification"
               and Path(mount.get("Source", "")).resolve() == directory for mount in mounts):
            owned.append(container)
    if len(owned) != 1:
        raise SystemExit("Exactly one owned qualification container required")
    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        delivery_file = directory / "native-raw-last-delivery.json"
        try:
            job = output_job(json.loads(delivery_file.read_text()), args.selector)
        except (FileNotFoundError, json.JSONDecodeError):
            job = None
        if job:
            raw = docker("exec", owned[0], "/bin/sh", "-c",
                         'for p in /proc/[0-9]*/cmdline; do '
                         'test -r "$p" || continue; '
                         'n=${p#/proc/}; n=${n%/cmdline}; '
                         'printf "%s\\0" "$n"; cat "$p" 2>/dev/null; '
                         'printf "\\0\\0"; done')
            private = directory / "native-ffmpeg-argv.bin"
            descriptor = os.open(private, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(descriptor, "wb") as output:
                output.write(raw)
            maps = matching_maps(raw, job)
            count = maps.count(f"0:{args.selector}")
            if count == 1:
                result = {"selector": args.selector, "matching_output_processes": count,
                          "ready_delivery_output_matched": True}
                (directory / "native-audio-map.json").write_text(json.dumps(result))
                print(json.dumps(result))
                return
        time.sleep(0.1)
    raise SystemExit("Owned ready output did not expose the required numeric input map")


if __name__ == "__main__":
    main()
