//! SQLite persistence: settings, shows, per-drive settings, the action log,
//! the review queue, ripped-disc fingerprints and the rip history.
//!
//! Opens the database of the old Python version as-is and migrates it
//! (new tables/columns; the old single show becomes the first show profile).

use crate::config::Config;
use crate::detection::Row;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_KEEP: i64 = 50_000;

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub id: i64,
    pub ts: f64,
    pub level: String,
    pub message: String,
    pub drive: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Show {
    pub id: i64,
    pub name: String,
    pub season: i64,
    pub next_episode: i64,
    pub next_special: i64,
    /// Library folder for this show; empty = the global TV library folder.
    pub output_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DriveSettings {
    pub nickname: String,
    pub show_id: Option<i64>,
    pub automatic: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReviewItem {
    pub id: i64,
    pub created_ts: f64,
    pub drive_index: i64,
    pub drive_key: Option<String>,
    pub show_id: Option<i64>,
    pub disc_label: String,
    pub disc_type: String,
    pub rows: Vec<Row>,
    pub reason: String,
    pub status: String,
    pub fingerprint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct HistoryEntry {
    pub id: i64,
    pub ts: f64,
    pub show_id: Option<i64>,
    pub show_name: String,
    pub drive: String,
    pub disc_label: String,
    pub title_index: i64,
    pub season: i64,
    pub episode: i64,
    pub special: bool,
    pub path: String,
    pub size_bytes: i64,
    pub duration_sec: i64,
    pub elapsed_sec: f64,
    pub status: String,
    pub error: String,
}

pub struct Db {
    conn: Mutex<Connection>,
}

type R<T> = Result<T, String>;

fn e(err: rusqlite::Error) -> String {
    format!("database error: {err}")
}

impl Db {
    pub fn open(path: &Path) -> R<Db> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let conn = Connection::open(path).map_err(e)?;
        let db = Db { conn: Mutex::new(conn) };
        db.migrate()?;
        Ok(db)
    }

    fn c(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn migrate(&self) -> R<()> {
        let c = self.c();
        c.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA busy_timeout=5000;
             CREATE TABLE IF NOT EXISTS config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS action_log (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, ts REAL NOT NULL,
                 level TEXT NOT NULL, message TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS review_queue (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, created_ts REAL NOT NULL,
                 drive_index INTEGER NOT NULL, disc_label TEXT, rows_json TEXT NOT NULL,
                 reason TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending');
             CREATE TABLE IF NOT EXISTS ripped_discs (
                 fingerprint TEXT PRIMARY KEY, disc_label TEXT, ts REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS shows (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL,
                 season INTEGER NOT NULL DEFAULT 1, next_episode INTEGER NOT NULL DEFAULT 1,
                 next_special INTEGER NOT NULL DEFAULT 1, output_dir TEXT NOT NULL DEFAULT '',
                 created_ts REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS drive_settings (
                 drive_key TEXT PRIMARY KEY, nickname TEXT NOT NULL DEFAULT '',
                 show_id INTEGER, automatic INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE IF NOT EXISTS rip_history (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, ts REAL NOT NULL, show_id INTEGER,
                 show_name TEXT NOT NULL, drive TEXT NOT NULL, disc_label TEXT NOT NULL,
                 title_index INTEGER NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
                 special INTEGER NOT NULL, path TEXT NOT NULL, size_bytes INTEGER NOT NULL,
                 duration_sec INTEGER NOT NULL, elapsed_sec REAL NOT NULL,
                 status TEXT NOT NULL, error TEXT NOT NULL);",
        )
        .map_err(e)?;

        let add_col = |table: &str, col: &str, decl: &str| -> R<()> {
            let mut stmt = c.prepare(&format!("PRAGMA table_info({table})")).map_err(e)?;
            let cols: Vec<String> =
                stmt.query_map([], |r| r.get::<_, String>(1)).map_err(e)?.flatten().collect();
            if !cols.iter().any(|x| x == col) {
                c.execute(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}"), []).map_err(e)?;
            }
            Ok(())
        };
        add_col("review_queue", "fingerprint", "TEXT")?;
        add_col("review_queue", "drive_key", "TEXT")?;
        add_col("review_queue", "show_id", "INTEGER")?;
        add_col("review_queue", "disc_type", "TEXT")?;
        add_col("action_log", "drive", "TEXT")?;

        c.execute(
            "DELETE FROM action_log WHERE id <= (SELECT MAX(id) FROM action_log) - ?1",
            params![LOG_KEEP],
        )
        .map_err(e)?;

        // Python version: one show stored as plain settings. Turn it into the
        // first show profile, and Automatic Mode into the per-drive default.
        let shows: i64 = c.query_row("SELECT COUNT(*) FROM shows", [], |r| r.get(0)).map_err(e)?;
        let legacy = |key: &str| -> Option<Value> {
            c.query_row("SELECT value FROM config WHERE key = ?1", params![key], |r| r.get::<_, String>(0))
                .optional()
                .ok()
                .flatten()
                .and_then(|v| serde_json::from_str(&v).ok())
        };
        if shows == 0 {
            if let Some(Value::String(name)) = legacy("show_name") {
                if !name.trim().is_empty() {
                    let season = legacy("season").and_then(|v| v.as_i64()).unwrap_or(1);
                    let next = legacy("next_episode").and_then(|v| v.as_i64()).unwrap_or(1);
                    c.execute(
                        "INSERT INTO shows(name, season, next_episode, next_special, output_dir, created_ts)
                         VALUES (?1, ?2, ?3, 1, '', ?4)",
                        params![name.trim(), season, next, now()],
                    )
                    .map_err(e)?;
                }
            }
        }
        c.execute(
            "DELETE FROM config WHERE key IN ('show_name','season','next_episode','drive_indexes')",
            [],
        )
        .map_err(e)?;
        Ok(())
    }

    // ------------------------------------------------------------ config --

    fn config_map(&self) -> Map<String, Value> {
        let mut map = match serde_json::to_value(Config::default()) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        let c = self.c();
        let Ok(mut stmt) = c.prepare("SELECT key, value FROM config") else { return map };
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for (k, v) in rows.flatten() {
                if map.contains_key(&k) {
                    if let Ok(val) = serde_json::from_str::<Value>(&v) {
                        map.insert(k, val);
                    }
                }
            }
        }
        map
    }

    pub fn config(&self) -> Config {
        let mut map = self.config_map();
        loop {
            match serde_json::from_value::<Config>(Value::Object(map.clone())) {
                Ok(mut cfg) => {
                    cfg.sanitize();
                    return cfg;
                }
                Err(_) => {
                    // A stored value has the wrong type: drop the first one
                    // that doesn't fit and use its default instead.
                    let defaults = match serde_json::to_value(Config::default()) {
                        Ok(Value::Object(m)) => m,
                        _ => return Config::default(),
                    };
                    let bad = map.keys().find(|k| {
                        let mut probe = defaults.clone();
                        probe.insert((*k).clone(), map[*k].clone());
                        serde_json::from_value::<Config>(Value::Object(probe)).is_err()
                    });
                    match bad.cloned() {
                        Some(k) => {
                            map.insert(k.clone(), defaults[&k].clone());
                        }
                        None => return Config::default(),
                    }
                }
            }
        }
    }

    pub fn update_config(&self, patch: &Map<String, Value>) -> R<Config> {
        let mut map = self.config_map();
        for (k, v) in patch {
            if map.contains_key(k) {
                map.insert(k.clone(), v.clone());
            }
        }
        let mut cfg: Config =
            serde_json::from_value(Value::Object(map)).map_err(|err| format!("invalid setting: {err}"))?;
        cfg.sanitize();
        let Value::Object(clean) = serde_json::to_value(&cfg).map_err(|x| x.to_string())? else {
            return Err("invalid settings".into());
        };
        let c = self.c();
        for (k, v) in clean {
            c.execute(
                "INSERT INTO config(key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![k, v.to_string()],
            )
            .map_err(e)?;
        }
        Ok(cfg)
    }

    // --------------------------------------------------------------- log --

    pub fn insert_log(&self, level: &str, message: &str, drive: Option<&str>) -> R<LogEntry> {
        let ts = now();
        let c = self.c();
        c.execute(
            "INSERT INTO action_log(ts, level, message, drive) VALUES (?1, ?2, ?3, ?4)",
            params![ts, level, message, drive],
        )
        .map_err(e)?;
        Ok(LogEntry {
            id: c.last_insert_rowid(),
            ts,
            level: level.into(),
            message: message.into(),
            drive: drive.map(str::to_string),
        })
    }

    /// Most recent `limit` entries newer than `since_id`, oldest first; or,
    /// with `before_id`, the `limit` entries just before that id.
    pub fn get_log(&self, since_id: i64, limit: i64, before_id: Option<i64>) -> Vec<LogEntry> {
        let c = self.c();
        let (sql, a) = match before_id {
            Some(b) => ("SELECT id, ts, level, message, drive FROM action_log WHERE id < ?1 ORDER BY id DESC LIMIT ?2", b),
            None => ("SELECT id, ts, level, message, drive FROM action_log WHERE id > ?1 ORDER BY id DESC LIMIT ?2", since_id),
        };
        let Ok(mut stmt) = c.prepare(sql) else { return vec![] };
        let mut out: Vec<LogEntry> = stmt
            .query_map(params![a, limit.clamp(1, 5000)], |r| {
                Ok(LogEntry { id: r.get(0)?, ts: r.get(1)?, level: r.get(2)?, message: r.get(3)?, drive: r.get(4)? })
            })
            .map(|it| it.flatten().collect())
            .unwrap_or_default();
        out.reverse();
        out
    }

    // ------------------------------------------------------------- shows --

    fn show_from(r: &rusqlite::Row) -> rusqlite::Result<Show> {
        Ok(Show {
            id: r.get(0)?,
            name: r.get(1)?,
            season: r.get(2)?,
            next_episode: r.get(3)?,
            next_special: r.get(4)?,
            output_dir: r.get(5)?,
        })
    }

    pub fn list_shows(&self) -> Vec<Show> {
        let c = self.c();
        let Ok(mut stmt) = c.prepare(
            "SELECT id, name, season, next_episode, next_special, output_dir FROM shows ORDER BY name COLLATE NOCASE",
        ) else {
            return vec![];
        };
        stmt.query_map([], Self::show_from).map(|it| it.flatten().collect()).unwrap_or_default()
    }

    pub fn get_show(&self, id: i64) -> Option<Show> {
        self.c()
            .query_row(
                "SELECT id, name, season, next_episode, next_special, output_dir FROM shows WHERE id = ?1",
                params![id],
                Self::show_from,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn create_show(&self, s: &Show) -> R<Show> {
        let c = self.c();
        c.execute(
            "INSERT INTO shows(name, season, next_episode, next_special, output_dir, created_ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![s.name, s.season, s.next_episode, s.next_special, s.output_dir, now()],
        )
        .map_err(e)?;
        Ok(Show { id: c.last_insert_rowid(), ..s.clone() })
    }

    pub fn update_show(&self, s: &Show) -> R<()> {
        self.c()
            .execute(
                "UPDATE shows SET name=?2, season=?3, next_episode=?4, next_special=?5, output_dir=?6 WHERE id=?1",
                params![s.id, s.name, s.season, s.next_episode, s.next_special, s.output_dir],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn set_show_numbers(&self, id: i64, next_episode: i64, next_special: i64) -> R<()> {
        self.c()
            .execute(
                "UPDATE shows SET next_episode=?2, next_special=?3 WHERE id=?1",
                params![id, next_episode, next_special],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn delete_show(&self, id: i64) -> R<()> {
        let c = self.c();
        c.execute("DELETE FROM shows WHERE id=?1", params![id]).map_err(e)?;
        c.execute("UPDATE drive_settings SET show_id=NULL WHERE show_id=?1", params![id]).map_err(e)?;
        Ok(())
    }

    // ------------------------------------------------------------ drives --

    /// Settings of a drive, created with Automatic Mode = `auto_default` the
    /// first time the drive is seen.
    pub fn drive_settings(&self, key: &str, auto_default: bool) -> DriveSettings {
        let c = self.c();
        let found = c
            .query_row(
                "SELECT nickname, show_id, automatic FROM drive_settings WHERE drive_key=?1",
                params![key],
                |r| Ok(DriveSettings { nickname: r.get(0)?, show_id: r.get(1)?, automatic: r.get::<_, i64>(2)? != 0 }),
            )
            .optional()
            .ok()
            .flatten();
        match found {
            Some(s) => s,
            None => {
                let s = DriveSettings { automatic: auto_default, ..Default::default() };
                let _ = c.execute(
                    "INSERT OR IGNORE INTO drive_settings(drive_key, nickname, show_id, automatic) VALUES (?1, '', NULL, ?2)",
                    params![key, s.automatic as i64],
                );
                s
            }
        }
    }

    pub fn save_drive_settings(&self, key: &str, s: &DriveSettings) -> R<()> {
        self.c()
            .execute(
                "INSERT INTO drive_settings(drive_key, nickname, show_id, automatic) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(drive_key) DO UPDATE SET nickname=excluded.nickname,
                     show_id=excluded.show_id, automatic=excluded.automatic",
                params![key, s.nickname, s.show_id, s.automatic as i64],
            )
            .map_err(e)?;
        Ok(())
    }

    // ------------------------------------------------------------ review --

    #[allow(clippy::too_many_arguments)]
    pub fn add_review_item(
        &self,
        drive_index: i64,
        drive_key: &str,
        show_id: Option<i64>,
        disc_label: &str,
        disc_type: &str,
        rows: &[Row],
        reason: &str,
        fingerprint: Option<&str>,
    ) -> R<i64> {
        let c = self.c();
        c.execute(
            "INSERT INTO review_queue(created_ts, drive_index, drive_key, show_id, disc_label, disc_type,
                                      rows_json, reason, status, fingerprint)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', ?9)",
            params![
                now(),
                drive_index,
                drive_key,
                show_id,
                disc_label,
                disc_type,
                serde_json::to_string(rows).unwrap_or_else(|_| "[]".into()),
                reason,
                fingerprint
            ],
        )
        .map_err(e)?;
        Ok(c.last_insert_rowid())
    }

    fn review_from(r: &rusqlite::Row) -> rusqlite::Result<ReviewItem> {
        let rows_json: String = r.get(6)?;
        Ok(ReviewItem {
            id: r.get(0)?,
            created_ts: r.get(1)?,
            drive_index: r.get(2)?,
            drive_key: r.get(3)?,
            show_id: r.get(4)?,
            disc_label: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            rows: serde_json::from_str(&rows_json).unwrap_or_default(),
            reason: r.get(7)?,
            status: r.get(8)?,
            fingerprint: r.get(9)?,
            disc_type: r.get::<_, Option<String>>(10)?.unwrap_or_default(),
        })
    }

    const REVIEW_COLS: &'static str =
        "id, created_ts, drive_index, drive_key, show_id, disc_label, rows_json, reason, status, fingerprint, disc_type";

    pub fn list_review_items(&self, status: &str) -> Vec<ReviewItem> {
        let c = self.c();
        let sql = format!("SELECT {} FROM review_queue WHERE status=?1 ORDER BY id DESC", Self::REVIEW_COLS);
        let Ok(mut stmt) = c.prepare(&sql) else { return vec![] };
        stmt.query_map(params![status], Self::review_from).map(|it| it.flatten().collect()).unwrap_or_default()
    }

    pub fn count_pending_reviews(&self) -> i64 {
        self.c()
            .query_row("SELECT COUNT(*) FROM review_queue WHERE status='pending'", [], |r| r.get(0))
            .unwrap_or(0)
    }

    pub fn get_review_item(&self, id: i64) -> Option<ReviewItem> {
        let sql = format!("SELECT {} FROM review_queue WHERE id=?1", Self::REVIEW_COLS);
        self.c().query_row(&sql, params![id], Self::review_from).optional().ok().flatten()
    }

    pub fn set_review_status(&self, id: i64, status: &str) -> R<()> {
        self.c()
            .execute("UPDATE review_queue SET status=?2 WHERE id=?1", params![id, status])
            .map_err(e)?;
        Ok(())
    }

    // ------------------------------------------------- ripped / history --

    pub fn was_ripped(&self, fingerprint: &str) -> bool {
        self.c()
            .query_row("SELECT 1 FROM ripped_discs WHERE fingerprint=?1", params![fingerprint], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    pub fn mark_ripped(&self, fingerprint: &str, disc_label: &str) -> R<()> {
        self.c()
            .execute(
                "INSERT OR REPLACE INTO ripped_discs(fingerprint, disc_label, ts) VALUES (?1, ?2, ?3)",
                params![fingerprint, disc_label, now()],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn add_history(&self, h: &HistoryEntry) -> R<()> {
        self.c()
            .execute(
                "INSERT INTO rip_history(ts, show_id, show_name, drive, disc_label, title_index, season, episode,
                     special, path, size_bytes, duration_sec, elapsed_sec, status, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    now(),
                    h.show_id,
                    h.show_name,
                    h.drive,
                    h.disc_label,
                    h.title_index,
                    h.season,
                    h.episode,
                    h.special as i64,
                    h.path,
                    h.size_bytes,
                    h.duration_sec,
                    h.elapsed_sec,
                    h.status,
                    h.error
                ],
            )
            .map_err(e)?;
        Ok(())
    }

    pub fn list_history(&self, show_id: Option<i64>, limit: i64, before_id: Option<i64>) -> Vec<HistoryEntry> {
        let c = self.c();
        let Ok(mut stmt) = c.prepare(
            "SELECT id, ts, show_id, show_name, drive, disc_label, title_index, season, episode, special, path,
                    size_bytes, duration_sec, elapsed_sec, status, error
             FROM rip_history
             WHERE (?1 IS NULL OR show_id = ?1) AND (?2 IS NULL OR id < ?2)
             ORDER BY id DESC LIMIT ?3",
        ) else {
            return vec![];
        };
        stmt.query_map(params![show_id, before_id, limit.clamp(1, 2000)], |r| {
            Ok(HistoryEntry {
                id: r.get(0)?,
                ts: r.get(1)?,
                show_id: r.get(2)?,
                show_name: r.get(3)?,
                drive: r.get(4)?,
                disc_label: r.get(5)?,
                title_index: r.get(6)?,
                season: r.get(7)?,
                episode: r.get(8)?,
                special: r.get::<_, i64>(9)? != 0,
                path: r.get(10)?,
                size_bytes: r.get(11)?,
                duration_sec: r.get(12)?,
                elapsed_sec: r.get(13)?,
                status: r.get(14)?,
                error: r.get(15)?,
            })
        })
        .map(|it| it.flatten().collect())
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_python_database() {
        let path = std::env::temp_dir().join(format!("sr-db-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                r#"CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                   INSERT INTO config VALUES ('show_name', '"Kare Kano"'), ('season', '1'),
                       ('next_episode', '17'), ('poll_interval_sec', '15'), ('auto_eject', 'false');
                   CREATE TABLE review_queue (id INTEGER PRIMARY KEY AUTOINCREMENT, created_ts REAL NOT NULL,
                       drive_index INTEGER NOT NULL, disc_label TEXT, rows_json TEXT NOT NULL,
                       reason TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending');
                   INSERT INTO review_queue(created_ts, drive_index, disc_label, rows_json, reason)
                       VALUES (1.0, 0, 'X', '[{"title_index":1,"include":true,"episode":3,"warnings":[],"flags":[],"hint":null}]', 'why');"#,
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let shows = db.list_shows();
        assert_eq!(shows.len(), 1);
        assert_eq!((shows[0].name.as_str(), shows[0].next_episode), ("Kare Kano", 17));
        let cfg = db.config();
        assert_eq!(cfg.poll_interval_sec, 15);
        assert!(!cfg.auto_eject);
        let items = db.list_review_items("pending");
        assert_eq!(items[0].rows[0].episode, Some(3));
        assert_eq!(items[0].drive_key, None);
        // migration runs again on the next start without duplicating the show
        drop(db);
        let db = Db::open(&path).unwrap();
        assert_eq!(db.list_shows().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_stored_value_falls_back_to_default() {
        let path = std::env::temp_dir().join(format!("sr-db2-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = Db::open(&path).unwrap();
        db.c().execute("INSERT INTO config VALUES ('min_length_sec', '\"abc\"')", []).unwrap();
        assert_eq!(db.config().min_length_sec, 900);
        let mut patch = Map::new();
        patch.insert("min_length_sec".into(), Value::from(600));
        assert_eq!(db.update_config(&patch).unwrap().min_length_sec, 600);
        let _ = std::fs::remove_file(&path);
    }
}
