//! OYAKATA-owned Claude Code sessions.
//!
//! A session started from the browser is a `claude -p --input-format stream-json` child
//! process. Prompts go in on stdin, permission prompts come back as `control_request` lines
//! on stdout and are answered with `control_response`. The conversation itself is still
//! written by Claude Code to `~/.claude/projects`, so the viewer renders it like any other.

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, Mutex as AsyncMutex};

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub display_name: Option<String>,
    pub input: Value,
    pub description: Option<String>,
    pub suggestions: Value,
    pub tool_use_id: Option<String>,
    pub requires_user_interaction: bool,
    pub received_at: u64,
}

/// A prompt written to stdin while Claude was busy. Claude Code holds it and takes it up at
/// the next tool boundary (steering), or as the next turn if the current one ends first.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QueuedMessage {
    pub text: String,
    pub sent_at: u64,
}

/// How long after a turn ends we wait for Claude to take up a still-queued prompt before
/// concluding it was dropped. Normally the next turn's replay arrives within ~2 s.
const QUEUE_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
pub struct RunView {
    pub session_id: String,
    pub cwd: String,
    /// `busy` | `idle` | `waiting` | `exited`
    pub status: String,
    pub pending: Vec<PermissionRequest>,
    /// Prompts sent that Claude has not taken up yet (`--replay-user-messages` echoes each one
    /// back the moment it does).
    pub queued: Vec<QueuedMessage>,
    /// When a turn ended with prompts still queued: the time of that `result`, so the
    /// watchdog can tell whether Claude has moved on since.
    #[serde(skip)]
    awaiting_queued: Option<u64>,
    pub started_at: u64,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
    pub resumed: bool,
    pub turns: u32,
    pub total_cost_usd: f64,
    /// Context window of the model, as reported by Claude Code at the end of a turn.
    pub context_window: Option<u64>,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub stderr_tail: Vec<String>,
}

pub struct Runner {
    view: Arc<Mutex<RunView>>,
    stdin: Arc<AsyncMutex<Option<ChildStdin>>>,
    child: Arc<AsyncMutex<Child>>,
}

#[derive(Debug, Clone)]
pub enum ClaudeExe {
    Direct(PathBuf),
    /// A `.cmd` shim (npm on Windows) that has to go through `cmd /C`.
    Cmd(PathBuf),
}

impl ClaudeExe {
    pub fn display(&self) -> String {
        match self {
            ClaudeExe::Direct(p) | ClaudeExe::Cmd(p) => p.display().to_string(),
        }
    }
}

/// Permission mode for sessions OYAKATA starts when the caller does not pick one.
pub const DEFAULT_PERMISSION_MODE: &str = "auto";

pub struct StartOptions {
    pub cwd: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
    /// Id for a new session, chosen by the browser so it can show the chat before Claude
    /// writes anything. Ignored when resuming.
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
}

pub struct RunnerRegistry {
    runners: Mutex<HashMap<String, Arc<Runner>>>,
    exe: Option<ClaudeExe>,
    /// Session ids whose run state changed; the server turns these into SSE events.
    pub changed: broadcast::Sender<String>,
}

impl RunnerRegistry {
    pub fn new(exe: Option<ClaudeExe>) -> Self {
        let (changed, _) = broadcast::channel(256);
        Self { runners: Mutex::new(HashMap::new()), exe, changed }
    }

    pub fn exe(&self) -> Option<&ClaudeExe> {
        self.exe.as_ref()
    }

    pub fn views(&self) -> Vec<RunView> {
        let map = self.runners.lock().unwrap_or_else(|e| e.into_inner());
        let mut v: Vec<RunView> = map.values().map(|r| r.view.lock().unwrap_or_else(|e| e.into_inner()).clone()).collect();
        v.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        v
    }

    pub fn view(&self, id: &str) -> Option<RunView> {
        let map = self.runners.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).map(|r| r.view.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    fn get(&self, id: &str) -> Result<Arc<Runner>> {
        let map = self.runners.lock().unwrap_or_else(|e| e.into_inner());
        map.get(id).cloned().ok_or_else(|| anyhow!("no OYAKATA-run session {id}"))
    }

    fn notify(&self, id: &str) {
        let _ = self.changed.send(id.to_string());
    }

    pub async fn start(self: &Arc<Self>, opts: StartOptions) -> Result<String> {
        let exe = self.exe.clone().ok_or_else(|| anyhow!("claude executable not found; set --claude or OYAKATA_CLAUDE"))?;
        if !opts.cwd.is_dir() {
            bail!("working directory does not exist: {}", opts.cwd.display());
        }
        let session_id = match (&opts.resume, &opts.session_id) {
            (Some(id), _) => id.clone(),
            (None, Some(id)) => {
                uuid::Uuid::parse_str(id).map_err(|_| anyhow!("session id must be a UUID"))?;
                id.to_lowercase()
            }
            (None, None) => uuid::Uuid::new_v4().to_string(),
        };
        let permission_mode = opts.permission_mode.clone().unwrap_or_else(|| DEFAULT_PERMISSION_MODE.to_string());
        if self.view(&session_id).map(|v| v.status != "exited").unwrap_or(false) {
            bail!("session {session_id} is already running under OYAKATA");
        }

        let mut cmd = match &exe {
            ClaudeExe::Direct(p) => Command::new(p),
            ClaudeExe::Cmd(p) => {
                let mut c = Command::new("cmd");
                c.arg("/C").arg(p);
                c
            }
        };
        cmd.args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--replay-user-messages",
            "--permission-prompt-tool",
            "stdio",
        ]);
        if opts.resume.is_some() {
            cmd.args(["--resume", &session_id]);
        } else {
            cmd.args(["--session-id", &session_id]);
        }
        if let Some(m) = &opts.model {
            cmd.args(["--model", m]);
        }
        cmd.args(["--permission-mode", &permission_mode]);
        if let Some(e) = &opts.effort {
            cmd.args(["--effort", e]);
        }
        cmd.current_dir(&opts.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // When OYAKATA itself was launched from inside a Claude Code session, the child must
        // not inherit that session's identity.
        for (k, _) in std::env::vars_os() {
            let key = k.to_string_lossy();
            if key.starts_with("CLAUDE") && key != "CLAUDE_CONFIG_DIR" {
                cmd.env_remove(&k);
            }
        }
        #[cfg(windows)]
        {
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = cmd.spawn().with_context(|| format!("spawn {}", exe.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let view = Arc::new(Mutex::new(RunView {
            session_id: session_id.clone(),
            cwd: opts.cwd.display().to_string(),
            status: "busy".into(),
            pending: Vec::new(),
            queued: Vec::new(),
            awaiting_queued: None,
            started_at: now_ms(),
            model: opts.model.clone(),
            permission_mode: Some(permission_mode.clone()),
            effort: opts.effort.clone(),
            resumed: opts.resume.is_some(),
            turns: 0,
            total_cost_usd: 0.0,
            context_window: None,
            exit_code: None,
            last_error: None,
            stderr_tail: Vec::new(),
        }));
        let runner = Arc::new(Runner {
            view: view.clone(),
            stdin: Arc::new(AsyncMutex::new(Some(stdin))),
            child: Arc::new(AsyncMutex::new(child)),
        });
        self.runners.lock().unwrap_or_else(|e| e.into_inner()).insert(session_id.clone(), runner.clone());

        runner.write(&user_message(&opts.prompt)).await?;
        self.notify(&session_id);

        // stdout reader
        {
            let reg = self.clone();
            let view = view.clone();
            let id = session_id.clone();
            let child = runner.child.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if handle_event(&view, &v) {
                            reg.notify(&id);
                        }
                        if v.get("type").and_then(Value::as_str) == Some("result") {
                            let stamp = view.lock().unwrap_or_else(|e| e.into_inner()).awaiting_queued;
                            if let Some(stamp) = stamp {
                                queue_watchdog(reg.clone(), view.clone(), id.clone(), stamp);
                            }
                        }
                    }
                }
                let code = child.lock().await.wait().await.ok().and_then(|s| s.code());
                {
                    let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
                    v.status = "exited".into();
                    v.exit_code = code;
                    if code.unwrap_or(0) != 0 && v.last_error.is_none() {
                        v.last_error = Some(format!("claude exited with code {}", code.unwrap_or(-1)));
                    }
                }
                reg.notify(&id);
            });
        }
        // stderr reader (kept for diagnostics only)
        {
            let view = view.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
                    v.stderr_tail.push(line);
                    if v.stderr_tail.len() > 40 {
                        v.stderr_tail.remove(0);
                    }
                }
            });
        }
        Ok(session_id)
    }

    /// Send a prompt. While Claude is busy it still goes to stdin: Claude Code holds it and
    /// takes it up at the next tool boundary (or as the next turn), like typing into the
    /// terminal mid-task. Until then it is listed in `queued`.
    pub async fn send(&self, id: &str, text: &str) -> Result<()> {
        let r = self.get(id)?;
        {
            let v = r.view.lock().unwrap_or_else(|e| e.into_inner());
            match v.status.as_str() {
                "idle" | "busy" => {}
                "exited" => bail!("session has exited; resume it to continue"),
                _ => bail!("answer the pending permission request first"),
            }
        }
        r.write(&user_message(text)).await?;
        {
            let mut v = r.view.lock().unwrap_or_else(|e| e.into_inner());
            v.queued.push(QueuedMessage { text: text.to_string(), sent_at: now_ms() });
            v.status = "busy".into();
            v.last_error = None;
        }
        self.notify(id);
        Ok(())
    }

    /// Answer a `can_use_tool` request. `response` is the inner response object
    /// (`{"behavior":"allow", ...}` or `{"behavior":"deny", ...}`).
    pub async fn respond_permission(&self, id: &str, request_id: &str, response: Value) -> Result<()> {
        let r = self.get(id)?;
        {
            let mut v = r.view.lock().unwrap_or_else(|e| e.into_inner());
            let before = v.pending.len();
            v.pending.retain(|p| p.request_id != request_id);
            if v.pending.len() == before {
                bail!("no pending request {request_id}");
            }
            if v.pending.is_empty() {
                v.status = "busy".into();
            }
        }
        r.write(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response }
        }))
        .await?;
        self.notify(id);
        Ok(())
    }

    pub async fn interrupt(&self, id: &str) -> Result<()> {
        let r = self.get(id)?;
        r.write(&json!({
            "type": "control_request",
            "request_id": uuid::Uuid::new_v4().to_string(),
            "request": { "subtype": "interrupt" }
        }))
        .await?;
        Ok(())
    }

    /// Switch the permission mode of a running session (what Shift+Tab does in a terminal).
    pub async fn set_permission_mode(&self, id: &str, mode: &str) -> Result<()> {
        const MODES: &[&str] = &["default", "acceptEdits", "auto", "plan", "bypassPermissions", "dontAsk"];
        if !MODES.contains(&mode) {
            bail!("unknown permission mode: {mode}");
        }
        let r = self.get(id)?;
        r.write(&json!({
            "type": "control_request",
            "request_id": uuid::Uuid::new_v4().to_string(),
            "request": { "subtype": "set_permission_mode", "mode": mode }
        }))
        .await?;
        r.view.lock().unwrap_or_else(|e| e.into_inner()).permission_mode = Some(mode.to_string());
        self.notify(id);
        Ok(())
    }

    /// Switch the model of a running session (`/model`). `None` returns to the default.
    pub async fn set_model(&self, id: &str, model: Option<&str>) -> Result<()> {
        if let Some(m) = model {
            if m.is_empty() || m.starts_with('-') || m.chars().any(char::is_whitespace) {
                bail!("invalid model name");
            }
        }
        let r = self.get(id)?;
        let mut request = json!({ "subtype": "set_model" });
        if let Some(m) = model {
            request["model"] = Value::String(m.to_string());
        }
        r.write(&json!({ "type": "control_request", "request_id": uuid::Uuid::new_v4().to_string(), "request": request })).await?;
        r.view.lock().unwrap_or_else(|e| e.into_inner()).model = model.map(str::to_string);
        self.notify(id);
        Ok(())
    }

    pub async fn stop(&self, id: &str) -> Result<()> {
        let r = self.get(id)?;
        r.shutdown().await;
        self.notify(id);
        Ok(())
    }

    pub async fn stop_all(&self) {
        let all: Vec<Arc<Runner>> = self.runners.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        for r in all {
            r.shutdown().await;
        }
    }

    /// Forget exited sessions so a resume can start a fresh process under the same id.
    pub fn forget_exited(&self, id: &str) {
        let mut map = self.runners.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = map.get(id) {
            if r.view.lock().unwrap_or_else(|e| e.into_inner()).status == "exited" {
                map.remove(id);
            }
        }
    }
}

impl Runner {
    async fn write(&self, v: &Value) -> Result<()> {
        let mut guard = self.stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| anyhow!("session stdin is closed"))?;
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        stdin.write_all(&line).await.context("write to claude stdin")?;
        stdin.flush().await.context("flush claude stdin")?;
        Ok(())
    }

    /// Close stdin (Claude Code exits at end of input), then kill if it lingers.
    async fn shutdown(&self) {
        {
            let mut guard = self.stdin.lock().await;
            guard.take();
        }
        let mut child = self.child.lock().await;
        let waited = tokio::time::timeout(Duration::from_secs(4), child.wait()).await;
        if waited.is_err() {
            let _ = child.kill().await;
        }
        let mut v = self.view.lock().unwrap_or_else(|e| e.into_inner());
        if v.status != "exited" {
            v.status = "exited".into();
        }
    }
}

/// Update the view from one stdout event; returns true when something the UI shows changed.
fn handle_event(view: &Arc<Mutex<RunView>>, v: &Value) -> bool {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    let mut view = view.lock().unwrap_or_else(|e| e.into_inner());
    match kind {
        "control_request" => {
            let req = v.get("request").cloned().unwrap_or(Value::Null);
            if req.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
                return false;
            }
            let s = |k: &str| req.get(k).and_then(Value::as_str).map(str::to_string);
            view.pending.push(PermissionRequest {
                request_id: v.get("request_id").and_then(Value::as_str).unwrap_or("").to_string(),
                tool_name: s("tool_name").unwrap_or_else(|| "tool".into()),
                display_name: s("display_name"),
                input: req.get("input").cloned().unwrap_or(Value::Null),
                description: s("description"),
                suggestions: req.get("permission_suggestions").cloned().unwrap_or(Value::Null),
                tool_use_id: s("tool_use_id"),
                requires_user_interaction: req.get("requires_user_interaction").and_then(Value::as_bool).unwrap_or(false),
                received_at: now_ms(),
            });
            view.status = "waiting".into();
            true
        }
        "result" => {
            if view.queued.is_empty() {
                view.status = "idle".into();
                view.awaiting_queued = None;
            } else {
                // Claude Code takes the queued prompts up as the next turn: stay busy rather
                // than flashing idle (and ringing the done chime) in between.
                view.status = "busy".into();
                view.awaiting_queued = Some(now_ms());
            }
            view.turns += 1;
            if let Some(c) = v.get("total_cost_usd").and_then(Value::as_f64) {
                view.total_cost_usd = c;
            }
            // {"modelUsage": {"<model>": {"contextWindow": 1000000, ...}}}
            let window = v
                .get("modelUsage")
                .and_then(Value::as_object)
                .and_then(|m| m.values().filter_map(|u| u.get("contextWindow").and_then(Value::as_u64)).max());
            if window.is_some() {
                view.context_window = window;
            }
            let subtype = v.get("subtype").and_then(Value::as_str).unwrap_or("");
            if subtype.starts_with("error") {
                view.last_error = Some(
                    v.get("result")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| subtype.to_string()),
                );
            }
            true
        }
        "assistant" => {
            view.awaiting_queued = None;
            if view.status == "idle" {
                view.status = "busy".into();
                return true;
            }
            false
        }
        "user" => {
            // `--replay-user-messages`: Claude Code echoes a prompt from stdin at the moment it
            // takes it up, which is when it leaves our queue.
            if v.get("isReplay").and_then(Value::as_bool) != Some(true) {
                return false;
            }
            view.awaiting_queued = None;
            let text = prompt_text(v);
            let pos = view
                .queued
                .iter()
                .position(|q| text.as_deref() == Some(q.text.as_str()))
                .or_else(|| (!view.queued.is_empty()).then_some(0));
            let mut changed = false;
            if let Some(i) = pos {
                view.queued.remove(i);
                changed = true;
            }
            if view.status == "idle" {
                view.status = "busy".into();
                changed = true;
            }
            changed
        }
        "system" => {
            if v.get("subtype").and_then(Value::as_str) == Some("init") {
                if let Some(m) = v.get("model").and_then(Value::as_str) {
                    view.model = Some(m.to_string());
                }
                if let Some(m) = v.get("permissionMode").and_then(Value::as_str) {
                    view.permission_mode = Some(m.to_string());
                }
                return true;
            }
            false
        }
        "control_response" => {
            // Answers to our own requests (mode / model switches); surface failures.
            let r = v.get("response").cloned().unwrap_or(Value::Null);
            if r.get("subtype").and_then(Value::as_str) == Some("error") {
                view.last_error = Some(r.get("error").and_then(Value::as_str).unwrap_or("Claude Code rejected the request").to_string());
                return true;
            }
            false
        }
        _ => false,
    }
}

fn user_message(text: &str) -> Value {
    json!({ "type": "user", "message": { "role": "user", "content": text } })
}

/// The text of a user message as Claude Code replays it (a string, or text blocks).
fn prompt_text(v: &Value) -> Option<String> {
    match v.get("message")?.get("content")? {
        Value::String(s) => Some(s.clone()),
        Value::Array(blocks) => {
            let texts: Vec<&str> = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect();
            (!texts.is_empty()).then(|| texts.join("\n"))
        }
        _ => None,
    }
}

/// A turn ended while prompts were still queued. If Claude has not taken one up (or started
/// anything else) within the grace period, treat them as dropped: go idle and say so.
fn queue_watchdog(reg: Arc<RunnerRegistry>, view: Arc<Mutex<RunView>>, id: String, stamp: u64) {
    tokio::spawn(async move {
        tokio::time::sleep(QUEUE_GRACE).await;
        {
            let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
            if v.awaiting_queued != Some(stamp) || v.status != "busy" {
                return;
            }
            v.awaiting_queued = None;
            v.status = "idle".into();
            let dropped = std::mem::take(&mut v.queued).len();
            if dropped > 0 {
                v.last_error = Some(format!(
                    "作業中に送った指示 {dropped} 件が Claude に渡りませんでした。もう一度送ってください。"
                ));
            }
        }
        reg.notify(&id);
    });
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Locate the `claude` executable the way a shell would, plus the common install locations.
pub fn find_claude(explicit: Option<PathBuf>) -> Option<ClaudeExe> {
    if let Some(p) = explicit.or_else(|| std::env::var_os("OYAKATA_CLAUDE").map(PathBuf::from)) {
        return Some(classify(p));
    }
    if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECPATH").map(PathBuf::from) {
        if p.is_file() {
            return Some(classify(p));
        }
    }
    let names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd"] } else { &["claude"] };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for n in names {
                let p = dir.join(n);
                if p.is_file() {
                    return Some(classify(p));
                }
            }
        }
    }
    let home = crate::paths::home_dir();
    let mut candidates = vec![home.join(".local").join("bin").join(if cfg!(windows) { "claude.exe" } else { "claude" })];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        candidates.push(Path::new(&appdata).join("npm").join("claude.cmd"));
    }
    candidates.into_iter().find(|p| p.is_file()).map(classify)
}

fn classify(p: PathBuf) -> ClaudeExe {
    if p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")).unwrap_or(false) {
        ClaudeExe::Cmd(p)
    } else {
        ClaudeExe::Direct(p)
    }
}
