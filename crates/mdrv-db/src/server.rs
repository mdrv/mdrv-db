//! Admin REST + SSE + console serving for `mdrv-db daemon run`.
//!
//! Contract (v1):
//!   POST /login {token}            -> HttpOnly SameSite=Strict session cookie
//!   POST /logout                   -> clears the session
//!   GET  /api/status               -> fleet snapshot (per-slug state + backups)
//!   GET  /api/slugs/{slug}/backups -> recovery dir listing + manifest summaries
//!   GET  /api/slugs/{slug}/report  -> offline report dump (409 when owner runs)
//!   POST /api/slugs/{slug}/backup  -> run one job now (backup->verify->checkpoint)
//!   POST /api/slugs/{slug}/prune   -> retention prune now
//!   GET  /api/events               -> SSE: job.started / job.finished / status
//!   GET  /*                        -> embedded console (or --console dir)
//!
//! Auth: `MDRV_DB_ADMIN_TOKEN`; when unset a token is generated and printed
//! once. All routes except POST /login require the session cookie.

use crate::daemon::{self, Job, State as JobState};
use axum::extract::{Path as ReqPath, Query, State as AxState};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio_stream::wrappers::{BroadcastStream, IntervalStream};
use tokio_stream::StreamExt;

static CONSOLE: include_dir::Dir =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/../../packages/console/dist");

const COOKIE: &str = "mdrv_db_session";

pub(crate) struct App {
    jobs: Vec<Job>,
    token: String,
    sessions: Mutex<HashSet<String>>,
    events: tokio::sync::broadcast::Sender<String>,
    states: Mutex<HashMap<String, JobState>>,
    console: Option<PathBuf>,
    #[allow(dead_code)]
    lock: std::fs::File,
}

fn urandom_hex(n: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    let ok = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_ok();
    if !ok {
        // Fallback: address-space + time entropy (localhost-only surface).
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0) as u64
            ^ (&buf as *const _ as u64);
        buf[..8].copy_from_slice(&t.to_le_bytes());
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

fn cookie_of(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for part in raw.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(COOKIE) {
            return v.strip_prefix('=').map(|s| s.to_string());
        }
    }
    None
}

fn authed(app: &App, headers: &HeaderMap) -> bool {
    cookie_of(headers).is_some_and(|s| app.sessions.lock().map(|m| m.contains(&s)).unwrap_or(false))
}

fn content_type(p: &str) -> &'static str {
    match p.rsplit('.').next().unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "map" => "application/json",
        _ => "application/octet-stream",
    }
}

async fn console_asset(app: Arc<App>, path: &str) -> Response {
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if let Some(dir) = &app.console {
        let p = dir.join(rel);
        if p.is_file() && p.starts_with(dir) {
            if let Ok(bytes) = std::fs::read(&p) {
                return (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, content_type(rel))],
                    bytes,
                )
                    .into_response();
            }
        }
    }
    if let Some(f) = CONSOLE.get_file(rel) {
        return (
            StatusCode::OK,
            [(header::CONTENT_TYPE, content_type(rel))],
            f.contents().to_vec(),
        )
            .into_response();
    }
    // SPA fallback: unknown non-asset paths get the shell.
    if !rel.contains('.') {
        if let Some(f) = CONSOLE.get_file("index.html") {
            return (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                f.contents().to_vec(),
            )
                .into_response();
        }
    }
    (StatusCode::NOT_FOUND, "not found").into_response()
}

async fn index(AxState(app): AxState<Arc<App>>) -> Response {
    console_asset(app, "/").await
}

async fn fallback(AxState(app): AxState<Arc<App>>, req: axum::extract::Request) -> Response {
    console_asset(app, req.uri().path()).await
}

async fn login(AxState(app): AxState<Arc<App>>, Json(body): Json<serde_json::Value>) -> Response {
    let given = body.get("token").and_then(|v| v.as_str()).unwrap_or("");
    // Constant-time-ish compare via blake3 digests.
    let a = blake3::hash(given.as_bytes());
    let b = blake3::hash(app.token.as_bytes());
    if a != b {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "bad token"})),
        )
            .into_response();
    }
    let sid = urandom_hex(32);
    if app
        .sessions
        .lock()
        .map(|mut m| m.insert(sid.clone()))
        .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, "session store").into_response();
    }
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!("{COOKIE}={sid}; HttpOnly; SameSite=Strict; Path=/; Max-Age=604800")
            .parse()
            .unwrap(),
    );
    resp
}

async fn logout(AxState(app): AxState<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(s) = cookie_of(&headers) {
        if let Ok(mut m) = app.sessions.lock() {
            m.remove(&s);
        }
    }
    let mut resp = Json(json!({"ok": true})).into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
            .parse()
            .unwrap(),
    );
    resp
}

fn snapshot(app: &App) -> serde_json::Value {
    let states = app.states.lock().ok();
    let slugs: Vec<serde_json::Value> = app
        .jobs
        .iter()
        .map(|j| {
            let st = states
                .as_ref()
                .and_then(|m| m.get(&j.slug))
                .cloned()
                .or_else(|| daemon::load_state(j));
            let (last_run_ms, last_result, detail, next_run_ms, last_backup, applied_lsn) = match st
            {
                Some(s) => (
                    Some(s.last_run_ms),
                    s.last_result,
                    s.detail,
                    Some(s.next_run_ms),
                    s.last_backup,
                    s.applied_lsn,
                ),
                None => (None, "never".into(), String::new(), None, None, None),
            };
            json!({
                "slug": j.slug,
                "name": j.name,
                "data_dir": j.data_dir.display().to_string(),
                "cron": j.cron,
                "retention_days": j.retention_days,
                "initialized": j.data_dir.join("live/fjall").is_dir(),
                "backups": daemon::count_backups(&j.data_dir.join("recovery")),
                "last_run_ms": last_run_ms,
                "last_result": last_result,
                "detail": detail,
                "next_run_ms": next_run_ms,
                "last_backup": last_backup,
                "applied_lsn": applied_lsn,
            })
        })
        .collect();
    json!({"type": "status", "slugs": slugs})
}

async fn status(AxState(app): AxState<Arc<App>>, headers: HeaderMap) -> Response {
    if !authed(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    Json(snapshot(&app)).into_response()
}

fn job_of<'a>(app: &'a App, slug: &str) -> Option<&'a Job> {
    app.jobs.iter().find(|j| j.slug == slug)
}

async fn backups(
    AxState(app): AxState<Arc<App>>,
    ReqPath(slug): ReqPath<String>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let Some(j) = job_of(&app, &slug) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such slug"})),
        )
            .into_response();
    };
    let recovery = j.data_dir.join("recovery");
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&recovery) {
        let mut dirs: Vec<(i64, PathBuf)> = rd
            .flatten()
            .map(|e| e.path())
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                daemon::backup_ts(&name).map(|ts| (ts, p))
            })
            .collect();
        dirs.sort_by(|a, b| b.0.cmp(&a.0));
        for (ts, p) in dirs {
            let mut bytes = 0u64;
            let mut files = 0usize;
            let mut lsn = None;
            if let Ok(text) = std::fs::read_to_string(p.join("manifest.json")) {
                if let Ok(m) = serde_json::from_str::<serde_json::Value>(&text) {
                    lsn = m.get("applied_lsn").and_then(|v| v.as_i64());
                    if let Some(fs) = m.get("files").and_then(|v| v.as_array()) {
                        files = fs.len();
                        bytes = fs
                            .iter()
                            .filter_map(|f| f.get("bytes").and_then(|b| b.as_u64()))
                            .sum();
                    }
                }
            }
            out.push(json!({
                "name": p.file_name().map(|n| n.to_string_lossy().into_owned()),
                "ts_ms": ts,
                "bytes": bytes,
                "files": files,
                "applied_lsn": lsn,
            }));
        }
    }
    Json(json!({"type": "backups", "slug": slug, "backups": out})).into_response()
}

async fn report(
    AxState(app): AxState<Arc<App>>,
    ReqPath(slug): ReqPath<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let Some(j) = job_of(&app, &slug) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such slug"})),
        )
            .into_response();
    };
    let limit: usize = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
        .clamp(1, 10_000);
    let dir = j.data_dir.clone();
    let name = j.name.clone();
    let opened = tokio::task::spawn_blocking(move || {
        let n = crate::resolve_name(&dir, &name);
        crate::port_turso::open_engine(&dir, &n, false, false).map(|mut e| {
            let out = e.report_export(0, limit);
            let _ = e.close();
            out
        })
    })
    .await
    .map_err(|e| e.to_string());
    match opened {
        Ok(Ok(Ok(lines))) => {
            Json(json!({"type": "report", "slug": slug, "entries": lines})).into_response()
        }
        Ok(Ok(Err(e))) => (
            StatusCode::CONFLICT,
            Json(json!({"error": e.to_string(), "hint": "database in use (owner running?)"})),
        )
            .into_response(),
        _ => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": "report read failed"})),
        )
            .into_response(),
    }
}

async fn backup_now(
    AxState(app): AxState<Arc<App>>,
    ReqPath(slug): ReqPath<String>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let Some(j) = job_of(&app, &slug) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such slug"})),
        )
            .into_response();
    };
    let j2 = j.clone();
    let st = tokio::task::spawn_blocking(move || daemon::execute(&j2))
        .await
        .map_err(|e| e.to_string());
    let st = match st {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response()
        }
    };
    publish(app.clone(), &st);
    Json(json!({"type": "job.finished", "state": st})).into_response()
}

async fn prune(
    AxState(app): AxState<Arc<App>>,
    ReqPath(slug): ReqPath<String>,
    headers: HeaderMap,
) -> Response {
    if !authed(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let Some(j) = job_of(&app, &slug) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "no such slug"})),
        )
            .into_response();
    };
    let recovery = j.data_dir.join("recovery");
    let retention = j.retention_days.unwrap_or(0);
    let pruned =
        tokio::task::spawn_blocking(move || daemon::prune_backups(&recovery, retention)).await;
    match pruned {
        Ok(Ok((kept, removed))) => {
            Json(json!({"type": "prune", "slug": slug, "kept": kept, "removed": removed}))
                .into_response()
        }
        Ok(Err(e)) => {
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e}))).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

fn publish(app: Arc<App>, st: &JobState) {
    if let Ok(mut m) = app.states.lock() {
        m.insert(st.slug.clone(), st.clone());
    }
    let _ = app.events.send(
        json!({
            "type": "job.finished",
            "slug": st.slug,
            "result": st.last_result,
            "detail": st.detail,
            "last_run_ms": st.last_run_ms,
            "next_run_ms": st.next_run_ms,
            "last_backup": st.last_backup,
        })
        .to_string(),
    );
    let _ = app.events.send(snapshot(&app).to_string());
}

async fn events(AxState(app): AxState<Arc<App>>, headers: HeaderMap) -> Response {
    if !authed(&app, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    let rx = app.events.subscribe();
    let first = Event::default().data(snapshot(&app).to_string());
    let stream = BroadcastStream::new(rx).filter_map(|x| match x {
        Ok(data) => Some(Ok(Event::default().data(data))),
        Err(_) => None,
    });
    let hb = IntervalStream::new(tokio::time::interval(std::time::Duration::from_secs(15)))
        .map(|_| Ok::<_, std::convert::Infallible>(Event::default().comment("ping")));
    let merged = tokio_stream::StreamExt::merge(stream, hb);
    Sse::new(tokio_stream::once(Ok::<_, std::convert::Infallible>(first)).chain(merged))
        .keep_alive(KeepAlive::default())
        .into_response()
}

pub(crate) struct Opts {
    pub bind: Option<String>,
    pub port: Option<u16>,
    pub console: Option<PathBuf>,
    pub interval_secs: u64,
    pub jobs: Vec<Job>,
    pub data_root: PathBuf,
}

/// Blocking entry called from `daemon::run` (already inside no runtime).
pub(crate) fn serve(opts: Opts) -> Result<(), String> {
    // Single-instance guard on the fleet data root.
    let lock_path = opts.data_root.join(".mdrv-db-daemon.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .map_err(|e| format!("cannot open {}: {e}", lock_path.display()))?;
    use fs2::FileExt;
    lock.try_lock_exclusive().map_err(|_| {
        format!(
            "another mdrv-db daemon is already running (lock {})",
            lock_path.display()
        )
    })?;

    let token = match std::env::var("MDRV_DB_ADMIN_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => {
            let t = urandom_hex(32);
            println!("admin token (ephemeral; set MDRV_DB_ADMIN_TOKEN to pin): {t}");
            t
        }
    };

    let host = opts.bind.clone().unwrap_or_else(|| "127.0.0.1".into());
    let port = opts.port.unwrap_or(8300);

    let (tx, _) = tokio::sync::broadcast::channel::<String>(256);
    let states: Mutex<HashMap<String, JobState>> = Mutex::new(HashMap::new());
    // Preload persisted states so /api/status is warm before first runs.
    for j in &opts.jobs {
        if let Some(s) = daemon::load_state(j) {
            if let Ok(mut m) = states.lock() {
                m.insert(j.slug.clone(), s);
            }
        }
    }
    let app = Arc::new(App {
        jobs: opts.jobs.clone(),
        token,
        sessions: Mutex::new(HashSet::new()),
        events: tx,
        states,
        console: opts.console.clone(),
        lock,
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;

    rt.block_on(async move {
        // Scheduler loop (same policy as the pre-REST daemon).
        let sched_app = app.clone();
        let interval = opts.interval_secs.max(1);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(interval));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let due: Vec<Job> = {
                    let states = match sched_app.states.lock() {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    sched_app
                        .jobs
                        .iter()
                        .filter(|j| j.cron.is_some())
                        .filter(|j| {
                            states
                                .get(&j.slug)
                                .map(|s| mdrv_db::now_ms() >= s.next_run_ms)
                                .unwrap_or(false)
                        })
                        .cloned()
                        .collect()
                };
                for j in due {
                    let app2 = sched_app.clone();
                    tokio::spawn(async move {
                        let st = tokio::task::spawn_blocking(move || daemon::execute(&j))
                            .await
                            .ok();
                        if let Some(st) = st {
                            println!(
                                "{} [{}] {} {}",
                                daemon::ts(st.last_run_ms),
                                st.slug,
                                st.last_result,
                                st.detail
                            );
                            publish(app2, &st);
                        }
                    });
                }
            }
        });

        let router = Router::new()
            .route("/login", post(login))
            .route("/logout", post(logout))
            .route("/api/status", get(status))
            .route("/api/slugs/{slug}/backups", get(backups))
            .route("/api/slugs/{slug}/report", get(report))
            .route("/api/slugs/{slug}/backup", post(backup_now))
            .route("/api/slugs/{slug}/prune", post(prune))
            .route("/api/events", get(events))
            .route("/", get(index))
            .fallback(fallback)
            .with_state(app.clone());

        let listener = tokio::net::TcpListener::bind((host.as_str(), port))
            .await
            .map_err(|e| format!("bind {host}:{port}: {e}"))?;
        println!(
            "mdrv-db daemon: REST + console on http://{host}:{port} (ctrl-c stops; skipped = owner running)"
        );
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await
            .map_err(|e| format!("serve: {e}"))
    })?;
    Ok(())
}
