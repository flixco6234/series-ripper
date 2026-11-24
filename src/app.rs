//! Shared application state: database, environment, drives, the live event
//! hub (log lines and state snapshots pushed to the web UI over SSE) and the
//! episode reservations of running rips.

use crate::config::{Config, Env};
use crate::db::{Db, LogEntry};
use crate::drives::DriveManager;
use crate::license::LicenseStatus;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub enum HubEvent {
    Log(LogEntry),
    /// Full state snapshot, already serialized.
    State(Arc<String>),
}

/// Episodes a running rip has claimed, so two drives ripping the same show
/// never write the same episode number.
#[derive(Clone, Debug)]
pub struct Reservation {
    pub show_id: i64,
    pub episodes: Vec<(bool, i64)>,
    /// Show counters before and after the rip claimed its numbers.
    pub before: (i64, i64),
    pub after: (i64, i64),
}

pub struct App {
    pub db: Db,
    pub env: Env,
    pub hub: broadcast::Sender<HubEvent>,
    pub drives: DriveManager,
    pub makemkv_version: Mutex<String>,
    pub license: Mutex<LicenseStatus>,
    pub reservations: Mutex<HashMap<String, Reservation>>,
}

pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl App {
    pub fn new(db: Db, env: Env) -> Arc<App> {
        let (hub, _) = broadcast::channel(2048);
        Arc::new(App {
            db,
            env,
            hub,
            drives: DriveManager::default(),
            makemkv_version: Mutex::new(String::new()),
            license: Mutex::new(LicenseStatus::default()),
            reservations: Mutex::new(HashMap::new()),
        })
    }

    pub fn cfg(&self) -> Config {
        self.db.config()
    }

    pub fn makemkv_path(&self) -> String {
        self.cfg().makemkv_path
    }

    pub fn log(&self, level: &str, msg: impl Into<String>) {
        self.log_for(level, None, msg)
    }

    /// Writes to the action log (and the container log), tagged with the
    /// drive it is about, and pushes it to connected browsers.
    pub fn log_for(&self, level: &str, drive: Option<&str>, msg: impl Into<String>) {
        let msg = msg.into();
        if level != "debug" {
            match drive {
                Some(d) => println!("[{level}] [{d}] {msg}"),
                None => println!("[{level}] {msg}"),
            }
        }
        match self.db.insert_log(level, &msg, drive) {
            Ok(entry) => {
                let _ = self.hub.send(HubEvent::Log(entry));
            }
            Err(err) => eprintln!("[error] couldn't write the log: {err}"),
        }
    }

    pub fn mark_dirty(&self) {
        self.drives.dirty.store(true, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> serde_json::Value {
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "makemkv_version": lock(&self.makemkv_version).clone(),
            "license": lock(&self.license).clone(),
            "temp_dir": self.env.temp_root.display().to_string(),
            "auth": self.env.ui_password.is_some(),
            "review_pending": self.db.count_pending_reviews(),
            "shows": self.db.list_shows(),
            "drives": self.drives.all().iter().map(|d| d.to_json()).collect::<Vec<_>>(),
        })
    }

    /// Pushes a fresh snapshot to the browsers whenever something changed,
    /// at most every 400 ms (progress lines arrive many times a second).
    pub async fn state_pusher(self: Arc<Self>) {
        let mut tick = tokio::time::interval(std::time::Duration::from_millis(400));
        loop {
            tick.tick().await;
            if self.drives.dirty.swap(false, Ordering::Relaxed) && self.hub.receiver_count() > 0 {
                let s = self.snapshot().to_string();
                let _ = self.hub.send(HubEvent::State(Arc::new(s)));
            }
        }
    }
}
