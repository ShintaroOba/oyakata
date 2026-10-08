//! The coding agents OYAKATA can show and drive. Claude Code is the native one: its
//! transcripts are read as-is. Every other agent gets an adapter that finds its sessions on
//! disk (or through its CLI) and converts them into the same JSONL shape Claude Code writes,
//! so the rest of OYAKATA (index, transcript parser, UI) needs no per-agent knowledge.
//!
//! | agent    | where sessions live                                   | how OYAKATA drives it                  |
//! |----------|-------------------------------------------------------|----------------------------------------|
//! | claude   | `~/.claude/projects/**/*.jsonl`                       | `claude -p --input-format stream-json` |
//! | codex    | `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-*.jsonl`     | `codex exec --json` / `exec resume`    |
//! | gemini   | `~/.gemini/tmp/<project>/chats/session-*.jsonl`       | `gemini -p --output-format stream-json`|
//! | copilot  | `$COPILOT_HOME/session-state/<id>/events.jsonl`       | `copilot -p --output-format json`      |
//! | opencode | SQLite behind `opencode db` (`~/.local/share/opencode`)| `opencode run --format json`           |

pub mod canon;
pub mod codex;
pub mod copilot;
pub mod gemini;
pub mod opencode;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Gemini,
    Copilot,
    Opencode,
}

impl AgentKind {
    pub const ALL: [AgentKind; 5] = [AgentKind::Claude, AgentKind::Codex, AgentKind::Gemini, AgentKind::Copilot, AgentKind::Opencode];

    pub fn id(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::Copilot => "copilot",
            AgentKind::Opencode => "opencode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex CLI",
            AgentKind::Gemini => "Gemini CLI",
            AgentKind::Copilot => "Copilot CLI",
            AgentKind::Opencode => "OpenCode",
        }
    }

    /// The executable on `PATH`.
    pub fn exe_name(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Gemini => "gemini",
            AgentKind::Copilot => "copilot",
            AgentKind::Opencode => "opencode",
        }
    }

    /// Environment variable that overrides the executable (`OYAKATA_CLAUDE`, …).
    pub fn exe_env(self) -> &'static str {
        match self {
            AgentKind::Claude => "OYAKATA_CLAUDE",
            AgentKind::Codex => "OYAKATA_CODEX",
            AgentKind::Gemini => "OYAKATA_GEMINI",
            AgentKind::Copilot => "OYAKATA_COPILOT",
            AgentKind::Opencode => "OYAKATA_OPENCODE",
        }
    }

    pub fn parse(s: &str) -> Option<AgentKind> {
        AgentKind::ALL.iter().copied().find(|k| k.id().eq_ignore_ascii_case(s.trim()))
    }

    /// Where the agent keeps its data, honoring the agent's own override variable.
    pub fn default_data_dir(self, home: &Path) -> PathBuf {
        let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        match self {
            AgentKind::Claude => env("CLAUDE_CONFIG_DIR").unwrap_or_else(|| home.join(".claude")),
            AgentKind::Codex => env("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            AgentKind::Gemini => home.join(".gemini"),
            AgentKind::Copilot => env("COPILOT_HOME").unwrap_or_else(|| home.join(".copilot")),
            AgentKind::Opencode => env("XDG_DATA_HOME").map(|p| p.join("opencode")).unwrap_or_else(|| home.join(".local").join("share").join("opencode")),
        }
    }

}

/// An agent executable. npm installs `.cmd` shims on Windows, which have to go through
/// `cmd /C`.
#[derive(Debug, Clone)]
pub enum Exe {
    Direct(PathBuf),
    Cmd(PathBuf),
}

impl Exe {
    pub fn display(&self) -> String {
        match self {
            Exe::Direct(p) | Exe::Cmd(p) => p.display().to_string(),
        }
    }

    pub fn std_command(&self) -> std::process::Command {
        match self {
            Exe::Direct(p) => std::process::Command::new(p),
            Exe::Cmd(p) => {
                let mut c = std::process::Command::new("cmd");
                c.arg("/C").arg(p);
                c
            }
        }
    }

    pub fn tokio_command(&self) -> tokio::process::Command {
        match self {
            Exe::Direct(p) => tokio::process::Command::new(p),
            Exe::Cmd(p) => {
                let mut c = tokio::process::Command::new("cmd");
                c.arg("/C").arg(p);
                c
            }
        }
    }
}

fn classify(p: PathBuf) -> Exe {
    let is_cmd = p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")).unwrap_or(false);
    if is_cmd {
        Exe::Cmd(p)
    } else {
        Exe::Direct(p)
    }
}

/// Locate an agent executable: an explicit path, then its `OYAKATA_<AGENT>` variable, then
/// `PATH` (with the Windows `.cmd` / `.exe` variants npm and installers produce).
pub fn find_exe(kind: AgentKind, explicit: Option<PathBuf>) -> Option<Exe> {
    if let Some(p) = explicit {
        return Some(classify(p));
    }
    if let Some(p) = std::env::var_os(kind.exe_env()).filter(|v| !v.is_empty()) {
        return Some(classify(PathBuf::from(p)));
    }
    let name = kind.exe_name();
    let candidates: Vec<String> = if cfg!(windows) {
        vec![format!("{name}.exe"), format!("{name}.cmd"), name.to_string()]
    } else {
        vec![name.to_string()]
    };
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for c in &candidates {
                let p = dir.join(c);
                if p.is_file() {
                    return Some(classify(p));
                }
            }
        }
    }
    // Claude Code exports its own path to the processes it spawns.
    if kind == AgentKind::Claude {
        if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECPATH").map(PathBuf::from).filter(|p| p.is_file()) {
            return Some(classify(p));
        }
    }
    // Common install locations that a trimmed PATH (tool sandboxes) can miss.
    let home = crate::paths::home_dir();
    let mut extra = vec![home.join(".local").join("bin").join(if cfg!(windows) { format!("{name}.exe") } else { name.to_string() })];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        extra.push(Path::new(&appdata).join("npm").join(format!("{name}.cmd")));
    }
    extra.into_iter().find(|p| p.is_file()).map(classify)
}

/// Per-agent settings from `config.json`: `{"agents": {"codex": {"enabled": false, "exe": "...", "data_dir": "..."}}}`.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AgentOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_dir: Option<PathBuf>,
}

/// What OYAKATA knows about one agent on this machine.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub kind: AgentKind,
    pub label: &'static str,
    pub data_dir: PathBuf,
    #[serde(skip)]
    pub exe: Option<Exe>,
    pub exe_path: Option<String>,
    /// Its data directory exists or its executable was found.
    pub installed: bool,
    /// Sessions are indexed and the agent is offered in the new-session dialog.
    pub enabled: bool,
}

/// Detect every agent. `claude_dir` and `claude_exe` keep the dedicated `--claude-dir` /
/// `--claude` flags working; everything else comes from defaults and `overrides`.
pub fn detect(home: &Path, claude_dir: &Path, claude_exe: Option<PathBuf>, overrides: &std::collections::BTreeMap<String, AgentOverride>) -> Vec<AgentInfo> {
    AgentKind::ALL
        .iter()
        .map(|&kind| {
            let ov = overrides.get(kind.id()).cloned().unwrap_or_default();
            let data_dir = match kind {
                AgentKind::Claude => claude_dir.to_path_buf(),
                _ => ov.data_dir.clone().unwrap_or_else(|| kind.default_data_dir(home)),
            };
            let explicit = match kind {
                AgentKind::Claude => claude_exe.clone().or(ov.exe.clone()),
                _ => ov.exe.clone(),
            };
            let exe = find_exe(kind, explicit);
            let installed = data_dir.is_dir() || exe.is_some();
            let enabled = ov.enabled.unwrap_or(installed);
            AgentInfo { kind, label: kind.label(), data_dir, exe_path: exe.as_ref().map(Exe::display), exe, installed, enabled }
        })
        .collect()
}

/// A session file (or, for OpenCode, a session row) found by an adapter.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub id: String,
    pub agent: AgentKind,
    /// Transcript file for file-based agents; a virtual `opencode:<id>` path otherwise.
    pub path: PathBuf,
    /// Grouping key when the transcript carries no cwd.
    pub project_dir: String,
    /// Known up front for agents whose listing carries it (OpenCode); otherwise read from the
    /// transcript.
    pub cwd: Option<String>,
    pub title: Option<String>,
    /// Last update in epoch milliseconds when the listing knows it (OpenCode).
    pub updated_ms: Option<u64>,
}

/// OpenCode sessions are listed through its CLI; file-based agents are walked on disk.
pub fn discover(agent: &AgentInfo) -> Vec<Discovered> {
    if !agent.enabled {
        return Vec::new();
    }
    match agent.kind {
        AgentKind::Claude => Vec::new(), // handled by the index itself (projects/ walk)
        AgentKind::Codex => codex::discover(&agent.data_dir),
        AgentKind::Gemini => gemini::discover(&agent.data_dir),
        AgentKind::Copilot => copilot::discover(&agent.data_dir),
        AgentKind::Opencode => agent.exe.as_ref().map(opencode::discover).unwrap_or_default(),
    }
}

/// A permission mode in OYAKATA's vocabulary (`auto` | `default` | `acceptEdits` | `plan` |
/// `bypassPermissions`), translated to each agent's non-interactive flags.
pub fn permission_args(kind: AgentKind, mode: &str) -> Vec<String> {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    match kind {
        AgentKind::Claude => s(&["--permission-mode", mode]),
        AgentKind::Codex => match mode {
            "plan" => s(&["-s", "read-only"]),
            "bypassPermissions" => s(&["--dangerously-bypass-approvals-and-sandbox"]),
            _ => s(&["-s", "workspace-write"]),
        },
        AgentKind::Gemini => match mode {
            "plan" => s(&["--approval-mode", "plan"]),
            "default" => s(&["--approval-mode", "default"]),
            "acceptEdits" => s(&["--approval-mode", "auto_edit"]),
            _ => s(&["--approval-mode", "yolo"]),
        },
        AgentKind::Copilot => match mode {
            "plan" | "default" => Vec::new(),
            _ => s(&["--allow-all-tools"]),
        },
        AgentKind::Opencode => match mode {
            "plan" => s(&["--agent", "plan"]),
            "default" => Vec::new(),
            _ => s(&["--auto"]),
        },
    }
}

/// How a non-interactive turn is started for an agent other than Claude Code.
#[derive(Debug, Clone)]
pub struct ExecOpts {
    pub cwd: PathBuf,
    pub prompt: String,
    /// Continue this session (the agent's own id).
    pub resume: Option<String>,
    /// Id for a new session when the agent lets the caller choose one (Gemini, Copilot).
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub mode: String,
    pub effort: Option<String>,
}

/// The command line for one turn, and the prompt to write to stdin when the CLI reads it
/// from there (safer than argv for multi-line prompts behind npm's `.cmd` shims).
#[derive(Debug, Clone, PartialEq)]
pub struct ExecCommand {
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

/// What OYAKATA learns from one line of an agent's JSON event stream.
#[derive(Debug, Clone, PartialEq)]
pub enum ExecEvent {
    /// The agent assigned (or confirmed) the session id.
    SessionId(String),
    TurnStarted,
    /// A tool is running: `name: summary`.
    Tool(String),
    Message(String),
    TurnDone { error: Option<String> },
    Error(String),
    Ignore,
}

pub fn exec_command(kind: AgentKind, opts: &ExecOpts) -> ExecCommand {
    match kind {
        AgentKind::Claude => ExecCommand { args: Vec::new(), stdin: None },
        AgentKind::Codex => codex::exec_command(opts),
        AgentKind::Gemini => gemini::exec_command(opts),
        AgentKind::Copilot => copilot::exec_command(opts),
        AgentKind::Opencode => opencode::exec_command(opts),
    }
}

pub fn exec_event(kind: AgentKind, v: &Value) -> ExecEvent {
    match kind {
        AgentKind::Claude => ExecEvent::Ignore,
        AgentKind::Codex => codex::exec_event(v),
        AgentKind::Gemini => gemini::exec_event(v),
        AgentKind::Copilot => copilot::exec_event(v),
        AgentKind::Opencode => opencode::exec_event(v),
    }
}

pub(crate) fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// Read a JSON file's first line without loading the rest (session ids live there).
pub fn first_line(path: &Path) -> Option<Value> {
    use std::io::{BufRead, BufReader};
    let f = std::fs::File::open(path).ok()?;
    let mut r = BufReader::new(f);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    serde_json::from_str(line.trim_end()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip() {
        for k in AgentKind::ALL {
            assert_eq!(AgentKind::parse(k.id()), Some(k));
            assert_eq!(serde_json::to_string(&k).unwrap(), format!("\"{}\"", k.id()));
        }
        assert_eq!(AgentKind::parse("Codex "), Some(AgentKind::Codex));
        assert_eq!(AgentKind::parse("cursor"), None);
    }

    #[test]
    fn permission_modes_map_to_each_cli() {
        assert_eq!(permission_args(AgentKind::Codex, "plan"), vec!["-s", "read-only"]);
        assert_eq!(permission_args(AgentKind::Gemini, "acceptEdits"), vec!["--approval-mode", "auto_edit"]);
        assert_eq!(permission_args(AgentKind::Copilot, "auto"), vec!["--allow-all-tools"]);
        assert!(permission_args(AgentKind::Copilot, "plan").is_empty());
        assert_eq!(permission_args(AgentKind::Opencode, "plan"), vec!["--agent", "plan"]);
    }

    #[test]
    fn cmd_shims_go_through_cmd() {
        assert!(matches!(classify(PathBuf::from("C:/x/codex.cmd")), Exe::Cmd(_)));
        assert!(matches!(classify(PathBuf::from("/usr/bin/codex")), Exe::Direct(_)));
    }
}
