#!/usr/bin/env python3
"""Read-only proof for the owned synthetic gateway's current native output.

Raw argv stays in the private qualification directory. Identical silent tracks
cannot identify audible language; this proves the engine's selected input map.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time
from urllib.parse import urlsplit, urljoin
from urllib.request import urlopen


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
    return job if re.fullmatch(r"viewer_[0-9a-f]{32}", job) else None


def matching_maps(raw, job=None):
    result = []
    for record in raw.split(b"\0\0\0"):
        argv = record.split(b"\0")
        if len(argv) < 3 or not argv[0].isdigit():
            continue
        argv = [os.fsdecode(arg) for arg in argv[1:] if arg]
        if not argv or Path(argv[0]).name != "ffmpeg":
            continue
        outputs = [arg for arg in argv if re.search(
            r"/state/media/[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}/index\.m3u8$", arg)]
        if job is not None:
            outputs = [arg for arg in outputs if arg.endswith(f"/{job}/index.m3u8")]
        if len(outputs) != 1:
            continue
        maps = [argv[index + 1] for index, arg in enumerate(argv[:-1]) if arg == "-map"]
        result.append((outputs[0], maps))
    return result


def served_segment(directory, delivery):
    config = json.loads((directory / "backend.json").read_text())
    endpoint = urlsplit(config["gateway_endpoint"])
    control = urlsplit(config["gateway_control"])
    media = urlsplit(delivery["delivery"]["url"])
    if control.hostname != "127.0.0.1" or control.scheme != "http":
        raise SystemExit("Owned loopback gateway control required")
    prefix = endpoint.path.rstrip("/") + "/"
    if not media.path.startswith(prefix + "media/"):
        raise SystemExit("Owned gateway media path required")
    local = urljoin(config["gateway_control"], media.path[len(prefix):])
    with urlopen(local, timeout=5) as response:
        playlist = response.read(65537)
    if len(playlist) > 65536:
        raise SystemExit("Owned playlist exceeds bound")
    names = [line for line in playlist.decode().splitlines() if line and not line.startswith("#")]
    if not names or not re.fullmatch(r"[A-Za-z0-9_.-]+\.ts", names[0]):
        raise SystemExit("Owned MPEGTS segment required")
    with urlopen(urljoin(local, names[0]), timeout=5) as response:
        segment = response.read(8 * 1024 * 1024 + 1)
    if not segment or len(segment) > 8 * 1024 * 1024:
        raise SystemExit("Owned segment exceeds bound")
    return names[0], hashlib.sha256(segment).digest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("selector", type=int)
    parser.add_argument("--timeout", type=int, default=120)
    args = parser.parse_args()
    if not 0 <= args.selector <= 65535 or not 1 <= args.timeout <= 180:
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
    observed = {}
    while time.monotonic() < deadline:
        raw = docker("exec", owned[0], "/bin/sh", "-c",
                     'for p in /proc/[0-9]*/cmdline; do '
                     'test -r "$p" || continue; '
                     'n=${p#/proc/}; n=${n%/cmdline}; '
                     'printf "%s\\0" "$n"; cat "$p" 2>/dev/null; '
                     'printf "\\0\\0"; done')
        candidates = matching_maps(raw)
        if candidates:
            private = directory / "native-ffmpeg-argv.bin"
            descriptor = os.open(private, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
            with os.fdopen(descriptor, "wb") as output:
                if os.fstat(output.fileno()).st_size + len(raw) > 4 * 1024 * 1024:
                    raise SystemExit("Private process-observation byte bound reached")
                output.write(raw)
            observed.update(candidates)
        if len(observed) > 16:
            raise SystemExit("Owned output-observation count bound reached")
        delivery_file = directory / "native-raw-last-delivery.json"
        try:
            delivery = json.loads(delivery_file.read_text())
            job = output_job(delivery, args.selector)
        except (FileNotFoundError, json.JSONDecodeError):
            job = None
        if job:
            name, served_hash = served_segment(directory, delivery)
            count = 0
            for playlist, maps in observed.items():
                if f"0:{args.selector}" not in maps:
                    continue
                try:
                    generated = docker("exec", owned[0], "cat", str(Path(playlist).parent / name))
                except subprocess.CalledProcessError:
                    continue
                if hashlib.sha256(generated).digest() == served_hash:
                    count += 1
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
