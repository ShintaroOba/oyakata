//! HTTP server: the single-page UI, the JSON API (sessions, repositories, git, files,
//! OYAKATA-run sessions, typing into terminal sessions) and a Server-Sent Events stream.

use crate::console::{self, Keys, Target};
use crate::gitops;
use crate::index::{Event, State};
use crate::live::{self, LiveInfo};
use crate::runner::{RunnerRegistry, StartOptions};
use crate::transcript::Transcript;
use axum::{
    body::Body,
    extract::{Path, Query, State as AxState},
    http::{header, HeaderMap, Request, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event as SseEvent, KeepAlive, Sse},
        Html, IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream::{Stream, StreamExt};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{broadcast, Notify};
use tokio_stream::wrappers::BroadcastStream;

pub struct App {
    pub state: Arc<Mutex<State>>,
    pub tx: broadcast::Sender<Event>,
    pub shutdown: Arc<Notify>,
    pub started_at_ms: u64,
    pub runners: Arc<RunnerRegistry>,
    pub default_permission_mode: Option<String>,
}

pub type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/assets/{*path}", get(asset))
        .route("/api/health", get(health))
        .route("/api/config", get(config))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/touch", post(touch_session))
        .route("/api/sessions/{id}/agents/{agent}", get(get_agent))
        .route("/api/repos", get(list_repos))
        .route("/api/git/status", get(git_status))
        .route("/api/git/tree", get(git_tree))
        .route("/api/git/file", get(git_file))
        .route("/api/git/raw", get(git_raw))
        .route("/api/git/diff", get(git_diff))
        .route("/api/git/log", get(git_log))
        .route("/api/git/show", get(git_show))
        .route("/api/git/branches", get(git_branches))
        .route("/api/git/commit", post(git_commit))
        .route("/api/git/push", post(git_push))
        .route("/api/git/pull", post(git_pull))
        .route("/api/fs/file", get(fs_file))
        .route("/api/fs/raw", get(fs_raw))
        .route("/api/fs/write", post(fs_write))
        .route("/api/fs/list", get(fs_list))
        .route("/api/run", get(run_list))
        .route("/api/run/start", post(run_start))
        .route("/api/run/{id}/send", post(run_send))
        .route("/api/run/{id}/permission", post(run_permission))
        .route("/api/run/{id}/interrupt", post(run_interrupt))
        .route("/api/run/{id}/stop", post(run_stop))
        .route("/api/terminal/{id}/send", post(terminal_send))
        .route("/api/terminal/{id}/interrupt", post(terminal_interrupt))
        .route("/api/events", get(events))
        .route("/api/shutdown", post(shutdown))
        .layer(middleware::from_fn(same_site_guard))
        .with_state(app)
}

/// The server only listens on loopback, but any web page the user visits could still make
/// requests to it. Refuse cross-site requests: browsers send `Sec-Fetch-Site` on every
/// request, and mutating calls additionally need a custom header (which forces a CORS
/// preflight that we never answer).
async fn same_site_guard(req: Request<Body>, next: Next) -> Response {
    let is_api = req.uri().path().starts_with("/api/");
    if is_api {
        let headers = req.headers();
        if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
            if site != "same-origin" && site != "none" {
                return (StatusCode::FORBIDDEN, "cross-site request refused").into_response();
            }
        }
        if req.method() != axum::http::Method::GET && !headers.contains_key("x-oyakata") {
            return (StatusCode::FORBIDDEN, "missing X-Oyakata header").into_response();
        }
    }
    next.run(req).await
}

/// Poll the filesystem and broadcast what changed. Runs for the life of the server.
pub async fn poll_loop(app: Shared, interval: Duration) {
    let mut tick: u64 = 0;
    loop {
        tokio::time::sleep(interval).await;
        tick += 1;
        let app2 = app.clone();
        let events = tokio::task::spawn_blocking(move || {
            let mut st = app2.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut ev = Vec::new();
            st.discover();
            ev.extend(st.refresh_all());
            st.refresh_live();
            if let Some(e) = st.take_sessions_event() {
                ev.push(e);
            }
            if tick % 60 == 0 {
                st.evict_idle(Duration::from_secs(15 * 60));
            }
            ev
        })
        .await
        .unwrap_or_default();
        for e in events {
            let _ = app.tx.send(e);
        }
    }
}

/// Mirror runner state changes into the index and onto the event stream.
pub async fn run_events_loop(app: Shared) {
    let mut rx = app.runners.changed.subscribe();
    loop {
        match rx.recv().await {
            Ok(id) => {
                let view = app.runners.view(&id);
                let sessions_event = {
                    let mut st = app.state.lock().unwrap_or_else(|e| e.into_inner());
                    st.set_run(&id, view.as_ref());
                    st.take_sessions_event()
                };
                if let Some(v) = view {
                    let _ = app.tx.send(Event::Run { run: v });
                }
                if let Some(e) = sessions_event {
                    let _ = app.tx.send(e);
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

// ---- static assets --------------------------------------------------------------------------

async fn index_html() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

const JS: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";
const SVG: &str = "image/svg+xml";
const NO_CACHE: &str = "no-cache";
const LONG_CACHE: &str = "public, max-age=604800, immutable";

macro_rules! embedded {
    ($file:literal, $ct:expr, $cache:expr) => {
        (&include_bytes!(concat!("../web/", $file))[..], $ct, $cache)
    };
}

async fn asset(Path(path): Path<String>) -> Response {
    let (body, ct, cache): (&'static [u8], &str, &str) = match path.as_str() {
        "app.js" => embedded!("app.js", JS, NO_CACHE),
        "workbench.js" => embedded!("workbench.js", JS, NO_CACHE),
        "chat.js" => embedded!("chat.js", JS, NO_CACHE),
        "editors.js" => embedded!("editors.js", JS, NO_CACHE),
        "sidebar.js" => embedded!("sidebar.js", JS, NO_CACHE),
        "style.css" => embedded!("style.css", CSS, NO_CACHE),
        "icon.svg" => embedded!("icon.svg", SVG, NO_CACHE),
        "vendor/codemirror.js" => embedded!("vendor/codemirror.js", JS, LONG_CACHE),
        "vendor/cm-modes.js" => embedded!("vendor/cm-modes.js", JS, LONG_CACHE),
        "vendor/codemirror.css" => embedded!("vendor/codemirror.css", CSS, LONG_CACHE),
        "vendor/cm-dracula.css" => embedded!("vendor/cm-dracula.css", CSS, LONG_CACHE),
        "vendor/marked.umd.js" => embedded!("vendor/marked.umd.js", JS, LONG_CACHE),
        "vendor/purify.min.js" => embedded!("vendor/purify.min.js", JS, LONG_CACHE),
        "vendor/highlight.min.js" => embedded!("vendor/highlight.min.js", JS, LONG_CACHE),
        "vendor/powershell.min.js" => embedded!("vendor/powershell.min.js", JS, LONG_CACHE),
        "vendor/mermaid.min.js" => embedded!("vendor/mermaid.min.js", JS, LONG_CACHE),
        "vendor/hljs-github.min.css" => embedded!("vendor/hljs-github.min.css", CSS, LONG_CACHE),
        "vendor/hljs-github-dark.min.css" => embedded!("vendor/hljs-github-dark.min.css", CSS, LONG_CACHE),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, ct), (header::CACHE_CONTROL, cache)], body).into_response()
}

// ---- helpers --------------------------------------------------------------------------------

fn lock(app: &Shared) -> std::sync::MutexGuard<'_, State> {
    app.state.lock().unwrap_or_else(|e| e.into_inner())
}

fn err(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "error": msg.into() }))).into_response()
}

fn bad(msg: impl Into<String>) -> Response {
    err(StatusCode::BAD_REQUEST, msg)
}

fn json_bytes(result: Result<anyhow::Result<Vec<u8>>, tokio::task::JoinError>) -> Response {
    match result {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "application/json; charset=utf-8")], bytes).into_response(),
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            let status = if msg.starts_with("unknown") { StatusCode::NOT_FOUND } else { StatusCode::INTERNAL_SERVER_ERROR };
            err(status, msg)
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

/// Run a blocking job and return its JSON-serializable result.
async fn blocking_json<T, F>(f: F) -> Response
where
    T: Serialize + Send + 'static,
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(v)) => Json(v).into_response(),
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn repo_root(q: &HashMap<String, String>) -> Result<PathBuf, Response> {
    let root = q.get("root").map(|s| s.trim()).filter(|s| !s.is_empty()).ok_or_else(|| bad("root is required"))?;
    let p = PathBuf::from(root);
    if !p.is_dir() {
        return Err(err(StatusCode::NOT_FOUND, format!("{root} is not a directory")));
    }
    if !gitops::is_repo(&p) {
        return Err(bad(format!("{root} is not a git repository")));
    }
    Ok(p)
}

fn body_str<'a>(body: &'a Value, key: &str) -> Option<&'a str> {
    body.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
}

// ---- sessions -------------------------------------------------------------------------------

async fn health(AxState(app): AxState<Shared>) -> Json<Value> {
    let (claude_dir, sessions, live) = {
        let st = lock(&app);
        (st.claude_dir.display().to_string(), st.sessions.len(), st.live.len())
    };
    Json(json!({
        "name": "oyakata",
        "version": env!("CARGO_PKG_VERSION"),
        "pid": std::process::id(),
        "started_at": app.started_at_ms,
        "claude_dir": claude_dir,
        "sessions": sessions,
        "live": live,
        "runs": app.runners.views().len(),
    }))
}

async fn config(AxState(app): AxState<Shared>) -> Json<Value> {
    let (claude_dir, ghq_root, repo_roots) = {
        let st = lock(&app);
        (
            st.claude_dir.display().to_string(),
            st.ghq_root.as_ref().map(|p| p.display().to_string()),
            st.repo_roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        )
    };
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "claude_dir": claude_dir,
        "claude_exe": app.runners.exe().map(|e| e.display()),
        "can_run": app.runners.exe().is_some(),
        "can_type": cfg!(windows),
        "default_permission_mode": app.default_permission_mode,
        "home": crate::paths::home_dir().display().to_string(),
        "ghq_root": ghq_root,
        "repo_roots": repo_roots,
    }))
}

async fn list_sessions(AxState(app): AxState<Shared>) -> Json<Value> {
    let st = lock(&app);
    Json(json!({ "sessions": st.summaries() }))
}

async fn list_repos(AxState(app): AxState<Shared>) -> Json<Value> {
    let mut st = lock(&app);
    Json(json!({ "repos": st.repos() }))
}

#[derive(Serialize)]
struct SessionResponse<'a> {
    session: crate::index::SessionSummary,
    meta: &'a crate::transcript::TranscriptMeta,
    items: &'a [crate::transcript::Item],
    agents: &'a [crate::index::AgentMeta],
    run: Option<crate::runner::RunView>,
}

/// Parse a session file outside the state lock so a 30 MB transcript does not stall the
/// sidebar or the poll loop for everyone else.
fn ensure_loaded(app: &Shared, id: &str) -> anyhow::Result<()> {
    let (path, loaded) = lock(app).path_of(id).ok_or_else(|| anyhow::anyhow!("unknown session: {id}"))?;
    if loaded {
        lock(app).touch(id);
        return Ok(());
    }
    let bytes = std::fs::read(&path)?;
    let mut t = Transcript::new(true);
    t.feed(&bytes);
    lock(app).install(id, t)
}

async fn get_session(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    let app2 = app.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        ensure_loaded(&app2, &id)?;
        let run = app2.runners.view(&id);
        let st = lock(&app2);
        let e = st.sessions.get(&id).ok_or_else(|| anyhow::anyhow!("unknown session"))?;
        let body = SessionResponse {
            session: st.summary_of(e),
            meta: &e.transcript.meta,
            items: &e.transcript.items,
            agents: &e.agents,
            run,
        };
        Ok(serde_json::to_vec(&body)?)
    })
    .await;
    json_bytes(result)
}

async fn touch_session(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    if lock(&app).touch(&id) {
        Json(json!({ "ok": true })).into_response()
    } else {
        err(StatusCode::NOT_FOUND, "unknown session")
    }
}

#[derive(Serialize)]
struct AgentResponse<'a> {
    agent: crate::index::AgentMeta,
    meta: &'a crate::transcript::TranscriptMeta,
    items: &'a [crate::transcript::Item],
}

async fn get_agent(AxState(app): AxState<Shared>, Path((id, agent)): Path<(String, String)>) -> Response {
    let app2 = app.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        ensure_loaded(&app2, &id)?;
        let meta = lock(&app2).agent_meta(&id, &agent)?;
        let bytes = std::fs::read(&meta.path)?;
        let mut transcript = Transcript::for_agent();
        transcript.feed(&bytes);
        let body = AgentResponse { agent: meta, meta: &transcript.meta, items: &transcript.items };
        Ok(serde_json::to_vec(&body)?)
    })
    .await;
    json_bytes(result)
}

// ---- git & files ----------------------------------------------------------------------------

async fn git_status(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    blocking_json(move || gitops::status(&root)).await
}

async fn git_tree(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    blocking_json(move || gitops::tree(&root).map(|files| json!({ "files": files }))).await
}

async fn git_file(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let Some(rel) = q.get("path").cloned() else { return bad("path is required") };
    blocking_json(move || {
        let full = gitops::safe_join(&root, &rel)?;
        gitops::read_text_file(&full, &rel)
    })
    .await
}

async fn git_raw(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let Some(rel) = q.get("path").cloned() else { return bad("path is required") };
    let full = match gitops::safe_join(&root, &rel) { Ok(p) => p, Err(e) => return bad(e.to_string()) };
    raw_file(full).await
}

async fn git_diff(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let path = q.get("path").cloned();
    let staged = q.get("staged").map(|s| s == "1" || s == "true").unwrap_or(false);
    blocking_json(move || {
        let text = match path {
            Some(p) if !p.is_empty() => gitops::diff(&root, &p, staged)?,
            _ => gitops::diff_all(&root)?,
        };
        Ok(json!({ "diff": text }))
    })
    .await
}

async fn git_log(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let n: usize = q.get("n").and_then(|s| s.parse().ok()).unwrap_or(60);
    blocking_json(move || gitops::log(&root, n).map(|commits| json!({ "commits": commits }))).await
}

async fn git_show(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let Some(hash) = q.get("hash").cloned() else { return bad("hash is required") };
    blocking_json(move || gitops::show(&root, &hash).map(|text| json!({ "text": text }))).await
}

async fn git_branches(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    blocking_json(move || gitops::branches(&root).map(|b| json!({ "branches": b }))).await
}

async fn git_commit(Json(body): Json<Value>) -> Response {
    let q: HashMap<String, String> = [("root".to_string(), body_str(&body, "root").unwrap_or("").to_string())].into();
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let Some(message) = body_str(&body, "message").map(str::to_string) else { return bad("message is required") };
    let paths: Vec<String> = body
        .get("paths")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    blocking_json(move || gitops::commit(&root, &message, &paths).map(|out| json!({ "ok": true, "output": out }))).await
}

async fn git_push(Json(body): Json<Value>) -> Response {
    let q: HashMap<String, String> = [("root".to_string(), body_str(&body, "root").unwrap_or("").to_string())].into();
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    blocking_json(move || gitops::push(&root).map(|out| json!({ "ok": true, "output": out }))).await
}

async fn git_pull(Json(body): Json<Value>) -> Response {
    let q: HashMap<String, String> = [("root".to_string(), body_str(&body, "root").unwrap_or("").to_string())].into();
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    blocking_json(move || gitops::pull(&root).map(|out| json!({ "ok": true, "output": out }))).await
}

async fn fs_file(Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(path) = q.get("path").cloned() else { return bad("path is required") };
    let p = PathBuf::from(&path);
    if !p.is_absolute() {
        return bad("path must be absolute");
    }
    blocking_json(move || gitops::read_text_file(&p, &path)).await
}

async fn fs_write(Json(body): Json<Value>) -> Response {
    let Some(path) = body.get("path").and_then(Value::as_str).map(str::to_string) else { return bad("path is required") };
    let p = PathBuf::from(&path);
    if !p.is_absolute() {
        return bad("path must be absolute");
    }
    let Some(content) = body.get("content").and_then(Value::as_str).map(str::to_string) else { return bad("content is required") };
    let eol = body.get("eol").and_then(Value::as_str).unwrap_or("\n").to_string();
    let base = body.get("base_mtime_ms").and_then(Value::as_u64);
    match tokio::task::spawn_blocking(move || gitops::write_text_file(&p, &content, &eol, base)).await {
        Ok(Ok(r)) => Json(r).into_response(),
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            let status = if msg.starts_with("conflict") { StatusCode::CONFLICT } else { StatusCode::BAD_REQUEST };
            err(status, msg)
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn fs_list(Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(path) = q.get("path").cloned() else { return bad("path is required") };
    let p = PathBuf::from(&path);
    if !p.is_absolute() || !p.is_dir() {
        return bad("path must be an absolute directory");
    }
    blocking_json(move || gitops::list_dir(&p).map(|entries| json!({ "entries": entries }))).await
}

async fn fs_raw(Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(path) = q.get("path").cloned() else { return bad("path is required") };
    let p = PathBuf::from(&path);
    if !p.is_absolute() {
        return bad("path must be absolute");
    }
    raw_file(p).await
}

/// Serve a local file as-is (images, HTML artifacts, PDFs) for the right-hand panel. The
/// iframe that shows it is sandboxed on the client side.
async fn raw_file(path: PathBuf) -> Response {
    let mime = gitops::mime_for(&path);
    match tokio::task::spawn_blocking(move || std::fs::read(&path)).await {
        Ok(Ok(bytes)) => (
            [
                (header::CONTENT_TYPE, mime.as_str()),
                (header::CACHE_CONTROL, "no-cache"),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        Ok(Err(e)) => err(StatusCode::NOT_FOUND, e.to_string()),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

// ---- OYAKATA-run sessions -------------------------------------------------------------------

async fn run_list(AxState(app): AxState<Shared>) -> Json<Value> {
    Json(json!({ "runs": app.runners.views() }))
}

async fn run_start(AxState(app): AxState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(cwd) = body_str(&body, "cwd") else { return bad("cwd is required") };
    let Some(prompt) = body_str(&body, "prompt") else { return bad("prompt is required") };
    let resume = body_str(&body, "resume").map(str::to_string);
    if let Some(id) = &resume {
        app.runners.forget_exited(id);
        let st = lock(&app);
        if st.live.contains_key(id) {
            return err(StatusCode::CONFLICT, "そのセッションはターミナルで稼働中です。終了してから引き継いでください。");
        }
    }
    let opts = StartOptions {
        cwd: PathBuf::from(cwd),
        prompt: prompt.to_string(),
        resume,
        model: body_str(&body, "model").map(str::to_string),
        permission_mode: body_str(&body, "permission_mode").map(str::to_string),
        effort: body_str(&body, "effort").map(str::to_string),
    };
    match app.runners.start(opts).await {
        Ok(id) => Json(json!({ "session_id": id })).into_response(),
        Err(e) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

async fn run_send(AxState(app): AxState<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let Some(text) = body_str(&body, "text") else { return bad("text is required") };
    match app.runners.send(&id, text).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, format!("{e:#}")),
    }
}

async fn run_permission(AxState(app): AxState<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let Some(request_id) = body_str(&body, "request_id") else { return bad("request_id is required") };
    let behavior = body_str(&body, "behavior").unwrap_or("deny");
    let pending = app
        .runners
        .view(&id)
        .and_then(|v| v.pending.into_iter().find(|p| p.request_id == request_id));
    let Some(req) = pending else { return err(StatusCode::NOT_FOUND, "no such pending request") };
    let response = if behavior == "allow" {
        let mut input = body.get("updated_input").cloned().unwrap_or_else(|| req.input.clone());
        if let Some(answers) = body.get("answers") {
            if let Value::Object(map) = &mut input {
                map.insert("answers".into(), answers.clone());
            }
        }
        let mut r = json!({ "behavior": "allow", "updatedInput": input });
        if body.get("apply_suggestions").and_then(Value::as_bool).unwrap_or(false) && req.suggestions.is_array() {
            r["updatedPermissions"] = req.suggestions.clone();
        }
        r
    } else {
        json!({
            "behavior": "deny",
            "message": body_str(&body, "message").unwrap_or("The user declined this action in OYAKATA."),
        })
    };
    match app.runners.respond_permission(&id, request_id, response).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, format!("{e:#}")),
    }
}

async fn run_interrupt(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    match app.runners.interrupt(&id).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, format!("{e:#}")),
    }
}

async fn run_stop(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    match app.runners.stop(&id).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, format!("{e:#}")),
    }
}

// ---- sessions running in a terminal ---------------------------------------------------------

/// The registry entry of a session running in a terminal, read fresh rather than from the
/// last poll so a prompt that just appeared there is not typed over.
fn terminal_session(app: &Shared, id: &str) -> Result<LiveInfo, Response> {
    if app.runners.view(id).is_some_and(|v| v.status != "exited") {
        return Err(err(StatusCode::CONFLICT, "このセッションは OYAKATA が実行しています。"));
    }
    let dir = lock(app).sessions_dir();
    let Some(l) = live::read_registry(&dir).remove(id) else {
        return Err(err(StatusCode::CONFLICT, "このセッションはターミナルで稼働していません。"));
    };
    if !l.typeable {
        return Err(err(StatusCode::CONFLICT, "このセッションはターミナル以外（IDE や SDK）で動いているため、ここからは送れません。"));
    }
    Ok(l)
}

async fn terminal_send(AxState(app): AxState<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let Some(text) = body_str(&body, "text") else { return bad("text is required") };
    let l = match terminal_session(&app, &id) {
        Ok(l) => l,
        Err(r) => return r,
    };
    // Enter on a permission prompt would pick its default answer.
    if l.status == "waiting" {
        return err(StatusCode::CONFLICT, "ターミナルで確認待ちです。ターミナル側で答えてから送ってください。");
    }
    type_into(&l, Keys::Prompt(text.to_string())).await
}

async fn terminal_interrupt(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    let l = match terminal_session(&app, &id) {
        Ok(l) => l,
        Err(r) => return r,
    };
    // Esc outside a turn would cancel a prompt or, pressed twice, open the rewind menu.
    if l.status != "busy" {
        return err(StatusCode::CONFLICT, "Claude は作業中ではありません。");
    }
    type_into(&l, Keys::Escape).await
}

async fn type_into(l: &LiveInfo, keys: Keys) -> Response {
    match console::type_into(&Target { pid: l.pid, proc_start: l.proc_start }, keys).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::BAD_GATEWAY, format!("{e:#}")),
    }
}

// ---- events ---------------------------------------------------------------------------------

async fn events(AxState(app): AxState<Shared>) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let rx = app.tx.subscribe();
    let stream = BroadcastStream::new(rx).map(|r| match r {
        Ok(ev) => {
            let kind = match &ev {
                Event::Sessions { .. } => "sessions",
                Event::Append { .. } => "append",
                Event::Patch { .. } => "patch",
                Event::Reset { .. } => "reset",
                Event::Run { .. } => "run",
            };
            let data = serde_json::to_string(&ev).unwrap_or_else(|_| "{}".into());
            Ok(SseEvent::default().event(kind).data(data))
        }
        Err(_) => Ok(SseEvent::default().event("lagged").data("{}")),
    });
    Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)).text("ping"))
}

async fn shutdown(AxState(app): AxState<Shared>, _headers: HeaderMap) -> Json<Value> {
    app.shutdown.notify_one();
    Json(json!({ "ok": true }))
}
