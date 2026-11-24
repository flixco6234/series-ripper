//! Applies the MakeMKV license key from the MAKEMKV_KEY environment variable.
//!
//! - `MAKEMKV_KEY=BETA` (default, also when unset): fetch the free beta key
//!   MakeMKV's author posts on the forum and re-check daily - the key
//!   rotates roughly monthly.
//! - `MAKEMKV_KEY=T-...`: a purchased (or manually copied) key, written as-is.
//! - `MAKEMKV_KEY=` (empty): leave MakeMKV's settings alone.

use crate::app::{lock, App};
use crate::db::now;
use regex::Regex;
use serde::Serialize;
use std::io;
use std::path::Path;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

pub const BETA_KEY_URL: &str = "https://forum.makemkv.com/forum/viewtopic.php?t=1053";

#[derive(Debug, Clone, Serialize, Default)]
pub struct LicenseStatus {
    pub mode: String,
    pub detail: String,
    pub checked_ts: Option<f64>,
}

static KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<code>\s*(T-[A-Za-z0-9_@-]{20,})\s*</code>").unwrap());
static VALID_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)valid until ([^<\n.]{3,40})").unwrap());
static SETTING_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*app_Key\s*=").unwrap());

/// (key, "valid until" text) from the forum page.
pub fn extract_beta_key(page: &str) -> Option<(String, Option<String>)> {
    let key = KEY_RE.captures(page)?[1].to_string();
    let until = VALID_RE.captures(page).map(|c| c[1].trim().to_string());
    Some((key, until))
}

pub fn current_key(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .find(|l| SETTING_RE.is_match(l))
        .and_then(|l| l.split_once('='))
        .map(|(_, v)| v.trim().trim_matches('"').to_string())
}

/// Sets app_Key in MakeMKV's settings.conf, keeping every other setting.
pub fn write_key(path: &Path, key: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<&str> = existing.lines().filter(|l| !SETTING_RE.is_match(l)).collect();
    let new_line = format!("app_Key = \"{key}\"");
    lines.push(&new_line);
    let tmp = path.with_extension("conf.tmp");
    std::fs::write(&tmp, lines.join("\n") + "\n")?;
    std::fs::rename(&tmp, path)
}

fn fetch_beta_key() -> Result<(String, Option<String>), String> {
    let page = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (series-ripper)")
        .build()
        .get(BETA_KEY_URL)
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    extract_beta_key(&page).ok_or_else(|| "no key found on the page (forum layout may have changed)".into())
}

pub async fn apply_once(app: &Arc<App>) {
    let mode = std::env::var("MAKEMKV_KEY").map(|v| v.trim().to_string()).unwrap_or_else(|_| "BETA".into());
    let path = app.env.makemkv_settings.clone();
    let mut st = LicenseStatus { checked_ts: Some(now()), ..Default::default() };
    if mode.is_empty() {
        st.mode = "unset".into();
        st.detail = "MAKEMKV_KEY is empty - using whatever MakeMKV already has (trial if nothing). \
                     Set it to BETA or your key."
            .into();
        app.log("warn", st.detail.clone());
    } else if !mode.eq_ignore_ascii_case("BETA") {
        st.mode = "custom".into();
        if current_key(&path).as_deref() != Some(mode.as_str()) {
            match write_key(&path, &mode) {
                Ok(()) => app.log("info", "MakeMKV key set from MAKEMKV_KEY."),
                Err(e) => app.log("error", format!("Couldn't write {}: {e}", path.display())),
            }
        }
        st.detail = "Using the key from MAKEMKV_KEY.".into();
    } else {
        st.mode = "beta".into();
        let fetched = tokio::task::spawn_blocking(fetch_beta_key).await.unwrap_or_else(|e| Err(e.to_string()));
        match fetched {
            Ok((key, until)) => {
                let suffix = until.map(|u| format!(" (valid until {u})")).unwrap_or_default();
                if current_key(&path).as_deref() != Some(key.as_str()) {
                    match write_key(&path, &key) {
                        Ok(()) => app.log("info", format!("Installed the current MakeMKV beta key{suffix}.")),
                        Err(e) => app.log("error", format!("Couldn't write {}: {e}", path.display())),
                    }
                }
                st.detail = format!("Free beta key from the MakeMKV forum{suffix}.");
            }
            Err(err) => {
                let have = current_key(&path).is_some();
                st.detail = format!(
                    "Couldn't fetch the beta key ({err}). {}",
                    if have { "Keeping the previously saved key." } else { "No key saved yet - MakeMKV runs as a trial." }
                );
                app.log("warn", st.detail.clone());
            }
        }
    }
    *lock(&app.license) = st;
    app.mark_dirty();
}

pub async fn run(app: Arc<App>) {
    loop {
        apply_once(&app).await;
        if lock(&app.license).mode != "beta" {
            return;
        }
        tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r##"<p>The current beta key is valid until end of November 2026.</p><div class="codebox"><p>Code: <a href="#">Select all</a></p><pre><code>T-AbC_123-xyz0987654321qwertyuiopasdfghjk</code></pre></div>"##;

    #[test]
    fn extract() {
        let (key, until) = extract_beta_key(PAGE).unwrap();
        assert_eq!(key, "T-AbC_123-xyz0987654321qwertyuiopasdfghjk");
        assert_eq!(until.as_deref(), Some("end of November 2026"));
        assert!(extract_beta_key("<html>maintenance</html>").is_none());
    }

    #[test]
    fn write_key_keeps_other_settings() {
        let dir = std::env::temp_dir().join(format!("sr-lic-{}", std::process::id()));
        let path = dir.join("settings.conf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "app_DataDir = \"/x\"\napp_Key = \"T-old\"\n").unwrap();
        write_key(&path, "T-new").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("app_DataDir = \"/x\""));
        assert!(text.contains("app_Key = \"T-new\""));
        assert!(!text.contains("T-old"));
        assert_eq!(current_key(&path).as_deref(), Some("T-new"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
