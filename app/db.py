"""SQLite-backed persistence: config, the action log, and the automatic-mode
review queue. Everything the web UI shows or edits lives here so it survives
container restarts.
"""

import json
import sqlite3
import threading
import time
from pathlib import Path

_lock = threading.Lock()
_conn = None
LOG_KEEP = 50000   # action-log entries kept across restarts

DEFAULT_CONFIG = {
    "show_name": "",
    "season": 1,
    "next_episode": 1,
    "output_dir": "/output",
    "makemkv_path": "",
    "min_length_sec": 900,
    "long_warn_sec": 3600,
    "cluster_tolerance": 0.25,
    "detect_length_filter": True,
    "detect_long_warn": True,
    "detect_cluster": True,
    "detect_names": True,
    "detect_play_all": True,
    "folder_layout": "show_season",  # or "show" (all in the show folder) or "flat"
    "pad_season_folder": False,      # "Season 1" (False) or "Season 01" (True)
    "automatic_mode": False,
    "auto_eject": True,
    "poll_interval_sec": 15,
    "drive_indexes": [],          # empty = watch every drive MakeMKV reports
}


def init(db_path: str):
    global _conn
    Path(db_path).parent.mkdir(parents=True, exist_ok=True)
    _conn = sqlite3.connect(db_path, check_same_thread=False)
    _conn.row_factory = sqlite3.Row
    with _lock:
        _conn.executescript(
            """
            CREATE TABLE IF NOT EXISTS config (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS action_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts REAL NOT NULL,
                level TEXT NOT NULL,
                message TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS review_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_ts REAL NOT NULL,
                drive_index INTEGER NOT NULL,
                disc_label TEXT,
                rows_json TEXT NOT NULL,
                reason TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
            );
            CREATE TABLE IF NOT EXISTS ripped_discs (
                fingerprint TEXT PRIMARY KEY,
                disc_label TEXT,
                ts REAL NOT NULL
            );
            """
        )
        cols = [r[1] for r in _conn.execute("PRAGMA table_info(review_queue)").fetchall()]
        if "fingerprint" not in cols:
            _conn.execute("ALTER TABLE review_queue ADD COLUMN fingerprint TEXT")
        _conn.execute(
            "DELETE FROM action_log WHERE id <= (SELECT MAX(id) FROM action_log) - ?",
            (LOG_KEEP,),
        )
        _conn.commit()
    # Seed any config keys that don't exist yet without clobbering saved ones.
    existing = get_config()
    for k, v in DEFAULT_CONFIG.items():
        if k not in existing:
            set_config_value(k, v)


def get_config() -> dict:
    with _lock:
        rows = _conn.execute("SELECT key, value FROM config").fetchall()
    cfg = dict(DEFAULT_CONFIG)
    for row in rows:
        try:
            cfg[row["key"]] = json.loads(row["value"])
        except (ValueError, TypeError):
            pass
    return cfg


def set_config_value(key: str, value):
    with _lock:
        _conn.execute(
            "INSERT INTO config(key, value) VALUES (?, ?) "
            "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, json.dumps(value)),
        )
        _conn.commit()


def update_config(patch: dict):
    for k, v in patch.items():
        if k in DEFAULT_CONFIG:
            set_config_value(k, v)


_log_subscribers = []
_log_subscribers_lock = threading.Lock()


def subscribe_log():
    """Returns a queue-like list the SSE endpoint can poll for new entries."""
    import queue
    q = queue.Queue()
    with _log_subscribers_lock:
        _log_subscribers.append(q)
    return q


def unsubscribe_log(q):
    with _log_subscribers_lock:
        if q in _log_subscribers:
            _log_subscribers.remove(q)


def log(level: str, message: str):
    ts = time.time()
    with _lock:
        cur = _conn.execute(
            "INSERT INTO action_log(ts, level, message) VALUES (?, ?, ?)",
            (ts, level, message),
        )
        _conn.commit()
        row_id = cur.lastrowid
    entry = {"id": row_id, "ts": ts, "level": level, "message": message}
    with _log_subscribers_lock:
        for q in list(_log_subscribers):
            q.put(entry)
    return entry


def get_log(since_id: int = 0, limit: int = 200, before_id: int = None) -> list:
    """By default returns the most recent `limit` entries newer than
    `since_id`, oldest-first. Pass `before_id` instead to page backwards -
    the `limit` entries immediately older than that id (for "load more"
    in the UI); `since_id` is ignored when `before_id` is given.
    """
    with _lock:
        if before_id is not None:
            rows = _conn.execute(
                "SELECT id, ts, level, message FROM action_log "
                "WHERE id < ? ORDER BY id DESC LIMIT ?",
                (before_id, limit),
            ).fetchall()
        else:
            rows = _conn.execute(
                "SELECT id, ts, level, message FROM action_log "
                "WHERE id > ? ORDER BY id DESC LIMIT ?",
                (since_id, limit),
            ).fetchall()
    return [dict(r) for r in rows][::-1]


def add_review_item(drive_index: int, disc_label: str, rows: list, reason: str,
                    fingerprint: str = None) -> int:
    with _lock:
        cur = _conn.execute(
            "INSERT INTO review_queue(created_ts, drive_index, disc_label, rows_json, reason, status, fingerprint) "
            "VALUES (?, ?, ?, ?, ?, 'pending', ?)",
            (time.time(), drive_index, disc_label, json.dumps(rows), reason, fingerprint),
        )
        _conn.commit()
        return cur.lastrowid


def list_review_items(status: str = "pending") -> list:
    with _lock:
        rows = _conn.execute(
            "SELECT * FROM review_queue WHERE status = ? ORDER BY id DESC", (status,)
        ).fetchall()
    out = []
    for r in rows:
        d = dict(r)
        d["rows"] = json.loads(d.pop("rows_json"))
        out.append(d)
    return out


def get_review_item(item_id: int):
    with _lock:
        row = _conn.execute("SELECT * FROM review_queue WHERE id = ?", (item_id,)).fetchone()
    if not row:
        return None
    d = dict(row)
    d["rows"] = json.loads(d.pop("rows_json"))
    return d


def set_review_status(item_id: int, status: str):
    with _lock:
        _conn.execute("UPDATE review_queue SET status = ? WHERE id = ?", (status, item_id))
        _conn.commit()


def was_ripped(fingerprint: str) -> bool:
    with _lock:
        row = _conn.execute("SELECT 1 FROM ripped_discs WHERE fingerprint = ?", (fingerprint,)).fetchone()
    return row is not None


def mark_ripped(fingerprint: str, disc_label: str):
    with _lock:
        _conn.execute(
            "INSERT OR REPLACE INTO ripped_discs(fingerprint, disc_label, ts) VALUES (?, ?, ?)",
            (fingerprint, disc_label, time.time()),
        )
        _conn.commit()
