//! Plex/Jellyfin-style output paths. With the default "show_season" layout:
//!
//! ```text
//! <output>/Show Name/Season 1/Show Name - S01E01.mkv
//! <output>/Show Name/Season 1/Show Name - S01E02 - Episode Title.mkv
//! <output>/Show Name/Season 0/Show Name - S00E01.mkv        (Special)
//! ```
//!
//! The season folder can also be zero-padded ("Season 01"). With "show"
//! everything goes into the show folder, with "flat" straight into <output>.

use crate::detection::Row;
use crate::robot::safe_filename;
use std::path::{Path, PathBuf};

pub fn season_for(row: &Row, season: i64) -> i64 {
    if row.special {
        0
    } else {
        season
    }
}

pub fn build_filename(show: &str, season: i64, row: &Row) -> String {
    let mut base = format!(
        "{} - S{:02}E{:02}",
        safe_filename(show),
        season_for(row, season),
        row.episode.unwrap_or(0)
    );
    let title = safe_filename(row.episode_title.trim());
    if !title.is_empty() {
        base.push_str(" - ");
        base.push_str(&title);
    }
    base + ".mkv"
}

pub fn build_dest_dir(output: &Path, show: &str, season: i64, row: &Row, layout: &str, pad: bool) -> PathBuf {
    if layout == "flat" {
        return output.to_path_buf();
    }
    let show_dir = output.join(safe_filename(show));
    if layout == "show" {
        return show_dir;
    }
    let n = season_for(row, season);
    show_dir.join(if pad { format!("Season {n:02}") } else { format!("Season {n}") })
}

/// `name.mkv`, or `name (1).mkv`, `name (2).mkv`... if it already exists.
/// An existing episode is never overwritten.
pub fn unique_dest_path(dir: &Path, filename: &str) -> PathBuf {
    let (base, ext) = match filename.rfind('.') {
        Some(i) => (&filename[..i], &filename[i..]),
        None => (filename, ""),
    };
    let mut dest = dir.join(filename);
    let mut n = 1;
    while dest.exists() {
        dest = dir.join(format!("{base} ({n}){ext}"));
        n += 1;
    }
    dest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ep: i64, special: bool, title: &str) -> Row {
        Row { episode: Some(ep), special, episode_title: title.into(), ..Default::default() }
    }

    #[test]
    fn show_season_layout() {
        let r = row(3, false, "Pilot?");
        let o = Path::new("/output");
        assert_eq!(build_dest_dir(o, "Kare Kano", 1, &r, "show_season", false), o.join("Kare Kano/Season 1"));
        assert_eq!(build_dest_dir(o, "Kare Kano", 1, &r, "show_season", true), o.join("Kare Kano/Season 01"));
        assert_eq!(build_filename("Kare Kano", 1, &r), "Kare Kano - S01E03 - Pilot.mkv");
    }

    #[test]
    fn special_goes_to_season_0() {
        let r = row(1, true, "");
        let o = Path::new("/o");
        assert_eq!(build_dest_dir(o, "Show", 2, &r, "show_season", false), o.join("Show/Season 0"));
        assert_eq!(build_dest_dir(o, "Show", 2, &r, "show", false), o.join("Show"));
        assert_eq!(build_dest_dir(o, "Show", 2, &r, "flat", false), o.to_path_buf());
        assert_eq!(build_filename("Show", 2, &r), "Show - S00E01.mkv");
    }

    #[test]
    fn unique_paths() {
        let dir = std::env::temp_dir().join(format!("sr-naming-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mkv"), b"x").unwrap();
        assert_eq!(unique_dest_path(&dir, "a.mkv"), dir.join("a (1).mkv"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
