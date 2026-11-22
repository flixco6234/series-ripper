"""Talks to makemkvcon in "robot" (-r) mode. Robot-mode output is a stream
of lines like

    DRV:index,visible,enabled,flags,"drive name","disc name","device path"
    CINFO:attr_id,code,"value"
    TINFO:title_index,attr_id,code,"value"
    MSG:code,flags,count,"message","format",params...
    PRGV:current,total,max
    PRGT/PRGC:code,id,"name"

Attribute ids used here were checked against real MakeMKV 2.0.0 output
(see tests/fixtures): CINFO 1 = disc type, 2 = disc name; TINFO 2 = title
name, 8 = chapter count, 9 = duration "H:MM:SS", 11 = size in bytes,
16 = source file (e.g. 00800.mpls on Blu-ray).
"""

import os
import re
import shutil
import subprocess
import threading
import queue
from dataclasses import dataclass, field

# MakeMKV doesn't like two makemkvcon processes touching drives at once (a
# drive-list poll during a rip can disturb the rip), so every call goes
# through this lock, and the drive list is served from cache while a scan or
# rip holds it.
_mkv_lock = threading.Lock()
_drive_cache = []

SCAN_TIMEOUT_SEC = 1800      # UHD/Blu-ray scans can take many minutes
DRIVES_TIMEOUT_SEC = 120


def split_robot_fields(text: str) -> list:
    fields = []
    i = 0
    n = len(text)
    while i < n:
        if text[i] == '"':
            i += 1
            buf = []
            while i < n and text[i] != '"':
                if text[i] == '\\' and i + 1 < n:
                    buf.append(text[i + 1])
                    i += 2
                else:
                    buf.append(text[i])
                    i += 1
            i += 1  # closing quote
            fields.append("".join(buf))
            if i < n and text[i] == ',':
                i += 1
        else:
            start = i
            while i < n and text[i] != ',':
                i += 1
            fields.append(text[start:i])
            if i < n and text[i] == ',':
                i += 1
    return fields


_LINE_RE = re.compile(r'^([A-Z]+):(.*)$')


def parse_robot_line(line: str):
    m = _LINE_RE.match(line)
    if not m:
        return None
    return m.group(1), split_robot_fields(m.group(2))


def find_makemkv_exe():
    return shutil.which("makemkvcon") or shutil.which("makemkvcon64")


def _run(args: list, timeout: int) -> str:
    proc = subprocess.run(
        args, capture_output=True, timeout=timeout,
        text=True, encoding="utf-8", errors="replace",
    )
    return proc.stdout


def parse_drives(stdout: str) -> list:
    drives = []
    for line in stdout.splitlines():
        parsed = parse_robot_line(line)
        if not parsed or parsed[0] != "DRV":
            continue
        f = parsed[1]
        try:
            index = int(f[0])
        except (ValueError, IndexError):
            continue
        drive_name = f[4] if len(f) > 4 else ""
        disc_name = f[5] if len(f) > 5 else ""
        device = f[6] if len(f) > 6 else ""
        if not drive_name.strip() and not device.strip():
            continue  # MakeMKV always prints 16 slots; the unused ones are empty
        drives.append({
            "index": index,
            "drive_name": drive_name,
            "disc_name": disc_name,
            "has_disc": bool(disc_name.strip()),
            "device": device,
        })
    return sorted(drives, key=lambda d: d["index"])


def get_drives(makemkv_path: str) -> list:
    """Every optical drive MakeMKV can see, disc loaded or not, via the
    "info disc:9999" trick: that index doesn't exist, so MakeMKV lists all
    drives (DRV lines) before erroring on the bogus index. While a scan or
    rip is running the last known list is returned instead.
    """
    global _drive_cache
    if not _mkv_lock.acquire(blocking=False):
        return [dict(d) for d in _drive_cache]
    try:
        out = _run([makemkv_path, "-r", "info", "disc:9999"], DRIVES_TIMEOUT_SEC)
    finally:
        _mkv_lock.release()
    _drive_cache = parse_drives(out)
    return [dict(d) for d in _drive_cache]


_DURATION_RE = re.compile(r'^\d{1,2}:[0-5]\d:[0-5]\d$')


def _to_int(value, default=0):
    try:
        return int(value)
    except (TypeError, ValueError):
        return default


def parse_disc_info(stdout: str, on_message=None) -> dict:
    disc = {"disc_type": "", "disc_name": "", "titles": []}
    attrs = {}
    order = []
    for line in stdout.splitlines():
        parsed = parse_robot_line(line)
        if not parsed:
            continue
        kind, f = parsed
        if kind == "TINFO" and len(f) >= 4:
            t_idx = _to_int(f[0], -1)
            if t_idx < 0:
                continue
            if t_idx not in attrs:
                attrs[t_idx] = {}
                order.append(t_idx)
            attrs[t_idx].setdefault(_to_int(f[1], -1), f[-1])
        elif kind == "CINFO" and len(f) >= 3:
            attr = _to_int(f[0], -1)
            if attr == 1:
                disc["disc_type"] = f[-1]
            elif attr == 2:
                disc["disc_name"] = f[-1]
        elif kind == "MSG" and on_message:
            msg = f[3] if len(f) >= 4 else " ".join(f)
            on_message(msg)

    for t_idx in order:
        a = attrs[t_idx]
        duration_text = a.get(9, "")
        if not _DURATION_RE.match(duration_text):
            duration_text = next((v for v in a.values() if _DURATION_RE.match(v)), "")
        seconds = 0
        if duration_text:
            h, m, s = (int(x) for x in duration_text.split(":"))
            seconds = h * 3600 + m * 60 + s
        name = a.get(2, "")
        disc["titles"].append({
            "title_index": t_idx,
            "duration_text": duration_text or "0:00:00",
            "duration_sec": seconds,
            "chapters": _to_int(a.get(8)),
            "size_bytes": _to_int(a.get(11)),
            "source_file": a.get(16, ""),
            "name": name,
            # Only the real title name: other text attributes are generated by
            # MakeMKV (output file name, source codes like "E4") and produced a
            # false "Episode 4" on a real disc.
            "text_values": [name] if re.search(r'[A-Za-z]{2,}', name) else [],
        })
    disc["titles"].sort(key=lambda t: t["title_index"])
    return disc


def get_disc_info(makemkv_path: str, drive_index: int, on_message=None) -> dict:
    """Scans a disc. --minlength=0 lists every title, so the title indexes
    match the ones start_rip() uses (it passes the same flag)."""
    with _mkv_lock:
        out = _run([makemkv_path, "-r", "--minlength=0", "info", f"disc:{drive_index}"],
                   SCAN_TIMEOUT_SEC)
    return parse_disc_info(out, on_message)


_HINT_PATTERNS = [
    (re.compile(r'(?i)\bepisode\s*0*(\d{1,3})\b'), "High"),
    (re.compile(r'(?i)\bep\.?\s*0*(\d{1,3})\b'), "Medium"),
    (re.compile(r'(?i)\bS\d{1,2}E0*(\d{1,3})\b'), "Medium"),
    (re.compile(r'(?i)\bE0*(\d{1,3})\b'), "Low"),
    (re.compile(r'^\s*0*(\d{1,3})\s*[-._]\s*[A-Za-z]'), "Low"),
]


def find_episode_hint(text_values: list):
    """Looks for something that reads like an episode marker in a title's
    name. Tried from most to least confidence-inspiring; returns
    {"episode": int, "source": str, "confidence": str} or None.
    """
    for tier in ("High", "Medium", "Low"):
        for pattern, conf in _HINT_PATTERNS:
            if conf != tier:
                continue
            for val in text_values:
                m = pattern.search(val)
                if m:
                    return {"episode": int(m.group(1)), "source": val, "confidence": tier}
    return None


_INVALID_FS_CHARS = re.compile(r'[<>:"/\\|?*\x00-\x1f]')


def safe_filename(name: str) -> str:
    cleaned = _INVALID_FS_CHARS.sub(" ", name)
    cleaned = re.sub(r'\s+', ' ', cleaned).strip().rstrip(".")
    return cleaned


@dataclass
class RipHandle:
    """A running makemkvcon rip. `lines` receives every stdout line as it
    arrives (from a background reader thread); poll `process.poll()` to
    detect completion and call finish() afterwards."""
    process: subprocess.Popen
    lines: "queue.Queue[str]" = field(default_factory=queue.Queue)
    _threads: list = field(default_factory=list)
    _finished: bool = False

    def stop(self):
        if self.process.poll() is None:
            try:
                self.process.terminate()
                self.process.wait(timeout=20)
            except (OSError, subprocess.TimeoutExpired):
                self.process.kill()
        self.finish()

    def finish(self):
        if self._finished:
            return
        self._finished = True
        for t in self._threads:
            t.join(timeout=5)
        _mkv_lock.release()


def parse_progress(fields: list):
    """PRGV:current,total,max -> overall percent of the running task."""
    try:
        total, mx = float(fields[1]), float(fields[2])
    except (ValueError, IndexError):
        return None
    if mx <= 0:
        return None
    return max(0, min(100, int(total / mx * 100)))


def start_rip(makemkv_path: str, drive_index: int, title_index: int, out_dir: str) -> RipHandle:
    """--minlength=0: without it MakeMKV drops titles under its own minimum
    length before numbering them, so the scan's title index would point at a
    different title. --progress=-same: PRGV/PRGC lines are only emitted on
    request."""
    os.makedirs(out_dir, exist_ok=True)
    _mkv_lock.acquire()
    try:
        proc = subprocess.Popen(
            [makemkv_path, "-r", "--progress=-same", "--minlength=0",
             "mkv", f"disc:{drive_index}", str(title_index), out_dir],
            stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, encoding="utf-8", errors="replace", bufsize=1,
        )
    except Exception:
        _mkv_lock.release()
        raise
    handle = RipHandle(process=proc)

    def _reader():
        try:
            for line in proc.stdout:
                handle.lines.put(line.rstrip("\n"))
        except (ValueError, OSError):
            pass

    t = threading.Thread(target=_reader, daemon=True)
    t.start()
    handle._threads.append(t)
    return handle


def eject(device: str) -> bool:
    """Best-effort tray eject after an unattended rip, so you know the next
    disc can go in."""
    eject_bin = shutil.which("eject")
    if not eject_bin or not device:
        return False
    try:
        r = subprocess.run([eject_bin, device], capture_output=True, timeout=30)
        return r.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False
