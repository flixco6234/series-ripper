//! Global settings (edited in the web UI, stored in the database) and the
//! environment the container was started with.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// TV library root inside the container.
    pub output_dir: String,
    pub makemkv_path: String,
    /// Extra makemkvcon options added to every call, e.g. "--cache=1024".
    pub makemkv_extra_args: String,
    pub min_length_sec: i64,
    pub long_warn_sec: i64,
    pub cluster_tolerance: f64,
    pub detect_length_filter: bool,
    pub detect_long_warn: bool,
    pub detect_cluster: bool,
    pub detect_names: bool,
    pub detect_play_all: bool,
    /// "show_season" (Show/Season 1/), "show" (Show/) or "flat".
    pub folder_layout: String,
    /// "Season 01" instead of "Season 1".
    pub pad_season_folder: bool,
    /// Automatic Mode setting given to a drive the first time it is seen.
    pub automatic_mode: bool,
    pub auto_eject: bool,
    pub poll_interval_sec: i64,
    /// Automatic Mode skips a disc that was already ripped completely.
    pub skip_ripped_discs: bool,
    /// "off", "discord", "ntfy", "gotify" or "json".
    pub notify_format: String,
    pub notify_url: String,
    pub notify_on_done: bool,
    pub notify_on_review: bool,
    pub notify_on_error: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            output_dir: "/output".into(),
            makemkv_path: String::new(),
            makemkv_extra_args: String::new(),
            min_length_sec: 900,
            long_warn_sec: 3600,
            cluster_tolerance: 0.25,
            detect_length_filter: true,
            detect_long_warn: true,
            detect_cluster: true,
            detect_names: true,
            detect_play_all: true,
            folder_layout: "show_season".into(),
            pad_season_folder: false,
            automatic_mode: false,
            auto_eject: true,
            poll_interval_sec: 5,
            skip_ripped_discs: true,
            notify_format: "off".into(),
            notify_url: String::new(),
            notify_on_done: true,
            notify_on_review: true,
            notify_on_error: true,
        }
    }
}

impl Config {
    /// Fixes values that would break the app (negative intervals, unknown
    /// layout names) instead of rejecting the whole save.
    pub fn sanitize(&mut self) {
        self.poll_interval_sec = self.poll_interval_sec.clamp(2, 3600);
        self.min_length_sec = self.min_length_sec.max(0);
        self.long_warn_sec = self.long_warn_sec.max(60);
        if !(0.01..=5.0).contains(&self.cluster_tolerance) {
            self.cluster_tolerance = 0.25;
        }
        if !["show_season", "show", "flat"].contains(&self.folder_layout.as_str()) {
            self.folder_layout = "show_season".into();
        }
        if !["off", "discord", "ntfy", "gotify", "json"].contains(&self.notify_format.as_str()) {
            self.notify_format = "off".into();
        }
        self.output_dir = self.output_dir.trim().to_string();
        if self.output_dir.is_empty() {
            self.output_dir = "/output".into();
        }
        self.notify_url = self.notify_url.trim().to_string();
    }

    pub fn extra_args(&self) -> Vec<String> {
        self.makemkv_extra_args.split_whitespace().map(str::to_string).collect()
    }
}

/// Settings that come from the container's environment variables.
#[derive(Debug, Clone)]
pub struct Env {
    pub db_path: PathBuf,
    pub port: u16,
    pub temp_root: PathBuf,
    pub puid: u32,
    pub pgid: u32,
    pub umask: u32,
    /// Detect discs with the kernel's CD-ROM ioctl (cheap) instead of
    /// running makemkvcon every few seconds. Off only for tests.
    pub use_ioctl: bool,
    /// Optional password for the web UI (HTTP basic auth, any user name).
    pub ui_password: Option<String>,
    /// MakeMKV's settings.conf (holds the license key).
    pub makemkv_settings: PathBuf,
}

fn is_mount_point(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(path), path.parent().map(std::fs::metadata)) {
        (Ok(m), Some(Ok(parent))) => m.is_dir() && m.dev() != parent.dev(),
        _ => false,
    }
}

impl Env {
    pub fn from_env() -> Env {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let temp_root = if let Some(t) = var("DVDRIPPER_TEMP") {
            PathBuf::from(t)
        } else if is_mount_point(Path::new("/temp")) {
            PathBuf::from("/temp/series-ripper")
        } else {
            PathBuf::from("/output/.series-ripper-tmp")
        };
        let home = var("HOME").unwrap_or_else(|| "/root".into());
        Env {
            db_path: PathBuf::from(var("DVDRIPPER_DB").unwrap_or_else(|| "/config/series-ripper.db".into())),
            port: var("DVDRIPPER_PORT").and_then(|p| p.parse().ok()).unwrap_or(8080),
            temp_root,
            puid: var("PUID").and_then(|v| v.parse().ok()).unwrap_or(99),
            pgid: var("PGID").and_then(|v| v.parse().ok()).unwrap_or(100),
            umask: var("UMASK").and_then(|v| u32::from_str_radix(&v, 8).ok()).unwrap_or(0),
            use_ioctl: var("DVDRIPPER_NO_IOCTL").is_none(),
            ui_password: var("UI_PASSWORD"),
            makemkv_settings: PathBuf::from(home).join(".MakeMKV").join("settings.conf"),
        }
    }
}
