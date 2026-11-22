"""Starts the real server against tests/mock_makemkvcon.py and drives it over
HTTP the way the web UI does: manual scan, Automatic Mode parking a disc in
the review queue, resolving it, skipping an already-ripped disc, and a
manual Special rip."""

import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.request

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MOCK = os.path.join(ROOT, "tests", "mock_makemkvcon.py")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


class Server(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="series-ripper-test-")
        bindir = os.path.join(cls.tmp, "bin")
        os.makedirs(bindir)
        exe = os.path.join(bindir, "makemkvcon")
        with open(exe, "w") as fh:
            fh.write(f'#!/bin/sh\nexec "{sys.executable}" "{MOCK}" "$@"\n')
        os.chmod(exe, 0o755)
        cls.out = os.path.join(cls.tmp, "output")
        cls.flag = os.path.join(cls.tmp, "disc_in")
        cls.port = free_port()
        env = dict(os.environ,
                   PATH=bindir + os.pathsep + os.environ["PATH"],
                   HOME=os.path.join(cls.tmp, "home"),
                   DVDRIPPER_DB=os.path.join(cls.tmp, "config", "test.db"),
                   DVDRIPPER_TEMP=os.path.join(cls.out, ".series-ripper-tmp"),
                   DVDRIPPER_PORT=str(cls.port),
                   MAKEMKV_KEY="T-testkey-for-the-integration-test-only",
                   PUID=str(os.getuid()), PGID=str(os.getgid()),
                   MOCK_DISC_FLAG=cls.flag, MOCK_SCAN_DELAY="1")
        cls.proc = subprocess.Popen([sys.executable, "-m", "app.main"], cwd=ROOT, env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        for _ in range(100):
            try:
                cls.get("/api/status")
                break
            except OSError:
                time.sleep(0.1)

    @classmethod
    def tearDownClass(cls):
        cls.proc.terminate()
        cls.proc.wait(timeout=10)
        shutil.rmtree(cls.tmp, ignore_errors=True)

    @classmethod
    def call(cls, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(f"http://127.0.0.1:{cls.port}{path}", data=data, method=method,
                                     headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.loads(r.read() or b"null")
        except urllib.error.HTTPError as e:
            raise AssertionError(f"{method} {path} -> {e.code}: {e.read().decode()}")

    @classmethod
    def get(cls, path):
        return cls.call("GET", path)

    def wait_for(self, check, timeout=60, what="condition"):
        end = time.time() + timeout
        while time.time() < end:
            result = check()
            if result:
                return result
            time.sleep(0.5)
        logs = [e["message"] for e in self.get("/api/logs?limit=40")["entries"]]
        self.fail(f"timed out waiting for {what}; recent log:\n" + "\n".join(logs))

    def log_has(self, text):
        return any(text in e["message"] for e in self.get("/api/logs?limit=500")["entries"])

    def wait_scan(self):
        def done():
            s = self.get("/api/scan")
            return None if s["running"] else s
        scan = self.wait_for(done, what="scan")
        self.assertIsNone(scan["error"])
        return scan

    def wait_idle(self):
        self.wait_for(lambda: self.get("/api/status")["status"] == "idle", what="idle")

    def read(self, *parts):
        with open(os.path.join(self.out, *parts), "rb") as fh:
            return fh.read()

    def test_full_flow(self):
        st = self.get("/api/status")
        self.assertEqual(st["makemkv_version"], "MakeMKV v2.0.0 linux(x64-release)")
        self.wait_for(lambda: self.get("/api/status")["license"]["mode"] == "custom", what="license")
        with open(os.path.join(self.tmp, "home", ".MakeMKV", "settings.conf")) as fh:
            self.assertIn('app_Key = "T-testkey-for-the-integration-test-only"', fh.read())

        open(self.flag, "w").close()
        drives = self.get("/api/drives")["drives"]
        self.assertEqual([(d["index"], d["disc_name"], d["device"]) for d in drives],
                         [(0, "KARE_KANO_VOL5", "/dev/sr0")])

        self.call("POST", "/api/config", {"show_name": "Kare Kano", "season": 1, "next_episode": 14,
                                          "output_dir": self.out})

        # Manual scan runs in the background; the UI polls GET /api/scan.
        self.assertTrue(self.call("POST", "/api/scan", {"drive_index": 0})["started"])
        scan = self.wait_scan()
        rows = scan["result"]["rows"]
        self.assertEqual(scan["result"]["disc_type"], "DVD")
        self.assertEqual([(r["title_index"], r["include"], r["episode"]) for r in rows],
                         [(0, False, None), (1, True, 14), (2, True, 15), (3, True, 16),
                          (4, False, None), (5, False, None)])
        self.assertIn("play_all", rows[0]["flags"])

        # Automatic Mode with Play All detection off: the 70-minute title would be
        # ripped as an episode, so the disc must be parked for review instead.
        os.remove(self.flag)
        self.call("POST", "/api/config", {"automatic_mode": True, "poll_interval_sec": 5,
                                          "detect_play_all": False})
        time.sleep(6)
        open(self.flag, "w").close()
        items = self.wait_for(lambda: self.get("/api/review")["items"], what="review item")
        self.assertIn("long-title", items[0]["reason"])
        self.assertEqual(os.listdir(self.out), [".series-ripper-tmp"])

        review_rows = items[0]["rows"]
        self.assertEqual([r["episode"] for r in review_rows[:4]], [14, 15, 16, 17])
        # What the UI does when the Play All row is unticked: renumber the rest.
        review_rows[0].update(include=False, episode=None)
        for r, ep in zip(review_rows[1:4], (14, 15, 16)):
            r["episode"] = ep
        self.call("POST", f"/api/review/{items[0]['id']}/resolve", {"rows": review_rows})
        self.wait_for(lambda: self.log_has("Disc complete."), what="review rip")
        self.wait_idle()
        season = ("Kare Kano", "Season 1")
        self.assertEqual(self.read(*season, "Kare Kano - S01E14.mkv"), b"MOCKMKV title 1\n")
        self.assertEqual(self.read(*season, "Kare Kano - S01E15.mkv"), b"MOCKMKV title 2\n")
        self.assertEqual(self.read(*season, "Kare Kano - S01E16.mkv"), b"MOCKMKV title 3\n")
        self.assertEqual(self.get("/api/config")["next_episode"], 17)
        self.assertEqual(sorted(os.listdir(os.path.join(self.out, ".series-ripper-tmp"))),
                         [".ignore", ".plexignore"])

        # Same disc again (e.g. container restart with it still in the tray): skipped.
        os.remove(self.flag)
        time.sleep(6)
        open(self.flag, "w").close()
        self.wait_for(lambda: self.log_has("was already ripped completely"), what="duplicate skip")

        # Manual rip of the 13:50 extra as a Special.
        self.call("POST", "/api/config", {"automatic_mode": False, "detect_play_all": True})
        self.wait_idle()
        self.call("POST", "/api/scan", {"drive_index": 0})
        scan = self.wait_scan()
        rows = scan["result"]["rows"]
        for r in rows:
            r["include"] = r["title_index"] == 4
        rows[4].update(special=True, episode=1, episode_title="Bonus: Making of")
        self.call("POST", "/api/rip", {"drive_index": 0, "rows": rows})
        self.wait_for(lambda: self.log_has("Kare Kano - S00E01 - Bonus Making of.mkv"), what="special rip")
        self.wait_idle()
        self.assertEqual(self.read("Kare Kano", "Season 0", "Kare Kano - S00E01 - Bonus Making of.mkv"),
                         b"MOCKMKV title 4\n")
        self.assertEqual(self.get("/api/config")["next_episode"], 17)


if __name__ == "__main__":
    unittest.main()
