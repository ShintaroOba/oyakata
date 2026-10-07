//! The session index: every transcript under `~/.claude/projects`, its metadata, which ones
//! are live, which ones OYAKATA itself is running, and incremental refresh that turns file
//! growth into events for the browser.

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
    #[serde(skip)]
    pub path: PathBuf,
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
}

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
}

#[derive(Debug, Clone, Serialize)]
pub struct RepoEntry {
    pub key: String,
    pub name: String,
    pub root: String,
    pub sessions: usize,
    pub last_at: Option<String>,
    pub source: String,
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
    pub status: String,
    pub waiting_for: Option<String>,
    pub cwd: String,
}

pub struct State {
    pub claude_dir: PathBuf,
    pub sessions: HashMap<String, SessionEntry>,
    pub live: HashMap<String, LiveInfo>,
    pub runs: HashMap<String, RunState>,
    pub repo_roots: Vec<PathBuf>,
    pub ghq_root: Option<PathBuf>,
    repo_cache: HashMap<String, RepoInfo>,
    scanned: Vec<RepoInfo>,
    scanned_at: Option<Instant>,
    last_signature: u64,
}

impl State {
    pub fn new(claude_dir: PathBuf) -> Self {
        Self {
            claude_dir,
            sessions: HashMap::new(),
            live: HashMap::new(),
            runs: HashMap::new(),
            repo_roots: Vec::new(),
            ghq_root: None,
            repo_cache: HashMap::new(),
            scanned: Vec::new(),
            scanned_at: None,
            last_signature: 0,
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
                self.sessions.insert(
                    stem.clone(),
                    SessionEntry {
                        id: stem.clone(),
                        path: fpath,
                        project_dir: project_dir.clone(),
                        size: 0,
                        mtime_ms: 0,
                        transcript: Transcript::new(false),
                        last_viewed: None,
                        repo: None,
                        agents: Vec::new(),
                        agents_scanned_at: None,
                    },
                );
                added.push(stem);
            }
        }
        added
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
        let size = md.len();
        let mtime_ms = mtime_millis(&md);
        if size == entry.size && mtime_ms == entry.mtime_ms {
            return events;
        }
        if size < entry.transcript.consumed {
            let keep = entry.transcript.keeps_items();
            entry.transcript = Transcript::new(keep);
            if keep {
                events.push(Event::Reset { session: id.to_string() });
            }
        }
        let old_len = entry.transcript.items.len();
        let changes = match read_from(&entry.path, entry.transcript.consumed) {
            Ok(bytes) => entry.transcript.feed(&bytes),
            Err(_) => return events,
        };
        entry.size = size;
        entry.mtime_ms = mtime_ms;
        if entry.repo.is_none() {
            if let Some(cwd) = entry.transcript.meta.cwd.clone() {
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
                self.runs.insert(id.to_string(), RunState { status: v.status.clone(), waiting_for, cwd: v.cwd.clone() });
            }
            _ => {
                self.runs.remove(id);
            }
        }
    }

    /// Where a session's file is and whether its items are already in memory. Lets callers
    /// parse a large file without holding the state lock, then hand the result to `install`.
    pub fn path_of(&self, id: &str) -> Option<(PathBuf, bool)> {
        self.sessions.get(id).map(|e| (e.path.clone(), e.transcript.keeps_items()))
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
            });
        let (owner, status, waiting_for) = match (run, &live) {
            (Some(r), _) => (Some("oyakata".to_string()), r.status.clone(), r.waiting_for.clone()),
            (None, Some(l)) => (Some("terminal".to_string()), l.status.clone(), l.waiting_for.clone()),
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
            project_dir: e.project_dir.clone(),
            cwd,
            repo,
            title: m.title().or_else(|| live.as_ref().and_then(|l| l.name.clone())),
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
        }
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
                project_dir: String::new(),
                cwd: Some(run.cwd.clone()),
                repo,
                title: Some("（開始中）".into()),
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
                });
            }
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
        }
        h.finish()
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

fn repo_for(cache: &mut HashMap<String, RepoInfo>, cwd: &str) -> RepoInfo {
    if let Some(r) = cache.get(cwd) {
        return r.clone();
    }
    let r = repo::detect(cwd);
    cache.insert(cwd.to_string(), r.clone());
    r
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
