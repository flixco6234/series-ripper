#!/usr/bin/env python3
"""Stand-in for makemkvcon used by the integration test. Replays the real
Kare Kano Vol. 5 scan (tests/fixtures) and mimics two behaviours of the
real MakeMKV 2.0.0 that the app depends on:
  - without --minlength=0, titles under 120 s are dropped before numbering
  - without --progress=-same, no PRGV/PRGC lines are printed
The disc counts as "inserted" while the file named by MOCK_DISC_FLAG exists.
"""

import os
import sys
import time

FIXTURE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fixtures", "kare_kano_vol5_info.txt")


def lines():
    with open(FIXTURE, encoding="utf-8") as fh:
        return fh.read().splitlines()


def disc_present():
    flag = os.environ.get("MOCK_DISC_FLAG")
    return not flag or os.path.exists(flag)


def drive_lines():
    out = []
    for l in lines():
        if l.startswith("DRV:0,"):
            l = l.replace('"F:"', '"/dev/sr0"')
            if not disc_present():
                l = l.replace('"KARE_KANO_VOL5"', '""')
        if l.startswith("DRV:"):
            out.append(l)
    return out


def main(argv):
    args = [a for a in argv if not a.startswith("-")]
    minlength0 = "--minlength=0" in argv
    progress = "--progress=-same" in argv
    print('MSG:1005,0,1,"MakeMKV v2.0.0 linux(x64-release) started","%1 started","MakeMKV v2.0.0 linux(x64-release)"')
    for l in drive_lines():
        print(l)
    cmd, source = args[0], args[1]
    if source == "disc:9999" or not disc_present():
        print('MSG:5010,0,0,"Failed to open disc","Failed to open disc"')
        return 0
    titles = [l for l in lines() if l.startswith(("TINFO:", "CINFO:", "TCOUNT:"))]
    durations = {}
    for l in titles:
        if l.startswith("TINFO:") and ",9,0," in l:
            idx = int(l[6:].split(",")[0])
            h, m, s = l.split('"')[1].split(":")
            durations[idx] = int(h) * 3600 + int(m) * 60 + int(s)
    kept = sorted(i for i, d in durations.items() if minlength0 or d >= 120)
    if cmd == "info":
        time.sleep(float(os.environ.get("MOCK_SCAN_DELAY", "0")))
        for l in titles:
            if l.startswith("TINFO:"):
                idx = int(l[6:].split(",")[0])
                if idx in kept:
                    print("TINFO:%d,%s" % (kept.index(idx), l[6:].split(",", 1)[1]))
            else:
                print(l)
        return 0
    if cmd == "mkv":
        title, out_dir = int(args[2]), args[3]
        if title >= len(kept):
            print('MSG:5016,0,0,"Title #%d not found","Title #%%1 not found"' % title)
            return 12
        real = kept[title]
        if progress:
            print('PRGT:5024,0,"Saving all titles to MKV files"')
            print('PRGC:5017,0,"Saving to MKV file"')
            for step in range(0, 65537, 16384):
                print("PRGV:%d,%d,65536" % (step, step))
                sys.stdout.flush()
                time.sleep(0.05)
        with open(os.path.join(out_dir, "title_t%02d.mkv" % real), "wb") as fh:
            fh.write(b"MOCKMKV title %d\n" % real)
        print('MSG:5036,260,1,"Copy complete. 1 titles saved.","Copy complete. %1 titles saved.","1"')
        return 0
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
