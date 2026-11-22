"""Stdlib-only HTTP server: JSON API + a static single-page web UI. No
third-party dependencies (no Flask/FastAPI) so the image stays small and the
build doesn't depend on PyPI being reachable.
"""

import json
import os
import queue
import re
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse, parse_qs

from . import db, state, makemkv, detection, ripper, watcher, license

STATIC_DIR = os.path.join(os.path.dirname(__file__), "static")
MAKEMKV_VERSION = ""

ROUTES = []


def route(method, pattern):
    regex = re.compile("^" + pattern + "$")

    def deco(fn):
        ROUTES.append((method, regex, fn))
        return fn
    return deco


def _json(obj, status=200):
    return status, "application/json", json.dumps(obj).encode("utf-8")


def _find_drive(cfg, drive_index):
    try:
        for d in makemkv.get_drives(cfg["makemkv_path"]):
            if d["index"] == drive_index:
                return d
    except Exception:
        pass
    return {}


def _run_rip_in_background(cfg, drive_index, picked, on_done=None, fingerprint=None):
    drive = _find_drive(cfg, drive_index)

    def _bg():
        try:
            ripper.rip_rows(cfg, drive_index, drive.get("device"), picked,
                            fingerprint=fingerprint, disc_label=drive.get("disc_name", ""))
        except Exception as e:
            db.log("error", f"Rip failed: {e}")
            state.set_status("idle")
        finally:
            if on_done:
                on_done()

    threading.Thread(target=_bg, daemon=True, name="rip").start()


# ---------------------------------------------------------------- routes --

@route("GET", r"/api/config")
def api_get_config(handler, m, query, body):
    return _json(db.get_config())


@route("POST", r"/api/config")
def api_set_config(handler, m, query, body):
    patch = json.loads(body or b"{}")
    db.update_config(patch)
    db.log("info", "Settings updated.")
    return _json(db.get_config())


@route("GET", r"/api/drives")
def api_drives(handler, m, query, body):
    cfg = db.get_config()
    path = cfg.get("makemkv_path") or makemkv.find_makemkv_exe()
    if not path:
        return _json({"error": "MakeMKV path not set and makemkvcon not found on PATH"}, 400)
    try:
        drives = makemkv.get_drives(path)
    except Exception as e:
        return _json({"error": str(e)}, 500)
    busy = {d["index"]: state.is_drive_busy(d["index"]) for d in drives}
    for d in drives:
        d["busy"] = busy[d["index"]]
    return _json({"drives": drives})


def _scan_worker(path, drive_index, cfg):
    try:
        disc = makemkv.get_disc_info(path, drive_index, on_message=lambda msg: db.log("debug", msg))
        titles = disc["titles"]
        if not titles:
            state.set_scan_error("No titles found. Is there a disc in the drive?")
            db.log("warn", "No titles found. Is there a disc in the drive?")
            return
        rows, notes = detection.classify_titles(titles, cfg)
        for n in notes:
            db.log("info", n)
        detection.assign_episode_numbers(rows, cfg["next_episode"])
        conflict = detection.check_name_conflict(rows) if cfg["detect_names"] else None
        for r in rows:
            for w in r["warnings"]:
                if not w.startswith("Disc metadata"):
                    db.log("info", f"Title {r['title_index']}: {w}")
        state.set_staged_scan({
            "drive_index": drive_index,
            "disc_name": disc["disc_name"],
            "disc_type": disc["disc_type"],
            "fingerprint": detection.disc_fingerprint(disc),
            "rows": rows,
            "notes": notes,
            "conflict": conflict,
        })
        db.log("info", f"Scanned {len(titles)} title(s) on drive {drive_index} "
                       f"({disc['disc_type'] or 'disc'} '{disc['disc_name']}').")
    except Exception as e:
        state.set_scan_error(f"Scan failed: {e}")
        db.log("error", f"Scan failed: {e}")
    finally:
        state.set_status("idle")


@route("POST", r"/api/scan")
def api_scan(handler, m, query, body):
    payload = json.loads(body or b"{}")
    drive_index = int(payload.get("drive_index", 0))
    cfg = db.get_config()
    path = cfg.get("makemkv_path") or makemkv.find_makemkv_exe()
    if not path:
        return _json({"error": "MakeMKV path not configured"}, 400)
    if not state.try_begin("scanning"):
        return _json({"error": f"Busy ({state.get()['status']}) - wait until it finishes."}, 409)
    state.set_staged_scan(None)
    state.set_scan_error(None)
    db.log("info", f"Scanning drive {drive_index} (Blu-ray / UHD discs can take several minutes)...")
    threading.Thread(target=_scan_worker, args=(path, drive_index, cfg), daemon=True, name="scan").start()
    return _json({"started": True}, 202)


@route("GET", r"/api/scan")
def api_scan_result(handler, m, query, body):
    return _json({
        "running": state.get()["status"] == "scanning",
        "result": state.get_staged_scan(),
        "error": state.get_scan_error(),
    })


@route("POST", r"/api/scan/apply_names")
def api_scan_apply_names(handler, m, query, body):
    staged = state.get_staged_scan()
    if not staged:
        return _json({"error": "No staged scan"}, 400)
    conflict = detection.check_name_conflict(staged["rows"])
    if conflict:
        detection.apply_name_hints(staged["rows"], conflict["hinted_indices"])
        state.set_staged_scan(staged)
        db.log("info", "Applied episode numbers detected from disc title names.")
    return _json({"rows": staged["rows"]})


@route("POST", r"/api/rip")
def api_rip(handler, m, query, body):
    payload = json.loads(body or b"{}")
    staged = state.get_staged_scan()
    rows = payload.get("rows")
    if rows is None:
        if not staged:
            return _json({"error": "No rows supplied and no staged scan"}, 400)
        rows = staged["rows"]
    drive_index = int(payload.get("drive_index", staged["drive_index"] if staged else 0))

    cfg = db.get_config()
    if not cfg.get("show_name"):
        return _json({"error": "Set a show name first"}, 400)
    try:
        picked = detection.validate_rip_rows(rows)
    except ValueError as e:
        return _json({"error": str(e)}, 400)
    if not state.try_begin("ripping"):
        return _json({"error": f"Busy ({state.get()['status']}) - wait until it finishes."}, 409)

    fingerprint = staged.get("fingerprint") if staged and staged["drive_index"] == drive_index else None
    _run_rip_in_background(cfg, drive_index, picked,
                           on_done=lambda: state.set_staged_scan(None), fingerprint=fingerprint)
    return _json({"started": True, "count": len(picked)}, 202)


@route("POST", r"/api/cancel")
def api_cancel(handler, m, query, body):
    state.request_cancel()
    db.log("info", "Cancel requested.")
    return _json({"ok": True})


@route("GET", r"/api/status")
def api_status(handler, m, query, body):
    st = state.get()
    return _json({
        "status": st["status"],
        "rip_progress": state.get_rip_progress(),
        "automatic_mode": db.get_config().get("automatic_mode", False),
        "makemkv_version": MAKEMKV_VERSION,
        "license": license.status,
        "temp_dir": ripper.TEMP_ROOT,
    })


@route("GET", r"/api/review")
def api_review_list(handler, m, query, body):
    return _json({"items": db.list_review_items()})


@route("POST", r"/api/review/(\d+)/resolve")
def api_review_resolve(handler, m, query, body):
    item_id = int(m.group(1))
    item = db.get_review_item(item_id)
    if not item or item["status"] != "pending":
        return _json({"error": "Not found or already resolved"}, 404)
    payload = json.loads(body or b"{}")
    rows = payload.get("rows", item["rows"])

    cfg = db.get_config()
    if not cfg.get("show_name"):
        return _json({"error": "Set a show name first"}, 400)
    try:
        picked = detection.validate_rip_rows(rows)
    except ValueError as e:
        return _json({"error": str(e)}, 400)
    if not state.try_begin("ripping"):
        return _json({"error": f"Busy ({state.get()['status']}) - wait until it finishes."}, 409)

    db.set_review_status(item_id, "ripping")
    db.log("info", f"Resolving review item #{item_id}: ripping {len(picked)} title(s). "
                   f"Make sure disc '{item['disc_label']}' is still in drive {item['drive_index']}.")
    _run_rip_in_background(cfg, item["drive_index"], picked,
                           on_done=lambda: db.set_review_status(item_id, "done"),
                           fingerprint=item.get("fingerprint"))
    return _json({"started": True}, 202)


@route("POST", r"/api/review/(\d+)/dismiss")
def api_review_dismiss(handler, m, query, body):
    item_id = int(m.group(1))
    db.set_review_status(item_id, "dismissed")
    db.log("info", f"Review item #{item_id} dismissed without ripping.")
    return _json({"ok": True})


@route("GET", r"/api/logs")
def api_logs(handler, m, query, body):
    since_id = int(query.get("since_id", ["0"])[0])
    limit = int(query.get("limit", ["200"])[0])
    before_raw = query.get("before_id", [None])[0]
    before_id = int(before_raw) if before_raw is not None else None
    return _json({"entries": db.get_log(since_id, limit, before_id)})


# ------------------------------------------------------------------ http --

class Handler(BaseHTTPRequestHandler):
    server_version = "SeriesRipper/2.0"

    def log_message(self, fmt, *args):
        pass  # app-level activity goes through db.log; keep container logs quiet

    def _send(self, status, content_type, body):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _read_body(self):
        length = int(self.headers.get("Content-Length", 0) or 0)
        return self.rfile.read(length) if length else b""

    def _serve_static(self, rel_path):
        rel_path = rel_path or "index.html"
        full = os.path.normpath(os.path.join(STATIC_DIR, rel_path))
        if not full.startswith(os.path.normpath(STATIC_DIR) + os.sep) and full != os.path.normpath(STATIC_DIR):
            self._send(403, "application/json", b'{"error":"forbidden"}')
            return
        if not os.path.isfile(full):
            self._send(404, "application/json", b'{"error":"not found"}')
            return
        ctype = "text/html; charset=utf-8"
        if full.endswith(".js"):
            ctype = "application/javascript; charset=utf-8"
        elif full.endswith(".css"):
            ctype = "text/css; charset=utf-8"
        with open(full, "rb") as fh:
            data = fh.read()
        self._send(200, ctype, data)

    def _stream_logs(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        q = db.subscribe_log()
        try:
            # No history replay here on purpose: the client loads its initial
            # batch via GET /api/logs once, and this stream only ever carries
            # entries created after the client subscribed - replaying recent
            # history here too would double up whatever the client already
            # fetched.
            while True:
                try:
                    entry = q.get(timeout=15)
                    self.wfile.write(f"data: {json.dumps(entry)}\n\n".encode())
                except queue.Empty:
                    self.wfile.write(b": heartbeat\n\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            db.unsubscribe_log(q)

    def _dispatch(self, method):
        parsed = urlparse(self.path)
        path = parsed.path
        query = parse_qs(parsed.query)

        if method == "GET" and path == "/api/logs/stream":
            self._stream_logs()
            return
        if method == "GET" and path in ("/", "/index.html"):
            self._serve_static("index.html")
            return
        if method == "GET" and path.startswith("/static/"):
            self._serve_static(path[len("/static/"):])
            return

        body = self._read_body() if method in ("POST", "PUT") else b""
        for m_method, regex, fn in ROUTES:
            if m_method != method:
                continue
            match = regex.match(path)
            if match:
                try:
                    status, ctype, out = fn(self, match, query, body)
                except Exception as e:
                    status, ctype, out = 500, "application/json", json.dumps({"error": str(e)}).encode()
                self._send(status, ctype, out)
                return
        self._send(404, "application/json", b'{"error":"not found"}')

    def do_GET(self):
        self._dispatch("GET")

    def do_POST(self):
        self._dispatch("POST")


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    global MAKEMKV_VERSION
    os.umask(ripper.UMASK)
    db_path = os.environ.get("DVDRIPPER_DB", "/config/series-ripper.db")
    db.init(db_path)

    cfg = db.get_config()
    found = makemkv.find_makemkv_exe()
    if found and (not cfg.get("makemkv_path") or not os.path.exists(cfg["makemkv_path"])):
        db.set_config_value("makemkv_path", found)
        cfg["makemkv_path"] = found
    if cfg.get("makemkv_path"):
        try:
            first = makemkv._run([cfg["makemkv_path"], "-r", "info", "disc:9999"], 120).splitlines()[:1]
            parsed = makemkv.parse_robot_line(first[0]) if first else None
            if parsed and parsed[0] == "MSG" and len(parsed[1]) >= 6:
                MAKEMKV_VERSION = parsed[1][5]
        except Exception as e:
            db.log("error", f"Couldn't run makemkvcon: {e}")
    try:
        ripper.prepare_temp_root()
    except OSError as e:
        db.log("error", f"Couldn't create the temp folder {ripper.TEMP_ROOT}: {e}")

    license.start()
    watcher.start()

    port = int(os.environ.get("DVDRIPPER_PORT", "8080"))
    server = Server(("0.0.0.0", port), Handler)
    db.log("info", f"Series Ripper web UI listening on port {port} ({MAKEMKV_VERSION or 'MakeMKV version unknown'}).")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        watcher.stop()


if __name__ == "__main__":
    main()
