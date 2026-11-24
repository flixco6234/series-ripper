//! Rips a validated list of titles from one drive. Shared by Manual Rip, the
//! Review Queue and Automatic Mode, so all paths behave the same once the
//! decision to rip is made.
//!
//! Rips go to a temp folder (never the container's own file system, which on
//! Unraid lives in docker.img), then get moved into place: by default a
//! hidden folder on the library share, so the move is an instant rename.

use crate::app::{lock, App, Reservation};
use crate::db::{HistoryEntry, Show};
use crate::detection::Row;
use crate::drives::{Activity, DriveSlot, Progress};
use crate::{cdrom, fsutil, makemkv, naming, notify, robot};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

pub const SLEEP_BUG_HINT: &str = "If this is a 4K UHD drive: LibreDrive drives can stop reading after sitting \
     idle for a couple of minutes - eject and re-insert the disc, then rip again.";

pub struct JobRequest {
    pub show: Show,
    /// Validated and ordered (detection::validate_rip_rows).
    pub rows: Vec<Row>,
    pub fingerprint: Option<String>,
    pub disc_label: String,
    pub disc_type: String,
    pub review_id: Option<i64>,
}

fn ep_name(show: &Show, special: bool, ep: i64) -> String {
    if special {
        format!("S00E{ep:02}")
    } else {
        format!("S{:02}E{ep:02}", show.season)
    }
}

/// Claims the episode numbers for this rip and moves the show's "next
/// episode" past them right away, so a disc scanned on another drive
/// meanwhile continues after them.
fn reserve(app: &App, slot: &DriveSlot, req: &JobRequest) -> Result<(), String> {
    let mut res = lock(&app.reservations);
    let mine: Vec<(bool, i64)> = req.rows.iter().map(|r| (r.special, r.episode.unwrap_or(0))).collect();
    for (key, other) in res.iter() {
        if key == &slot.key || other.show_id != req.show.id {
            continue;
        }
        if let Some(&(sp, ep)) = mine.iter().find(|e| other.episodes.contains(e)) {
            return Err(format!(
                "{} of '{}' is already being ripped on another drive - change the episode numbers.",
                ep_name(&req.show, sp, ep),
                req.show.name
            ));
        }
    }
    let show = app.db.get_show(req.show.id).ok_or_else(|| "That show was deleted.".to_string())?;
    let before = (show.next_episode, show.next_special);
    let max_reg = mine.iter().filter(|e| !e.0).map(|e| e.1).max();
    let max_sp = mine.iter().filter(|e| e.0).map(|e| e.1).max();
    let after = (
        max_reg.map_or(before.0, |m| before.0.max(m + 1)),
        max_sp.map_or(before.1, |m| before.1.max(m + 1)),
    );
    if after != before {
        app.db.set_show_numbers(show.id, after.0, after.1)?;
    }
    res.insert(slot.key.clone(), Reservation { show_id: show.id, episodes: mine, before, after });
    Ok(())
}

/// Frees the claimed numbers. `rollback` (after a cancel) hands the numbers
/// that weren't ripped back to the show, unless another rip has moved the
/// counter on since.
fn release(app: &App, slot: &DriveSlot, rollback: Option<(Option<i64>, Option<i64>)>) {
    let mut res = lock(&app.reservations);
    let Some(r) = res.remove(&slot.key) else { return };
    if let (Some((ok_reg, ok_sp)), Some(show)) = (rollback, app.db.get_show(r.show_id)) {
        let mut next = (show.next_episode, show.next_special);
        if next.0 == r.after.0 {
            next.0 = ok_reg.map_or(r.before.0, |m| r.before.0.max(m + 1));
        }
        if next.1 == r.after.1 {
            next.1 = ok_sp.map_or(r.before.1, |m| r.before.1.max(m + 1));
        }
        if next != (show.next_episode, show.next_special) {
            let _ = app.db.set_show_numbers(show.id, next.0, next.1);
        }
    }
    app.mark_dirty();
}

/// Takes the drive (it must be in `expect` state: Idle for a manual start,
/// Scanning when Automatic Mode goes straight from scan to rip), reserves
/// the episode numbers and starts the rip in the background.
pub fn start_job(app: &Arc<App>, slot: &Arc<DriveSlot>, req: JobRequest, expect: Activity) -> Result<usize, String> {
    let label = slot.label();
    {
        let mut s = slot.state();
        if s.activity != expect {
            return Err(format!("{label} is busy ({}) - wait until it finishes.", s.activity.as_str()));
        }
        s.activity = Activity::Ripping;
        s.progress = None;
        s.last_message.clear();
    }
    if expect == Activity::Idle {
        slot.cancel.store(false, Ordering::SeqCst);
    }
    if let Err(e) = reserve(app, slot, &req) {
        app.drives.finish(slot, None);
        return Err(e);
    }
    app.mark_dirty();
    let n = req.rows.len();
    tokio::spawn(run_job(app.clone(), slot.clone(), req));
    Ok(n)
}

enum RipError {
    Cancelled,
    Failed(String),
}

async fn rip_title(app: &Arc<App>, slot: &Arc<DriveSlot>, row: &Row, temp_dir: &Path) -> Result<(), RipError> {
    let cfg = app.cfg();
    let label = slot.label();
    // --minlength=0: without it MakeMKV drops short titles before numbering
    // them, so the scan's title index would point at a different title.
    let mut args: Vec<String> = vec!["-r".into(), "--progress=-same".into(), "--minlength=0".into()];
    args.extend(cfg.extra_args());
    args.extend([
        "mkv".into(),
        slot.source(),
        row.title_index.to_string(),
        temp_dir.display().to_string(),
    ]);
    let size = row.size_bytes;
    let started = Instant::now();
    let mut last_error: Option<String> = None;
    let outcome = makemkv::run(&cfg.makemkv_path, &args, Some(&slot.cancel), makemkv::RIP_TIMEOUT, |line| {
        let Some((kind, f)) = robot::parse_line(line) else { return };
        match kind.as_str() {
            "PRGV" => {
                if let Some(p) = robot::parse_progress(&f) {
                    let elapsed = started.elapsed().as_secs_f64();
                    if let Some(pr) = slot.state().progress.as_mut() {
                        pr.percent = p;
                        if size > 0 && elapsed > 3.0 && p > 0.5 {
                            let done = size as f64 * p / 100.0;
                            pr.bytes_per_sec = done / elapsed;
                            pr.eta_sec = Some((size as f64 - done) / pr.bytes_per_sec);
                        }
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
            "MSG" => {
                let m = robot::msg_text(&f);
                if !m.is_empty() {
                    let lower = m.to_lowercase();
                    if ["fail", "error", "can't", "cannot"].iter().any(|w| lower.contains(w)) {
                        last_error = Some(m.clone());
                    }
                    app.log_for("debug", Some(&label), m);
                }
            }
            _ => {}
        }
    })
    .await
    .map_err(|e| RipError::Failed(format!("couldn't run makemkvcon: {e}")))?;

    if outcome.cancelled {
        return Err(RipError::Cancelled);
    }
    if outcome.timed_out {
        return Err(RipError::Failed("makemkvcon was stopped after 12 hours.".into()));
    }
    if outcome.exit_code != Some(0) || fsutil::largest_mkv(temp_dir).is_none() {
        let detail = last_error.map(|m| format!(" Last MakeMKV message: {m}")).unwrap_or_default();
        let code = outcome.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "none".into());
        return Err(RipError::Failed(format!(
            "Ripping title {} failed (makemkvcon exit code {code}).{detail} {SLEEP_BUG_HINT}",
            row.title_index
        )));
    }
    Ok(())
}

/// "S01E14-S01E16 + S00E01"
fn fmt_eps(show: &Show, eps: &[(bool, i64)]) -> String {
    let group = |special: bool| -> Option<String> {
        let mut n: Vec<i64> = eps.iter().filter(|e| e.0 == special).map(|e| e.1).collect();
        n.sort();
        match n.as_slice() {
            [] => None,
            [one] => Some(ep_name(show, special, *one)),
            [first, .., last] => Some(format!("{}-{}", ep_name(show, special, *first), ep_name(show, special, *last))),
        }
    };
    [group(false), group(true)].into_iter().flatten().collect::<Vec<_>>().join(" + ")
}

async fn run_job(app: Arc<App>, slot: Arc<DriveSlot>, req: JobRequest) {
    let label = slot.label();
    let cfg = app.cfg();
    let show = req.show.clone();
    let out_root = PathBuf::from(if show.output_dir.trim().is_empty() { &cfg.output_dir } else { &show.output_dir });
    let total = req.rows.len();
    let mut saved: Vec<(bool, i64)> = Vec::new();
    let mut failed: Vec<(Row, String)> = Vec::new();
    let mut cancelled = false;

    if let Err(e) = fsutil::prepare_temp_root(&app.env) {
        app.log_for("error", Some(&label), format!("Couldn't create the temp folder {}: {e}", app.env.temp_root.display()));
    }

    for (i, row) in req.rows.iter().enumerate() {
        if slot.cancel.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        let ep = row.episode.unwrap_or(0);
        let filename = naming::build_filename(&show.name, show.season, row);
        slot.state().progress = Some(Progress {
            position: i + 1,
            total,
            title_index: Some(row.title_index),
            season: naming::season_for(row, show.season),
            episode: ep,
            special: row.special,
            filename: filename.clone(),
            size_bytes: row.size_bytes,
            ..Default::default()
        });
        app.mark_dirty();

        let mut hist = HistoryEntry {
            show_id: Some(show.id),
            show_name: show.name.clone(),
            drive: label.clone(),
            disc_label: req.disc_label.clone(),
            title_index: row.title_index,
            season: naming::season_for(row, show.season),
            episode: ep,
            special: row.special,
            size_bytes: row.size_bytes,
            duration_sec: row.duration_sec,
            ..Default::default()
        };

        if let Err(why) = fsutil::enough_space(row.size_bytes, &[("temp", &app.env.temp_root), ("output", &out_root)]) {
            let m = format!("Skipping title {} ({filename}): {why}.", row.title_index);
            app.log_for("error", Some(&label), m.clone());
            hist.status = "failed".into();
            hist.error = m.clone();
            let _ = app.db.add_history(&hist);
            failed.push((row.clone(), m));
            continue;
        }

        app.log_for(
            "info",
            Some(&label),
            format!("Starting rip: title {} -> {filename} ({}/{total})", row.title_index, i + 1),
        );
        let temp_dir = app.env.temp_root.join(format!("drive{}_title{}", slot.id, row.title_index));
        let _ = std::fs::remove_dir_all(&temp_dir);
        if let Err(e) = fsutil::makedirs_owned(&temp_dir, &app.env) {
            let m = format!("Couldn't create {}: {e}", temp_dir.display());
            app.log_for("error", Some(&label), m.clone());
            failed.push((row.clone(), m));
            continue;
        }

        let started = Instant::now();
        let result = rip_title(&app, &slot, row, &temp_dir).await;
        hist.elapsed_sec = started.elapsed().as_secs_f64();
        match result {
            Ok(()) => {
                let src = fsutil::largest_mkv(&temp_dir).expect("checked in rip_title");
                let dest_dir = naming::build_dest_dir(
                    &out_root,
                    &show.name,
                    show.season,
                    row,
                    &cfg.folder_layout,
                    cfg.pad_season_folder,
                );
                let env = app.env.clone();
                let moved = tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
                    fsutil::makedirs_owned(&dest_dir, &env).map_err(|e| format!("{}: {e}", dest_dir.display()))?;
                    let dest = naming::unique_dest_path(&dest_dir, &filename);
                    fsutil::move_file(&src, &dest).map_err(|e| format!("moving to {}: {e}", dest.display()))?;
                    fsutil::own(&dest, false, &env);
                    Ok(dest)
                })
                .await
                .unwrap_or_else(|e| Err(e.to_string()));
                match moved {
                    Ok(dest) => {
                        app.log_for("info", Some(&label), format!("Saved: {}", dest.display()));
                        if let Ok(meta) = std::fs::metadata(&dest) {
                            hist.size_bytes = meta.len() as i64;
                        }
                        hist.path = dest.display().to_string();
                        hist.status = "ok".into();
                        saved.push((row.special, ep));
                    }
                    Err(e) => {
                        let m = format!("Ripped title {} but couldn't save it: {e}", row.title_index);
                        app.log_for("error", Some(&label), m.clone());
                        hist.status = "failed".into();
                        hist.error = m.clone();
                        failed.push((row.clone(), m));
                    }
                }
                let _ = app.db.add_history(&hist);
            }
            Err(RipError::Cancelled) => {
                cancelled = true;
                app.log_for("warn", Some(&label), "Rip cancelled; stopped the current title.");
                let _ = std::fs::remove_dir_all(&temp_dir);
                break;
            }
            Err(RipError::Failed(m)) => {
                app.log_for("error", Some(&label), m.clone());
                hist.status = "failed".into();
                hist.error = m.clone();
                let _ = app.db.add_history(&hist);
                failed.push((row.clone(), m));
            }
        }
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    let max_ok = |special: bool| saved.iter().filter(|e| e.0 == special).map(|e| e.1).max();
    let summary = fmt_eps(&show, &saved);
    let message;
    if cancelled {
        release(&app, &slot, Some((max_ok(false), max_ok(true))));
        message = format!("Cancelled after {} of {total} title(s).", saved.len());
        app.log_for("warn", Some(&label), message.clone());
    } else if !failed.is_empty() {
        // Keep the numbers reserved and offer a retry of just the failed
        // titles in the Review Queue.
        release(&app, &slot, None);
        let rows: Vec<Row> = failed
            .iter()
            .map(|(r, why)| {
                let mut r = r.clone();
                r.include = true;
                r.warnings.insert(0, format!("Failed: {why}"));
                r
            })
            .collect();
        let reason = format!(
            "{} of {total} title(s) failed. Put the disc back in (clean it if needed) and click Rip These to retry.",
            failed.len()
        );
        let index = slot.state().index as i64;
        if let Ok(id) = app.db.add_review_item(
            index,
            &slot.key,
            Some(show.id),
            &req.disc_label,
            &req.disc_type,
            &rows,
            &reason,
            req.fingerprint.as_deref(),
        ) {
            app.log_for("warn", Some(&label), format!("Disc finished with failures - retry is waiting in the Review Queue as #{id}."));
        }
        message = format!("{} saved, {} failed - see the Review Queue.", saved.len(), failed.len());
        notify::send(&app, "error", &format!("{label}: rip failed"), &format!("{} '{}': {message}", show.name, req.disc_label));
    } else {
        release(&app, &slot, None);
        message = format!("Disc complete: {} title(s) saved ({summary}).", saved.len());
        app.log_for("info", Some(&label), "Disc complete.");
        if let Some(fp) = &req.fingerprint {
            let _ = app.db.mark_ripped(fp, &req.disc_label);
        }
        notify::send(&app, "done", &format!("{label}: disc complete"), &format!("{} - {summary} saved.", show.name));
        let device = slot.device();
        if cfg.auto_eject && device.starts_with("/dev/") {
            let dev = device.clone();
            if tokio::task::spawn_blocking(move || cdrom::eject(&dev)).await.unwrap_or(false) {
                app.log_for("info", Some(&label), format!("Ejected {device} - ready for the next disc."));
            }
        }
    }

    if let Some(id) = req.review_id {
        let _ = app.db.set_review_status(id, "done");
    }
    slot.state().staged = None;
    app.drives.finish(&slot, Some(message));
}
