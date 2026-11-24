//! HTTP API + the embedded single-page web UI. Live updates (log lines and
//! drive state) are pushed to the browser over Server-Sent Events.

use crate::app::{lock, App, HubEvent};
use crate::db::Show;
use crate::detection::{self, Row};
use crate::drives::{self, Activity, DriveSlot};
use crate::ripper::{self, JobRequest};
use crate::{cdrom, notify};
use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};

const INDEX_HTML: &str = include_str!("../static/index.html");
const APP_JS: &str = include_str!("../static/app.js");
const STYLE_CSS: &str = include_str!("../static/style.css");

pub struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

fn bad(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, msg.into())
}
fn busy(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, msg.into())
}
fn not_found(msg: impl Into<String>) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, msg.into())
}

type ApiResult = Result<Response, ApiError>;
type St = State<Arc<App>>;

fn ok(v: impl Serialize) -> ApiResult {
    Ok(Json(v).into_response())
}

fn accepted(v: impl Serialize) -> ApiResult {
    Ok((StatusCode::ACCEPTED, Json(v)).into_response())
}

/// Parses a JSON body; an empty body means "all defaults".
fn parse<T: DeserializeOwned + Default>(b: &Bytes) -> Result<T, ApiError> {
    if b.iter().all(|c| c.is_ascii_whitespace()) {
        return Ok(T::default());
    }
    serde_json::from_slice(b).map_err(|e| bad(format!("invalid request: {e}")))
}

fn slot(app: &App, id: u32) -> Result<Arc<DriveSlot>, ApiError> {
    app.drives.get(id).ok_or_else(|| not_found("No such drive - refresh the drive list."))
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/static/:file", get(static_file))
        .route("/api/status", get(status))
        .route("/api/events", get(events))
        .route("/api/config", get(get_config).post(set_config))
        .route("/api/drives", get(list_drives))
        .route("/api/drives/refresh", post(refresh_drives))
        .route("/api/drives/:id/settings", post(drive_settings))
        .route("/api/drives/:id/scan", get(scan_result).post(scan_start))
        .route("/api/drives/:id/apply_names", post(apply_names))
        .route("/api/drives/:id/rip", post(rip))
        .route("/api/drives/:id/cancel", post(cancel))
        .route("/api/drives/:id/eject", post(eject))
        .route("/api/drives/:id/close", post(close_tray))
        .route("/api/shows", get(list_shows).post(create_show))
        .route("/api/shows/:id", post(update_show).delete(delete_show))
        .route("/api/review", get(review_list))
        .route("/api/review/:id/resolve", post(review_resolve))
        .route("/api/review/:id/dismiss", post(review_dismiss))
        .route("/api/history", get(history))
        .route("/api/logs", get(logs))
        .route("/api/notify/test", post(notify_test))
        .layer(middleware::from_fn_with_state(app.clone(), auth))
        // Docker HEALTHCHECK, no password needed.
        .route("/api/health", get(|| async { "ok" }))
        .with_state(app)
}

/// Optional HTTP basic auth (UI_PASSWORD; any user name).
async fn auth(State(app): St, req: Request, next: Next) -> Response {
    let Some(pw) = app.env.ui_password.as_deref() else {
        return next.run(req).await;
    };
    let given = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b.trim()).ok())
        .and_then(|raw| String::from_utf8(raw).ok());
    let ok = given.as_deref().and_then(|s| s.split_once(':')).is_some_and(|(_, p)| p == pw);
    if ok {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"Series Ripper\", charset=\"UTF-8\"")],
            "Password required",
        )
            .into_response()
    }
}

async fn index() -> Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], INDEX_HTML)
        .into_response()
}

async fn static_file(Path(file): Path<String>) -> Response {
    let (ctype, body) = match file.as_str() {
        "app.js" => ("application/javascript; charset=utf-8", APP_JS),
        "style.css" => ("text/css; charset=utf-8", STYLE_CSS),
        _ => return (StatusCode::NOT_FOUND, "not found").into_response(),
    };
    ([(header::CONTENT_TYPE, ctype), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

async fn status(State(app): St) -> ApiResult {
    ok(app.snapshot())
}

async fn events(State(app): St) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let rx = app.hub.subscribe();
    let first = Event::default().event("state").data(app.snapshot().to_string());
    let rest = BroadcastStream::new(rx).filter_map(|m| match m {
        Ok(HubEvent::Log(e)) => Some(Ok(Event::default().event("log").data(serde_json::to_string(&e).unwrap_or_default()))),
        Ok(HubEvent::State(s)) => Some(Ok(Event::default().event("state").data(s.as_str()))),
        Err(_) => None, // a slow client missed some events; the next snapshot catches it up
    });
    Sse::new(tokio_stream::once(Ok(first)).chain(rest)).keep_alive(KeepAlive::default())
}

// ------------------------------------------------------------- config --

async fn get_config(State(app): St) -> ApiResult {
    ok(app.cfg())
}

async fn set_config(State(app): St, body: Bytes) -> ApiResult {
    let patch: Map<String, Value> = parse(&body)?;
    let before = app.cfg();
    let cfg = app.db.update_config(&patch).map_err(bad)?;
    app.log("info", "Settings updated.");
    if cfg.makemkv_path != before.makemkv_path || cfg.makemkv_extra_args != before.makemkv_extra_args {
        let app2 = app.clone();
        tokio::spawn(async move {
            if let Err(e) = drives::refresh(&app2).await {
                app2.log("error", e);
            }
        });
    }
    app.mark_dirty();
    ok(cfg)
}

// ------------------------------------------------------------- drives --

async fn list_drives(State(app): St) -> ApiResult {
    ok(json!({ "drives": app.drives.all().iter().map(|d| d.to_json()).collect::<Vec<_>>() }))
}

async fn refresh_drives(State(app): St) -> ApiResult {
    drives::refresh(&app).await.map_err(|e| ApiError(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    list_drives(State(app)).await
}

async fn drive_settings(State(app): St, Path(id): Path<u32>, body: Bytes) -> ApiResult {
    let slot = slot(&app, id)?;
    let patch: Map<String, Value> = parse(&body)?;
    let mut s = slot.state().settings.clone();
    if let Some(v) = patch.get("nickname") {
        s.nickname = v.as_str().unwrap_or("").trim().to_string();
    }
    if let Some(v) = patch.get("show_id") {
        s.show_id = v.as_i64();
        if let Some(show_id) = s.show_id {
            if app.db.get_show(show_id).is_none() {
                return Err(bad("No such show."));
            }
        }
    }
    if let Some(v) = patch.get("automatic") {
        s.automatic = v.as_bool().ok_or_else(|| bad("automatic must be true or false"))?;
    }
    app.db.save_drive_settings(&slot.key, &s).map_err(bad)?;
    let (was_auto, had_show, label) = {
        let mut st = slot.state();
        let was = (st.settings.automatic, st.settings.show_id.is_some());
        st.settings = s.clone();
        (was.0, was.1, slot_label(&st.settings.nickname, st.index))
    };
    // Turned on, or a show picked for a disc that was left alone for lack of
    // one: let Automatic Mode look at the disc in the tray.
    if (s.automatic && !was_auto) || (s.show_id.is_some() && !had_show) {
        slot.rearm_auto();
    }
    if was_auto != s.automatic {
        app.log_for(
            "info",
            Some(&label),
            if s.automatic {
                "Automatic Mode on - an inserted disc is scanned and ripped on its own."
            } else {
                "Automatic Mode off."
            },
        );
    }
    app.mark_dirty();
    ok(slot.to_json())
}

fn slot_label(nickname: &str, index: i32) -> String {
    if nickname.trim().is_empty() {
        format!("Drive {index}")
    } else {
        nickname.trim().to_string()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ScanReq {
    show_id: Option<i64>,
}

fn pick_show(app: &App, slot: &DriveSlot, show_id: Option<i64>) -> Result<Show, ApiError> {
    match show_id {
        Some(id) => app.db.get_show(id).ok_or_else(|| bad("No such show.")),
        None => drives::resolve_show(app, slot)
            .ok_or_else(|| bad("Pick a show first (create one under Shows if there is none).")),
    }
}

async fn scan_start(State(app): St, Path(id): Path<u32>, body: Bytes) -> ApiResult {
    let slot = slot(&app, id)?;
    let req: ScanReq = parse(&body)?;
    let show = pick_show(&app, &slot, req.show_id)?;
    drives::start_manual_scan(&app, &slot, show).map_err(busy)?;
    accepted(json!({ "started": true }))
}

async fn scan_result(State(app): St, Path(id): Path<u32>) -> ApiResult {
    let slot = slot(&app, id)?;
    let s = slot.state();
    let show = s.staged.as_ref().and_then(|st| app.db.get_show(st.show_id));
    ok(json!({
        "running": s.activity == Activity::Scanning,
        "result": s.staged,
        "error": s.scan_error,
        "progress": s.progress,
        "show": show,
    }))
}

async fn apply_names(State(app): St, Path(id): Path<u32>) -> ApiResult {
    let slot = slot(&app, id)?;
    let mut s = slot.state();
    let staged = s.staged.as_mut().ok_or_else(|| bad("No scan to apply names to."))?;
    if let Some(c) = detection::check_name_conflict(&staged.rows) {
        detection::apply_name_hints(&mut staged.rows, &c.hinted_indices);
        staged.conflict = None;
    }
    let rows = staged.rows.clone();
    drop(s);
    app.log("info", "Applied episode numbers detected from disc title names.");
    ok(json!({ "rows": rows }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RipReq {
    rows: Option<Vec<Row>>,
    show_id: Option<i64>,
}

async fn rip(State(app): St, Path(id): Path<u32>, body: Bytes) -> ApiResult {
    let slot = slot(&app, id)?;
    let req: RipReq = parse(&body)?;
    let (staged, disc_label, disc_type) = {
        let s = slot.state();
        (s.staged.clone(), s.disc_label.clone(), s.disc_type.clone())
    };
    let rows = match (req.rows, &staged) {
        (Some(r), _) => r,
        (None, Some(st)) => st.rows.clone(),
        (None, None) => return Err(bad("No rows sent and no scan for this drive.")),
    };
    let show = pick_show(&app, &slot, req.show_id.or(staged.as_ref().map(|s| s.show_id)))?;
    let picked = detection::validate_rip_rows(&rows).map_err(bad)?;
    let n = ripper::start_job(
        &app,
        &slot,
        JobRequest {
            show,
            rows: picked,
            fingerprint: staged.as_ref().map(|s| s.fingerprint.clone()),
            disc_label: staged.as_ref().map(|s| s.disc_name.clone()).unwrap_or(disc_label),
            disc_type: staged.as_ref().map(|s| s.disc_type.clone()).unwrap_or(disc_type),
            review_id: None,
        },
        Activity::Idle,
    )
    .map_err(busy)?;
    accepted(json!({ "started": true, "count": n }))
}

async fn cancel(State(app): St, Path(id): Path<u32>) -> ApiResult {
    let slot = slot(&app, id)?;
    if slot.activity() == Activity::Idle {
        return ok(json!({ "ok": true, "idle": true }));
    }
    slot.cancel.store(true, Ordering::SeqCst);
    app.log_for("info", Some(&slot.label()), "Cancel requested.");
    ok(json!({ "ok": true }))
}

async fn tray_action(app: Arc<App>, id: u32, close: bool) -> ApiResult {
    let slot = slot(&app, id)?;
    if slot.activity() != Activity::Idle {
        return Err(busy("The drive is busy - cancel first."));
    }
    let dev = slot.device();
    if !dev.starts_with("/dev/") {
        return Err(bad("The drive's /dev/srN device isn't known."));
    }
    let d = dev.clone();
    let done = tokio::task::spawn_blocking(move || if close { cdrom::close_tray(&d) } else { cdrom::eject(&d) })
        .await
        .unwrap_or(false);
    if !done {
        let what = if close { "Closing the tray" } else { "Ejecting" };
        return Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{what} didn't work - is {dev} passed to the container? (Slim drives can't close the tray.)"),
        ));
    }
    app.log_for("info", Some(&slot.label()), if close { "Tray closed." } else { "Ejected." });
    ok(json!({ "ok": true }))
}

async fn eject(State(app): St, Path(id): Path<u32>) -> ApiResult {
    tray_action(app, id, false).await
}

async fn close_tray(State(app): St, Path(id): Path<u32>) -> ApiResult {
    tray_action(app, id, true).await
}

// -------------------------------------------------------------- shows --

/// Applies the fields present in `patch` to `show` and checks them.
fn apply_show_patch(show: &mut Show, patch: &Map<String, Value>) -> Result<(), ApiError> {
    let int = |k: &str, min: i64| -> Result<Option<i64>, ApiError> {
        match patch.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => match v.as_i64() {
                Some(n) if n >= min => Ok(Some(n)),
                _ => Err(bad(format!("{k} must be a whole number of at least {min}."))),
            },
        }
    };
    if let Some(v) = patch.get("name") {
        show.name = v.as_str().unwrap_or("").trim().to_string();
    }
    if let Some(n) = int("season", 0)? {
        show.season = n;
    }
    if let Some(n) = int("next_episode", 1)? {
        show.next_episode = n;
    }
    if let Some(n) = int("next_special", 1)? {
        show.next_special = n;
    }
    if let Some(v) = patch.get("output_dir") {
        show.output_dir = v.as_str().unwrap_or("").trim().trim_end_matches('/').to_string();
    }
    if show.name.is_empty() {
        return Err(bad("The show needs a name."));
    }
    if !show.output_dir.is_empty() && !show.output_dir.starts_with('/') {
        return Err(bad("The library folder must be a full path inside the container, e.g. /output/Anime."));
    }
    Ok(())
}

async fn list_shows(State(app): St) -> ApiResult {
    ok(json!({ "shows": app.db.list_shows() }))
}

async fn create_show(State(app): St, body: Bytes) -> ApiResult {
    let patch: Map<String, Value> = parse(&body)?;
    let mut show = Show { season: 1, next_episode: 1, next_special: 1, ..Default::default() };
    apply_show_patch(&mut show, &patch)?;
    let show = app.db.create_show(&show).map_err(bad)?;
    app.log("info", format!("Show added: {} (season {}, next episode {}).", show.name, show.season, show.next_episode));
    app.mark_dirty();
    ok(show)
}

async fn update_show(State(app): St, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let patch: Map<String, Value> = parse(&body)?;
    let mut show = app.db.get_show(id).ok_or_else(|| not_found("No such show."))?;
    apply_show_patch(&mut show, &patch)?;
    app.db.update_show(&show).map_err(bad)?;
    app.mark_dirty();
    ok(show)
}

async fn delete_show(State(app): St, Path(id): Path<i64>) -> ApiResult {
    let show = app.db.get_show(id).ok_or_else(|| not_found("No such show."))?;
    if lock(&app.reservations).values().any(|r| r.show_id == id) {
        return Err(busy("A disc of this show is being ripped right now."));
    }
    app.db.delete_show(id).map_err(bad)?;
    for d in app.drives.all() {
        let mut s = d.state();
        if s.settings.show_id == Some(id) {
            s.settings.show_id = None;
        }
    }
    app.log("info", format!("Show removed: {} (ripped files are kept).", show.name));
    app.mark_dirty();
    ok(json!({ "ok": true }))
}

// ------------------------------------------------------------- review --

async fn review_list(State(app): St) -> ApiResult {
    ok(json!({ "items": app.db.list_review_items("pending") }))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ResolveReq {
    rows: Option<Vec<Row>>,
    drive_id: Option<u32>,
    show_id: Option<i64>,
}

async fn review_resolve(State(app): St, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let item = app
        .db
        .get_review_item(id)
        .filter(|i| i.status == "pending")
        .ok_or_else(|| not_found("Not found or already resolved."))?;
    let req: ResolveReq = parse(&body)?;
    let slot = match req.drive_id {
        Some(d) => slot(&app, d)?,
        None => item
            .drive_key
            .as_deref()
            .and_then(|k| app.drives.by_key(k))
            .or_else(|| app.drives.by_index(item.drive_index))
            .ok_or_else(|| bad("Pick the drive the disc is in."))?,
    };
    let show = pick_show(&app, &slot, req.show_id.or(item.show_id))?;
    let rows = req.rows.unwrap_or(item.rows.clone());
    let picked = detection::validate_rip_rows(&rows).map_err(bad)?;
    app.db.set_review_status(id, "ripping").map_err(bad)?;
    let label = slot.label();
    let started = ripper::start_job(
        &app,
        &slot,
        JobRequest {
            show,
            rows: picked,
            fingerprint: item.fingerprint.clone(),
            disc_label: item.disc_label.clone(),
            disc_type: item.disc_type.clone(),
            review_id: Some(id),
        },
        Activity::Idle,
    );
    match started {
        Ok(n) => {
            app.log_for(
                "info",
                Some(&label),
                format!(
                    "Review item #{id}: ripping {n} title(s). Make sure disc '{}' is in {label}.",
                    item.disc_label
                ),
            );
            app.mark_dirty();
            accepted(json!({ "started": true, "count": n }))
        }
        Err(e) => {
            let _ = app.db.set_review_status(id, "pending");
            Err(busy(e))
        }
    }
}

async fn review_dismiss(State(app): St, Path(id): Path<i64>) -> ApiResult {
    app.db.get_review_item(id).ok_or_else(|| not_found("No such review item."))?;
    app.db.set_review_status(id, "dismissed").map_err(bad)?;
    app.log("info", format!("Review item #{id} dismissed without ripping."));
    app.mark_dirty();
    ok(json!({ "ok": true }))
}

// ------------------------------------------------------- history / log --

fn q_i64(q: &HashMap<String, String>, k: &str) -> Option<i64> {
    q.get(k).and_then(|v| v.parse().ok())
}

async fn history(State(app): St, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let entries = app.db.list_history(q_i64(&q, "show_id"), q_i64(&q, "limit").unwrap_or(200), q_i64(&q, "before_id"));
    ok(json!({ "entries": entries }))
}

async fn logs(State(app): St, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let entries = app.db.get_log(
        q_i64(&q, "since_id").unwrap_or(0),
        q_i64(&q, "limit").unwrap_or(200),
        q_i64(&q, "before_id"),
    );
    ok(json!({ "entries": entries }))
}

async fn notify_test(State(app): St) -> ApiResult {
    notify::test(&app).await.map_err(bad)?;
    ok(json!({ "ok": true }))
}
