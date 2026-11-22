import os
import tempfile
import unittest

from app import detection, license, makemkv, naming

HERE = os.path.dirname(os.path.abspath(__file__))
with open(os.path.join(HERE, "fixtures", "kare_kano_vol5_info.txt"), encoding="utf-8") as fh:
    REAL_SCAN = fh.read()

CFG = {
    "min_length_sec": 900, "long_warn_sec": 3600, "cluster_tolerance": 0.25,
    "detect_length_filter": True, "detect_long_warn": True, "detect_cluster": True,
    "detect_names": True, "detect_play_all": True,
}


def title(idx, sec, name=""):
    return {"title_index": idx, "duration_text": "", "duration_sec": sec, "size_bytes": 0,
            "text_values": [name] if name else []}


class ParseRealScan(unittest.TestCase):
    def test_disc_and_titles(self):
        disc = makemkv.parse_disc_info(REAL_SCAN)
        self.assertEqual(disc["disc_type"], "DVD")
        self.assertEqual(disc["disc_name"], "KARE_KANO_VOL5")
        self.assertEqual([t["duration_sec"] for t in disc["titles"]], [4230, 1387, 1386, 1457, 830, 16])
        self.assertEqual(disc["titles"][0]["size_bytes"], 3395061760)
        self.assertEqual(disc["titles"][0]["chapters"], 22)

    def test_generated_names_are_not_episode_hints(self):
        # TINFO 27/30/49 hold MakeMKV-made codes like "E4" - must not become "Episode 4".
        disc = makemkv.parse_disc_info(REAL_SCAN)
        self.assertTrue(all(t["text_values"] == [] for t in disc["titles"]))

    def test_drives_skip_empty_slots(self):
        drives = makemkv.parse_drives(REAL_SCAN)
        self.assertEqual(len(drives), 1)
        self.assertEqual(drives[0]["disc_name"], "KARE_KANO_VOL5")

    def test_messages_keep_umlauts(self):
        msgs = []
        makemkv.parse_disc_info(REAL_SCAN, on_message=msgs.append)
        self.assertIn("Titel #1 wurde hinzugefügt (27 cell(s), 1:10:30)", msgs)

    def test_progress_uses_total_field(self):
        self.assertEqual(makemkv.parse_progress(["65536", "32768", "65536"]), 50)
        self.assertIsNone(makemkv.parse_progress(["0", "0", "0"]))


class Detection(unittest.TestCase):
    def classify(self, titles, cfg=CFG, next_ep=14):
        rows, notes = detection.classify_titles(titles, cfg)
        detection.assign_episode_numbers(rows, next_ep)
        return rows, notes

    def test_real_disc(self):
        rows, _ = self.classify(makemkv.parse_disc_info(REAL_SCAN)["titles"])
        by = {r["title_index"]: r for r in rows}
        self.assertFalse(by[0]["include"])
        self.assertIn("play_all", by[0]["flags"])
        self.assertEqual([by[i]["episode"] for i in (1, 2, 3)], [14, 15, 16])
        self.assertTrue(by[4]["special"] and not by[4]["include"])
        self.assertTrue(by[5]["special"] and not by[5]["include"])
        self.assertIsNone(detection.review_reason(rows, detection.check_name_conflict(rows)))

    def test_without_play_all_detection_auto_mode_asks(self):
        cfg = dict(CFG, detect_play_all=False)
        rows, _ = self.classify(makemkv.parse_disc_info(REAL_SCAN)["titles"], cfg)
        self.assertTrue(rows[0]["include"])
        self.assertIn("long-title", detection.review_reason(rows, None))

    def test_long_double_episode_is_not_play_all(self):
        # 44:40 is not 22:07 + 22:06 within the 3 s slack.
        rows, _ = self.classify([title(0, 1327), title(1, 1326), title(2, 2680)])
        self.assertTrue(all(r["include"] for r in rows))

    def test_play_all_of_blu_ray_style_disc(self):
        rows, _ = self.classify([title(0, 2600), title(1, 2650), title(2, 2610), title(3, 7861)])
        self.assertFalse(rows[3]["include"])
        self.assertEqual([r["episode"] for r in rows[:3]], [14, 15, 16])

    def test_shared_disc_label_is_not_a_hint(self):
        rows, _ = self.classify([title(0, 1300, "SHOW Episode 1"), title(1, 1300, "SHOW Episode 1")])
        self.assertTrue(all(r["hint"] is None for r in rows))

    def test_name_hint_conflict(self):
        rows, _ = self.classify([title(0, 1300, "Episode 3"), title(1, 1300, "Episode 1")], next_ep=1)
        conflict = detection.check_name_conflict(rows)
        self.assertTrue(conflict["conflict"])
        self.assertIn("disagree", detection.review_reason(rows, conflict))

    def test_e_ac3_is_not_an_episode(self):
        self.assertIsNone(makemkv.find_episode_hint(["E-AC3 5.1"]))

    def test_fingerprint_changes_with_layout(self):
        disc = makemkv.parse_disc_info(REAL_SCAN)
        other = dict(disc, titles=disc["titles"][:-1])
        self.assertNotEqual(detection.disc_fingerprint(disc), detection.disc_fingerprint(other))


class Naming(unittest.TestCase):
    def test_show_season_layout(self):
        row = {"episode": 3, "special": False, "episode_title": "Pilot?"}
        self.assertEqual(naming.build_dest_dir("/output", "Kare Kano", 1, row, "show_season"),
                         os.path.join("/output", "Kare Kano", "Season 1"))
        self.assertEqual(naming.build_dest_dir("/output", "Kare Kano", 1, row, "show_season", pad_season=True),
                         os.path.join("/output", "Kare Kano", "Season 01"))
        self.assertEqual(naming.build_filename("Kare Kano", 1, row), "Kare Kano - S01E03 - Pilot.mkv")

    def test_special_goes_to_season_0(self):
        row = {"episode": 1, "special": True}
        self.assertEqual(naming.build_dest_dir("/o", "Show", 2, row, "show_season"), os.path.join("/o", "Show", "Season 0"))
        self.assertEqual(naming.build_dest_dir("/o", "Show", 2, row, "show"), os.path.join("/o", "Show"))
        self.assertEqual(naming.build_dest_dir("/o", "Show", 2, row, "flat"), "/o")


class License(unittest.TestCase):
    PAGE = ('<p>The current beta key is valid until end of November 2026.</p>'
            '<div class="codebox"><p>Code: <a href="#">Select all</a></p>'
            '<pre><code>T-AbC_123-xyz0987654321qwertyuiopasdfghjk</code></pre></div>')

    def test_extract(self):
        key, until = license.extract_beta_key(self.PAGE)
        self.assertEqual(key, "T-AbC_123-xyz0987654321qwertyuiopasdfghjk")
        self.assertEqual(until, "end of November 2026")

    def test_no_key(self):
        self.assertEqual(license.extract_beta_key("<html>maintenance</html>"), (None, None))

    def test_write_key_keeps_other_settings(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "settings.conf")
            with open(path, "w") as fh:
                fh.write('app_DataDir = "/x"\napp_Key = "T-old"\n')
            license.write_key("T-new", path)
            with open(path) as fh:
                text = fh.read()
            self.assertIn('app_DataDir = "/x"', text)
            self.assertIn('app_Key = "T-new"', text)
            self.assertNotIn("T-old", text)
            self.assertEqual(license.current_key(path), "T-new")


if __name__ == "__main__":
    unittest.main()
