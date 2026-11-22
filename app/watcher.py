"""Background thread for Automatic Mode: polls MakeMKV's drive list and
reacts when a drive goes from "no disc" to "disc present".

There's nobody at the console of a headless container, so this never
guesses through an ambiguous disc: if the detectors disagree or flag a title
that would be ripped, the disc is parked in the review queue (unripped) and
waits for a decision in the web UI. A disc that was already ripped
completely (same label and title layout) is skipped, so a container restart
with a disc still in the tray doesn't rip it again.
"""

import threading

from . import db, state, makemkv, detection, ripper

_stop_event = threading.Event()
_thread = None


def start():
    global _thread
    if _thread and _thread.is_alive():
        return
    _stop_event.clear()
    _thread = threading.Thread(target=_loop, daemon=True, name="watcher")
    _thread.start()


def stop():
    _stop_event.set()


def _loop():
    db.log("info", "Automation watcher started.")
    while not _stop_event.is_set():
        cfg = db.get_config()
        interval = max(5, int(cfg.get("poll_interval_sec", 15)))
        # Only poll while nothing else is using MakeMKV.
        if cfg.get("automatic_mode") and cfg.get("makemkv_path") and state.get()["status"] == "idle":
            try:
                _poll_once(cfg)
            except Exception as e:  # keep the watcher alive across a bad poll
                db.log("error", f"Watcher error: {e}")
                state.set_status("idle")
        _stop_event.wait(interval)
    db.log("info", "Automation watcher stopped.")


def _poll_once(cfg: dict):
    drives = makemkv.get_drives(cfg["makemkv_path"])
    watched = cfg.get("drive_indexes") or [d["index"] for d in drives]
    for d in drives:
        if d["index"] not in watched or state.is_drive_busy(d["index"]):
            continue
        last_seen = state.get_last_seen(d["index"])
        current = d["disc_name"] if d["has_disc"] else None
        state.set_last_seen(d["index"], current)
        if current and current != last_seen:
            if not state.try_begin("scanning"):
                state.set_last_seen(d["index"], last_seen)  # try again next poll
                return
            db.log("info", f"Disc detected in drive {d['index']}: '{current}'")
            _process_disc(cfg, d)


def _process_disc(cfg: dict, drive: dict):
    drive_index = drive["index"]
    handed_to_ripper = False
    state.set_drive_busy(drive_index, True)
    try:
        if not cfg.get("show_name"):
            db.log("warn", "Automatic Mode: set a show name in Settings first - disc left alone.")
            return
        disc = makemkv.get_disc_info(cfg["makemkv_path"], drive_index,
                                     on_message=lambda m: db.log("debug", m))
        titles = disc["titles"]
        if not titles:
            db.log("warn", f"No titles found on disc in drive {drive_index}.")
            return

        fingerprint = detection.disc_fingerprint(disc)
        if db.was_ripped(fingerprint):
            db.log("info", f"Disc '{drive['disc_name']}' was already ripped completely - skipping it. "
                           f"Use Manual Rip to rip it again.")
            return

        rows, notes = detection.classify_titles(titles, cfg)
        for note in notes:
            db.log("info", note)
        detection.assign_episode_numbers(rows, cfg["next_episode"])
        conflict = detection.check_name_conflict(rows) if cfg["detect_names"] else None

        reason = detection.review_reason(rows, conflict)
        if not reason:
            try:
                picked = detection.validate_rip_rows(rows)
            except ValueError as e:
                reason = str(e)
        if reason:
            item_id = db.add_review_item(drive_index, drive["disc_name"], rows, reason, fingerprint)
            db.log("warn", f"Disc in drive {drive_index} needs review ({reason}). "
                           f"Added to the review queue as #{item_id} - nothing ripped yet.")
            return

        db.log("info", f"Auto-ripping {len(picked)} title(s) from drive {drive_index} "
                       f"({disc.get('disc_type') or 'disc'}).")
        state.set_status("ripping")
        handed_to_ripper = True
        ripper.rip_rows(cfg, drive_index, drive.get("device"), picked,
                        fingerprint=fingerprint, disc_label=drive["disc_name"])
    finally:
        state.set_drive_busy(drive_index, False)
        if not handed_to_ripper:
            state.set_status("idle")
