"""Executes a validated, ordered list of rip rows against one drive. Shared
by the manual "Rip Selected" route, the review queue and the automatic-mode
watcher so all paths behave identically once a decision to rip is made.

Rips go to a temp folder (never the container's own filesystem, which on
Unraid lives inside docker.img and would fill up with a single UHD title),
then get moved into place: by default a hidden folder on the output share,
so the move is an instant rename; or /temp if a separate share is mapped
there (e.g. a cache-only share), in which case the finished file is copied
across.
"""

import os
import queue
import shutil

from . import makemkv, naming, db, state


def _default_temp_root() -> str:
    if os.environ.get("DVDRIPPER_TEMP"):
        return os.environ["DVDRIPPER_TEMP"]
    if os.path.ismount("/temp"):
        return "/temp/series-ripper"
    return "/output/.series-ripper-tmp"


TEMP_ROOT = _default_temp_root()

PUID = int(os.environ.get("PUID", "99"))
PGID = int(os.environ.get("PGID", "100"))
UMASK = int(os.environ.get("UMASK", "000"), 8)

SLEEP_BUG_HINT = ("If this is a 4K UHD drive: LibreDrive drives can stop reading after sitting "
                  "idle for a couple of minutes - eject and re-insert the disc, then rip again.")


def _own(path: str, is_dir: bool):
    try:
        os.chown(path, PUID, PGID)
    except OSError:
        pass
    try:
        os.chmod(path, (0o777 if is_dir else 0o666) & ~UMASK)
    except OSError:
        pass


def makedirs_owned(path: str):
    """Like os.makedirs, but every directory it creates gets PUID/PGID, so
    Unraid shares stay writable for SMB users."""
    missing = []
    p = os.path.abspath(path)
    while p and not os.path.isdir(p):
        missing.append(p)
        p = os.path.dirname(p)
    for d in reversed(missing):
        os.makedirs(d, exist_ok=True)
        _own(d, True)


def prepare_temp_root():
    makedirs_owned(TEMP_ROOT)
    # Keep media servers from picking up half-finished rips.
    for marker in (".plexignore", ".ignore"):
        path = os.path.join(TEMP_ROOT, marker)
        if not os.path.exists(path):
            with open(path, "w") as fh:
                fh.write("*\n")
            _own(path, False)


def _enough_space(row: dict, cfg: dict):
    need = int(row.get("size_bytes") or 0)
    if need <= 0:
        return True, ""
    for label, path in (("temp", TEMP_ROOT), ("output", cfg["output_dir"])):
        probe = path
        while probe and not os.path.exists(probe):
            probe = os.path.dirname(probe)
        free = shutil.disk_usage(probe or "/").free
        if free < need * 1.05:
            return False, (f"not enough space on the {label} drive: title needs "
                           f"{need / 1e9:.1f} GB, {free / 1e9:.1f} GB free")
    return True, ""


def _rip_one(cfg: dict, drive_index: int, row: dict, temp_dir: str) -> str:
    """Rips one title into temp_dir. Returns "ok", "failed" or "cancelled"."""
    handle = makemkv.start_rip(cfg["makemkv_path"], drive_index, row["title_index"], temp_dir)
    last_error = None
    try:
        def _handle_line(line):
            nonlocal last_error
            parsed = makemkv.parse_robot_line(line)
            if not parsed:
                return
            kind, f = parsed
            if kind == "PRGV":
                pct = makemkv.parse_progress(f)
                if pct is not None:
                    state.update_rip_progress(percent=pct)
            elif kind in ("PRGC", "PRGT") and len(f) >= 3 and f[2]:
                state.update_rip_progress(step=f[2])
            elif kind == "MSG":
                msg = f[3] if len(f) >= 4 else " ".join(f)
                if msg:
                    db.log("debug", msg)
                    if any(w in msg.lower() for w in ("fail", "error", "can't", "cannot")):
                        last_error = msg

        while True:
            if state.cancel_requested():
                handle.stop()
                return "cancelled"
            try:
                _handle_line(handle.lines.get(timeout=0.5))
                continue
            except queue.Empty:
                pass
            if handle.process.poll() is not None:
                for t in handle._threads:
                    t.join(timeout=5)
                while True:
                    try:
                        _handle_line(handle.lines.get_nowait())
                    except queue.Empty:
                        break
                break
    finally:
        handle.finish()

    exit_code = handle.process.returncode
    has_file = any(f.lower().endswith(".mkv") for f in os.listdir(temp_dir))
    if exit_code != 0 or not has_file:
        detail = f" Last MakeMKV message: {last_error}" if last_error else ""
        db.log("error", f"Ripping title {row['title_index']} failed (makemkvcon exit code "
                        f"{exit_code}).{detail} {SLEEP_BUG_HINT}")
        return "failed"
    return "ok"


def rip_rows(cfg: dict, drive_index: int, device: str, rows: list, fingerprint: str = None,
             disc_label: str = "") -> bool:
    """`rows` must already be validated and ordered (detection.validate_rip_rows).
    Caller must have taken the "ripping" status via state.try_begin. Returns
    True if every title ripped, False if any failed or the run was cancelled.
    """
    state.clear_cancel()
    state.set_drive_busy(drive_index, True)
    max_episode_seen = None
    failures = 0

    try:
        prepare_temp_root()
        for i, row in enumerate(rows):
            if state.cancel_requested():
                db.log("warn", "Rip cancelled by user.")
                return False

            state.set_rip_progress({
                "drive_index": drive_index,
                "index": i,
                "total": len(rows),
                "title_index": row["title_index"],
                "season": naming.season_for(row, cfg["season"]),
                "episode": row["episode"],
                "special": row["special"],
                "percent": 0,
                "step": "",
            })

            filename = naming.build_filename(cfg["show_name"], cfg["season"], row)
            ok, why = _enough_space(row, cfg)
            if not ok:
                db.log("error", f"Skipping title {row['title_index']} ({filename}): {why}.")
                failures += 1
                continue

            db.log("info", f"Starting rip: title {row['title_index']} -> {filename} "
                           f"({i + 1}/{len(rows)})")
            temp_dir = os.path.join(TEMP_ROOT, f"drive{drive_index}_title{row['title_index']}")
            shutil.rmtree(temp_dir, ignore_errors=True)
            makedirs_owned(temp_dir)

            try:
                result = _rip_one(cfg, drive_index, row, temp_dir)
                if result == "cancelled":
                    db.log("warn", "Rip cancelled by user; stopped the current title.")
                    return False
                if result != "ok":
                    failures += 1
                    continue

                src = max((os.path.join(temp_dir, f) for f in os.listdir(temp_dir)
                           if f.lower().endswith(".mkv")), key=os.path.getsize)
                dest_dir = naming.build_dest_dir(cfg["output_dir"], cfg["show_name"],
                                                 cfg["season"], row, cfg.get("folder_layout", "show_season"),
                                                 bool(cfg.get("pad_season_folder")))
                makedirs_owned(dest_dir)
                dest = naming.unique_dest_path(dest_dir, filename)
                shutil.move(src, dest)
                _own(dest, False)
                db.log("info", f"Saved: {dest}")
                if not row["special"]:
                    max_episode_seen = row["episode"] if max_episode_seen is None else max(max_episode_seen, row["episode"])
            finally:
                shutil.rmtree(temp_dir, ignore_errors=True)

        if max_episode_seen is not None and max_episode_seen + 1 > db.get_config()["next_episode"]:
            db.update_config({"next_episode": max_episode_seen + 1})

        if failures:
            db.log("warn", f"Disc finished with {failures} failed title(s) - see the errors above.")
        else:
            db.log("info", "Disc complete.")
            if fingerprint:
                db.mark_ripped(fingerprint, disc_label)
            if cfg.get("auto_eject") and device:
                if makemkv.eject(device):
                    db.log("info", f"Ejected {device} - ready for the next disc.")
        return failures == 0
    finally:
        state.set_rip_progress(None)
        state.set_drive_busy(drive_index, False)
        state.set_status("idle")
