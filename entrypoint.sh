#!/usr/bin/env bash
set -euo pipefail

mkdir -p /config/makemkv /output

# Keep MakeMKV's own data dir (license key, settings, downloaded LibreDrive
# data) in /config so it survives container updates.
if [ ! -L /root/.MakeMKV ]; then
    if [ -d /root/.MakeMKV ]; then
        cp -an /root/.MakeMKV/. /config/makemkv/ 2>/dev/null || true
        rm -rf /root/.MakeMKV
    fi
    ln -s /config/makemkv /root/.MakeMKV
fi

echo "[entrypoint] $(makemkvcon -r info disc:9999 2>/dev/null | head -1 | cut -d'"' -f2)"

# MakeMKV on Linux finds drives through the SCSI generic device (/dev/sgN);
# /dev/srN alone is not enough. Warn early instead of failing silently.
if ! ls /dev/sg* >/dev/null 2>&1; then
    echo "[entrypoint] WARNING: no /dev/sg* device passed to the container. MakeMKV will not" >&2
    echo "[entrypoint] see the drive. On the host run 'lsscsi -g' and pass the optical drive's" >&2
    echo "[entrypoint] /dev/sgN (and /dev/srN) with --device." >&2
else
    echo "[entrypoint] devices: $(ls /dev/sr* /dev/sg* 2>/dev/null | tr '\n' ' ')"
fi

# The license key (MAKEMKV_KEY, default BETA = fetch the free beta key from
# the MakeMKV forum) is applied by the app at startup and re-checked daily.
exec python3 -m app.main
