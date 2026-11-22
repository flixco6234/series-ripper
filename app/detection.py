"""Episode-detection heuristics. Every heuristic is independently toggleable
via config so a disc that confuses one of them can still be handled by
turning it off.

Pipeline: classify_titles() -> assign_episode_numbers() -> check_name_conflict()
(-> review_reason() in Automatic Mode). The web UI and the automatic-mode
watcher drive the same pipeline; only what happens with an ambiguous result
differs (a banner for a human at the UI vs. parking the disc in the review
queue when nobody's watching).
"""

import hashlib

from . import makemkv

# Flags (machine-readable twins of the human-readable warnings).
SHORT, LONG, OUTLIER, PLAY_ALL, HINT = "short", "long", "outlier", "play_all", "hint"


def _fmt_titles(rows):
    idx = [r["title_index"] for r in rows]
    return f"{idx[0]}-{idx[-1]}" if len(idx) > 2 else " + ".join(str(i) for i in idx)


def _find_play_all(rows):
    """A "Play All" title plays several episodes back to back, so its length
    equals a run of consecutive other titles added up. Each title's length is
    rounded to the second, so allow one second of slack per summed title.
    Checked longest-first; a title found to be a Play All is not used as a
    part of another one."""
    marked = set()
    included = [r for r in rows if r["include"]]
    for r in sorted(included, key=lambda x: -x["duration_sec"]):
        if r["duration_sec"] <= 0:
            continue
        others = [o for o in included
                  if o is not r and id(o) not in marked and o["duration_sec"] > 0]
        found = None
        for i in range(len(others)):
            total = 0
            for j in range(i, len(others)):
                total += others[j]["duration_sec"]
                count = j - i + 1
                slack = count + 1
                if count >= 2 and abs(total - r["duration_sec"]) <= slack:
                    found = others[i:j + 1]
                    break
                if total > r["duration_sec"] + slack:
                    break
            if found:
                break
        if found:
            marked.add(id(r))
            r["include"] = False
            r["flags"].append(PLAY_ALL)
            r["warnings"].append(
                f'Looks like a "Play All" title: as long as titles {_fmt_titles(found)} '
                f"combined. Unticked; tick it again if it really is one long episode."
            )


def classify_titles(titles: list, cfg: dict):
    """Length filter, Play All detection, long-title warning, disc-name hints
    and duration-cluster outliers. Returns (rows, notes): `rows` is one
    JSON-serializable dict per disc title, `notes` are strings worth logging.
    """
    rows = []
    for t in titles:
        rows.append({
            "title_index": t["title_index"],
            "duration_text": t["duration_text"],
            "duration_sec": t["duration_sec"],
            "chapters": t.get("chapters", 0),
            "size_bytes": t.get("size_bytes", 0),
            "source_file": t.get("source_file", ""),
            "include": True,
            "special": False,
            "episode": None,
            "episode_title": "",
            "warnings": [],
            "flags": [],
            "hint": None,
        })

    for row in rows:
        if cfg["detect_length_filter"] and row["duration_sec"] < cfg["min_length_sec"]:
            row["include"] = False
            row["special"] = True
            row["flags"].append(SHORT)
            row["warnings"].append(
                "Short - looks like a menu/extra, not an episode. Marked "
                "Special; uncheck Special if this IS a real (short) episode."
            )

    if cfg.get("detect_play_all", True):
        _find_play_all(rows)

    for row in rows:
        if cfg["detect_long_warn"] and row["include"] and row["duration_sec"] > cfg["long_warn_sec"]:
            row["flags"].append(LONG)
            row["warnings"].append(
                'Over 1h - this may be several episodes combined into one '
                'title ("Play All"). Check the disc menu; you may need to split it.'
            )

    if cfg["detect_names"]:
        by_index = {t["title_index"]: t for t in titles}
        hints = {r["title_index"]: makemkv.find_episode_hint(by_index[r["title_index"]]["text_values"])
                 for r in rows}
        # The same name on several titles is a disc label, not an episode name.
        sources = [h["source"] for h in hints.values() if h]
        for row in rows:
            hint = hints[row["title_index"]]
            if hint and sources.count(hint["source"]) == 1:
                row["hint"] = hint
                row["flags"].append(HINT)
                row["warnings"].append(
                    f"Disc metadata suggests: Episode {hint['episode']} "
                    f"(from '{hint['source']}', {hint['confidence']} confidence)"
                )

    notes = []
    if cfg["detect_cluster"]:
        candidates = [r for r in rows if r["include"]]
        if len(candidates) >= 2:
            buckets = {}
            for r in candidates:
                buckets.setdefault(round(r["duration_sec"] / 120), []).append(r)
            top_bucket = buckets[max(buckets, key=lambda k: len(buckets[k]))]
            if len(top_bucket) >= 2:
                typical_sec = sum(r["duration_sec"] for r in top_bucket) / len(top_bucket)
                tolerance = cfg.get("cluster_tolerance", 0.25)
                if typical_sec > 0:
                    for r in candidates:
                        if abs(r["duration_sec"] - typical_sec) / typical_sec > tolerance:
                            r["flags"].append(OUTLIER)
                            r["warnings"].append(
                                f"Duration is unusually different from this disc's "
                                f"typical episode length (~{round(typical_sec / 60)}m) - "
                                f"double-check this is really an episode."
                            )
            else:
                notes.append(
                    "Couldn't detect a typical episode length automatically "
                    "(durations too varied) - relying on the length threshold only."
                )

    return rows, notes


def assign_episode_numbers(rows: list, next_episode: int):
    """Keeps Episode # sequential and gap-free among included, non-Special
    rows (in disc order), starting from `next_episode`. Special rows are
    numbered independently and default to 1 if not already set. Excluded
    rows always get a blank episode number."""
    ep = next_episode
    for row in rows:
        if row["include"] and not row["special"]:
            row["episode"] = ep
            ep += 1
        elif row["include"] and row["special"]:
            if not row["episode"]:
                row["episode"] = 1
        else:
            row["episode"] = None
    return rows


def check_name_conflict(rows: list):
    """Does the disc-name detector disagree with (or only partly cover) the
    default disc-order numbering? None if there's nothing to ask about."""
    candidate_idx = [i for i, r in enumerate(rows) if r["include"]]
    hinted_idx = [i for i in candidate_idx if rows[i]["hint"]]
    if not hinted_idx:
        return None

    conflict = any(rows[i]["episode"] != rows[i]["hint"]["episode"] for i in hinted_idx)
    partial = len(hinted_idx) < len(candidate_idx)
    if not (conflict or partial):
        return None

    return {
        "conflict": conflict,
        "partial": partial,
        "hinted_indices": hinted_idx,
        "candidate_count": len(candidate_idx),
        "hinted_count": len(hinted_idx),
        "examples": [
            {
                "title_index": rows[i]["title_index"],
                "source": rows[i]["hint"]["source"],
                "episode": rows[i]["hint"]["episode"],
                "confidence": rows[i]["hint"]["confidence"],
            }
            for i in hinted_idx[:5]
        ],
    }


def review_reason(rows: list, conflict):
    """Why an unattended rip should stop and ask, or None if it's safe to go
    ahead. A wrong guess here costs a mis-numbered season, so anything the
    heuristics flagged on a title that would be ripped counts."""
    reasons = []
    if conflict:
        if conflict["conflict"]:
            reasons.append("detected episode names disagree with disc-order numbering")
        if conflict["partial"]:
            reasons.append(f"only {conflict['hinted_count']} of {conflict['candidate_count']} "
                           f"titles had a detected name")
    long_rows = [r["title_index"] for r in rows if r["include"] and LONG in r["flags"]]
    if long_rows:
        reasons.append(f"title(s) {', '.join(map(str, long_rows))} are over the long-title limit")
    odd_rows = [r["title_index"] for r in rows if r["include"] and OUTLIER in r["flags"]]
    if odd_rows:
        reasons.append(f"title(s) {', '.join(map(str, odd_rows))} don't match the typical episode length")
    return "; ".join(reasons) or None


def apply_name_hints(rows: list, hinted_indices: list):
    for i in hinted_indices:
        rows[i]["episode"] = rows[i]["hint"]["episode"]
    return rows


def disc_fingerprint(disc: dict) -> str:
    """Identifies a physical disc by its label plus the length and size of
    every title, so the same disc reinserted (or still in the drive after a
    container restart) isn't ripped twice, while different discs that share a
    generic label ("DISC1") still differ."""
    parts = [disc.get("disc_name", "")]
    parts += [f"{t['title_index']}:{t['duration_sec']}:{t.get('size_bytes', 0)}" for t in disc["titles"]]
    return hashlib.sha1("|".join(parts).encode("utf-8")).hexdigest()


def validate_rip_rows(rows: list):
    """Checked, ready-to-rip rows only, with duplicate-episode validation per
    group (regular vs. Special): S01E01 and S00E01 can coexist, two S01E01s
    can't. Raises ValueError with a human-readable message."""
    seen = set()
    picked = []
    for r in rows:
        if not r.get("include"):
            continue
        ep = r.get("episode")
        if not isinstance(ep, int) or isinstance(ep, bool) or ep <= 0:
            raise ValueError(f"Title {r['title_index']} has an invalid episode number: {ep!r}")
        key = (bool(r.get("special")), ep)
        if key in seen:
            what = f"Special episode {ep}" if key[0] else f"Episode {ep}"
            raise ValueError(f"{what} is used more than once. Fix duplicates first.")
        seen.add(key)
        picked.append(r)
    if not picked:
        raise ValueError("No titles selected to rip.")
    picked.sort(key=lambda r: (bool(r.get("special")), r["episode"]))
    return picked
