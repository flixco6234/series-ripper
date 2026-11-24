//! Series Ripper: rips TV series from DVD / Blu-ray / 4K UHD discs with
//! MakeMKV and names the episodes for Plex/Jellyfin. Runs as a Docker
//! container (made for Unraid) with a web UI.

mod app;
mod cdrom;
mod config;
mod db;
mod detection;
mod drives;
mod fsutil;
mod license;
mod makemkv;
mod naming;
mod notify;
mod ripper;
mod robot;
mod web;

use app::{lock, App};
use config::Env;
use db::Db;
use serde_json::{Map, Value};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        return healthcheck();
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let code = rt.block_on(run());
    rt.shutdown_timeout(Duration::from_secs(5));
    code
}

/// `series-ripper healthcheck`: used by Docker's HEALTHCHECK (the image has
/// no curl/wget).
fn healthcheck() -> ExitCode {
    let port: u16 = std::env::var("DVDRIPPER_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(8080);
    let check = || -> std::io::Result<bool> {
        let mut s = TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(5))?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        s.write_all(b"GET /api/health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut buf = [0u8; 64];
        let n = s.read(&mut buf)?;
        Ok(String::from_utf8_lossy(&buf[..n]).contains(" 200 "))
    };
    if check().unwrap_or(false) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

async fn shutdown_signal() {
    use tokio::signal::unix::{signal, SignalKind};
    let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
}

async fn run() -> ExitCode {
    let env = Env::from_env();
    unsafe {
        libc::umask(env.umask as libc::mode_t);
    }
    let db = match Db::open(&env.db_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("[error] can't open {}: {e}", env.db_path.display());
            return ExitCode::FAILURE;
        }
    };
    let app = App::new(db, env);

    let cfg = app.cfg();
    if cfg.makemkv_path.is_empty() || !Path::new(&cfg.makemkv_path).exists() {
        match fsutil::which("makemkvcon") {
            Some(p) => {
                let mut m = Map::new();
                m.insert("makemkv_path".into(), Value::String(p.display().to_string()));
                let _ = app.db.update_config(&m);
            }
            None => app.log("error", "makemkvcon not found on PATH - set its path in Settings."),
        }
    }
    if let Err(e) = fsutil::prepare_temp_root(&app.env) {
        app.log("error", format!("Couldn't create the temp folder {}: {e}", app.env.temp_root.display()));
    }
    // The web UI comes up first: asking MakeMKV for the drives can take long
    // (or hang on a misbehaving USB drive), and the UI must stay reachable.
    let port = app.env.port;
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            app.log("error", format!("Can't listen on port {port}: {e}"));
            return ExitCode::FAILURE;
        }
    };
    app.log(
        "info",
        format!(
            "Series Ripper {} web UI listening on port {port} (temp folder {}). Looking for drives...",
            env!("CARGO_PKG_VERSION"),
            app.env.temp_root.display()
        ),
    );

    tokio::spawn(license::run(app.clone()));
    tokio::spawn(app.clone().state_pusher());
    let app2 = app.clone();
    tokio::spawn(async move {
        // First drive listing (also tells us the MakeMKV version), then watch.
        match drives::refresh(&app2).await {
            Ok(n) => {
                let version = lock(&app2.makemkv_version).clone();
                app2.log(
                    "info",
                    format!(
                        "{}: {n} drive(s) found.",
                        if version.is_empty() { "MakeMKV version unknown" } else { &version }
                    ),
                );
                if n == 0 {
                    app2.log(
                        "warn",
                        "MakeMKV sees no optical drive. Pass the drive's /dev/sgN (and /dev/srN) to the container - \
                         'lsscsi -g' on the host lists them.",
                    );
                }
            }
            Err(e) => app2.log("error", e),
        }
        drives::watcher(app2).await;
    });

    tokio::select! {
        r = axum::serve(listener, web::router(app.clone())) => {
            if let Err(e) = r {
                eprintln!("[error] web server stopped: {e}");
                return ExitCode::FAILURE;
            }
        }
        _ = shutdown_signal() => println!("[info] Shutting down."),
    }
    ExitCode::SUCCESS
}
