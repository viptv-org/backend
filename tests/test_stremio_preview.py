"""Offline tests for the read-only Stremio migration preview."""

import base64
import importlib.util
from pathlib import Path
import unittest
import zlib

SCRIPT = Path(__file__).resolve().parents[1] / "scripts" / "stremio-preview.py"
spec = importlib.util.spec_from_file_location("stremio_preview", SCRIPT)
preview_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview_module)


class PreviewTests(unittest.TestCase):
    def test_failed_preview_reports_only_the_error_type(self):
        from contextlib import redirect_stderr
        from io import StringIO
        from unittest.mock import patch

        output = StringIO()
        with patch.object(preview_module.sys, "argv", ["stremio-preview.py"]), \
                patch.object(preview_module, "credentials", side_effect=ValueError("private source diagnostic")), \
                redirect_stderr(output):
            self.assertEqual(preview_module.main(), 1)
        self.assertEqual(output.getvalue(), "Preview failed (ValueError); no account writes attempted.\n")

    def test_anchor_alignment_and_invalid_metadata(self):
        videos = [{"id": f"tt12345:1:{n}", "season": 1, "episode": n} for n in range(1, 6)]
        # Source bits correspond to episodes 1, 2 and 3; a new episode before
        # them moves the anchor in the metadata ordering.
        encoded = base64.b64encode(zlib.compress(bytes([0b00000111]))).decode()
        watched = f"tt12345:1:3:3:{encoded}"
        self.assertEqual([v["id"] for v in preview_module.decode_episodes(watched, videos)],
                         [v["id"] for v in videos[:3]])
        with self.assertRaises(ValueError):
            preview_module.decode_episodes(watched, videos[3:])
        with self.assertRaises(ValueError):
            preview_module.decode_episodes("tt12345:1:3:3:not-base64", videos)

    def test_preview_saves_only_saved_titles_and_preserves_newer_progress(self):
        def item(ident, kind, *, removed=False, temp=False, offset=0, duration=100000,
                 video=None, watched="", stamp="2025-01-01T00:00:00Z"):
            return {"_id": ident, "name": "fixture", "type": kind, "removed": removed,
                    "temp": temp, "state": {"lastWatched": stamp, "timeOffset": offset,
                    "duration": duration, "video_id": video, "watched": watched, "timesWatched": 0}}
        rows = [item("tt12345", "series", offset=40000, video="tt12345:1:3", watched="present"),
                item("tt23456", "movie", removed=True, temp=True, offset=30000),
                item("unsupported-id", "series", watched="present"),
                item("tt34567", "movie", offset=0)]
        fetched = []

        def fake_episodes(row):
            fetched.append(row["_id"])
            return [{"id": "tt12345:1:1"}, {"id": "tt12345:1:2"}]

        target = ({("movie", "tt34567")},
                  {("series", "tt12345:1:3"): 1735689601, ("series", "tt12345:1:1"): 1})
        result = preview_module.preview(rows, target, fetch_episodes=fake_episodes)
        self.assertEqual(fetched, ["tt12345"])
        self.assertEqual(result["saved_titles"], 3)
        self.assertEqual(result["favorite_candidates"], 1)
        self.assertEqual(result["saved_titles_needing_id_review"], 1)
        self.assertEqual(result["favorite_already_present"], 1)
        self.assertEqual(result["resume_existing_newer_or_equal"], 1)
        self.assertEqual(result["resume_candidates"], 1)  # Removed, temporary movie still has a resumable play.
        self.assertNotIn("resume_needing_id_review", result)
        self.assertEqual(result["verified_watched_episodes"], 2)
        self.assertEqual(result["episodes_already_in_viptv"], 1)
        self.assertEqual(result["episodes_without_viptv_progress"], 1)
        self.assertTrue(result["target_compared"])

    def test_cleared_removed_history_is_excluded_without_losing_resumable_items(self):
        def item(ident, *, offset=0, watched="", times=0, removed=True, temp=True):
            return {"_id": ident, "type": "movie", "name": "fixture", "removed": removed,
                    "temp": temp, "state": {"timeOffset": offset, "duration": 100000,
                    "timesWatched": times, "watched": watched,
                    "lastWatched": "2025-01-01T00:00:00Z"}}

        rows = [item("tt12345"), item("tt23456", offset=10000),
                item("tt34567", times=1), item("tt45678", removed=False, temp=False)]
        result = preview_module.preview(rows, fetch_episodes=lambda _: self.fail("No series expected"))
        self.assertEqual(result["cleared_removed_items_skipped"], 1)
        self.assertEqual(result["resume_candidates"], 1)
        self.assertEqual(result["movie_watched_candidates"], 1)
        self.assertEqual(result["favorite_candidates"], 1)
        self.assertEqual(result["source_items"], 4)


    def test_target_database_is_read_only_and_profile_scoped(self):
        import sqlite3
        import tempfile

        with tempfile.TemporaryDirectory(dir=Path(__file__).parent) as directory:
            path = Path(directory) / "test.sqlite"
            with sqlite3.connect(path) as db:
                db.executescript("""CREATE TABLE profile_owners(profile_id INTEGER,account_id INTEGER);
                    CREATE TABLE favorites(profile_id INTEGER,type TEXT,id TEXT);
                    CREATE TABLE progress(profile_id INTEGER,type TEXT,id TEXT,updated_at INTEGER);
                    INSERT INTO profile_owners VALUES(1,1);
                    INSERT INTO favorites VALUES(1,'movie','tt12345');
                    INSERT INTO favorites VALUES(2,'movie','tt23456');
                    INSERT INTO progress VALUES(1,'movie','tt12345',123);
                    """)
            favorites, progress = preview_module.read_target(path, "1")
            self.assertEqual(favorites, {("movie", "tt12345")})
            self.assertEqual(progress, {("movie", "tt12345"): 123})
            with self.assertRaises(ValueError):
                preview_module.read_target(path, "2")
            with self.assertRaises(ValueError):
                preview_module.read_target(path, "1; DROP TABLE favorites")


    def test_unavailable_episode_metadata_is_not_counted_as_watched(self):
        row = {"_id": "tt12345", "name": "fixture", "type": "series", "removed": True,
               "temp": False, "state": {"watched": "opaque", "timeOffset": 0}}
        result = preview_module.preview([row], fetch_episodes=lambda _: None)
        self.assertEqual(result["series_needing_episode_review"], 1)
        self.assertNotIn("verified_watched_episodes", result)
        self.assertFalse(result["target_compared"])


if __name__ == "__main__":
    unittest.main()
