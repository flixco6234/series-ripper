"""Builds Plex/Jellyfin-style output paths. With the default "show_season"
layout:
    <output>/Show Name/Season 1/Show Name - S01E01.mkv
    <output>/Show Name/Season 1/Show Name - S01E02 - Episode Title.mkv
    <output>/Show Name/Season 0/Show Name - S00E01.mkv        (Special)
The season folder can also be zero-padded ("Season 01"); both are
understood by Plex and Jellyfin. With "show" everything goes into the show
folder, with "flat" straight into <output>.
"""

import os
from . import makemkv


def season_for(row: dict, season: int) -> int:
    return 0 if row.get("special") else season


def build_filename(show_name: str, season: int, row: dict) -> str:
    base = f"{makemkv.safe_filename(show_name)} - S{season_for(row, season):02d}E{row['episode']:02d}"
    ep_title = (row.get("episode_title") or "").strip()
    if ep_title:
        base += f" - {makemkv.safe_filename(ep_title)}"
    return base + ".mkv"


def build_dest_dir(output_dir: str, show_name: str, season: int, row: dict, layout: str,
                   pad_season: bool = False) -> str:
    if layout == "flat":
        return output_dir
    show_dir = os.path.join(output_dir, makemkv.safe_filename(show_name))
    if layout == "show":
        return show_dir
    n = season_for(row, season)
    return os.path.join(show_dir, f"Season {n:02d}" if pad_season else f"Season {n}")


def unique_dest_path(dest_dir: str, filename: str) -> str:
    base, ext = os.path.splitext(filename)
    dest = os.path.join(dest_dir, filename)
    n = 1
    while os.path.exists(dest):
        dest = os.path.join(dest_dir, f"{base} ({n}){ext}")
        n += 1
    return dest
