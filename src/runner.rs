//! OYAKATA-owned sessions.
//!
//! Claude Code runs as a long-lived `claude -p --input-format stream-json` child: prompts go
//! in on stdin, permission prompts come back as `control_request` lines and are answered with
//! `control_response`. Every other agent is driven one turn at a time: `codex exec`,
//! `gemini`, `copilot` and `opencode run` each run to completion with their JSON event
//! stream on stdout and the prompt on stdin, and the next prompt resumes the same session.
//! In both cases the agent writes the conversation itself, so the viewer renders it like any
//! other session.

use crate::agents::{self, AgentInfo, AgentKind, Exe, ExecEvent, ExecOpts};
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{broadcast, oneshot, Mutex as AsyncMutex};

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

/// A prompt written while the agent was busy. Claude Code holds it and takes it up at the
/// next tool boundary (steering), or as the next turn; other agents get it as the next turn
/// once the current one ends.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct QueuedMessage {
    pub text: String,
    pub sent_at: u64,
}

/// How long after a turn ends we wait for Claude to take up a still-queued prompt before
/// concluding it was dropped. Normally the next turn's replay arrives within ~2 s.
const QUEUE_GRACE: Duration = Duration::from_secs(10);

/// How long to wait for an agent that assigns its own session id (Codex, OpenCode) to say
/// what it is before giving up on the start.
const ID_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Serialize)]
pub struct RunView {
    pub session_id: String,
    pub agent: AgentKind,
    pub cwd: String,
    /// `busy` | `idle` | `waiting` | `exited`
    pub status: String,
    pub pending: Vec<PermissionRequest>,
    /// Prompts sent that the agent has not taken up yet.
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
    /// Context window of the model, as reported by the agent.
    pub context_window: Option<u64>,
    pub exit_code: Option<i32>,
    pub last_error: Option<String>,
    pub stderr_tail: Vec<String>,
    /// What the agent is doing right now, from its event stream (agents other than Claude,
    /// whose transcript shows it directly).
    pub last_tool: Option<String>,
}

/// Permission mode for sessions OYAKATA starts when the caller does not pick one.
pub const DEFAULT_PERMISSION_MODE: &str = "auto";

pub struct StartOptions {
    pub agent: AgentKind,
    pub cwd: PathBuf,
    pub prompt: String,
    pub resume: Option<String>,
    /// Id for a new session, chosen by the browser so it can show the chat before the agent
    /// writes anything. Ignored when resuming, and by agents that assign their own ids.
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
}

/// The current turn of a one-turn-per-process agent.
struct ExecState {
    kind: AgentKind,
    exe: Exe,
    cwd: PathBuf,
    child: Option<Child>,
    queue: VecDeque<String>,
    /// Tells `start` the id the agent assigned.
    id_tx: Option<oneshot::Sender<String>>,
}

enum Driver {
    Claude { stdin: Arc<AsyncMutex<Option<ChildStdin>>>, child: Arc<AsyncMutex<Child>> },
    Exec(Arc<AsyncMutex<ExecState>>),
}

pub struct Runner {
    view: Arc<Mutex<RunView>>,
    driver: Driver,
}

pub struct RunnerRegistry {
    runners: Mutex<HashMap<String, Arc<Runner>>>,
    agents: Mutex<Vec<AgentInfo>>,
    /// Session ids whose run state changed; the server turns these into SSE events.
    pub changed: broadcast::Sender<String>,
}

impl RunnerRegistry {
    pub fn new(agents: Vec<AgentInfo>) -> Self {
        let (changed, _) = broadcast::channel(256);
        Self { runners: Mutex::new(HashMap::new()), agents: Mutex::new(agents), changed }
    }

    pub fn set_enabled(&self, kind: AgentKind, enabled: bool) {
        if let Some(a) = self.agents.lock().unwrap_or_else(|e| e.into_inner()).iter_mut().find(|a| a.kind == kind) {
            a.enabled = enabled;
        }
    }

    /// The executable of an enabled agent.
    pub fn exe(&self, kind: AgentKind) -> Option<Exe> {
        self.agents.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|a| a.kind == kind && a.enabled).and_then(|a| a.exe.clone())
    }

    pub fn can_run(&self, kind: AgentKind) -> bool {
        self.exe(kind).is_some()
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

    fn register(&self, id: &str, runner: Arc<Runner>) {
        self.runners.lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string(), runner);
    }

    pub async fn start(self: &Arc<Self>, opts: StartOptions) -> Result<String> {
        let exe = self
            .exe(opts.agent)
            .ok_or_else(|| anyhow!("{} executable not found; set it in config.json or {}", opts.agent.label(), opts.agent.exe_env()))?;
        if !opts.cwd.is_dir() {
            bail!("working directory does not exist: {}", opts.cwd.display());
        }
        match opts.agent {
            AgentKind::Claude => self.start_claude(exe, opts).await,
            _ => self.start_exec(exe, opts).await,
        }
    }

    async fn start_claude(self: &Arc<Self>, exe: Exe, opts: StartOptions) -> Result<String> {
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

        let mut cmd = exe.tokio_command();
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
        prepare_env(&mut cmd);

        let mut child = cmd.spawn().with_context(|| format!("spawn {}", exe.display()))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
        let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;

        let view = Arc::new(Mutex::new(new_view(AgentKind::Claude, &session_id, &opts, Some(permission_mode))));
        let runner = Arc::new(Runner {
            view: view.clone(),
            driver: Driver::Claude { stdin: Arc::new(AsyncMutex::new(Some(stdin))), child: Arc::new(AsyncMutex::new(child)) },
        });
        self.register(&session_id, runner.clone());

        runner.write(&user_message(&opts.prompt)).await?;
        self.notify(&session_id);

        // stdout reader
        {
            let reg = self.clone();
            let view = view.clone();
            let id = session_id.clone();
            let Driver::Claude { child, .. } = &runner.driver else { unreachable!() };
            let child = child.clone();
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
        spawn_stderr_tail(view.clone(), stderr);
        Ok(session_id)
    }

    async fn start_exec(self: &Arc<Self>, exe: Exe, opts: StartOptions) -> Result<String> {
        let kind = opts.agent;
        let assigns_own_id = matches!(kind, AgentKind::Codex | AgentKind::Opencode);
        let session_id: Option<String> = match (&opts.resume, &opts.session_id) {
            (Some(id), _) => Some(id.clone()),
            (None, _) if assigns_own_id => None,
            (None, Some(id)) => Some(id.to_lowercase()),
            (None, None) => Some(uuid::Uuid::new_v4().to_string()),
        };
        if let Some(id) = &session_id {
            if self.view(id).map(|v| v.status != "exited").unwrap_or(false) {
                bail!("session {id} is already running under OYAKATA");
            }
        }
        let permission_mode = opts.permission_mode.clone().unwrap_or_else(|| DEFAULT_PERMISSION_MODE.to_string());
        let view = Arc::new(Mutex::new(new_view(kind, session_id.as_deref().unwrap_or(""), &opts, Some(permission_mode))));
        let (id_tx, id_rx) = oneshot::channel();
        let state = ExecState { kind, exe, cwd: opts.cwd.clone(), child: None, queue: VecDeque::new(), id_tx: Some(id_tx) };
        let runner = Arc::new(Runner { view: view.clone(), driver: Driver::Exec(Arc::new(AsyncMutex::new(state))) });

        if let Some(id) = &session_id {
            self.register(id, runner.clone());
        }
        spawn_turn(self.clone(), runner.clone(), opts.prompt.clone(), opts.resume.clone()).await?;
        if let Some(id) = session_id {
            self.notify(&id);
            return Ok(id);
        }
        // Wait for the agent to tell us the id it assigned, then register under it.
        match tokio::time::timeout(ID_TIMEOUT, id_rx).await {
            Ok(Ok(id)) => {
                self.register(&id, runner.clone());
                self.notify(&id);
                Ok(id)
            }
            _ => {
                let v = view.lock().unwrap_or_else(|e| e.into_inner());
                let detail = v.last_error.clone().or_else(|| v.stderr_tail.last().cloned()).unwrap_or_else(|| "no session id was reported".into());
                Err(anyhow!("{} did not start a session: {detail}", kind.label()))
            }
        }
    }

    /// Send a prompt. Claude Code takes it up at the next tool boundary (or as the next
    /// turn); other agents get it as their next turn once the current one ends. Until then
    /// it is listed in `queued`.
    pub async fn send(self: &Arc<Self>, id: &str, text: &str) -> Result<()> {
        let r = self.get(id)?;
        {
            let v = r.view.lock().unwrap_or_else(|e| e.into_inner());
            match v.status.as_str() {
                "idle" | "busy" => {}
                "exited" => bail!("session has exited; resume it to continue"),
                _ => bail!("answer the pending permission request first"),
            }
        }
        match &r.driver {
            Driver::Claude { .. } => {
                r.write(&user_message(text)).await?;
                let mut v = r.view.lock().unwrap_or_else(|e| e.into_inner());
                v.queued.push(QueuedMessage { text: text.to_string(), sent_at: now_ms() });
                v.status = "busy".into();
                v.last_error = None;
            }
            Driver::Exec(state) => {
                let running = state.lock().await.child.is_some();
                if running {
                    state.lock().await.queue.push_back(text.to_string());
                    let mut v = r.view.lock().unwrap_or_else(|e| e.into_inner());
                    v.queued.push(QueuedMessage { text: text.to_string(), sent_at: now_ms() });
                } else {
                    let session_id = r.view.lock().unwrap_or_else(|e| e.into_inner()).session_id.clone();
                    spawn_turn(self.clone(), r.clone(), text.to_string(), Some(session_id)).await?;
                }
            }
        }
        self.notify(id);
        Ok(())
    }

    /// Answer a `can_use_tool` request. `response` is the inner response object
    /// (`{"behavior":"allow", ...}` or `{"behavior":"deny", ...}`).
    pub async fn respond_permission(&self, id: &str, request_id: &str, response: Value) -> Result<()> {
        let r = self.get(id)?;
        if !matches!(r.driver, Driver::Claude { .. }) {
            bail!("this agent does not ask for permissions interactively");
        }
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
        match &r.driver {
            Driver::Claude { .. } => {
                r.write(&json!({
                    "type": "control_request",
                    "request_id": uuid::Uuid::new_v4().to_string(),
                    "request": { "subtype": "interrupt" }
                }))
                .await?;
            }
            Driver::Exec(state) => {
                let mut st = state.lock().await;
                if let Some(child) = st.child.as_mut() {
                    let _ = child.kill().await;
                }
            }
        }
        Ok(())
    }

    /// Switch the permission mode of a running session (what Shift+Tab does in a terminal).
    /// For agents other than Claude it applies from the next turn.
    pub async fn set_permission_mode(&self, id: &str, mode: &str) -> Result<()> {
        const MODES: &[&str] = &["default", "acceptEdits", "auto", "plan", "bypassPermissions", "dontAsk"];
        if !MODES.contains(&mode) {
            bail!("unknown permission mode: {mode}");
        }
        let r = self.get(id)?;
        if matches!(r.driver, Driver::Claude { .. }) {
            r.write(&json!({
                "type": "control_request",
                "request_id": uuid::Uuid::new_v4().to_string(),
                "request": { "subtype": "set_permission_mode", "mode": mode }
            }))
            .await?;
        }
        r.view.lock().unwrap_or_else(|e| e.into_inner()).permission_mode = Some(mode.to_string());
        self.notify(id);
        Ok(())
    }

    /// Switch the model (`/model`). `None` returns to the default. For agents other than
    /// Claude it applies from the next turn.
    pub async fn set_model(&self, id: &str, model: Option<&str>) -> Result<()> {
        if let Some(m) = model {
            if m.is_empty() || m.starts_with('-') || m.chars().any(char::is_whitespace) {
                bail!("invalid model name");
            }
        }
        let r = self.get(id)?;
        if matches!(r.driver, Driver::Claude { .. }) {
            let mut request = json!({ "subtype": "set_model" });
            if let Some(m) = model {
                request["model"] = Value::String(m.to_string());
            }
            r.write(&json!({ "type": "control_request", "request_id": uuid::Uuid::new_v4().to_string(), "request": request })).await?;
        }
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

fn new_view(agent: AgentKind, session_id: &str, opts: &StartOptions, permission_mode: Option<String>) -> RunView {
    RunView {
        session_id: session_id.to_string(),
        agent,
        cwd: opts.cwd.display().to_string(),
        status: "busy".into(),
        pending: Vec::new(),
        queued: Vec::new(),
        awaiting_queued: None,
        started_at: now_ms(),
        model: opts.model.clone(),
        permission_mode,
        effort: opts.effort.clone(),
        resumed: opts.resume.is_some(),
        turns: 0,
        total_cost_usd: 0.0,
        context_window: None,
        exit_code: None,
        last_error: None,
        stderr_tail: Vec::new(),
        last_tool: None,
    }
}

/// When OYAKATA itself was launched from inside a Claude Code session, the child must not
/// inherit that session's identity. Hide the console window on Windows.
fn prepare_env(cmd: &mut Command) {
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
}

fn spawn_stderr_tail(view: Arc<Mutex<RunView>>, stderr: tokio::process::ChildStderr) {
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

/// Start one turn of a one-turn-per-process agent: spawn the CLI with the prompt on stdin,
/// follow its event stream, and when it exits take the next queued prompt as a resume.
/// Boxed because the exit handler recurses into the next turn.
fn spawn_turn(reg: Arc<RunnerRegistry>, runner: Arc<Runner>, prompt: String, resume: Option<String>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
    Box::pin(spawn_turn_inner(reg, runner, prompt, resume))
}

async fn spawn_turn_inner(reg: Arc<RunnerRegistry>, runner: Arc<Runner>, prompt: String, resume: Option<String>) -> Result<()> {
    let Driver::Exec(state) = &runner.driver else { bail!("not an exec session") };
    let view = runner.view.clone();
    let (kind, exe, cwd) = {
        let st = state.lock().await;
        (st.kind, st.exe.clone(), st.cwd.clone())
    };
    let opts = {
        let v = view.lock().unwrap_or_else(|e| e.into_inner());
        ExecOpts {
            cwd: cwd.clone(),
            prompt,
            resume: resume.clone().filter(|s| !s.is_empty()),
            session_id: if resume.is_none() && !v.session_id.is_empty() { Some(v.session_id.clone()) } else { None },
            model: v.model.clone(),
            mode: v.permission_mode.clone().unwrap_or_else(|| DEFAULT_PERMISSION_MODE.into()),
            effort: v.effort.clone(),
        }
    };
    let command = agents::exec_command(kind, &opts);
    let mut cmd = exe.tokio_command();
    cmd.args(&command.args)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    prepare_env(&mut cmd);
    let mut child = cmd.spawn().with_context(|| format!("spawn {}", exe.display()))?;
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let text = command.stdin.clone().unwrap_or_default();
        tokio::spawn(async move {
            let _ = stdin.write_all(text.as_bytes()).await;
            let _ = stdin.write_all(b"\n").await;
            let _ = stdin.shutdown().await;
        });
    }
    {
        let mut st = state.lock().await;
        st.child = Some(child);
    }
    {
        let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
        v.status = "busy".into();
        v.last_error = None;
        v.last_tool = None;
        v.exit_code = None;
    }
    spawn_stderr_tail(view.clone(), stderr);

    let state = state.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
            let event = agents::exec_event(kind, &ev);
            let mut changed = true;
            let id_now = {
                let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
                match event {
                    ExecEvent::SessionId(id) => {
                        if v.session_id.is_empty() {
                            v.session_id = id;
                        }
                    }
                    ExecEvent::TurnStarted => v.status = "busy".into(),
                    ExecEvent::Tool(t) => v.last_tool = Some(t),
                    ExecEvent::Message(_) => v.last_tool = None,
                    ExecEvent::TurnDone { error } => {
                        if let Some(e) = error {
                            v.last_error = Some(e);
                        }
                        v.last_tool = None;
                    }
                    ExecEvent::Error(e) => v.last_error = Some(e),
                    ExecEvent::Ignore => changed = false,
                }
                v.session_id.clone()
            };
            if !id_now.is_empty() {
                if let Some(tx) = state.lock().await.id_tx.take() {
                    let _ = tx.send(id_now.clone());
                }
                if changed {
                    reg.notify(&id_now);
                }
            }
        }
        // The process is done: settle the turn and start the next queued prompt, if any.
        let child = state.lock().await.child.take();
        let code = match child {
            Some(mut c) => c.wait().await.ok().and_then(|s| s.code()),
            None => None,
        };
        let next = state.lock().await.queue.pop_front();
        let id = {
            let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
            v.turns += 1;
            v.exit_code = code;
            v.last_tool = None;
            if code.unwrap_or(0) != 0 && v.last_error.is_none() {
                let tail = v.stderr_tail.iter().rev().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
                v.last_error = Some(format!("{} exited with code {}{}", kind.exe_name(), code.unwrap_or(-1), if tail.is_empty() { String::new() } else { format!(": {tail}") }));
            }
            if v.session_id.is_empty() {
                v.status = "exited".into();
            } else if next.is_none() {
                v.status = "idle".into();
            }
            if let Some(n) = &next {
                v.queued.retain(|q| &q.text != n);
            }
            v.session_id.clone()
        };
        if id.is_empty() {
            // Never learned an id: nobody is registered under it; drop the one-shot so
            // `start` reports the failure.
            state.lock().await.id_tx.take();
            return;
        }
        reg.notify(&id);
        if let Some(text) = next {
            if let Err(e) = spawn_turn(reg.clone(), runner.clone(), text, Some(id.clone())).await {
                let mut v = view.lock().unwrap_or_else(|e| e.into_inner());
                v.status = "idle".into();
                v.last_error = Some(format!("{e:#}"));
                drop(v);
                reg.notify(&id);
            }
        }
    });
    Ok(())
}

impl Runner {
    async fn write(&self, v: &Value) -> Result<()> {
        let Driver::Claude { stdin, .. } = &self.driver else { bail!("not a Claude session") };
        let mut guard = stdin.lock().await;
        let stdin = guard.as_mut().ok_or_else(|| anyhow!("session stdin is closed"))?;
        let mut line = serde_json::to_vec(v)?;
        line.push(b'\n');
        stdin.write_all(&line).await.context("write to claude stdin")?;
        stdin.flush().await.context("flush claude stdin")?;
        Ok(())
    }

    /// Claude: close stdin (it exits at end of input), then kill if it lingers. Others: kill
    /// the current turn and drop the queue.
    async fn shutdown(&self) {
        match &self.driver {
            Driver::Claude { stdin, child } => {
                {
                    let mut guard = stdin.lock().await;
                    guard.take();
                }
                let mut child = child.lock().await;
                let waited = tokio::time::timeout(Duration::from_secs(4), child.wait()).await;
                if waited.is_err() {
                    let _ = child.kill().await;
                }
            }
            Driver::Exec(state) => {
                let mut st = state.lock().await;
                st.queue.clear();
                if let Some(mut c) = st.child.take() {
                    let _ = c.kill().await;
                }
            }
        }
        let mut v = self.view.lock().unwrap_or_else(|e| e.into_inner());
        v.queued.clear();
        if v.status != "exited" {
            v.status = "exited".into();
        }
    }
}

/// Update the view from one Claude Code stdout event; returns true when something the UI
/// shows changed.
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
                v.last_error = Some(if crate::i18n::is_ja() {
                    format!("作業中に送った指示 {dropped} 件が Claude に渡りませんでした。もう一度送ってください。")
                } else {
                    format!("{dropped} prompt(s) sent while Claude was busy were not taken up. Please send them again.")
                });
            }
        }
        reg.notify(&id);
    });
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}
