//! HTTP server: the single-page UI, the JSON API (sessions, repositories, git, files,
//! OYAKATA-run sessions, typing into terminal sessions) and a Server-Sent Events stream.

use crate::agents::{self, AgentKind};
use crate::console::{self, Keys, Target};
use crate::gitops;
use crate::i18n::tr;
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
    /// Metadata-only parsers of subagent transcripts for the team view (体制図).
    pub agent_cache: Mutex<HashMap<PathBuf, Transcript>>,
}

pub type Shared = Arc<App>;

pub fn router(app: Shared) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/assets/{*path}", get(asset))
        .route("/api/health", get(health))
        .route("/api/config", get(config).post(set_config))
        .route("/api/sessions", get(list_sessions))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/touch", post(touch_session))
        .route("/api/sessions/{id}/delete", post(delete_session))
        .route("/api/sessions/{id}/restore", post(restore_session))
        .route("/api/sessions/{id}/team", get(get_team))
        .route("/api/sessions/{id}/agents/{agent}", get(get_agent))
        .route("/api/repos", get(list_repos))
        .route("/api/repos/add", post(add_repo))
        .route("/api/repos/remove", post(remove_repo))
        .route("/api/repos/clone", post(clone_repo))
        .route("/api/git/status", get(git_status))
        .route("/api/git/tree", get(git_tree))
        .route("/api/git/file", get(git_file))
        .route("/api/git/raw", get(git_raw))
        .route("/api/git/diff", get(git_diff))
        .route("/api/git/log", get(git_log))
        .route("/api/git/show", get(git_show))
        .route("/api/git/branches", get(git_branches))
        .route("/api/git/grep", get(git_grep))
        .route("/api/search/sessions", get(search_sessions))
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
        .route("/api/run/{id}/mode", post(run_mode))
        .route("/api/run/{id}/model", post(run_model))
        .route("/api/run/{id}/stop", post(run_stop))
        .route("/api/worktrees", get(worktree_list))
        .route("/api/worktree/remove", post(worktree_remove))
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

/// OpenCode keeps its sessions in a database that OYAKATA reads through the `opencode` CLI
/// (about a second per call), so listing and fetching run here, off the poll loop, and the
/// results are installed into the index as converted transcripts.
pub async fn opencode_loop(app: Shared) {
    loop {
        let exe = lock(&app).agents.iter().find(|a| a.kind == AgentKind::Opencode && a.enabled).and_then(|a| a.exe.clone());
        let mut remaining = 0;
        if let Some(exe) = exe {
            let exe2 = exe.clone();
            let found = tokio::task::spawn_blocking(move || agents::opencode::discover(&exe2)).await.unwrap_or_default();
            let stale = {
                let mut st = lock(&app);
                st.adopt(found);
                st.stale_opencode()
            };
            remaining = stale.len().saturating_sub(8);
            for (id, title, cwd, updated) in stale.into_iter().take(8) {
                let exe2 = exe.clone();
                let id2 = id.clone();
                let r = tokio::task::spawn_blocking(move || agents::opencode::fetch(&exe2, &id2, title.as_deref(), cwd.as_deref())).await;
                match r {
                    Ok(Ok(lines)) => {
                        let bytes = agents::canon::to_jsonl(&lines);
                        let events = lock(&app).install_canonical(&id, &bytes, updated);
                        for e in events {
                            let _ = app.tx.send(e);
                        }
                    }
                    _ => {
                        // Don't retry a failing session on every tick.
                        if let Some(e) = lock(&app).sessions.get_mut(&id) {
                            e.fetched_ms = e.updated_ms;
                        }
                    }
                }
            }
            push_sessions(&app);
        }
        tokio::time::sleep(Duration::from_secs(if remaining > 0 { 1 } else { 10 })).await;
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
        "i18n-en.js" => embedded!("i18n-en.js", JS, NO_CACHE),
        "i18n.js" => embedded!("i18n.js", JS, NO_CACHE),
        "app.js" => embedded!("app.js", JS, NO_CACHE),
        "workbench.js" => embedded!("workbench.js", JS, NO_CACHE),
        "chat.js" => embedded!("chat.js", JS, NO_CACHE),
        "team.js" => embedded!("team.js", JS, NO_CACHE),
        "fx.js" => embedded!("fx.js", JS, NO_CACHE),
        "code.js" => embedded!("code.js", JS, NO_CACHE),
        "palette.js" => embedded!("palette.js", JS, NO_CACHE),
        "search.js" => embedded!("search.js", JS, NO_CACHE),
        "editors.js" => embedded!("editors.js", JS, NO_CACHE),
        "sidebar.js" => embedded!("sidebar.js", JS, NO_CACHE),
        "worktrees.js" => embedded!("worktrees.js", JS, NO_CACHE),
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
    Json(config_json(&app))
}

fn config_json(app: &Shared) -> Value {
    let (claude_dir, ghq_root, repo_roots, lang, oyakata_dir, agents, worktree_default, worktree_root) = {
        let st = lock(app);
        (
            st.claude_dir.display().to_string(),
            st.ghq_root.as_ref().map(|p| p.display().to_string()),
            st.repo_roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            st.lang.clone(),
            st.oyakata_dir.display().to_string(),
            st.agents.clone(),
            st.worktree_default,
            st.worktrees_dir().display().to_string(),
        )
    };
    let home = crate::paths::home_dir();
    let clone_root = ghq_root.clone().unwrap_or_else(|| home.join("repos").display().to_string());
    let agents_json: Vec<Value> = agents
        .iter()
        .map(|a| {
            json!({
                "id": a.kind.id(),
                "label": a.label,
                "data_dir": a.data_dir.display().to_string(),
                "exe": a.exe_path,
                "installed": a.installed,
                "enabled": a.enabled,
                "can_run": a.enabled && a.exe.is_some(),
                // The agent assigns session ids itself (the browser learns the id on start).
                "assigns_id": matches!(a.kind, AgentKind::Codex | AgentKind::Opencode),
                // Permission prompts and mid-turn steering exist only for Claude Code.
                "interactive": a.kind == AgentKind::Claude,
            })
        })
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "claude_dir": claude_dir,
        "claude_exe": app.runners.exe(AgentKind::Claude).map(|e| e.display()),
        "can_run": app.runners.can_run(AgentKind::Claude),
        "can_type": cfg!(windows),
        "default_permission_mode": crate::runner::DEFAULT_PERMISSION_MODE,
        "home": home.display().to_string(),
        "ghq_root": ghq_root,
        "clone_root": clone_root,
        "path_sep": std::path::MAIN_SEPARATOR.to_string(),
        "repo_roots": repo_roots,
        "lang": lang,
        "oyakata_dir": oyakata_dir,
        "agents": agents_json,
        "worktree": worktree_default,
        "worktree_root": worktree_root,
    })
}

/// `{"lang": "ja" | "en" | null, "agents": {"codex": {"enabled": false}}}`.
async fn set_config(AxState(app): AxState<Shared>, Json(body): Json<Value>) -> Response {
    let app2 = app.clone();
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut st = lock(&app2);
        if let Some(l) = body.get("lang") {
            st.set_lang(l.as_str().map(str::to_string))?;
        }
        if let Some(w) = body.get("worktree").and_then(Value::as_bool) {
            st.worktree_default = w;
            st.save_config()?;
        }
        if let Some(map) = body.get("agents").and_then(Value::as_object) {
            for (k, v) in map {
                let Some(kind) = AgentKind::parse(k) else { continue };
                if let Some(enabled) = v.get("enabled").and_then(Value::as_bool) {
                    st.set_agent_enabled(kind, enabled)?;
                    app2.runners.set_enabled(kind, enabled);
                }
            }
        }
        Ok(())
    })
    .await;
    match r {
        Ok(Ok(())) => {
            push_sessions(&app);
            Json(config_json(&app)).into_response()
        }
        Ok(Err(e)) => err(StatusCode::BAD_REQUEST, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
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
/// Parse a session's whole transcript (converted for non-Claude agents) outside the lock,
/// then hand it to the index.
fn ensure_loaded(app: &Shared, id: &str) -> anyhow::Result<()> {
    let (path, loaded, agent, title, cwd, updated) = {
        let st = lock(app);
        let e = st.sessions.get(id).ok_or_else(|| anyhow::anyhow!("unknown session: {id}"))?;
        (e.path.clone(), e.transcript.keeps_items(), e.agent, e.title_hint.clone(), e.cwd_hint.clone(), e.updated_ms)
    };
    if loaded {
        lock(app).touch(id);
        return Ok(());
    }
    let bytes = match agent {
        AgentKind::Claude => std::fs::read(&path)?,
        AgentKind::Codex => {
            let raw = std::fs::read(&path)?;
            let mut conv = agents::codex::Converter::default();
            let mut lines = Vec::new();
            for l in raw.split(|&b| b == b'\n') {
                lines.extend(conv.convert_line(l));
            }
            agents::canon::to_jsonl(&lines)
        }
        AgentKind::Copilot => {
            let raw = std::fs::read(&path)?;
            let mut conv = agents::copilot::Converter::default();
            let mut lines = Vec::new();
            for l in raw.split(|&b| b == b'\n') {
                lines.extend(conv.convert_line(l));
            }
            agents::canon::to_jsonl(&lines)
        }
        AgentKind::Gemini => agents::canon::to_jsonl(&agents::gemini::convert_all(&std::fs::read(&path)?)),
        AgentKind::Opencode => {
            let exe = lock(app).agent_exe(AgentKind::Opencode).ok_or_else(|| anyhow::anyhow!("opencode executable not found"))?;
            agents::canon::to_jsonl(&agents::opencode::fetch(&exe, id, title.as_deref(), cwd.as_deref())?)
        }
    };
    let mut t = Transcript::new(true);
    t.feed(&bytes);
    let mut st = lock(app);
    st.install(id, t)?;
    if agent == AgentKind::Opencode {
        if let Some(e) = st.sessions.get_mut(id) {
            e.fetched_ms = updated;
            e.mtime_ms = updated;
        }
    }
    Ok(())
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

/// Broadcast the session list right away after a change the poll loop would otherwise only
/// notice on its next tick.
fn push_sessions(app: &Shared) {
    let ev = lock(app).take_sessions_event();
    if let Some(e) = ev {
        let _ = app.tx.send(e);
    }
}

async fn delete_session(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    // A session OYAKATA runs is ended first so it can be deleted from the browser; one
    // running in a terminal is still refused by `State::delete_session`.
    let stopped = app.runners.view(&id).is_some_and(|v| v.status != "exited");
    if stopped {
        if let Err(e) = app.runners.stop(&id).await {
            return err(StatusCode::CONFLICT, format!("{e:#}"));
        }
    }
    app.runners.forget_exited(&id);
    let app2 = app.clone();
    let r = tokio::task::spawn_blocking(move || {
        let mut st = lock(&app2);
        if stopped {
            // Don't wait for the run-events loop and the next poll to notice the exit, and
            // pick up a transcript the process only wrote as it exited.
            st.set_run(&id, None);
            st.refresh_live();
            st.discover();
            if !st.sessions.contains_key(&id) {
                return Ok(()); // it never wrote a transcript; ending it was all there was to do
            }
        }
        st.delete_session(&id)
    })
    .await;
    match r {
        Ok(Ok(())) => {
            push_sessions(&app);
            Json(json!({ "ok": true })).into_response()
        }
        Ok(Err(e)) => err(StatusCode::CONFLICT, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn restore_session(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    let app2 = app.clone();
    let r = tokio::task::spawn_blocking(move || lock(&app2).restore_session(&id)).await;
    match r {
        Ok(Ok(())) => {
            push_sessions(&app);
            Json(json!({ "ok": true })).into_response()
        }
        Ok(Err(e)) => err(StatusCode::CONFLICT, format!("{e:#}")),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn get_team(AxState(app): AxState<Shared>, Path(id): Path<String>) -> Response {
    let app2 = app.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        ensure_loaded(&app2, &id)?;
        let inputs = lock(&app2).team_inputs(&id)?;
        // Reading subagent transcripts can take a moment; keep the index unlocked meanwhile.
        let mut cache = app2.agent_cache.lock().unwrap_or_else(|e| e.into_inner());
        let agents = crate::index::build_team(&inputs, &mut cache);
        Ok(serde_json::to_vec(&json!({ "agents": agents }))?)
    })
    .await;
    json_bytes(result)
}

async fn add_repo(AxState(app): AxState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(path) = body_str(&body, "path").map(PathBuf::from) else { return bad("path is required") };
    if !path.is_absolute() {
        return bad(tr("フォルダは絶対パスで指定してください", "The folder must be an absolute path."));
    }
    let r = lock(&app).add_repo(path.clone());
    match r {
        Ok(()) => {
            let repos = lock(&app).repos();
            push_sessions(&app);
            Json(json!({ "ok": true, "root": path.display().to_string(), "repos": repos })).into_response()
        }
        Err(e) => bad(format!("{e:#}")),
    }
}

async fn remove_repo(AxState(app): AxState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(path) = body_str(&body, "path").map(str::to_string) else { return bad("path is required") };
    let r = lock(&app).remove_repo(&path);
    match r {
        Ok(removed) => {
            let repos = lock(&app).repos();
            push_sessions(&app);
            Json(json!({ "ok": true, "removed": removed, "repos": repos })).into_response()
        }
        Err(e) => bad(format!("{e:#}")),
    }
}

async fn clone_repo(AxState(app): AxState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(url) = body_str(&body, "url").map(str::to_string) else { return bad("url is required") };
    let dest = match body_str(&body, "dest") {
        Some(d) => PathBuf::from(d),
        None => {
            let (root, ghq) = {
                let st = lock(&app);
                match &st.ghq_root {
                    Some(g) => (g.clone(), true),
                    None => (crate::paths::home_dir().join("repos"), false),
                }
            };
            match gitops::clone_dest(&root, &url, ghq) {
                Some(d) => d,
                None => return bad(tr("URL からリポジトリ名を読み取れませんでした。clone 先を指定してください。", "Could not read a repository name from the URL. Specify where to clone.")),
            }
        }
    };
    if !dest.is_absolute() {
        return bad(tr("clone 先は絶対パスで指定してください", "The clone destination must be an absolute path."));
    }
    let dest2 = dest.clone();
    let out = match tokio::task::spawn_blocking(move || gitops::clone(&url, &dest2)).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return bad(format!("{e:#}")),
        Err(e) => return err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    };
    if let Err(e) = lock(&app).add_repo(dest.clone()) {
        return bad(format!("{e:#}"));
    }
    let repos = lock(&app).repos();
    push_sessions(&app);
    Json(json!({ "ok": true, "root": dest.display().to_string(), "output": out, "repos": repos })).into_response()
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
    let tracked_only = q.get("tracked").map(|v| v == "1").unwrap_or(false);
    blocking_json(move || gitops::tree(&root, tracked_only).map(|files| json!({ "files": files }))).await
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

async fn git_grep(Query(q): Query<HashMap<String, String>>) -> Response {
    let root = match repo_root(&q) { Ok(r) => r, Err(e) => return e };
    let flag = |k: &str| q.get(k).map(|v| v == "1" || v == "true").unwrap_or(false);
    let Some(pattern) = q.get("q").filter(|s| !s.is_empty()).cloned() else { return bad("q is required") };
    let opts = gitops::GrepOptions {
        pattern,
        regex: flag("regex"),
        pcre: flag("pcre"),
        case_sensitive: flag("case"),
        word: flag("word"),
        pathspecs: q.get("glob").map(|g| g.split(',').map(str::to_string).collect()).unwrap_or_default(),
        max: q.get("max").and_then(|s| s.parse().ok()).unwrap_or(2000).clamp(1, 10_000),
    };
    blocking_json(move || gitops::grep(&root, &opts).map(|(matches, truncated)| json!({ "matches": matches, "truncated": truncated }))).await
}

async fn search_sessions(AxState(app): AxState<Shared>, Query(q): Query<HashMap<String, String>>) -> Response {
    let Some(query) = q.get("q").map(|s| s.trim().to_string()).filter(|s| s.chars().count() >= 2) else {
        return bad(tr("2 文字以上で検索してください", "Enter at least 2 characters."));
    };
    let files = lock(&app).transcript_files();
    blocking_json(move || Ok(json!({ "hits": crate::index::search_transcripts(&files, &query, 300, 5) }))).await
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
    let session_id = body_str(&body, "session_id").map(str::to_string);
    let agent = match body_str(&body, "agent").and_then(AgentKind::parse) {
        Some(a) => a,
        None => resume.as_ref().and_then(|id| lock(&app).sessions.get(id).map(|e| e.agent)).unwrap_or(AgentKind::Claude),
    };
    if let Some(id) = &session_id {
        if lock(&app).sessions.contains_key(id) && resume.is_none() {
            return err(StatusCode::CONFLICT, tr("そのセッション ID は既に使われています。", "That session id is already in use."));
        }
    }
    if let Some(id) = &resume {
        app.runners.forget_exited(id);
        let st = lock(&app);
        if st.live.contains_key(id) {
            return err(StatusCode::CONFLICT, tr("そのセッションはターミナルで稼働中です。終了してから引き継いでください。", "That session is running in a terminal. End it there before taking it over."));
        }
        if let Some(e) = st.sessions.get(id) {
            if e.agent != AgentKind::Claude && st.summary_of(e).owner.as_deref() == Some("external") {
                return err(StatusCode::CONFLICT, tr("そのセッションは別のプロセスで稼働中です。終わってから送ってください。", "That session is running in another process. Wait for it to finish."));
            }
        }
    }
    if let Some(id) = &resume {
        let missing = lock(&app).sessions.get(id).map(|_| !std::path::Path::new(cwd).is_dir()).unwrap_or(false);
        if missing {
            return err(
                StatusCode::CONFLICT,
                if crate::i18n::is_ja() {
                    format!("このセッションの作業フォルダ（{cwd}）がありません。worktree を削除した場合は、新しいセッションとして始めてください。")
                } else {
                    format!("This session's working folder ({cwd}) is gone. If its worktree was removed, start a new session instead.")
                },
            );
        }
    }
    // A new session in a git repository works in its own worktree unless asked not to.
    let mut cwd = PathBuf::from(cwd);
    let mut worktree: Option<(PathBuf, String)> = None;
    let mut note: Option<String> = None;
    let want_worktree = resume.is_none() && body.get("worktree").and_then(Value::as_bool).unwrap_or_else(|| lock(&app).worktree_default);
    if want_worktree {
        let root_dir = lock(&app).worktrees_dir();
        let src = cwd.clone();
        let slug_src = prompt.to_string();
        let made = tokio::task::spawn_blocking(move || create_worktree(&src, &root_dir, slug_src)).await;
        match made {
            Ok(Ok(Some((new_cwd, path, branch)))) => {
                cwd = new_cwd;
                worktree = Some((path, branch));
            }
            Ok(Ok(None)) => {} // not a git repository: run in the folder itself
            Ok(Err(e)) => note = Some(format!("{}: {e:#}", tr("worktree を作れなかったため、元のフォルダで始めました", "Could not create a worktree, so the session runs in the folder itself"))),
            Err(e) => note = Some(e.to_string()),
        }
    }
    let opts = StartOptions {
        agent,
        cwd: cwd.clone(),
        prompt: prompt.to_string(),
        resume,
        session_id,
        model: body_str(&body, "model").map(str::to_string),
        permission_mode: body_str(&body, "permission_mode").map(str::to_string),
        effort: body_str(&body, "effort").map(str::to_string),
    };
    match app.runners.start(opts).await {
        Ok(id) => Json(json!({
            "session_id": id,
            "cwd": cwd.display().to_string(),
            "worktree": worktree.as_ref().map(|(p, _)| p.display().to_string()),
            "branch": worktree.as_ref().map(|(_, b)| b.clone()),
            "note": note,
        }))
        .into_response(),
        Err(e) => {
            // Don't leave an unused worktree behind when the agent failed to start.
            if let Some((path, _)) = &worktree {
                if let Some(main) = crate::repo::detect(&path.to_string_lossy()).root {
                    let _ = gitops::remove_worktree(std::path::Path::new(&main), path, true, true);
                }
            }
            err(StatusCode::BAD_REQUEST, format!("{e:#}"))
        }
    }
}

/// Create `<worktrees>/<repo>/<name>` on a new branch `oyakata/<name>` at the repository's
/// current commit. Returns (cwd inside the worktree, worktree root, branch), or `None` when
/// `src` is not inside a git repository.
fn create_worktree(src: &std::path::Path, worktrees: &std::path::Path, prompt: String) -> anyhow::Result<Option<(PathBuf, PathBuf, String)>> {
    let Some(top) = gitops::toplevel(src) else { return Ok(None) };
    let info = crate::repo::detect(&top.to_string_lossy());
    // Starting from inside a worktree branches off that worktree's checkout but files the new
    // one under the main repository's name.
    let repo_name: String = info.name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    let name = worktree_name(&prompt);
    let dest = worktrees.join(repo_name.trim_matches('-')).join(&name);
    let branch = format!("oyakata/{name}");
    gitops::add_worktree(&top, &dest, &branch)?;
    let rel = src.strip_prefix(&top).ok().filter(|r| !r.as_os_str().is_empty());
    let cwd = match rel {
        Some(r) if dest.join(r).is_dir() => dest.join(r),
        _ => dest.clone(),
    };
    Ok(Some((cwd, dest, branch)))
}

/// `fix-the-parser-3fa2` from an English prompt, `20261008-0412-3fa2` otherwise.
fn worktree_name(prompt: &str) -> String {
    let words: Vec<String> = prompt
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(4)
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let mut slug = words.join("-");
    slug.truncate(32);
    let slug = slug.trim_matches('-').to_string();
    let suffix: String = uuid::Uuid::new_v4().simple().to_string().chars().take(4).collect();
    if slug.len() >= 3 {
        format!("{slug}-{suffix}")
    } else {
        let iso = crate::agents::canon::ms_to_iso(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
        );
        // 2026-10-08T04:12:33.000Z -> 20261008-0412
        let stamp: String = iso.chars().filter(|c| c.is_ascii_digit()).take(12).collect();
        format!("{}-{}-{suffix}", &stamp[..8.min(stamp.len())], &stamp[8.min(stamp.len())..])
    }
}

#[cfg(test)]
mod worktree_tests {
    use super::worktree_name;

    #[test]
    fn names_come_from_english_prompts_or_the_time() {
        let n = worktree_name("Fix the parser bug, please");
        assert!(n.starts_with("fix-the-parser-bug-") && n.len() == "fix-the-parser-bug-".len() + 4, "{n}");
        let j = worktree_name("パーサーを直して");
        let parts: Vec<&str> = j.split('-').collect();
        assert_eq!(parts.len(), 3, "{j}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 4);
        assert!(parts[0].chars().chain(parts[1].chars()).all(|c| c.is_ascii_digit()));
        assert_eq!(worktree_name("ok").split('-').count(), 3, "too short for a slug");
    }
}

/// `{"path": "<worktree>", "force": false}`: remove a linked worktree (not while a session
/// runs in it) and delete its branch when merged.
/// Linked worktrees of every known repository that has any, with uncommitted changes and
/// commits not on the main checkout's branch, so the browser can tell what removal loses.
async fn worktree_list(AxState(app): AxState<Shared>) -> Response {
    let (roots, dir) = {
        let mut st = lock(&app);
        let mut seen = std::collections::HashSet::new();
        let mut roots: Vec<(String, String)> = Vec::new();
        for r in st.repos() {
            if r.is_git && seen.insert(crate::repo::normalize(std::path::Path::new(&r.root))) {
                roots.push((r.root, r.name));
            }
        }
        for s in st.summaries() {
            if let (Some(_), Some(root)) = (&s.repo.worktree, &s.repo.root) {
                if seen.insert(crate::repo::normalize(std::path::Path::new(root))) {
                    roots.push((root.clone(), s.repo.name.clone()));
                }
            }
        }
        (roots, st.worktrees_dir())
    };
    let dir_key = crate::repo::normalize(&dir);
    let r = tokio::task::spawn_blocking(move || {
        let roots: Vec<(String, String)> = roots.into_iter().filter(|(root, _)| gitops::has_linked_worktrees(std::path::Path::new(root))).collect();
        std::thread::scope(|sc| {
            let handles: Vec<_> = roots.iter().map(|(root, name)| sc.spawn(|| repo_worktrees(root, name, &dir_key))).collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<Value>>()
        })
    })
    .await;
    match r {
        Ok(repos) => Json(json!({ "repos": repos, "dir": dir.display().to_string() })).into_response(),
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

fn repo_worktrees(root: &str, name: &str, dir_key: &str) -> Value {
    match gitops::worktree_states(std::path::Path::new(root)) {
        Ok((base, list)) => {
            let worktrees: Vec<Value> = list
                .iter()
                .map(|w| {
                    let mut v = serde_json::to_value(w).unwrap_or(Value::Null);
                    // Created by OYAKATA (under its worktree folder) rather than by hand.
                    v["oyakata"] = Value::Bool(crate::repo::normalize(std::path::Path::new(&w.wt.path)).starts_with(&format!("{dir_key}/")));
                    v
                })
                .collect();
            json!({ "root": root, "name": name, "base": base, "worktrees": worktrees })
        }
        Err(e) => json!({ "root": root, "name": name, "error": format!("{e:#}"), "worktrees": [] }),
    }
}

async fn worktree_remove(AxState(app): AxState<Shared>, Json(body): Json<Value>) -> Response {
    let Some(path) = body_str(&body, "path").map(PathBuf::from) else { return bad("path is required") };
    let force = body.get("force").and_then(Value::as_bool).unwrap_or(false);
    // Also delete a branch that is not merged (`git branch -D`).
    let delete_branch = body.get("delete_branch").and_then(Value::as_bool).unwrap_or(false);
    let busy = {
        let st = lock(&app);
        let key = crate::repo::normalize(&path);
        st.summaries().iter().any(|s| {
            s.status != "ended"
                && s.cwd.as_deref().map(|c| {
                    let n = crate::repo::normalize(std::path::Path::new(c));
                    n == key || n.starts_with(&format!("{key}/"))
                }).unwrap_or(false)
        })
    };
    if busy {
        return err(StatusCode::CONFLICT, tr("この worktree ではセッションが動いています。終わってから削除してください。", "A session is running in this worktree. Wait for it to finish."));
    }
    let r = tokio::task::spawn_blocking(move || -> anyhow::Result<gitops::WorktreeRemoval> {
        // A worktree whose folder is gone can't be detected from the folder: find it in the
        // repository given by the browser.
        let main = body_str(&body, "root").map(str::to_string).or_else(|| crate::repo::detect(&path.to_string_lossy()).root).ok_or_else(|| anyhow::anyhow!("{} is not in a git repository", path.display()))?;
        gitops::remove_worktree(std::path::Path::new(&main), &path, force, delete_branch)
    })
    .await;
    match r {
        Ok(Ok(v)) => {
            push_sessions(&app);
            Json(json!({ "ok": true, "branch": v.branch, "branch_deleted": v.branch_deleted })).into_response()
        }
        Ok(Err(e)) => {
            let msg = format!("{e:#}");
            // git refuses when there are uncommitted changes; the UI then offers --force.
            let dirty = msg.contains("modified or untracked") || msg.contains("contains modified") || msg.contains("is dirty");
            (StatusCode::CONFLICT, Json(json!({ "error": msg, "dirty": dirty }))).into_response()
        }
        Err(e) => err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
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

async fn run_mode(AxState(app): AxState<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let Some(mode) = body_str(&body, "mode") else { return bad("mode is required") };
    match app.runners.set_permission_mode(&id, mode).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => err(StatusCode::CONFLICT, format!("{e:#}")),
    }
}

async fn run_model(AxState(app): AxState<Shared>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let model = body_str(&body, "model");
    match app.runners.set_model(&id, model).await {
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
        return Err(err(StatusCode::CONFLICT, tr("このセッションは OYAKATA が実行しています。", "OYAKATA is running this session.")));
    }
    let dir = lock(app).sessions_dir();
    let Some(l) = live::read_registry(&dir).remove(id) else {
        return Err(err(StatusCode::CONFLICT, tr("このセッションはターミナルで稼働していません。", "This session is not running in a terminal.")));
    };
    if !l.typeable {
        return Err(err(StatusCode::CONFLICT, tr("このセッションはターミナル以外（IDE や SDK）で動いているため、ここからは送れません。", "This session runs outside a terminal (IDE or SDK), so nothing can be typed into it from here.")));
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
        return err(StatusCode::CONFLICT, tr("ターミナルで確認待ちです。ターミナル側で答えてから送ってください。", "The terminal is waiting for a confirmation. Answer it there first."));
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
        return err(StatusCode::CONFLICT, tr("Claude は作業中ではありません。", "Claude is not working right now."));
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
