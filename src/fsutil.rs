//! File-system helpers: Unraid-friendly ownership (PUID/PGID/UMASK), free
//! space checks and moving finished rips into the library.

use crate::config::Env;
use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn own(path: &Path, is_dir: bool, env: &Env) {
    let _ = std::os::unix::fs::chown(path, Some(env.puid), Some(env.pgid));
    let mode = (if is_dir { 0o777 } else { 0o666 }) & !env.umask;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

/// Like create_dir_all, but every directory it creates gets PUID/PGID, so
/// Unraid shares stay writable for SMB users.
pub fn makedirs_owned(path: &Path, env: &Env) -> io::Result<()> {
    let mut missing = Vec::new();
    let mut p = path.to_path_buf();
    while !p.as_os_str().is_empty() && !p.is_dir() {
        missing.push(p.clone());
        if !p.pop() {
            break;
        }
    }
    for d in missing.iter().rev() {
        fs::create_dir_all(d)?;
        own(d, true, env);
    }
    Ok(())
}

/// Creates the temp folder with marker files that keep Plex and Jellyfin
/// from picking up half-finished rips.
pub fn prepare_temp_root(env: &Env) -> io::Result<()> {
    makedirs_owned(&env.temp_root, env)?;
    for marker in [".plexignore", ".ignore"] {
        let p = env.temp_root.join(marker);
        if !p.exists() {
            fs::write(&p, "*\n")?;
            own(&p, false, env);
        }
    }
    Ok(())
}

/// Free bytes on the file system holding `path` (or its nearest existing
/// parent).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut probe = path.to_path_buf();
    while !probe.exists() {
        if !probe.pop() {
            probe = PathBuf::from("/");
            break;
        }
    }
    let c = CString::new(probe.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

pub fn enough_space(need: i64, places: &[(&str, &Path)]) -> Result<(), String> {
    if need <= 0 {
        return Ok(());
    }
    for (label, path) in places {
        if let Some(free) = free_space(path) {
            if (free as f64) < need as f64 * 1.05 {
                return Err(format!(
                    "not enough space on the {label} drive: title needs {:.1} GB, {:.1} GB free",
                    need as f64 / 1e9,
                    free as f64 / 1e9
                ));
            }
        }
    }
    Ok(())
}

/// Rename, or copy + delete when source and target are on different file
/// systems (e.g. a separate /temp share).
pub fn move_file(src: &Path, dest: &Path) -> io::Result<()> {
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
            let partial = dest.with_extension("mkv.partial");
            fs::copy(src, &partial)?;
            fs::rename(&partial, dest)?;
            fs::remove_file(src)
        }
        Err(e) => Err(e),
    }
}

/// The largest .mkv in a folder (MakeMKV names its output itself).
pub fn largest_mkv(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("mkv")))
        .filter_map(|e| Some((e.metadata().ok()?.len(), e.path())))
        .max_by_key(|(len, _)| *len)
        .map(|(_, p)| p)
}

/// First `makemkvcon` on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(|d| Path::new(d).join(name))
        .find(|p| p.metadata().map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0).unwrap_or(false))
}
