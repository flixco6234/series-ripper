"""In-memory runtime state shared between the web routes, the ripper, and
the background watcher thread. Nothing here needs to survive a restart -
config and history live in the database (see db.py).
"""

import threading

_lock = threading.RLock()

_state = {
    "status": "idle",              # idle | scanning | ripping
    "drive_busy": {},              # drive_index -> bool
    "staged_scan": None,           # last manual scan awaiting review/rip
    "scan_error": None,            # error text of the last failed manual scan
    "rip_progress": None,          # current rip queue + position + percent
    "cancel_requested": False,
    "watcher_last_seen": {},       # drive_index -> disc_label seen last poll
}


def get():
    with _lock:
        return dict(_state)


def set_status(status: str):
    with _lock:
        _state["status"] = status


def is_drive_busy(drive_index: int) -> bool:
    with _lock:
        return _state["drive_busy"].get(drive_index, False)


def set_drive_busy(drive_index: int, busy: bool):
    with _lock:
        _state["drive_busy"][drive_index] = busy


def set_staged_scan(scan):
    with _lock:
        _state["staged_scan"] = scan


def get_staged_scan():
    with _lock:
        return _state["staged_scan"]


def try_begin(status: str) -> bool:
    """Atomically move from idle to `status`; False if something else runs.
    Only one MakeMKV operation (scan or rip) runs at a time."""
    with _lock:
        if _state["status"] != "idle":
            return False
        _state["status"] = status
        return True


def set_scan_error(err):
    with _lock:
        _state["scan_error"] = err


def get_scan_error():
    with _lock:
        return _state["scan_error"]


def update_rip_progress(**changes):
    with _lock:
        if _state["rip_progress"] is not None:
            _state["rip_progress"] = {**_state["rip_progress"], **changes}


def set_rip_progress(progress):
    with _lock:
        _state["rip_progress"] = progress


def get_rip_progress():
    with _lock:
        return _state["rip_progress"]


def request_cancel():
    with _lock:
        _state["cancel_requested"] = True


def clear_cancel():
    with _lock:
        _state["cancel_requested"] = False


def cancel_requested() -> bool:
    with _lock:
        return _state["cancel_requested"]


def get_last_seen(drive_index: int):
    with _lock:
        return _state["watcher_last_seen"].get(drive_index)


def set_last_seen(drive_index: int, disc_label):
    with _lock:
        _state["watcher_last_seen"][drive_index] = disc_label
