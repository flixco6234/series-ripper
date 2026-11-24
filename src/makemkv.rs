//! Runs makemkvcon and streams its robot-mode output line by line.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::Instant;

pub const ENUM_TIMEOUT: Duration = Duration::from_secs(120);
/// UHD / Blu-ray scans can take many minutes.
pub const SCAN_TIMEOUT: Duration = Duration::from_secs(1800);
pub const RIP_TIMEOUT: Duration = Duration::from_secs(12 * 3600);

#[derive(Debug)]
pub struct Outcome {
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub timed_out: bool,
}

fn spawn_reader<R: AsyncRead + Unpin + Send + 'static>(r: Option<R>, tx: mpsc::UnboundedSender<String>) {
    let Some(r) = r else { return };
    tokio::spawn(async move {
        let mut reader = BufReader::new(r);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    // MakeMKV prints UTF-8, but a disc label can be anything.
                    let line = String::from_utf8_lossy(&buf).trim_end_matches(['\r', '\n']).to_string();
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Runs `path args...`, calling `on_line` for every stdout/stderr line as it
/// arrives. Stops the process if `cancel` becomes true or `timeout` passes:
/// SIGTERM first so MakeMKV can let go of the drive, SIGKILL after 15 s.
pub async fn run<F>(
    path: &str,
    args: &[String],
    cancel: Option<&AtomicBool>,
    timeout: Duration,
    mut on_line: F,
) -> std::io::Result<Outcome>
where
    F: FnMut(&str) + Send,
{
    let mut child = Command::new(path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_reader(child.stdout.take(), tx.clone());
    spawn_reader(child.stderr.take(), tx);

    let deadline = Instant::now() + timeout;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    let mut stop: Option<(bool, bool)> = None; // (cancelled, timed_out)
    loop {
        tokio::select! {
            line = rx.recv() => match line {
                Some(l) => on_line(&l),
                None => break, // both pipes closed: the process is done
            },
            _ = tick.tick() => {
                let cancelled = cancel.is_some_and(|c| c.load(Ordering::SeqCst));
                let timed_out = Instant::now() >= deadline;
                if cancelled || timed_out {
                    stop = Some((cancelled, timed_out));
                    break;
                }
            }
        }
    }

    if let Some((cancelled, timed_out)) = stop {
        if let Some(pid) = child.id() {
            unsafe {
                libc::kill(pid as i32, libc::SIGTERM);
            }
        }
        if tokio::time::timeout(Duration::from_secs(15), child.wait()).await.is_err() {
            // A process stuck in the kernel on a hanging (USB) drive can't
            // even be killed - don't wait for it forever.
            let _ = tokio::time::timeout(Duration::from_secs(10), child.kill()).await;
        }
        return Ok(Outcome { exit_code: None, cancelled, timed_out });
    }
    let status = child.wait().await?;
    Ok(Outcome { exit_code: status.code(), cancelled: false, timed_out: false })
}

/// Collects the whole output of a short call (no cancel).
pub async fn run_collect(path: &str, args: &[String], timeout: Duration) -> std::io::Result<(String, Outcome)> {
    let mut out = String::new();
    let outcome = run(path, args, None, timeout, |l| {
        out.push_str(l);
        out.push('\n');
    })
    .await?;
    Ok((out, outcome))
}
