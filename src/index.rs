//! The session index: every transcript under `~/.claude/projects` plus the sessions of every
//! other enabled agent (see `agents`), their metadata, which ones are live, which ones OYAKATA
//! itself is running, and incremental refresh that turns file growth into events for the
//! browser. Non-Claude transcripts are converted to Claude Code's JSONL shape on the way in,
//! so one parser serves them all.

use crate::agents::{self, canon, AgentInfo, AgentKind, AgentOverride, Discovered};
use crate::gitops;
use crate::live::{self, LiveInfo};
use crate::repo::{self, RepoInfo};
use crate::runner::RunView;
use crate::transcript::{Change, Item, Transcript, TranscriptMeta};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct AgentMeta {
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    /// Started with `run_in_background`, so the spawning tool call returns right away.
    pub background: bool,
    #[serde(skip)]
    pub path: PathBuf,
}

/// One subagent as the team view (体制図) shows it.
#[derive(Debug, Clone, Serialize)]
pub struct TeamAgent {
    pub agent_id: String,
    pub agent_type: Option<String>,
    pub description: Option<String>,
    pub tool_use_id: Option<String>,
    /// `main`, or the agent id of the subagent that spawned this one.
    pub parent: String,
    /// Index of the spawning tool call in the main transcript, when it is there.
    pub index: Option<usize>,
    pub prompt: Option<String>,
    /// `running` | `done` | `error` | `stopped`
    pub status: String,
    pub background: bool,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub last_at: Option<String>,
    pub tool_calls: u32,
    pub last_tool: Option<String>,
    pub last_tool_at: Option<String>,
    pub last_text: Option<String>,
    pub result: Option<String>,
    pub context_tokens: u64,
}

pub struct SessionEntry {
    pub id: String,
    pub path: PathBuf,
    pub project_dir: String,
    pub size: u64,
    pub mtime_ms: u64,
    pub transcript: Transcript,
    pub last_viewed: Option<Instant>,
    pub repo: Option<RepoInfo>,
    pub agents: Vec<AgentMeta>,
    agents_scanned_at: Option<Instant>,
    pub agent: AgentKind,
    pub source: Source,
    /// Bytes of the native file already converted (file-based non-Claude agents).
    raw_offset: u64,
    /// Partial trailing line of the native file, kept until its newline arrives.
    raw_pending: Vec<u8>,
    /// From the agent's own listing, for agents whose transcript carries no title / cwd.
    pub title_hint: Option<String>,
    pub cwd_hint: Option<String>,
    /// Last update per the agent's listing (OpenCode), epoch ms.
    pub updated_ms: u64,
    /// `updated_ms` at the last transcript fetch (OpenCode).
    pub fetched_ms: u64,
}

/// How a session's transcript reaches the parser.
pub enum Source {
    /// Claude Code's own JSONL, fed as-is.
    Claude,
    /// Append-only native files converted line by line.
    Codex(agents::codex::Converter),
    Copilot(agents::copilot::Converter),
    /// Files whose earlier records can change: re-parsed whole on every change.
    Gemini,
    /// Pulled through the agent's CLI by the OpenCode loop (`install_canonical`).
    Opencode,
}

impl SessionEntry {
    fn new(id: String, path: PathBuf, project_dir: String, agent: AgentKind) -> Self {
        let source = match agent {
            AgentKind::Claude => Source::Claude,
            AgentKind::Codex => Source::Codex(Default::default()),
            AgentKind::Copilot => Source::Copilot(Default::default()),
            AgentKind::Gemini => Source::Gemini,
            AgentKind::Opencode => Source::Opencode,
        };
        Self {
            id,
            path,
            project_dir,
            size: 0,
            mtime_ms: 0,
            transcript: Transcript::new(false),
            last_viewed: None,
            repo: None,
            agents: Vec::new(),
            agents_scanned_at: None,
            agent,
            source,
            raw_offset: 0,
            raw_pending: Vec::new(),
            title_hint: None,
            cwd_hint: None,
            updated_ms: 0,
            fetched_ms: 0,
        }
    }
}

/// A session of another agent whose file has changed within this window counts as running
/// there ("external"), since those agents keep no registry of live processes.
const EXTERNAL_BUSY_MS: u64 = 20_000;

#[derive(Debug, Clone, Serialize)]
pub struct EditedFile {
    pub path: String,
    pub edits: u32,
    pub writes: u32,
}

/// What the sidebar needs per session.
#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub agent: AgentKind,
    pub agent_label: &'static str,
    pub project_dir: String,
    pub cwd: Option<String>,
    pub repo: RepoInfo,
    pub title: Option<String>,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    pub started_at: Option<String>,
    pub last_at: Option<String>,
    pub mtime_ms: u64,
    pub size: u64,
    pub user_turns: u32,
    pub assistant_messages: u32,
    pub tool_calls: u32,
    pub model: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub continued_in: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub compactions: u32,
    pub live: Option<LiveInfo>,
    /// `oyakata` when this process drives the session, `terminal` when a terminal does.
    pub owner: Option<String>,
    /// `busy` | `idle` | `waiting` | `ended`
    pub status: String,
    pub waiting_for: Option<String>,
    pub subagents: usize,
    pub last_text_snippet: Option<String>,
    pub edited_files: Vec<EditedFile>,
    pub artifacts: Vec<String>,
    pub loaded: bool,
    /// Tokens in the context window after the latest response.
    pub context_tokens: u64,
    /// Context window size when Claude Code reported it (OYAKATA-run sessions).
    pub context_window: Option<u64>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
    pub last_tool: Option<String>,
    pub last_tool_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoEntry {
    pub key: String,
    pub name: String,
    pub root: String,
    pub sessions: usize,
    pub last_at: Option<String>,
    pub source: String,
    /// Added by the user (import or clone), so it stays listed without sessions.
    pub added: bool,
    pub is_git: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Sessions { sessions: Vec<SessionSummary> },
    Append { session: String, start: usize, items: Vec<Item> },
    Patch { session: String, index: usize, item: Item },
    /// The file was rewritten or shrank; the client should refetch.
    Reset { session: String },
    /// An OYAKATA-run session changed state (status, pending permission prompts).
    Run { run: RunView },
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunState {
    pub agent: AgentKind,
    pub status: String,
    pub waiting_for: Option<String>,
    pub cwd: String,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
    pub context_window: Option<u64>,
}

pub struct State {
    pub claude_dir: PathBuf,
    /// OYAKATA's own folder (`~/.oyakata`): config.json, trash, log.
    pub oyakata_dir: PathBuf,
    pub agents: Vec<AgentInfo>,
    /// UI / CLI language chosen by the user (`ja` | `en`), `None` until chosen.
    pub lang: Option<String>,
    pub agent_overrides: BTreeMap<String, AgentOverride>,
    /// Sessions OYAKATA starts in a git repository get their own worktree unless asked not to.
    pub worktree_default: bool,
    /// Where those worktrees go (`~/.oyakata/worktrees` unless configured).
    pub worktree_root: Option<PathBuf>,
    pub sessions: HashMap<String, SessionEntry>,
    pub live: HashMap<String, LiveInfo>,
    pub runs: HashMap<String, RunState>,
    /// When OYAKATA's own run of a session ended (epoch ms), so the writes it made are not
    /// mistaken for another process working on the session.
    run_ended: HashMap<String, u64>,
    pub repo_roots: Vec<PathBuf>,
    pub ghq_root: Option<PathBuf>,
    repo_cache: HashMap<String, RepoInfo>,
    scanned: Vec<RepoInfo>,
    scanned_at: Option<Instant>,
    last_signature: u64,
    /// Folders the user added from the browser (import / clone), persisted in `oyakata.json`.
    pub added_repos: Vec<PathBuf>,
}

impl State {
    pub fn new(claude_dir: PathBuf, oyakata_dir: PathBuf) -> Self {
        Self {
            claude_dir,
            oyakata_dir,
            agents: Vec::new(),
            lang: None,
            agent_overrides: BTreeMap::new(),
            worktree_default: true,
            worktree_root: None,
            sessions: HashMap::new(),
            live: HashMap::new(),
            runs: HashMap::new(),
            run_ended: HashMap::new(),
            repo_roots: Vec::new(),
            ghq_root: None,
            repo_cache: HashMap::new(),
            scanned: Vec::new(),
            scanned_at: None,
            last_signature: 0,
            added_repos: Vec::new(),
        }
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.claude_dir.join("projects")
    }

    pub fn sessions_dir(&self) -> PathBuf {
        self.claude_dir.join("sessions")
    }

    /// Discover every transcript and read it once for metadata.
    pub fn initial_scan(&mut self) -> usize {
        self.discover();
        let ids: Vec<String> = self.sessions.keys().cloned().collect();
        for id in ids {
            self.refresh_one(&id);
        }
        self.refresh_live();
        self.rescan_repos(true);
        self.last_signature = self.signature();
        self.sessions.len()
    }

    /// Find transcript files that are not indexed yet. Cheap enough to run every tick.
    pub fn discover(&mut self) -> Vec<String> {
        let mut added = Vec::new();
        let Ok(rd) = fs::read_dir(self.projects_dir()) else {
            return added;
        };
        for proj in rd.flatten() {
            let ppath = proj.path();
            if !ppath.is_dir() {
                continue;
            }
            let project_dir = proj.file_name().to_string_lossy().into_owned();
            let Ok(files) = fs::read_dir(&ppath) else {
                continue;
            };
            for f in files.flatten() {
                let fpath = f.path();
                if fpath.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                    continue;
                }
                let Some(stem) = fpath.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
                    continue;
                };
                if self.sessions.contains_key(&stem) {
                    continue;
                }
                self.sessions.insert(stem.clone(), SessionEntry::new(stem.clone(), fpath, project_dir.clone(), AgentKind::Claude));
                added.push(stem);
            }
        }
        added.extend(self.discover_agents());
        added
    }

    /// Sessions of the other file-based agents (OpenCode is listed by its own loop).
    fn discover_agents(&mut self) -> Vec<String> {
        let infos: Vec<AgentInfo> = self.agents.iter().filter(|a| a.enabled && !matches!(a.kind, AgentKind::Claude | AgentKind::Opencode)).cloned().collect();
        let mut added = Vec::new();
        for info in infos {
            added.extend(self.adopt(agents::discover(&info)));
        }
        added
    }

    /// Register sessions an adapter found. Known ones only get their listing facts refreshed.
    pub fn adopt(&mut self, found: Vec<Discovered>) -> Vec<String> {
        let mut added = Vec::new();
        for d in found {
            if let Some(e) = self.sessions.get_mut(&d.id) {
                if let Some(t) = d.title {
                    e.title_hint = Some(t);
                }
                if let Some(u) = d.updated_ms {
                    e.updated_ms = u;
                    if e.agent == AgentKind::Opencode {
                        e.mtime_ms = u;
                    }
                }
                continue;
            }
            let mut e = SessionEntry::new(d.id.clone(), d.path, d.project_dir, d.agent);
            e.title_hint = d.title;
            e.cwd_hint = d.cwd.clone();
            e.updated_ms = d.updated_ms.unwrap_or(0);
            if d.agent == AgentKind::Opencode {
                e.mtime_ms = e.updated_ms;
            }
            if let Some(cwd) = &d.cwd {
                e.repo = Some(repo_for(&mut self.repo_cache, cwd));
            }
            self.sessions.insert(d.id.clone(), e);
            added.push(d.id);
        }
        added
    }

    /// Replace a session's transcript with converted canonical bytes (OpenCode, whose
    /// transcripts come through its CLI). Returns the events the browser needs.
    pub fn install_canonical(&mut self, id: &str, bytes: &[u8], updated_ms: u64) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(entry) = self.sessions.get_mut(id) else { return events };
        let keep = entry.transcript.keeps_items();
        entry.transcript = Transcript::new(keep);
        entry.transcript.feed(bytes);
        entry.size = bytes.len() as u64;
        entry.mtime_ms = updated_ms.max(entry.mtime_ms);
        entry.updated_ms = updated_ms.max(entry.updated_ms);
        entry.fetched_ms = entry.updated_ms;
        if entry.repo.is_none() {
            if let Some(cwd) = entry.transcript.meta.cwd.clone().or_else(|| entry.cwd_hint.clone()) {
                entry.repo = Some(repo_for(&mut self.repo_cache, &cwd));
            }
        }
        if keep {
            events.push(Event::Reset { session: id.to_string() });
            if !entry.transcript.items.is_empty() {
                events.push(Event::Append { session: id.to_string(), start: 0, items: entry.transcript.items.clone() });
            }
        }
        events
    }

    /// OpenCode sessions whose listing moved past what was fetched, newest first.
    pub fn stale_opencode(&self) -> Vec<(String, Option<String>, Option<String>, u64)> {
        let mut v: Vec<_> = self
            .sessions
            .values()
            .filter(|e| e.agent == AgentKind::Opencode && e.updated_ms > e.fetched_ms)
            .map(|e| (e.id.clone(), e.title_hint.clone(), e.cwd_hint.clone(), e.updated_ms))
            .collect();
        v.sort_by(|a, b| b.3.cmp(&a.3));
        v
    }

    pub fn agent_exe(&self, kind: AgentKind) -> Option<agents::Exe> {
        self.agents.iter().find(|a| a.kind == kind).and_then(|a| a.exe.clone())
    }

    /// Re-read one session if its file changed. Emits events only for loaded sessions.
    pub fn refresh_one(&mut self, id: &str) -> Vec<Event> {
        let mut events = Vec::new();
        let Some(entry) = self.sessions.get_mut(id) else {
            return events;
        };
        let Ok(md) = fs::metadata(&entry.path) else {
            return events;
        };
        if matches!(entry.source, Source::Opencode) {
            return events; // pulled by the OpenCode loop
        }
        let size = md.len();
        let mtime_ms = mtime_millis(&md);
        if size == entry.size && mtime_ms == entry.mtime_ms {
            return events;
        }
        let keep = entry.transcript.keeps_items();
        let mut old_len = entry.transcript.items.len();
        let changes = match &mut entry.source {
            Source::Claude => {
                if size < entry.transcript.consumed {
                    entry.transcript = Transcript::new(keep);
                    old_len = 0;
                    if keep {
                        events.push(Event::Reset { session: id.to_string() });
                    }
                }
                match read_from(&entry.path, entry.transcript.consumed) {
                    Ok(bytes) => entry.transcript.feed(&bytes),
                    Err(_) => return events,
                }
            }
            Source::Codex(_) | Source::Copilot(_) => {
                if size < entry.raw_offset {
                    entry.transcript = Transcript::new(keep);
                    entry.raw_offset = 0;
                    entry.raw_pending.clear();
                    entry.source = match entry.agent {
                        AgentKind::Codex => Source::Codex(Default::default()),
                        _ => Source::Copilot(Default::default()),
                    };
                    old_len = 0;
                    if keep {
                        events.push(Event::Reset { session: id.to_string() });
                    }
                }
                let Ok(bytes) = read_from(&entry.path, entry.raw_offset) else { return events };
                entry.raw_offset += bytes.len() as u64;
                let mut buf = std::mem::take(&mut entry.raw_pending);
                buf.extend_from_slice(&bytes);
                let mut lines = Vec::new();
                let mut start = 0;
                while let Some(rel) = buf[start..].iter().position(|&b| b == b'\n') {
                    let raw = &buf[start..start + rel];
                    start += rel + 1;
                    match &mut entry.source {
                        Source::Codex(c) => lines.extend(c.convert_line(raw)),
                        Source::Copilot(c) => lines.extend(c.convert_line(raw)),
                        _ => {}
                    }
                }
                entry.raw_pending = buf[start..].to_vec();
                entry.transcript.feed(&canon::to_jsonl(&lines))
            }
            Source::Gemini => {
                let Ok(bytes) = fs::read(&entry.path) else { return events };
                entry.transcript = Transcript::new(keep);
                old_len = 0;
                if keep {
                    events.push(Event::Reset { session: id.to_string() });
                }
                entry.transcript.feed(&canon::to_jsonl(&agents::gemini::convert_all(&bytes)))
            }
            Source::Opencode => return events,
        };
        entry.size = size;
        entry.mtime_ms = mtime_ms;
        if entry.repo.is_none() {
            if let Some(cwd) = entry.transcript.meta.cwd.clone().or_else(|| entry.cwd_hint.clone()) {
                entry.repo = Some(repo_for(&mut self.repo_cache, &cwd));
            }
        }
        if entry.transcript.keeps_items() {
            let items = &entry.transcript.items;
            if items.len() > old_len {
                events.push(Event::Append {
                    session: id.to_string(),
                    start: old_len,
                    items: items[old_len..].to_vec(),
                });
            }
            for c in changes {
                if let Change::Patched(i) = c {
                    if i < old_len {
                        events.push(Event::Patch { session: id.to_string(), index: i, item: items[i].clone() });
                    }
                }
            }
            if !events.is_empty() {
                for idx in self.scan_agents(id, false) {
                    if let Some(e) = self.sessions.get(id) {
                        if let Some(item) = e.transcript.items.get(idx) {
                            events.push(Event::Patch { session: id.to_string(), index: idx, item: item.clone() });
                        }
                    }
                }
            }
        }
        events
    }

    pub fn refresh_all(&mut self) -> Vec<Event> {
        let ids: Vec<String> = self.sessions.keys().cloned().collect();
        let mut events = Vec::new();
        for id in ids {
            events.extend(self.refresh_one(&id));
        }
        events
    }

    pub fn refresh_live(&mut self) {
        self.live = live::read_registry(&self.sessions_dir());
    }

    /// Record (or clear) the state of an OYAKATA-run session.
    pub fn set_run(&mut self, id: &str, view: Option<&RunView>) {
        match view {
            Some(v) if v.status != "exited" => {
                let waiting_for = (v.status == "waiting").then(|| {
                    v.pending
                        .first()
                        .map(|p| {
                            if p.tool_name == "AskUserQuestion" {
                                "question".to_string()
                            } else {
                                format!("permission: {}", p.tool_name)
                            }
                        })
                        .unwrap_or_else(|| "permission".into())
                });
                self.runs.insert(
                    id.to_string(),
                    RunState {
                        agent: v.agent,
                        status: v.status.clone(),
                        waiting_for,
                        cwd: v.cwd.clone(),
                        model: v.model.clone(),
                        permission_mode: v.permission_mode.clone(),
                        effort: v.effort.clone(),
                        context_window: v.context_window,
                    },
                );
            }
            _ => {
                if self.runs.remove(id).is_some() {
                    self.run_ended.insert(id.to_string(), now_ms());
                }
            }
        }
    }

    /// Adopt a transcript that was parsed outside the lock. If another caller got there first
    /// the parsed copy is simply dropped.
    pub fn install(&mut self, id: &str, transcript: Transcript) -> Result<()> {
        let entry = self.sessions.get_mut(id).ok_or_else(|| anyhow!("unknown session: {id}"))?;
        entry.last_viewed = Some(Instant::now());
        if !entry.transcript.keeps_items() {
            entry.transcript = transcript;
            // Force the next poll to re-check the file and continue from the new offset.
            entry.size = 0;
            entry.mtime_ms = 0;
            if entry.repo.is_none() {
                if let Some(cwd) = entry.transcript.meta.cwd.clone() {
                    entry.repo = Some(repo_for(&mut self.repo_cache, &cwd));
                }
            }
        }
        self.scan_agents(id, true);
        Ok(())
    }

    /// Metadata of one subagent, including the path of its transcript file.
    pub fn agent_meta(&self, id: &str, agent_id: &str) -> Result<AgentMeta> {
        let entry = self.sessions.get(id).ok_or_else(|| anyhow!("unknown session: {id}"))?;
        entry
            .agents
            .iter()
            .find(|a| a.agent_id == agent_id)
            .cloned()
            .ok_or_else(|| anyhow!("unknown agent: {agent_id}"))
    }

    pub fn touch(&mut self, id: &str) -> bool {
        match self.sessions.get_mut(id) {
            Some(e) => {
                e.last_viewed = Some(Instant::now());
                true
            }
            None => false,
        }
    }

    /// Read `<project>/<session>/subagents/*.jsonl` and link each to its spawning tool call.
    /// Returns indices of items newly linked.
    fn scan_agents(&mut self, id: &str, force: bool) -> Vec<usize> {
        let mut linked = Vec::new();
        let Some(entry) = self.sessions.get_mut(id) else {
            return linked;
        };
        if !force {
            if let Some(t) = entry.agents_scanned_at {
                if t.elapsed() < Duration::from_secs(5) {
                    return linked;
                }
            }
        }
        entry.agents_scanned_at = Some(Instant::now());
        let dir = entry.path.with_extension("").join("subagents");
        let Ok(rd) = fs::read_dir(&dir) else {
            return linked;
        };
        let mut agents = Vec::new();
        for f in rd.flatten() {
            let p = f.path();
            if p.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let agent_id = stem.strip_prefix("agent-").unwrap_or(stem).to_string();
            let meta: Option<Value> = fs::read_to_string(p.with_extension("meta.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok());
            let field = |k: &str| meta.as_ref().and_then(|m| m.get(k)).and_then(Value::as_str).map(str::to_string);
            agents.push(AgentMeta {
                agent_id,
                agent_type: field("agentType"),
                description: field("description"),
                tool_use_id: field("toolUseId"),
                background: field("requestShape").as_deref() == Some("background"),
                path: p,
            });
        }
        agents.sort_by(|a, b| a.agent_id.cmp(&b.agent_id));
        let known: Vec<String> = entry.agents.iter().map(|a| a.agent_id.clone()).collect();
        for a in &agents {
            if let Some(tu) = &a.tool_use_id {
                if let Some(idx) = entry.transcript.link_agent(tu, &a.agent_id) {
                    if !known.contains(&a.agent_id) {
                        linked.push(idx);
                    }
                }
            }
        }
        entry.agents = agents;
        linked
    }

    /// Drop item lists for sessions nobody looked at recently and that are not live.
    pub fn evict_idle(&mut self, max_idle: Duration) -> usize {
        let live = &self.live;
        let runs = &self.runs;
        let mut n = 0;
        for e in self.sessions.values_mut() {
            if !e.transcript.keeps_items() || live.contains_key(&e.id) || runs.contains_key(&e.id) {
                continue;
            }
            let idle = e.last_viewed.map(|t| t.elapsed() > max_idle).unwrap_or(true);
            if idle {
                e.transcript.drop_items();
                n += 1;
            }
        }
        n
    }

    pub fn summary_of(&self, e: &SessionEntry) -> SessionSummary {
        let m: &TranscriptMeta = &e.transcript.meta;
        let live = self.live.get(&e.id).cloned();
        let run = self.runs.get(&e.id);
        let cwd = m
            .cwd
            .clone()
            .or_else(|| e.cwd_hint.clone())
            .or_else(|| run.map(|r| r.cwd.clone()))
            .or_else(|| live.as_ref().and_then(|l| l.cwd.clone()));
        let repo = e
            .repo
            .clone()
            .or_else(|| cwd.as_deref().map(repo::detect))
            .unwrap_or_else(|| RepoInfo {
                key: e.project_dir.clone(),
                name: e.project_dir.clone(),
                root: None,
                subdir: None,
                worktree: None,
                branch: None,
            });
        let (owner, status, waiting_for) = match (run, &live) {
            (Some(r), _) => (Some("oyakata".to_string()), r.status.clone(), r.waiting_for.clone()),
            (None, Some(l)) => (Some("terminal".to_string()), l.status.clone(), l.waiting_for.clone()),
            (None, None) if self.external_busy(e) => (Some("external".to_string()), "busy".to_string(), None),
            (None, None) => (None, "ended".to_string(), None),
        };
        let mut edited_files: Vec<EditedFile> = m
            .edited_files
            .iter()
            .map(|(p, s)| EditedFile { path: p.clone(), edits: s.edits, writes: s.writes })
            .collect();
        edited_files.sort_by(|a, b| (b.edits + b.writes).cmp(&(a.edits + a.writes)).then_with(|| a.path.cmp(&b.path)));
        edited_files.truncate(80);
        SessionSummary {
            id: e.id.clone(),
            agent: e.agent,
            agent_label: e.agent.label(),
            project_dir: e.project_dir.clone(),
            cwd,
            repo,
            title: m.title().or_else(|| live.as_ref().and_then(|l| l.name.clone())).or_else(|| e.title_hint.clone()),
            first_prompt: m.first_prompt.clone(),
            last_prompt: m.last_prompt.clone(),
            started_at: m.started_at.clone(),
            last_at: m.last_at.clone(),
            mtime_ms: e.mtime_ms,
            size: e.size,
            user_turns: m.user_turns,
            assistant_messages: m.assistant_messages,
            tool_calls: m.tool_calls,
            model: m.model.clone(),
            git_branch: m.git_branch.clone(),
            version: m.version.clone(),
            continued_in: m.continued_in.clone(),
            input_tokens: m.input_tokens,
            output_tokens: m.output_tokens,
            cache_read_tokens: m.cache_read_tokens,
            compactions: m.compactions,
            live,
            owner,
            status,
            waiting_for,
            subagents: e.agents.len(),
            last_text_snippet: m.last_text_snippet.clone(),
            edited_files,
            artifacts: m.artifacts.clone(),
            loaded: e.transcript.keeps_items(),
            context_tokens: m.context_tokens,
            context_window: run.and_then(|r| r.context_window),
            permission_mode: run.and_then(|r| r.permission_mode.clone()).or_else(|| m.permission_mode.clone()),
            effort: run.and_then(|r| r.effort.clone()).or_else(|| m.effort.clone()),
            last_tool: m.last_tool.clone(),
            last_tool_at: m.last_tool_at.clone(),
        }
    }

    /// Another agent's session whose file changed moments ago: running in its own terminal.
    fn external_busy(&self, e: &SessionEntry) -> bool {
        // Writes from OYAKATA's own run (up to its last flush after exit) don't count.
        let ours = self.run_ended.get(&e.id).map(|&t| e.mtime_ms <= t + 3_000).unwrap_or(false);
        e.agent != AgentKind::Claude
            && !ours
            && !self.runs.contains_key(&e.id)
            && !self.live.contains_key(&e.id)
            && now_ms().saturating_sub(e.mtime_ms) < EXTERNAL_BUSY_MS
    }

    /// All sessions, most recently active first. Sessions OYAKATA is running but that have
    /// not written a transcript yet are included as placeholders so the UI can show them.
    pub fn summaries(&self) -> Vec<SessionSummary> {
        let mut v: Vec<SessionSummary> = self.sessions.values().map(|e| self.summary_of(e)).collect();
        for (id, run) in &self.runs {
            if self.sessions.contains_key(id) {
                continue;
            }
            let repo = repo::detect(&run.cwd);
            v.push(SessionSummary {
                id: id.clone(),
                agent: run.agent,
                agent_label: run.agent.label(),
                project_dir: String::new(),
                cwd: Some(run.cwd.clone()),
                repo,
                title: Some(crate::i18n::tr("（開始中）", "(starting)").into()),
                first_prompt: None,
                last_prompt: None,
                started_at: None,
                last_at: None,
                mtime_ms: 0,
                size: 0,
                user_turns: 0,
                assistant_messages: 0,
                tool_calls: 0,
                model: None,
                git_branch: None,
                version: None,
                continued_in: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                compactions: 0,
                live: None,
                owner: Some("oyakata".into()),
                status: run.status.clone(),
                waiting_for: run.waiting_for.clone(),
                subagents: 0,
                last_text_snippet: None,
                edited_files: Vec::new(),
                artifacts: Vec::new(),
                loaded: false,
                context_tokens: 0,
                context_window: run.context_window,
                permission_mode: run.permission_mode.clone(),
                effort: run.effort.clone(),
                last_tool: None,
                last_tool_at: None,
            });
        }
        v.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| b.mtime_ms.cmp(&a.mtime_ms)));
        v
    }

    /// Repositories: every git root a session ran in, plus what was found under the ghq
    /// root and any configured folders.
    pub fn repos(&mut self) -> Vec<RepoEntry> {
        self.rescan_repos(false);
        let mut map: BTreeMap<String, RepoEntry> = BTreeMap::new();
        for e in self.sessions.values() {
            let Some(r) = &e.repo else { continue };
            let Some(root) = &r.root else { continue };
            let ent = map.entry(r.key.clone()).or_insert_with(|| RepoEntry {
                key: r.key.clone(),
                name: r.name.clone(),
                root: root.clone(),
                sessions: 0,
                last_at: None,
                source: "sessions".into(),
                added: false,
                is_git: true,
            });
            if e.transcript.meta.user_turns > 0 || self.live.contains_key(&e.id) || self.runs.contains_key(&e.id) {
                ent.sessions += 1;
            }
            if e.transcript.meta.last_at > ent.last_at {
                ent.last_at = e.transcript.meta.last_at.clone();
            }
        }
        for r in &self.scanned {
            if let Some(root) = &r.root {
                map.entry(r.key.clone()).or_insert_with(|| RepoEntry {
                    key: r.key.clone(),
                    name: r.name.clone(),
                    root: root.clone(),
                    sessions: 0,
                    last_at: None,
                    source: "scan".into(),
                    added: false,
                    is_git: true,
                });
            }
        }
        for p in &self.added_repos {
            let r = repo::detect(&p.to_string_lossy());
            let is_git = r.root.is_some() && r.subdir.is_none();
            // A folder inside a repository is listed as itself, not as the repository.
            let (key, name, root) = if is_git {
                (r.key.clone(), r.name.clone(), r.root.clone().unwrap_or_default())
            } else {
                (repo::normalize(p), p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string()), p.display().to_string())
            };
            let ent = map.entry(key.clone()).or_insert_with(|| RepoEntry {
                key,
                name,
                root,
                sessions: 0,
                last_at: None,
                source: "added".into(),
                added: true,
                is_git,
            });
            ent.added = true;
        }
        let mut v: Vec<RepoEntry> = map.into_values().collect();
        v.sort_by(|a, b| b.last_at.cmp(&a.last_at).then_with(|| a.name.cmp(&b.name)));
        v
    }

    fn rescan_repos(&mut self, force: bool) {
        if !force {
            if let Some(t) = self.scanned_at {
                if t.elapsed() < Duration::from_secs(120) {
                    return;
                }
            }
        }
        self.scanned_at = Some(Instant::now());
        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(g) = &self.ghq_root {
            roots.extend(gitops::scan_ghq(g));
        }
        for folder in &self.repo_roots {
            roots.extend(gitops::scan_folder(folder));
        }
        self.scanned = roots
            .iter()
            .map(|p| repo::detect(&p.to_string_lossy()))
            .collect();
    }

    fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut ids: Vec<&String> = self.sessions.keys().collect();
        ids.sort();
        for id in ids {
            let e = &self.sessions[id];
            id.hash(&mut h);
            e.size.hash(&mut h);
            e.updated_ms.hash(&mut h);
            self.external_busy(e).hash(&mut h);
            e.transcript.meta.ai_title.hash(&mut h);
            e.transcript.meta.agent_name.hash(&mut h);
            e.agents.len().hash(&mut h);
            match self.live.get(id) {
                Some(l) => {
                    l.pid.hash(&mut h);
                    l.status.hash(&mut h);
                    l.waiting_for.hash(&mut h);
                    l.name.hash(&mut h);
                }
                None => 0u8.hash(&mut h),
            }
        }
        let mut run_ids: Vec<&String> = self.runs.keys().collect();
        run_ids.sort();
        for id in run_ids {
            id.hash(&mut h);
            let r = &self.runs[id];
            r.status.hash(&mut h);
            r.waiting_for.hash(&mut h);
            r.model.hash(&mut h);
            r.permission_mode.hash(&mut h);
            r.context_window.hash(&mut h);
        }
        self.added_repos.hash(&mut h);
        h.finish()
    }

    // ---- added repositories ---------------------------------------------------------------

    fn config_path(&self) -> PathBuf {
        self.oyakata_dir.join("config.json")
    }

    /// Read `~/.oyakata/config.json` (`repos`, `lang`, `agents`). The pre-0.2 location
    /// `~/.claude/oyakata.json` is imported when the new file does not exist yet.
    pub fn load_config(&mut self) {
        let text = match fs::read_to_string(self.config_path()) {
            Ok(t) => t,
            Err(_) => match fs::read_to_string(self.claude_dir.join("oyakata.json")) {
                Ok(t) => t,
                Err(_) => return,
            },
        };
        let Ok(v) = serde_json::from_str::<Value>(&text) else { return };
        self.added_repos = v
            .get("repos")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(PathBuf::from)
            .collect();
        self.lang = v.get("lang").and_then(Value::as_str).filter(|l| matches!(*l, "ja" | "en")).map(str::to_string);
        self.agent_overrides = v.get("agents").cloned().and_then(|a| serde_json::from_value(a).ok()).unwrap_or_default();
        self.worktree_default = v.get("worktree").and_then(Value::as_bool).unwrap_or(true);
        self.worktree_root = v.get("worktree_root").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(PathBuf::from);
    }

    /// Folder that holds the worktrees OYAKATA creates.
    pub fn worktrees_dir(&self) -> PathBuf {
        self.worktree_root.clone().unwrap_or_else(|| self.oyakata_dir.join("worktrees"))
    }

    pub fn save_config(&self) -> Result<()> {
        let mut v: Value = fs::read_to_string(self.config_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| serde_json::json!({}));
        v["repos"] = Value::Array(self.added_repos.iter().map(|p| Value::String(p.display().to_string())).collect());
        match &self.lang {
            Some(l) => v["lang"] = Value::String(l.clone()),
            None => {
                v.as_object_mut().map(|o| o.remove("lang"));
            }
        }
        v["agents"] = serde_json::to_value(&self.agent_overrides)?;
        v["worktree"] = Value::Bool(self.worktree_default);
        fs::create_dir_all(&self.oyakata_dir)?;
        fs::write(self.config_path(), serde_json::to_vec_pretty(&v)?)?;
        Ok(())
    }

    pub fn set_lang(&mut self, lang: Option<String>) -> Result<()> {
        self.lang = lang.filter(|l| matches!(l.as_str(), "ja" | "en"));
        self.save_config()
    }

    /// Enable or disable an agent; takes effect for new discoveries and the new-session dialog.
    pub fn set_agent_enabled(&mut self, kind: AgentKind, enabled: bool) -> Result<()> {
        self.agent_overrides.entry(kind.id().to_string()).or_default().enabled = Some(enabled);
        if let Some(a) = self.agents.iter_mut().find(|a| a.kind == kind) {
            a.enabled = enabled;
        }
        if !enabled {
            self.sessions.retain(|_, e| e.agent != kind);
        }
        self.save_config()
    }

    /// List a folder in the sidebar even when no session has run there yet.
    pub fn add_repo(&mut self, path: PathBuf) -> Result<()> {
        if !path.is_dir() {
            return Err(anyhow!("{}", if crate::i18n::is_ja() { format!("{} はフォルダではありません", path.display()) } else { format!("{} is not a folder", path.display()) }));
        }
        let key = repo::normalize(&path);
        if !self.added_repos.iter().any(|p| repo::normalize(p) == key) {
            self.added_repos.push(path);
            self.save_config()?;
        }
        Ok(())
    }

    pub fn remove_repo(&mut self, path: &str) -> Result<bool> {
        let key = repo::normalize(Path::new(path));
        let before = self.added_repos.len();
        self.added_repos.retain(|p| repo::normalize(p) != key);
        if self.added_repos.len() == before {
            return Ok(false);
        }
        self.save_config()?;
        Ok(true)
    }

    // ---- deleting sessions ----------------------------------------------------------------

    fn trash_dir(&self) -> PathBuf {
        self.oyakata_dir.join("trash")
    }

    /// Move a session's transcript (plus Claude's side folder of subagents, or Copilot's
    /// whole session folder) into `~/.oyakata/trash/<ms>-<id>/` so a mistaken delete can be
    /// undone. OpenCode keeps its sessions in a database, so those are deleted through its
    /// CLI and cannot be restored.
    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        if self.live.contains_key(id) || self.runs.contains_key(id) {
            return Err(anyhow!("{}", crate::i18n::tr("稼働中のセッションは削除できません。終了してから削除してください。", "A running session cannot be deleted. Stop it first.")));
        }
        let e = self.sessions.get(id).ok_or_else(|| anyhow!("unknown session: {id}"))?;
        if self.external_busy(e) {
            return Err(anyhow!("{}", crate::i18n::tr("別のプロセスで稼働中のセッションは削除できません。", "This session is running in another process and cannot be deleted.")));
        }
        let agent = e.agent;
        let path = e.path.clone();
        if agent == AgentKind::Opencode {
            let exe = self.agent_exe(agent).ok_or_else(|| anyhow!("opencode executable not found"))?;
            let out = exe.std_command().args(["session", "delete", id]).stdin(std::process::Stdio::null()).output()?;
            if !out.status.success() {
                return Err(anyhow!("opencode session delete: {}", String::from_utf8_lossy(&out.stderr).trim()));
            }
            self.sessions.remove(id);
            return Ok(());
        }
        // Copilot's transcript is one file inside a per-session folder: move the folder.
        let moved: PathBuf = if agent == AgentKind::Copilot { path.parent().map(Path::to_path_buf).unwrap_or(path.clone()) } else { path.clone() };
        let origin = moved.parent().map(Path::to_path_buf).ok_or_else(|| anyhow!("bad session path"))?;
        let dest = self.trash_dir().join(format!("{}-{id}", now_ms()));
        fs::create_dir_all(&dest)?;
        fs::write(dest.join("origin.txt"), origin.to_string_lossy().as_bytes())?;
        fs::write(dest.join("agent.txt"), agent.id())?;
        let file_name = moved.file_name().ok_or_else(|| anyhow!("bad session path"))?;
        fs::write(dest.join("name.txt"), file_name.to_string_lossy().as_bytes())?;
        fs::rename(&moved, dest.join(file_name)).map_err(|err| {
            let _ = fs::remove_dir_all(&dest);
            anyhow!("{}: {err}", crate::i18n::tr(&format!("{} を移動できませんでした", moved.display()), &format!("could not move {}", moved.display())))
        })?;
        if agent == AgentKind::Claude {
            let side = path.with_extension("");
            if side.is_dir() {
                let _ = fs::rename(&side, dest.join(id));
            }
        }
        self.sessions.remove(id);
        Ok(())
    }

    /// Undo `delete_session`.
    pub fn restore_session(&mut self, id: &str) -> Result<()> {
        let suffix = format!("-{id}");
        let found = fs::read_dir(self.trash_dir())
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.file_name().map(|n| n.to_string_lossy().ends_with(&suffix)).unwrap_or(false))
            .max()
            .ok_or_else(|| anyhow!("{}", crate::i18n::tr(&format!("ゴミ箱にセッション {id} が見つかりません"), &format!("session {id} is not in the trash"))))?;
        let origin = PathBuf::from(fs::read_to_string(found.join("origin.txt"))?.trim());
        fs::create_dir_all(&origin)?;
        let name = fs::read_to_string(found.join("name.txt")).map(|s| s.trim().to_string()).unwrap_or_else(|_| format!("{id}.jsonl"));
        if origin.join(&name).exists() {
            return Err(anyhow!("{}", crate::i18n::tr("元の場所に同じセッションが既にあります", "the session already exists at its original location")));
        }
        fs::rename(found.join(&name), origin.join(&name))?;
        if found.join(id).is_dir() {
            let _ = fs::rename(found.join(id), origin.join(id));
        }
        let _ = fs::remove_dir_all(&found);
        self.discover();
        self.refresh_one(id);
        Ok(())
    }

    /// Empty trash entries older than `max_age` (also in the pre-0.2 location).
    pub fn purge_trash(&self, max_age: Duration) {
        let cutoff = now_ms().saturating_sub(max_age.as_millis() as u64);
        for dir in [self.trash_dir(), self.claude_dir.join("oyakata-trash")] {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let ms: u64 = name.split('-').next().and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
                if ms < cutoff {
                    let _ = fs::remove_dir_all(e.path());
                }
            }
        }
    }

    // ---- team view --------------------------------------------------------------------------

    /// What the team view needs from the index, copied out so the subagent transcripts can
    /// be read without holding the state lock (see `build_team`).
    pub fn team_inputs(&mut self, id: &str) -> Result<TeamInputs> {
        self.scan_agents(id, false);
        let active = self.live.contains_key(id) || self.runs.contains_key(id);
        let entry = self.sessions.get(id).ok_or_else(|| anyhow!("unknown session: {id}"))?;
        let mut calls: HashMap<String, TeamCall> = HashMap::new();
        for (index, it) in entry.transcript.items.iter().enumerate() {
            if let Item::Tool { id: tu, name, input, result, ts, .. } = it {
                if name == "Agent" || name == "Task" {
                    calls.insert(
                        tu.clone(),
                        TeamCall { index, ts: ts.clone(), input: input.clone(), result: result.as_ref().map(|r| (r.is_error, r.text.clone())) },
                    );
                }
            }
        }
        Ok(TeamInputs { active, agents: entry.agents.clone(), calls })
    }

    /// (id, transcript path) of every session, most recently active first.
    pub fn transcript_files(&self) -> Vec<(String, PathBuf)> {
        let mut v: Vec<(&SessionEntry, Option<&String>)> =
            self.sessions.values().filter(|e| e.agent == AgentKind::Claude).map(|e| (e, e.transcript.meta.last_at.as_ref())).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| b.0.mtime_ms.cmp(&a.0.mtime_ms)));
        v.into_iter().map(|(e, _)| (e.id.clone(), e.path.clone())).collect()
    }

    /// A `Sessions` event when anything the sidebar shows has changed since the last call.
    pub fn take_sessions_event(&mut self) -> Option<Event> {
        let sig = self.signature();
        if sig == self.last_signature {
            return None;
        }
        self.last_signature = sig;
        Some(Event::Sessions { sessions: self.summaries() })
    }
}

pub struct TeamCall {
    index: usize,
    ts: Option<String>,
    input: Value,
    result: Option<(bool, String)>,
}

pub struct TeamInputs {
    active: bool,
    agents: Vec<AgentMeta>,
    calls: HashMap<String, TeamCall>,
}

/// The subagents of a session with their role, status and what they are doing now.
/// `cache` holds metadata-only parsers per subagent file, fed incrementally.
pub fn build_team(inputs: &TeamInputs, cache: &mut HashMap<PathBuf, Transcript>) -> Vec<TeamAgent> {
    let TeamInputs { active, agents, calls } = inputs;
    for a in agents {
        let size = fs::metadata(&a.path).map(|m| m.len()).unwrap_or(0);
        let t = cache.entry(a.path.clone()).or_insert_with(Transcript::for_agent_meta);
        if size < t.consumed {
            *t = Transcript::for_agent_meta();
        }
        if size > t.consumed {
            if let Ok(bytes) = read_from(&a.path, t.consumed) {
                t.feed(&bytes);
            }
        }
    }
    let mut out = Vec::new();
    for a in agents {
        let meta = cache.get(&a.path).map(|t| t.meta.clone()).unwrap_or_default();
        let call = a.tool_use_id.as_ref().and_then(|tu| calls.get(tu));
        let parent = match (&a.tool_use_id, call) {
            (Some(_), Some(_)) | (None, _) => "main".to_string(),
            (Some(tu), None) => agents
                .iter()
                .find(|o| o.agent_id != a.agent_id && cache.get(&o.path).map(|t| t.meta.agent_calls.contains(tu)).unwrap_or(false))
                .map(|o| o.agent_id.clone())
                .unwrap_or_else(|| "main".to_string()),
        };
        let input_str = |k: &str| call.and_then(|c| c.input.get(k)).and_then(Value::as_str).map(str::to_string);
        let background = a.background || call.and_then(|c| c.input.get("run_in_background")).and_then(Value::as_bool).unwrap_or(false);
        let result = call.and_then(|c| c.result.clone());
        let status = if result.as_ref().map(|(err, _)| *err).unwrap_or(false) {
            "error"
        } else if meta.finished || meta.last_stop_reason.as_deref() == Some("end_turn") || (result.is_some() && !background) {
            "done"
        } else if *active {
            "running"
        } else if meta.last_stop_reason.as_deref() == Some("tool_use") {
            // The session ended while this agent was between tool calls.
            "stopped"
        } else {
            "done"
        };
        out.push(TeamAgent {
            agent_id: a.agent_id.clone(),
            agent_type: a.agent_type.clone().or_else(|| input_str("subagent_type")),
            description: a.description.clone().or_else(|| input_str("description")),
            tool_use_id: a.tool_use_id.clone(),
            parent,
            index: call.map(|c| c.index),
            prompt: input_str("prompt").or(meta.first_prompt.clone()).map(|p| crate::transcript::snippet(&p, 600)),
            status: status.to_string(),
            background,
            model: meta.model.clone().or_else(|| input_str("model")),
            started_at: meta.started_at.clone().or_else(|| call.and_then(|c| c.ts.clone())),
            last_at: meta.last_at.clone(),
            tool_calls: meta.tool_calls,
            last_tool: meta.last_tool.clone(),
            last_tool_at: meta.last_tool_at.clone(),
            last_text: meta.last_text_snippet.clone(),
            // A background agent's tool result is only the launch notice; its report is its
            // own last message.
            result: result
                .filter(|(err, t)| !t.trim().is_empty() && (!background || *err))
                .map(|(_, t)| crate::transcript::snippet(&t, 400)),
            context_tokens: meta.context_tokens,
        });
    }
    out.sort_by(|a, b| a.index.unwrap_or(usize::MAX).cmp(&b.index.unwrap_or(usize::MAX)).then_with(|| a.started_at.cmp(&b.started_at)));
    out
}

/// One hit of the conversation search.
#[derive(Debug, Clone, Serialize)]
pub struct SessionHit {
    pub session: String,
    pub ts: Option<String>,
    /// `user` or `assistant`
    pub role: String,
    pub snippet: String,
}

/// Search what people and Claude wrote (not tool output) across transcripts, newest session
/// first. `files` are (session id, path). Case-insensitive for ASCII.
pub fn search_transcripts(files: &[(String, PathBuf)], query: &str, max: usize, per_session: usize) -> Vec<SessionHit> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() || files.is_empty() {
        return Vec::new();
    }
    // Files are independent: scan them on all cores, then keep the newest-first order.
    let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(files.len()).max(1);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut per_file: Vec<Vec<SessionHit>> = vec![Vec::new(); files.len()];
    let results = std::sync::Mutex::new(&mut per_file);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some((id, path)) = files.get(i) else { break };
                let hits = search_file(id, path, &q, per_session);
                if !hits.is_empty() {
                    results.lock().unwrap_or_else(|e| e.into_inner())[i] = hits;
                }
            });
        }
    });
    per_file.into_iter().flatten().take(max).collect()
}

/// ASCII case-insensitive substring test without allocating (`needle` is lowercase).
fn contains_ci(hay: &[u8], needle: &[u8]) -> bool {
    let Some((&first, rest)) = needle.split_first() else { return true };
    if hay.len() < needle.len() {
        return false;
    }
    let last = hay.len() - needle.len();
    let mut i = 0;
    while i <= last {
        if hay[i].to_ascii_lowercase() == first && hay[i + 1..i + needle.len()].iter().zip(rest).all(|(a, b)| a.to_ascii_lowercase() == *b) {
            return true;
        }
        i += 1;
    }
    false
}

fn search_file(id: &str, path: &Path, q: &str, per_session: usize) -> Vec<SessionHit> {
    let mut hits = Vec::new();
    let Ok(bytes) = fs::read(path) else { return hits };
    let qb = q.as_bytes();
    for raw in bytes.split(|&b| b == b'\n') {
        // One pass over the line for the query; JSON is parsed only for the rare hit.
        if !contains_ci(raw, qb) {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else { continue };
        if !(line.contains("\"type\":\"user\"") || line.contains("\"type\":\"assistant\"")) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) || v.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let role = v.get("type").and_then(Value::as_str).unwrap_or("").to_string();
        let text = match v.get("message").and_then(|m| m.get("content")) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n"),
            _ => continue,
        };
        // The hit may have been in tool output or metadata rather than in what was said.
        let lower = text.to_ascii_lowercase();
        let Some(pos) = lower.find(q) else { continue };
        // ASCII lowercasing keeps byte offsets, so `pos` is valid in `text` too.
        let start = text[..pos].char_indices().rev().nth(50).map(|(i, _)| i).unwrap_or(0);
        let snippet: String = text[start..].chars().take(180).collect::<String>().replace(['\n', '\r'], " ");
        hits.push(SessionHit {
            session: id.to_string(),
            ts: v.get("timestamp").and_then(Value::as_str).map(str::to_string),
            role,
            snippet: if start > 0 { format!("…{snippet}") } else { snippet },
        });
        if hits.len() >= per_session {
            break;
        }
    }
    hits
}

fn repo_for(cache: &mut HashMap<String, RepoInfo>, cwd: &str) -> RepoInfo {
    if let Some(r) = cache.get(cwd) {
        return r.clone();
    }
    let r = repo::detect(cwd);
    cache.insert(cwd.to_string(), r.clone());
    r
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn mtime_millis(md: &fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn read_from(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    if offset > 0 {
        f.seek(SeekFrom::Start(offset))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_insensitive_search_without_allocating() {
        assert!(contains_ci(b"Hello MonoRepo world", b"monorepo"));
        assert!(!contains_ci(b"mono repo", b"monorepo"));
        assert!(contains_ci("日本語のテスト".as_bytes(), "テスト".as_bytes()));
        assert!(!contains_ci(b"ab", b"abc"));
    }

    #[test]
    fn searches_what_was_said_not_tool_output() {
        let dir = std::env::temp_dir().join(format!("oyakata-search-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.jsonl");
        let lines = [
            r#"{"type":"user","timestamp":"t1","message":{"role":"user","content":"Please split into a MonoRepo"}}"#,
            r#"{"type":"user","timestamp":"t2","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"x","content":"monorepo in a tool result"}]}}"#,
            r#"{"type":"assistant","timestamp":"t3","message":{"role":"assistant","content":[{"type":"text","text":"Done: the monorepo has apps/web."}]}}"#,
        ];
        fs::write(&f, lines.join("\n")).unwrap();
        let hits = search_transcripts(&[("s".into(), f)], "monorepo", 10, 5);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(hits.iter().map(|h| h.ts.as_deref().unwrap_or("")).collect::<Vec<_>>(), vec!["t1", "t3"]);
        assert_eq!(hits[1].role, "assistant");
    }
}
