"""Ensure source replacement cannot borrow another output's map proof."""
import importlib.util
from pathlib import Path
import unittest

path = Path(__file__).resolve().parents[1] / "scripts/observe-native-torrent-map.py"
spec = importlib.util.spec_from_file_location("map_observer", path)
observer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(observer)
CURRENT = "10000000-0000-0000-0000-000000000001"
OLD = "10000000-0000-0000-0000-000000000002"


def record(pid, executable, job, selector):
    return b"\0".join(str(value).encode() for value in
                      [pid, executable, "-map", f"0:{selector}",
                       f"/private/media/{job}/index.m3u8"]) + b"\0\0\0"


class OutputMapTests(unittest.TestCase):
    def test_old_output_and_non_ffmpeg_cannot_prove_current_selection(self):
        raw = record(1, "/bin/ffmpeg", OLD, 2) + record(2, "/bin/sh", CURRENT, 2)
        self.assertEqual(observer.matching_maps(raw, CURRENT), [])

    def test_current_output_reads_actual_numeric_selector(self):
        raw = record(1, "/bin/ffmpeg", OLD, 1) + record(2, "/bin/ffmpeg", CURRENT, 2)
        self.assertEqual(observer.matching_maps(raw, CURRENT), ["0:2"])

    def test_pending_or_other_selected_input_is_not_ready_proof(self):
        delivery = {"status": "starting", "delivery": {
            "selected_audio": {"input_index": 2},
            "url": f"https://fixture.invalid/media/{CURRENT}/private/index.m3u8"}}
        self.assertIsNone(observer.output_job(delivery, 2))
        delivery["status"] = "ready"
        self.assertIsNone(observer.output_job(delivery, 1))
        self.assertEqual(observer.output_job(delivery, 2), CURRENT)


if __name__ == "__main__":
    unittest.main()
