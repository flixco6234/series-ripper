//! Optical drives: discovery, per-drive state, disc detection (the watcher)
//! and scanning. Every drive works on its own: two drives can scan or rip at
//! the same time; only one operation runs per drive.

use crate::app::{lock, App};
use crate::cdrom::{self, Tray};
use crate::db::{now, DriveSettings, Show};
use crate::detection::{self, Conflict, Row};
use crate::makemkv;
use crate::notify;
use crate::ripper::{self, JobRequest};
use crate::robot::{self, Disc, DriveInfo};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Idle,
    Scanning,
    Ripping,
}

impl Activity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Activity::Idle => "idle",
            Activity::Scanning => "scanning",
            Activity::Ripping => "ripping",
        }
    }
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Progress {
    pub percent: f64,
    pub step: String,
    /// 1-based position in the rip queue of this disc.
    pub position: usize,
    pub total: usize,
    pub title_index: Option<i64>,
    pub season: i64,
    pub episode: i64,
    pub special: bool,
    pub filename: String,
    pub size_bytes: i64,
    pub bytes_per_sec: f64,
    pub eta_sec: Option<f64>,
}

/// A finished manual scan waiting for the user to check the table.
#[derive(Debug, Clone, Serialize)]
pub struct StagedScan {
    pub show_id: i64,
    pub disc_name: String,
    pub disc_type: String,
    pub fingerprint: String,
    pub rows: Vec<Row>,
    pub notes: Vec<String>,
    pub conflict: Option<Conflict>,
    pub scanned_ts: f64,
}

#[derive(Debug, Clone)]
pub struct DriveState {
    pub index: i32,
    pub drive_name: String,
    pub device: String,
    /// Listed by MakeMKV at the last drive refresh.
    pub present: bool,
    pub settings: DriveSettings,
    pub tray: Tray,
    /// Whether the CD-ROM ioctl works for this drive (None = not tried yet).
    pub ioctl_ok: Option<bool>,
    /// MakeMKV's view at the last refresh: Some(label) if a disc is in.
    pub mk_disc: Option<String>,
    pub has_disc: bool,
    seen_label: String,
    pub disc_label: String,
    pub disc_type: String,
    auto_handled: bool,
    label_fetched: bool,
    pub activity: Activity,
    pub progress: Option<Progress>,
    pub staged: Option<StagedScan>,
    pub scan_error: Option<String>,
    pub last_message: String,
}

pub struct DriveSlot {
    pub id: u32,
    /// Stable identity across restarts and USB re-plugs: MakeMKV's drive
    /// name, which includes the firmware and serial number.
    pub key: String,
    pub cancel: AtomicBool,
    st: Mutex<DriveState>,
}

fn label_of(s: &DriveState) -> String {
    let nick = s.settings.nickname.trim();
    if nick.is_empty() {
        format!("Drive {}", s.index)
    } else {
        nick.to_string()
    }
}

fn key_of(d: &DriveInfo) -> String {
    if d.drive_name.trim().is_empty() {
        d.device.clone()
    } else {
        d.drive_name.clone()
    }
}

impl DriveSlot {
    pub fn state(&self) -> MutexGuard<'_, DriveState> {
        lock(&self.st)
    }

    pub fn label(&self) -> String {
        label_of(&self.state())
    }

    /// makemkvcon source argument for this drive.
    pub fn source(&self) -> String {
        format!("disc:{}", self.state().index)
    }

    pub fn device(&self) -> String {
        self.state().device.clone()
    }

    pub fn activity(&self) -> Activity {
        self.state().activity
    }

    /// Lets Automatic Mode take another look at the disc that is in the drive.
    pub fn rearm_auto(&self) {
        self.state().auto_handled = false;
    }

    pub fn to_json(&self) -> Value {
        let s = self.state();
        json!({
            "id": self.id,
            "key": self.key,
            "index": s.index,
            "drive_name": s.drive_name,
            "device": s.device,
            "present": s.present,
            "label": label_of(&s),
            "nickname": s.settings.nickname,
            "show_id": s.settings.show_id,
            "automatic": s.settings.automatic,
            "tray": s.tray,
            "ioctl": s.ioctl_ok,
            "has_disc": s.has_disc,
            "disc_label": s.disc_label,
            "disc_type": s.disc_type,
            "activity": s.activity,
            "progress": s.progress,
            "has_staged": s.staged.is_some(),
            "scan_error": s.scan_error,
            "last_message": s.last_message,
        })
    }
}

#[derive(Default)]
pub struct DriveManager {
    slots: Mutex<Vec<Arc<DriveSlot>>>,
    next_id: AtomicU32,
    pub dirty: AtomicBool,
    enum_lock: tokio::sync::Mutex<()>,
}

impl DriveManager {
    pub fn all(&self) -> Vec<Arc<DriveSlot>> {
        lock(&self.slots).clone()
    }

    pub fn get(&self, id: u32) -> Option<Arc<DriveSlot>> {
        lock(&self.slots).iter().find(|s| s.id == id).cloned()
    }

    pub fn by_key(&self, key: &str) -> Option<Arc<DriveSlot>> {
        lock(&self.slots).iter().find(|s| s.key == key).cloned()
    }

    pub fn by_index(&self, index: i64) -> Option<Arc<DriveSlot>> {
        lock(&self.slots)
            .iter()
            .find(|s| {
                let st = s.state();
                st.present && st.index as i64 == index
            })
            .cloned()
    }

    pub fn any_busy(&self) -> bool {
        lock(&self.slots).iter().any(|s| s.activity() != Activity::Idle)
    }

    /// Atomically moves an idle drive to `activity`; false if it is busy.
    pub fn try_begin(&self, slot: &DriveSlot, activity: Activity) -> bool {
        let mut s = slot.state();
        if s.activity != Activity::Idle {
            return false;
        }
        s.activity = activity;
        s.progress = None;
        s.last_message.clear();
        slot.cancel.store(false, Ordering::SeqCst);
        self.dirty.store(true, Ordering::Relaxed);
        true
    }

    pub fn finish(&self, slot: &DriveSlot, message: Option<String>) {
        let mut s = slot.state();
        s.activity = Activity::Idle;
        s.progress = None;
        if let Some(m) = message {
            s.last_message = m;
        }
        self.dirty.store(true, Ordering::Relaxed);
    }
}

fn base_args(app: &App, first: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = first.iter().map(|s| s.to_string()).collect();
    args.extend(app.cfg().extra_args());
    args
}

/// Asks MakeMKV for its drive list ("info disc:9999": that index doesn't
/// exist, so MakeMKV lists every drive before erroring on it). Adds new
/// drives, updates indexes/devices and MakeMKV's view of each disc.
pub async fn refresh(app: &Arc<App>) -> Result<usize, String> {
    let _guard = app.drives.enum_lock.lock().await;
    let cfg = app.cfg();
    if cfg.makemkv_path.is_empty() {
        return Err("makemkvcon not found - set its path in Settings.".into());
    }
    let mut args = base_args(app, &["-r"]);
    args.extend(["info".to_string(), "disc:9999".to_string()]);
    let (out, outcome) = makemkv::run_collect(&cfg.makemkv_path, &args, makemkv::ENUM_TIMEOUT)
        .await
        .map_err(|e| format!("couldn't run makemkvcon ({}): {e}", cfg.makemkv_path))?;
    if outcome.timed_out {
        app.log(
            "error",
            "MakeMKV didn't finish listing the drives within 2 minutes - a drive is probably hanging. \
             Unplug and reconnect it (USB: also try another port or cable), then restart the container.",
        );
    }
    if let Some(v) = robot::version_from_output(&out) {
        *lock(&app.makemkv_version) = v;
    }
    let found = robot::parse_drives(&out);
    let mut added = Vec::new();
    {
        let mut slots = lock(&app.drives.slots);
        for d in &found {
            let key = key_of(d);
            let slot = match slots.iter().find(|s| s.key == key) {
                Some(s) => s.clone(),
                None => {
                    let settings = app.db.drive_settings(&key, cfg.automatic_mode);
                    let slot = Arc::new(DriveSlot {
                        id: app.drives.next_id.fetch_add(1, Ordering::SeqCst) + 1,
                        key: key.clone(),
                        cancel: AtomicBool::new(false),
                        st: Mutex::new(DriveState {
                            index: d.index,
                            drive_name: d.drive_name.clone(),
                            device: d.device.clone(),
                            present: true,
                            settings,
                            tray: Tray::Unknown,
                            ioctl_ok: None,
                            mk_disc: None,
                            has_disc: false,
                            seen_label: String::new(),
                            disc_label: String::new(),
                            disc_type: String::new(),
                            auto_handled: false,
                            label_fetched: false,
                            activity: Activity::Idle,
                            progress: None,
                            staged: None,
                            scan_error: None,
                            last_message: String::new(),
                        }),
                    });
                    slots.push(slot.clone());
                    added.push(slot.clone());
                    slot
                }
            };
            let mut s = slot.state();
            s.index = d.index;
            s.drive_name = d.drive_name.clone();
            s.device = d.device.clone();
            s.present = true;
            s.mk_disc = d.has_disc.then(|| d.disc_name.clone());
            if d.has_disc && s.has_disc && s.activity == Activity::Idle && s.staged.is_none() {
                s.disc_label = d.disc_name.clone();
            }
        }
        for s in slots.iter() {
            if !found.iter().any(|d| key_of(d) == s.key) {
                s.state().present = false;
            }
        }
    }
    for slot in added {
        let (name, dev) = {
            let s = slot.state();
            (s.drive_name.clone(), s.device.clone())
        };
        app.log_for("info", Some(&slot.label()), format!("Found drive: {name} ({dev})"));
    }
    // Drives without a working /dev/srN: MakeMKV's listing is all we know
    // about their disc.
    for slot in app.drives.all() {
        let (present, ioctl, idle, mk) = {
            let s = slot.state();
            (s.present, s.ioctl_ok, s.activity == Activity::Idle, s.mk_disc.clone())
        };
        if present && ioctl != Some(true) && idle {
            handle_presence(app, &slot, mk);
        }
    }
    app.mark_dirty();
    Ok(found.len())
}

/// Reacts to "disc present" (Some(label), label may be unknown = "") or
/// "no disc" (None) for one drive.
fn handle_presence(app: &Arc<App>, slot: &Arc<DriveSlot>, presence: Option<String>) {
    let label = slot.label();
    let mut spawn_auto = false;
    let mut fetch_label = false;
    let mut message: Option<String> = None;
    {
        let mut s = slot.state();
        match presence {
            None => {
                if s.has_disc {
                    s.has_disc = false;
                    s.auto_handled = false;
                    s.label_fetched = false;
                    s.seen_label.clear();
                    s.disc_label.clear();
                    s.disc_type.clear();
                    if s.activity == Activity::Idle {
                        s.staged = None;
                        s.scan_error = None;
                    }
                    message = Some("Disc removed.".into());
                }
            }
            Some(l) => {
                let swapped = !l.is_empty() && !s.seen_label.is_empty() && l != s.seen_label;
                if !s.has_disc || swapped {
                    s.has_disc = true;
                    s.auto_handled = false;
                    s.label_fetched = !l.is_empty();
                    s.seen_label = l.clone();
                    s.disc_label = l.clone();
                    s.disc_type.clear();
                    if s.activity == Activity::Idle {
                        s.staged = None;
                        s.scan_error = None;
                    }
                    message = Some(if l.is_empty() { "Disc inserted.".into() } else { format!("Disc inserted: '{l}'") });
                }
                if s.activity == Activity::Idle && !s.auto_handled && s.settings.automatic {
                    s.auto_handled = true;
                    spawn_auto = true;
                } else if s.activity == Activity::Idle && !s.label_fetched && !s.settings.automatic {
                    s.label_fetched = true;
                    fetch_label = true;
                }
            }
        }
    }
    if let Some(m) = message {
        app.log_for("info", Some(&label), m);
        app.mark_dirty();
    }
    if spawn_auto {
        tokio::spawn(auto_process(app.clone(), slot.clone()));
    }
    if fetch_label {
        // Show the disc label on the dashboard (only while nothing else runs).
        let app = app.clone();
        tokio::spawn(async move {
            if !app.drives.any_busy() {
                let _ = refresh(&app).await;
            }
        });
    }
}

/// Watches every drive for discs going in and out. Uses the CD-ROM ioctl
/// where the /dev/srN device is available; otherwise (only /dev/sgN passed
/// to the container) asks makemkvcon, but only while no drive is busy and
/// only if Automatic Mode is on for such a drive.
pub async fn watcher(app: Arc<App>) {
    let mut last_enum = Instant::now();
    let mut last_error = String::new();
    loop {
        let cfg = app.cfg();
        let interval = Duration::from_secs(cfg.poll_interval_sec as u64);
        let slots = app.drives.all();
        let mut fallback_wanted = false;
        for slot in &slots {
            let (present, device, idle, auto) = {
                let s = slot.state();
                (s.present, s.device.clone(), s.activity == Activity::Idle, s.settings.automatic)
            };
            // Don't poke a drive while MakeMKV is reading it.
            if !present || !idle {
                continue;
            }
            let probe = if app.env.use_ioctl && device.starts_with("/dev/") {
                let dev = device.clone();
                tokio::task::spawn_blocking(move || cdrom::status(&dev)).await.ok()
            } else {
                None
            };
            match probe {
                Some(Ok(tray)) => {
                    {
                        let mut s = slot.state();
                        if s.tray != tray || s.ioctl_ok != Some(true) {
                            s.tray = tray;
                            s.ioctl_ok = Some(true);
                            app.mark_dirty();
                        }
                    }
                    handle_presence(&app, slot, (tray == Tray::DiscOk).then(String::new));
                }
                _ => {
                    {
                        let mut s = slot.state();
                        if s.ioctl_ok != Some(false) {
                            s.ioctl_ok = Some(false);
                            s.tray = Tray::Unknown;
                            app.mark_dirty();
                        }
                    }
                    if auto {
                        fallback_wanted = true;
                    }
                }
            }
        }

        let since = last_enum.elapsed();
        let due = (fallback_wanted && since >= interval)
            || since >= Duration::from_secs(300) // pick up drives plugged in later
            || (slots.is_empty() && since >= Duration::from_secs(30));
        if due && !app.drives.any_busy() && !cfg.makemkv_path.is_empty() {
            last_enum = Instant::now();
            match refresh(&app).await {
                Ok(_) => last_error.clear(),
                Err(e) => {
                    if e != last_error {
                        app.log("error", e.clone());
                        last_error = e;
                    }
                }
            }
        }
        tokio::time::sleep(interval).await;
    }
}

/// The show a drive rips into: the one picked for it, or the only show there is.
pub fn resolve_show(app: &App, slot: &DriveSlot) -> Option<Show> {
    let id = slot.state().settings.show_id;
    if let Some(show) = id.and_then(|id| app.db.get_show(id)) {
        return Some(show);
    }
    let shows = app.db.list_shows();
    (shows.len() == 1).then(|| shows[0].clone())
}

/// Reads the disc's title list (the drive must be in Scanning state).
pub async fn scan(app: &Arc<App>, slot: &Arc<DriveSlot>) -> Result<Disc, String> {
    let path = app.makemkv_path();
    let label = slot.label();
    // --minlength=0 lists every title, so the indexes match the ones the rip
    // uses (it passes the same flag). --progress=-same: progress lines.
    let mut args = base_args(app, &["-r", "--progress=-same", "--minlength=0"]);
    args.extend(["info".to_string(), slot.source()]);
    slot.state().progress = Some(Progress { step: "Opening disc".into(), ..Default::default() });
    app.mark_dirty();

    let mut out = String::new();
    let mut last_msg = String::new();
    let result = makemkv::run(&path, &args, Some(&slot.cancel), makemkv::SCAN_TIMEOUT, |line| {
        out.push_str(line);
        out.push('\n');
        let Some((kind, f)) = robot::parse_line(line) else { return };
        match kind.as_str() {
            "MSG" => {
                let m = robot::msg_text(&f);
                if !m.is_empty() {
                    app.log_for("debug", Some(&label), m.clone());
                    last_msg = m;
                }
            }
            "PRGV" => {
                if let Some(p) = robot::parse_progress(&f) {
                    if let Some(pr) = slot.state().progress.as_mut() {
                        pr.percent = p;
                    }
                    app.mark_dirty();
                }
            }
            "PRGT" | "PRGC" if f.len() >= 3 && !f[2].is_empty() => {
                if let Some(pr) = slot.state().progress.as_mut() {
                    pr.step = f[2].clone();
                }
                app.mark_dirty();
            }
            _ => {}
        }
    })
    .await;
    slot.state().progress = None;
    app.mark_dirty();

    let outcome = result.map_err(|e| format!("couldn't run makemkvcon ({path}): {e}"))?;
    if outcome.cancelled {
        return Err("Scan cancelled.".into());
    }
    if outcome.timed_out {
        return Err("Scan timed out after 30 minutes.".into());
    }
    let disc = robot::parse_disc_info(&out);
    if disc.titles.is_empty() {
        let said = if last_msg.is_empty() { String::new() } else { format!(" MakeMKV said: \"{last_msg}\".") };
        return Err(format!("No titles found. Is there a readable disc in the drive?{said}"));
    }
    {
        let mut s = slot.state();
        s.disc_label = disc.disc_name.clone();
        s.disc_type = disc.disc_type.clone();
        s.label_fetched = true;
    }
    app.log_for(
        "info",
        Some(&label),
        format!(
            "Scanned {} title(s): {} '{}'.",
            disc.titles.len(),
            if disc.disc_type.is_empty() { "disc" } else { &disc.disc_type },
            disc.disc_name
        ),
    );
    Ok(disc)
}

async fn scan_and_classify(app: &Arc<App>, slot: &Arc<DriveSlot>, show: &Show) -> Result<StagedScan, String> {
    let disc = scan(app, slot).await?;
    let cfg = app.cfg();
    let (mut rows, notes) = detection::classify_titles(&disc.titles, &cfg);
    // Re-read the show: another drive may have claimed numbers meanwhile.
    let show = app.db.get_show(show.id).unwrap_or_else(|| show.clone());
    detection::assign_episode_numbers(&mut rows, show.next_episode, show.next_special);
    let conflict = if cfg.detect_names { detection::check_name_conflict(&rows) } else { None };
    Ok(StagedScan {
        show_id: show.id,
        fingerprint: detection::disc_fingerprint(&disc.disc_name, &disc.titles),
        disc_name: disc.disc_name,
        disc_type: disc.disc_type,
        rows,
        notes,
        conflict,
        scanned_ts: now(),
    })
}

/// Manual Rip: scan in the background, keep the result for the web UI.
pub fn start_manual_scan(app: &Arc<App>, slot: &Arc<DriveSlot>, show: Show) -> Result<(), String> {
    let label = slot.label();
    if !app.drives.try_begin(slot, Activity::Scanning) {
        return Err(format!("{label} is busy ({}) - wait until it finishes.", slot.activity().as_str()));
    }
    {
        let mut s = slot.state();
        s.staged = None;
        s.scan_error = None;
    }
    app.log_for(
        "info",
        Some(&label),
        format!("Scanning for '{}' (Blu-ray / UHD discs can take several minutes)...", show.name),
    );
    let (app, slot) = (app.clone(), slot.clone());
    tokio::spawn(async move {
        match scan_and_classify(&app, &slot, &show).await {
            Ok(staged) => {
                for n in &staged.notes {
                    app.log_for("info", Some(&label), n.clone());
                }
                for r in &staged.rows {
                    for w in r.warnings.iter().filter(|w| !w.starts_with("Disc metadata")) {
                        app.log_for("info", Some(&label), format!("Title {}: {w}", r.title_index));
                    }
                }
                slot.state().staged = Some(staged);
                app.drives.finish(&slot, Some("Scanned - check the titles under Rip.".into()));
            }
            Err(e) => {
                app.log_for("warn", Some(&label), e.clone());
                slot.state().scan_error = Some(e.clone());
                app.drives.finish(&slot, Some(e));
            }
        }
    });
    Ok(())
}

pub fn park_for_review(app: &Arc<App>, slot: &DriveSlot, show: &Show, disc: (&str, &str), fingerprint: Option<&str>, rows: &[Row], reason: &str) {
    let label = slot.label();
    let index = slot.state().index as i64;
    match app.db.add_review_item(index, &slot.key, Some(show.id), disc.0, disc.1, rows, reason, fingerprint) {
        Ok(id) => {
            app.log_for(
                "warn",
                Some(&label),
                format!("Disc '{}' needs review ({reason}). Added to the Review Queue as #{id}.", disc.0),
            );
            notify::send(app, "review", &format!("{label}: disc needs review"), &format!("'{}' for {}: {reason}", disc.0, show.name));
        }
        Err(e) => app.log_for("error", Some(&label), e),
    }
    app.mark_dirty();
}

/// Automatic Mode for one inserted disc. Never guesses through an ambiguous
/// disc: anything the detectors flag goes to the Review Queue unripped.
pub async fn auto_process(app: Arc<App>, slot: Arc<DriveSlot>) {
    let label = slot.label();
    let Some(show) = resolve_show(&app, &slot) else {
        let m = "Automatic Mode: pick a show for this drive on the Dashboard first - disc left alone.";
        app.log_for("warn", Some(&label), m);
        app.drives.finish(&slot, Some(m.into()));
        return;
    };
    if !app.drives.try_begin(&slot, Activity::Scanning) {
        return;
    }
    app.log_for("info", Some(&label), format!("Automatic Mode: scanning the disc for '{}'...", show.name));
    let staged = match scan_and_classify(&app, &slot, &show).await {
        Ok(s) => s,
        Err(e) => {
            app.log_for("warn", Some(&label), e.clone());
            notify::send(&app, "error", &format!("{label}: scan failed"), &e);
            app.drives.finish(&slot, Some(e));
            return;
        }
    };
    let cfg = app.cfg();
    if cfg.skip_ripped_discs && app.db.was_ripped(&staged.fingerprint) {
        let m = format!(
            "Disc '{}' was already ripped completely - skipping it. Use Manual Rip to rip it again.",
            staged.disc_name
        );
        app.log_for("info", Some(&label), m.clone());
        app.drives.finish(&slot, Some(m));
        return;
    }
    for n in &staged.notes {
        app.log_for("info", Some(&label), n.clone());
    }
    let mut reason = detection::review_reason(&staged.rows, staged.conflict.as_ref());
    let mut picked = Vec::new();
    if reason.is_none() {
        match detection::validate_rip_rows(&staged.rows) {
            Ok(p) => picked = p,
            Err(e) => reason = Some(e),
        }
    }
    if let Some(reason) = reason {
        park_for_review(
            &app,
            &slot,
            &show,
            (&staged.disc_name, &staged.disc_type),
            Some(&staged.fingerprint),
            &staged.rows,
            &reason,
        );
        app.drives.finish(&slot, Some(format!("Needs review: {reason}")));
        return;
    }
    app.log_for(
        "info",
        Some(&label),
        format!(
            "Automatic Mode: ripping {} title(s) ({}).",
            picked.len(),
            if staged.disc_type.is_empty() { "disc" } else { &staged.disc_type }
        ),
    );
    let req = JobRequest {
        show,
        rows: picked,
        fingerprint: Some(staged.fingerprint.clone()),
        disc_label: staged.disc_name.clone(),
        disc_type: staged.disc_type.clone(),
        review_id: None,
    };
    if let Err(e) = ripper::start_job(&app, &slot, req, Activity::Scanning) {
        app.log_for("error", Some(&label), e.clone());
        app.drives.finish(&slot, Some(e));
    }
}
