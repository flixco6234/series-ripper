"""Applies the MakeMKV license key from the MAKEMKV_KEY environment variable.

    MAKEMKV_KEY=BETA   (default, also when unset) fetch the free beta key that MakeMKV's author
                       posts on the forum, and re-check daily - the key rotates
                       roughly monthly, so a long-running container would
                       otherwise stop working at the end of the month.
    MAKEMKV_KEY=T-...  a purchased (or manually copied) key, written as-is.
    MAKEMKV_KEY=       leave MakeMKV's settings alone (trial / manual setup).
"""

import os
import re
import threading
import time
import urllib.request

from . import db

BETA_KEY_URL = "https://forum.makemkv.com/forum/viewtopic.php?t=1053"
REFRESH_SEC = 24 * 3600

_KEY_RE = re.compile(r'<code>\s*(T-[A-Za-z0-9_@-]{20,})\s*</code>')
_VALID_RE = re.compile(r'valid until ([^<\n.]{3,40})', re.IGNORECASE)
_SETTING_RE = re.compile(r'^\s*app_Key\s*=')

status = {"mode": "", "detail": "", "checked_ts": None}


def settings_path() -> str:
    return os.path.join(os.path.expanduser("~"), ".MakeMKV", "settings.conf")


def extract_beta_key(page_html: str):
    """Returns (key, valid_until_text) from the forum page, or (None, None)."""
    m = _KEY_RE.search(page_html)
    if not m:
        return None, None
    v = _VALID_RE.search(page_html)
    return m.group(1), (v.group(1).strip() if v else None)


def current_key(path: str = None):
    path = path or settings_path()
    try:
        with open(path, encoding="utf-8") as fh:
            for line in fh:
                if _SETTING_RE.match(line):
                    return line.split("=", 1)[1].strip().strip('"')
    except OSError:
        pass
    return None


def write_key(key: str, path: str = None):
    """Sets app_Key in MakeMKV's settings.conf, keeping every other setting."""
    path = path or settings_path()
    os.makedirs(os.path.dirname(path), exist_ok=True)
    lines = []
    try:
        with open(path, encoding="utf-8") as fh:
            lines = [l for l in fh.read().splitlines() if not _SETTING_RE.match(l)]
    except OSError:
        pass
    lines.append(f'app_Key = "{key}"')
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        fh.write("\n".join(lines) + "\n")
    os.replace(tmp, path)


def fetch_beta_key(url: str = BETA_KEY_URL):
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 (series-ripper)"})
    with urllib.request.urlopen(req, timeout=30) as resp:
        page = resp.read().decode("utf-8", errors="replace")
    return extract_beta_key(page)


def apply_once():
    raw = os.environ.get("MAKEMKV_KEY")
    mode = "BETA" if raw is None else raw.strip()
    status["checked_ts"] = time.time()
    if not mode:
        status["mode"] = "unset"
        status["detail"] = ("MAKEMKV_KEY is empty - using whatever MakeMKV already has "
                            "(trial if nothing). Set it to BETA or your key.")
        db.log("warn", status["detail"])
        return
    if mode.upper() != "BETA":
        status["mode"] = "custom"
        if current_key() != mode:
            write_key(mode)
            db.log("info", "MakeMKV key set from MAKEMKV_KEY.")
        status["detail"] = "Using the key from MAKEMKV_KEY."
        return

    status["mode"] = "beta"
    try:
        key, valid_until = fetch_beta_key()
    except Exception as e:
        key, valid_until = None, None
        err = str(e)
    else:
        err = "no key found on the page (forum layout may have changed)"
    if not key:
        have = current_key()
        status["detail"] = (f"Couldn't fetch the beta key ({err}). "
                            + ("Keeping the previously saved key." if have else
                               "No key saved yet - MakeMKV runs as a trial."))
        db.log("warn", status["detail"])
        return
    suffix = f" (valid until {valid_until})" if valid_until else ""
    if current_key() != key:
        write_key(key)
        db.log("info", f"Installed the current MakeMKV beta key{suffix}.")
    status["detail"] = f"Free beta key from the MakeMKV forum{suffix}."


def start():
    def _loop():
        while True:
            try:
                apply_once()
            except Exception as e:
                db.log("error", f"License check failed: {e}")
            if status["mode"] != "beta":
                return
            time.sleep(REFRESH_SEC)

    threading.Thread(target=_loop, daemon=True, name="license").start()
