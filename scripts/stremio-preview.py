#!/usr/bin/env python3
"""Read-only, aggregate Stremio -> VIPTV migration preview. Never exports user records."""

import argparse
import base64
from collections import Counter
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
import json
import binascii
from pathlib import Path
import re
import sqlite3
import sys
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen
import zlib

API = "https://api.strem.io/api/"
META = "https://v3-cinemeta.strem.io/meta/series/"
IMDB = re.compile(r"tt[0-9]+\Z")
MAX_ITEMS = 10_000
MAX_EPISODES = 5_000


def request_json(url, body=None):
    request = Request(url, data=json.dumps(body).encode() if body is not None else None,
                      headers={"Content-Type": "application/json", "User-Agent": "VIPTV-readonly-preview/1"})
    with urlopen(request, timeout=15) as response:
        if int(response.headers.get("Content-Length", 0)) > 8_000_000:
            raise ValueError("Remote response too large")
        data = response.read(8_000_001)
        if len(data) > 8_000_000:
            raise ValueError("Remote response too large")
        return json.loads(data)


def stremio_post(path, body):
    response = request_json(API + path, body)
    if not isinstance(response, dict) or "result" not in response:
        raise ValueError("Stremio did not return a successful result")
    return response["result"]


def credentials(env_path):
    values = {}
    for line in env_path.read_text().splitlines():
        if "=" in line and not line.lstrip().startswith("#"):
            key, value = line.split("=", 1)
            if key.strip() in ("STREMIO_EMAIL", "STREMIO_PASSWORD"):
                values[key.strip()] = value.strip().strip("\"'")
    if not values.get("STREMIO_EMAIL") or not values.get("STREMIO_PASSWORD"):
        raise ValueError("STREMIO_EMAIL and STREMIO_PASSWORD must be set in the private .env")
    return values["STREMIO_EMAIL"], values["STREMIO_PASSWORD"]


def family(identifier):
    if not isinstance(identifier, str):
        return "unsupported"
    if IMDB.fullmatch(identifier):
        return "imdb"
    if identifier.startswith("kitsu:"):
        return "kitsu"
    return "unsupported"


def last_watched(item):
    try:
        value = item["state"]["lastWatched"]
        return int(datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp())
    except (KeyError, TypeError, ValueError, OverflowError, AttributeError):
        return None


def decode_episodes(watched, videos):
    """Follow Stremio's anchor alignment and little-endian per-byte bit order."""
    if not isinstance(watched, str) or len(watched) > 100_000 or len(videos) > MAX_EPISODES:
        raise ValueError("Invalid watched bitfield or metadata size")
    anchor, length, encoded = watched.rsplit(":", 2)
    if not length.isdecimal() or len(encoded) > 100_000:
        raise ValueError("Invalid bitfield encoding")
    compressed = base64.b64decode(encoded, validate=True)
    inflater = zlib.decompressobj()
    bits = inflater.decompress(compressed, MAX_EPISODES * 8 + 1)
    if len(bits) > MAX_EPISODES * 8 or inflater.unconsumed_tail or not inflater.eof:
        raise ValueError("Bitfield too large or truncated")
    ordered = sorted(videos, key=lambda v: (v.get("season", -1), v.get("episode", -1), v.get("released") or ""))
    ids = [v.get("id") for v in ordered]
    if not all(isinstance(ident, str) and ident for ident in ids) or len(set(ids)) != len(ids):
        raise ValueError("Incomplete or duplicated episode identities")
    if anchor not in ids:
        raise ValueError("Watched anchor absent from metadata")
    offset = int(length) - ids.index(anchor) - 1
    matched = []
    for index, video in enumerate(ordered):
        old = index + offset
        if 0 <= old < len(bits) * 8 and bits[old // 8] & (1 << (old % 8)):
            matched.append(video)
    if len(matched) != sum(byte.bit_count() for byte in bits):
        raise ValueError("Some watched bits do not map to metadata")
    return matched


def episode_candidates(item):
    identifier = item["_id"]
    try:
        response = request_json(META + identifier + ".json")
        videos = response.get("meta", {}).get("videos", [])
        if not isinstance(videos, list) or not videos:
            return None
        return decode_episodes(item["state"]["watched"], videos)
    except (HTTPError, URLError, TimeoutError, ValueError, KeyError, TypeError, zlib.error, binascii.Error, EOFError):
        return None


def read_target(path, profile):
    if not path.is_file() or not profile.isdecimal():
        raise ValueError("Target database must exist and profile ID must be numeric")
    db = sqlite3.connect(path.resolve().as_uri() + "?mode=ro", uri=True)
    try:
        owned = db.execute("SELECT 1 FROM profile_owners WHERE profile_id=?", (int(profile),)).fetchone()
        if not owned:
            raise ValueError("Profile has no account owner or does not exist")
        favorites = {(kind, ident) for kind, ident in db.execute(
            "SELECT type,id FROM favorites WHERE profile_id=?", (int(profile),))}
        progress = {(kind, ident): updated for kind, ident, updated in db.execute(
            "SELECT type,id,updated_at FROM progress WHERE profile_id=?", (int(profile),))}
        return favorites, progress
    finally:
        db.close()


def cleared_history(item):
    """A removed entry with no remaining resume or watched state is not importable history."""
    state = item["state"]
    return (item.get("removed") is True
            and not state.get("timeOffset")
            and not state.get("timesWatched")
            and not state.get("watched"))



def preview(items, target=None, fetch_episodes=episode_candidates):
    if not isinstance(items, list) or len(items) > MAX_ITEMS:
        raise ValueError("Unexpected library size")
    result = Counter()
    favorites, progress = target if target else (set(), {})
    result["source_items"] = len(items)
    imdb_watched = []
    for item in items:
        if not isinstance(item, dict) or not isinstance(item.get("state"), dict):
            result["malformed_items"] += 1
            continue
        kind = item.get("type")
        ident = item.get("_id")
        if kind not in ("movie", "series") or not isinstance(ident, str) or not ident:
            result["unsupported_items"] += 1
            continue
        if cleared_history(item):
            result["cleared_removed_items_skipped"] += 1
            continue
        group = family(ident)
        state = item["state"]
        if not item.get("removed") and not item.get("temp"):
            result["saved_titles"] += 1
            if group == "unsupported":
                result["saved_titles_needing_id_review"] += 1
            elif (kind, ident) in favorites:
                result["favorite_already_present"] += 1
            else:
                result["favorite_candidates"] += 1
        offset, duration = state.get("timeOffset"), state.get("duration")
        video = state.get("video_id")
        valid_resume = (type(offset) is int and type(duration) is int and 0 < offset <= duration <= 1_000_000_000
                        and (kind == "movie" or isinstance(video, str) and bool(video)))
        if valid_resume:
            result["resume_with_valid_offset"] += 1
            # Movie progress uses the movie title ID; series uses an exact video ID and parent context.
            key = (kind, ident if kind == "movie" else video)
            timestamp = last_watched(item)
            if group == "unsupported":
                result["resume_needing_id_review"] += 1
            elif timestamp is None:
                result["resume_needing_timestamp_review"] += 1
            elif key in progress:
                result["resume_existing_newer_or_equal"] += progress[key] >= timestamp
                result["resume_existing_older"] += progress[key] < timestamp
            else:
                result["resume_candidates"] += 1
        if kind == "movie" and type(state.get("timesWatched")) is int and state["timesWatched"] > 0:
            result["movie_watched_candidates"] += 1
        if kind == "series" and state.get("watched"):
            result["series_with_watched_flags"] += 1
            if group == "imdb":
                imdb_watched.append(item)
            else:
                result["series_needing_non_imdb_metadata"] += 1
        if group == "unsupported":
            result["titles_with_unsupported_id"] += 1
    # Fetch metadata in bounded parallel read-only requests, never print IDs or titles.
    with ThreadPoolExecutor(max_workers=4) as pool:
        for episodes in pool.map(fetch_episodes, imdb_watched):
            if episodes is None:
                result["series_needing_episode_review"] += 1
                continue
            result["series_with_verified_episodes"] += 1
            result["verified_watched_episodes"] += len(episodes)
            for video in episodes:
                ident = video["id"]
                if ("series", ident) in progress:
                    result["episodes_already_in_viptv"] += 1
                else:
                    result["episodes_without_viptv_progress"] += 1
    result["target_compared"] = target is not None
    return dict(sorted(result.items()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env", type=Path, default=Path(__file__).resolve().parents[2] / ".env")
    parser.add_argument("--db", type=Path, help="Optional VIPTV SQLite database for read-only conflict preview")
    parser.add_argument("--profile-id", help="Required with --db; numeric VIPTV profile ID")
    args = parser.parse_args()
    if bool(args.db) != bool(args.profile_id):
        parser.error("--db and --profile-id must be supplied together")
    try:
        email, password = credentials(args.env)
        login = stremio_post("login", {"type": "Login", "email": email, "password": password, "facebook": False})
        token = login["authKey"]
        items = stremio_post("datastoreGet", {"authKey": token, "collection": "libraryItem", "ids": [], "all": True})
        target = read_target(args.db, args.profile_id) if args.db else None
        print(json.dumps(preview(items, target), indent=2))
        print("Preview only: nothing was written to Stremio or VIPTV.")
    except (KeyError, ValueError, HTTPError, URLError, TimeoutError, OSError, sqlite3.Error) as error:
        print("Preview failed (" + type(error).__name__ + "); no account writes attempted.", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
