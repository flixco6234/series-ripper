//! Direct CD-ROM ioctls on /dev/srN: tray/disc status, eject, close tray.
//!
//! Asking the kernel whether a disc is in the drive costs microseconds,
//! while starting makemkvcon to find out costs seconds and wakes every
//! drive. So the watcher uses this whenever the /dev/srN device was passed
//! to the container, and only falls back to makemkvcon when it wasn't.

use serde::Serialize;
use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;

const CDROMEJECT: u64 = 0x5309;
const CDROMCLOSETRAY: u64 = 0x5319;
const CDROM_DRIVE_STATUS: u64 = 0x5326;
const CDROM_LOCKDOOR: u64 = 0x5329;
const CDSL_CURRENT: libc::c_int = libc::c_int::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Tray {
    Unknown,
    NoDisc,
    TrayOpen,
    NotReady,
    DiscOk,
}

fn open(dev: &str) -> io::Result<std::fs::File> {
    OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(dev)
}

fn ioctl(fd: libc::c_int, req: u64, arg: libc::c_int) -> io::Result<libc::c_int> {
    let r = unsafe { libc::ioctl(fd, req as _, arg) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

pub fn status(dev: &str) -> io::Result<Tray> {
    let f = open(dev)?;
    Ok(match ioctl(f.as_raw_fd(), CDROM_DRIVE_STATUS, CDSL_CURRENT)? {
        1 => Tray::NoDisc,
        2 => Tray::TrayOpen,
        3 => Tray::NotReady,
        4 => Tray::DiscOk,
        _ => Tray::Unknown,
    })
}

/// Unlocks the door (MakeMKV may leave it locked) and ejects. Falls back to
/// the `eject` tool, which knows a few more tricks for odd USB drives.
pub fn eject(dev: &str) -> bool {
    if let Ok(f) = open(dev) {
        let _ = ioctl(f.as_raw_fd(), CDROM_LOCKDOOR, 0);
        if ioctl(f.as_raw_fd(), CDROMEJECT, 0).is_ok() {
            return true;
        }
    }
    std::process::Command::new("eject")
        .arg(dev)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Closes a motorised tray. Slim/laptop drives can't do this.
pub fn close_tray(dev: &str) -> bool {
    match open(dev) {
        Ok(f) => ioctl(f.as_raw_fd(), CDROMCLOSETRAY, 0).is_ok(),
        Err(_) => false,
    }
}
