//! Incremental parser for Claude Code transcript files (`<session>.jsonl`).
//!
//! The file is append-only JSON lines. We turn the relevant line types into a flat list of
//! display items (user prompt, assistant text, tool call, …) and keep per-session metadata
//! (title, cwd, token counts). Feeding more bytes later continues where we left off, which is
//! what makes live tailing cheap.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

const MAX_RESULT_CHARS: usize = 60_000;
const SNIPPET_CHARS: usize = 200;
const MAX_ARTIFACTS: usize = 50;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct EditStat {
    pub edits: u32,
    pub writes: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ImageRef {
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub text: String,
    pub is_error: bool,
    #[serde(default)]
    pub images: Vec<ImageRef>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Item {
    User {
        ts: Option<String>,
        text: String,
        images: Vec<ImageRef>,
        /// Injected by the harness (task notifications, local command caveats…), not typed by a person.
        meta: bool,
        /// The summary Claude Code inserts when a session continues after running out of context.
        compact_summary: bool,
    },
    Text {
        ts: Option<String>,
        md: String,
        model: Option<String>,
    },
    Thinking {
        ts: Option<String>,
        text: String,
    },
    Tool {
        ts: Option<String>,
        id: String,
        name: String,
        input: Value,
        summary: String,
        result: Option<ToolResult>,
        /// Set when a subagent transcript exists for this tool call (Agent tool).
        agent_id: Option<String>,
    },
    Compact {
        ts: Option<String>,
        pre_tokens: Option<u64>,
        post_tokens: Option<u64>,
        trigger: Option<String>,
    },
    TurnEnd {
        ts: Option<String>,
        duration_ms: u64,
    },
    Note {
        ts: Option<String>,
        text: String,
    },
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct TranscriptMeta {
    pub ai_title: Option<String>,
    pub agent_name: Option<String>,
    pub summary: Option<String>,
    pub first_prompt: Option<String>,
    pub last_prompt: Option<String>,
    pub cwd: Option<String>,
    pub git_branch: Option<String>,
    pub version: Option<String>,
    pub model: Option<String>,
    pub started_at: Option<String>,
    pub last_at: Option<String>,
    pub user_turns: u32,
    pub assistant_messages: u32,
    pub tool_calls: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub continued_in: Option<String>,
    pub compactions: u32,
    pub last_text_snippet: Option<String>,
    /// Files touched by Edit/Write-style tools, keyed by path.
    pub edited_files: BTreeMap<String, EditStat>,
    /// claude.ai artifact links that appeared in tool results or assistant text.
    pub artifacts: Vec<String>,
    /// Tokens in the context window after the latest response (input + cache + output).
    pub context_tokens: u64,
    /// The session's permission mode as last recorded (`auto`, `default`, `plan`, …).
    pub permission_mode: Option<String>,
    /// Effort level of the latest response.
    pub effort: Option<String>,
    /// `Tool: summary` of the most recent tool call, and when it was made.
    pub last_tool: Option<String>,
    pub last_tool_at: Option<String>,
    /// `stop_reason` of the latest assistant line (`end_turn`, `tool_use`, or none mid-stream).
    pub last_stop_reason: Option<String>,
    /// tool_use ids of Agent/Task calls, so nested subagents can be attached to their parent.
    pub agent_calls: Vec<String>,
    /// A subagent reported back (`SubagentHandback`) or stopped (`SubagentStop` hook) and has
    /// not been given new work since.
    pub finished: bool,
}

impl TranscriptMeta {
    /// Best available title: AI-generated title, then the `-n` display name, then an older
    /// `summary` line, then the first typed prompt.
    pub fn title(&self) -> Option<String> {
        self.ai_title
            .clone()
            .or_else(|| self.agent_name.clone())
            .or_else(|| self.summary.clone())
            .or_else(|| self.first_prompt.as_deref().map(|p| snippet(p, 80)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Appended(usize),
    Patched(usize),
}

#[derive(Debug)]
pub struct Transcript {
    pub meta: TranscriptMeta,
    pub items: Vec<Item>,
    keep_items: bool,
    /// Subagent transcripts are written entirely as sidechain lines; main transcripts skip them.
    include_sidechain: bool,
    tool_index: HashMap<String, usize>,
    pending: Vec<u8>,
    /// Bytes consumed so far (including a trailing partial line held in `pending`).
    pub consumed: u64,
}

// ---- raw line shapes (only the fields we read) -------------------------------------------

#[derive(Deserialize)]
struct RawLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "gitBranch")]
    git_branch: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "isSidechain")]
    is_sidechain: bool,
    #[serde(default, rename = "isMeta")]
    is_meta: bool,
    #[serde(default, rename = "isCompactSummary")]
    is_compact_summary: bool,
    #[serde(default)]
    message: Option<RawMessage>,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default, rename = "durationMs")]
    duration_ms: Option<u64>,
    #[serde(default, rename = "aiTitle")]
    ai_title: Option<String>,
    #[serde(default, rename = "agentName")]
    agent_name: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default, rename = "continuedInSessionId")]
    continued_in: Option<String>,
    #[serde(default, rename = "compactMetadata")]
    compact_metadata: Option<Value>,
    #[serde(default, rename = "permissionMode")]
    permission_mode: Option<String>,
    #[serde(default)]
    effort: Option<String>,
    #[serde(default)]
    attachment: Option<Value>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    content: Value,
    #[serde(default)]
    usage: Option<RawUsage>,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cache_read_input_tokens: Option<u64>,
    #[serde(default)]
    cache_creation_input_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct RawBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    tool_use_id: Option<String>,
    #[serde(default)]
    content: Option<Value>,
    #[serde(default)]
    is_error: Option<bool>,
    #[serde(default)]
    source: Option<Value>,
}

impl Transcript {
    pub fn new(keep_items: bool) -> Self {
        Self {
            meta: TranscriptMeta::default(),
            items: Vec::new(),
            keep_items,
            include_sidechain: false,
            tool_index: HashMap::new(),
            pending: Vec::new(),
            consumed: 0,
        }
    }

    /// A parser for `subagents/agent-*.jsonl`, whose lines are all marked as sidechain.
    pub fn for_agent() -> Self {
        let mut t = Self::new(true);
        t.include_sidechain = true;
        t
    }

    /// Like `for_agent`, but keeps only metadata (for the team view's status cards).
    pub fn for_agent_meta() -> Self {
        let mut t = Self::new(false);
        t.include_sidechain = true;
        t
    }

    pub fn keeps_items(&self) -> bool {
        self.keep_items
    }

    /// Stop retaining items (metadata keeps updating). Used to evict idle sessions from memory.
    pub fn drop_items(&mut self) {
        self.keep_items = false;
        self.items = Vec::new();
        self.tool_index = HashMap::new();
    }

    /// Feed newly appended bytes. Only complete lines are processed; a trailing partial line
    /// is buffered until the next call.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Change> {
        self.consumed += bytes.len() as u64;
        let mut changes = Vec::new();
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(bytes);
        let mut start = 0;
        while let Some(rel) = buf[start..].iter().position(|&b| b == b'\n') {
            let line = &buf[start..start + rel];
            start += rel + 1;
            self.process_line(line, &mut changes);
        }
        self.pending = buf[start..].to_vec();
        changes
    }

    /// Attach a subagent transcript id to the tool call that spawned it.
    pub fn link_agent(&mut self, tool_use_id: &str, agent_id: &str) -> Option<usize> {
        let idx = *self.tool_index.get(tool_use_id)?;
        if let Some(Item::Tool { agent_id: slot, .. }) = self.items.get_mut(idx) {
            *slot = Some(agent_id.to_string());
            return Some(idx);
        }
        None
    }

    fn process_line(&mut self, raw: &[u8], changes: &mut Vec<Change>) {
        let raw = if raw.ends_with(b"\r") { &raw[..raw.len() - 1] } else { raw };
        if raw.is_empty() {
            return;
        }
        let Ok(line) = serde_json::from_slice::<RawLine>(raw) else {
            return;
        };
        match line.kind.as_str() {
            "ai-title" => {
                if let Some(t) = line.ai_title.filter(|t| !t.trim().is_empty()) {
                    self.meta.ai_title = Some(t);
                }
            }
            "agent-name" => {
                if let Some(n) = line.agent_name.filter(|n| !n.trim().is_empty()) {
                    self.meta.agent_name = Some(n);
                }
            }
            "summary" => {
                if let Some(s) = line.summary.filter(|s| !s.trim().is_empty()) {
                    self.meta.summary = Some(s);
                }
            }
            "continued-in" => self.meta.continued_in = line.continued_in,
            "permission-mode" => {
                if let Some(m) = line.permission_mode.filter(|m| !m.is_empty()) {
                    self.meta.permission_mode = Some(m);
                }
            }
            "attachment" => {
                let event = line.attachment.as_ref().and_then(|a| a.get("hookEvent")).and_then(Value::as_str);
                if event == Some("SubagentStop") {
                    self.meta.finished = true;
                }
            }
            "user" => {
                if line.is_sidechain && !self.include_sidechain {
                    return;
                }
                self.touch_common(&line);
                self.process_user(line, changes);
            }
            "assistant" => {
                if line.is_sidechain && !self.include_sidechain {
                    return;
                }
                self.touch_common(&line);
                self.process_assistant(line, changes);
            }
            "system" => {
                if line.is_sidechain && !self.include_sidechain {
                    return;
                }
                self.touch_common(&line);
                self.process_system(line, changes);
            }
            _ => {}
        }
    }

    fn touch_common(&mut self, line: &RawLine) {
        if let Some(ts) = &line.timestamp {
            if self.meta.started_at.is_none() {
                self.meta.started_at = Some(ts.clone());
            }
            self.meta.last_at = Some(ts.clone());
        }
        if let Some(cwd) = &line.cwd {
            if self.meta.cwd.is_none() {
                self.meta.cwd = Some(cwd.clone());
            }
        }
        if let Some(b) = &line.git_branch {
            self.meta.git_branch = Some(b.clone());
        }
        if let Some(v) = &line.version {
            self.meta.version = Some(v.clone());
        }
        if let Some(m) = line.permission_mode.as_ref().filter(|m| !m.is_empty()) {
            self.meta.permission_mode = Some(m.clone());
        }
    }

    fn push(&mut self, item: Item, changes: &mut Vec<Change>) {
        if !self.keep_items {
            return;
        }
        self.items.push(item);
        changes.push(Change::Appended(self.items.len() - 1));
    }

    fn process_user(&mut self, line: RawLine, changes: &mut Vec<Change>) {
        let Some(msg) = line.message else { return };
        let ts = line.timestamp.clone();
        let mut texts: Vec<String> = Vec::new();
        let mut images: Vec<ImageRef> = Vec::new();
        match msg.content {
            Value::String(s) => texts.push(s),
            Value::Array(blocks) => {
                for b in blocks {
                    let Ok(block) = serde_json::from_value::<RawBlock>(b) else { continue };
                    match block.kind.as_str() {
                        "text" => {
                            if let Some(t) = block.text {
                                texts.push(t);
                            }
                        }
                        "image" => {
                            if let Some(img) = image_from_source(block.source.as_ref()) {
                                images.push(img);
                            }
                        }
                        "tool_result" => {
                            let result = tool_result_from_block(&block);
                            collect_artifacts(&mut self.meta.artifacts, &result.text);
                            if let Some(id) = block.tool_use_id {
                                if let Some(&idx) = self.tool_index.get(&id) {
                                    if let Some(Item::Tool { result: slot, .. }) = self.items.get_mut(idx) {
                                        *slot = Some(result);
                                        changes.push(Change::Patched(idx));
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        let text = texts.join("\n\n");
        if text.trim().is_empty() && images.is_empty() {
            return;
        }
        let meta = line.is_meta || looks_meta(&text);
        let compact_summary = line.is_compact_summary;
        if !meta && !compact_summary {
            self.meta.finished = false;
            self.meta.user_turns += 1;
            if self.meta.first_prompt.is_none() {
                self.meta.first_prompt = Some(snippet(&text, SNIPPET_CHARS));
            }
            self.meta.last_prompt = Some(snippet(&text, SNIPPET_CHARS));
        }
        self.push(
            Item::User { ts, text, images, meta, compact_summary },
            changes,
        );
    }

    fn process_assistant(&mut self, line: RawLine, changes: &mut Vec<Change>) {
        let Some(msg) = line.message else { return };
        let ts = line.timestamp.clone();
        if let Some(model) = msg.model.as_deref() {
            if !model.starts_with('<') {
                self.meta.model = Some(model.to_string());
            }
        }
        if let Some(e) = line.effort.as_ref().filter(|e| !e.is_empty()) {
            self.meta.effort = Some(e.clone());
        }
        if let Some(u) = msg.usage {
            self.meta.input_tokens += u.input_tokens.unwrap_or(0);
            self.meta.output_tokens += u.output_tokens.unwrap_or(0);
            self.meta.cache_read_tokens += u.cache_read_input_tokens.unwrap_or(0);
            let context = u.input_tokens.unwrap_or(0)
                + u.cache_creation_input_tokens.unwrap_or(0)
                + u.cache_read_input_tokens.unwrap_or(0)
                + u.output_tokens.unwrap_or(0);
            if context > 0 {
                self.meta.context_tokens = context;
            }
        }
        self.meta.last_stop_reason = msg.stop_reason.clone();
        let Value::Array(blocks) = msg.content else { return };
        for b in blocks {
            let Ok(block) = serde_json::from_value::<RawBlock>(b) else { continue };
            match block.kind.as_str() {
                "text" => {
                    let Some(t) = block.text else { continue };
                    if t.trim().is_empty() {
                        continue;
                    }
                    self.meta.assistant_messages += 1;
                    self.meta.last_text_snippet = Some(snippet(&t, SNIPPET_CHARS));
                    collect_artifacts(&mut self.meta.artifacts, &t);
                    self.push(Item::Text { ts: ts.clone(), md: t, model: msg.model.clone() }, changes);
                }
                "thinking" => {
                    let Some(t) = block.thinking else { continue };
                    if t.trim().is_empty() {
                        continue;
                    }
                    self.push(Item::Thinking { ts: ts.clone(), text: t }, changes);
                }
                "tool_use" => {
                    let name = block.name.unwrap_or_else(|| "tool".into());
                    let input = block.input.unwrap_or(Value::Null);
                    let id = block.id.unwrap_or_default();
                    self.meta.tool_calls += 1;
                    self.note_edit(&name, &input);
                    let summary = summarize_tool(&name, &input);
                    self.meta.last_tool = Some(if summary.is_empty() { name.clone() } else { format!("{name}: {summary}") });
                    self.meta.last_tool_at = ts.clone();
                    if (name == "Agent" || name == "Task") && !id.is_empty() && self.meta.agent_calls.len() < 500 {
                        self.meta.agent_calls.push(id.clone());
                    }
                    if name == "SubagentHandback" {
                        self.meta.finished = true;
                    }
                    if self.keep_items {
                        let idx = self.items.len();
                        if !id.is_empty() {
                            self.tool_index.insert(id.clone(), idx);
                        }
                        self.push(
                            Item::Tool { ts: ts.clone(), id, name, input, summary, result: None, agent_id: None },
                            changes,
                        );
                    }
                }
                _ => {}
            }
        }
    }

    fn note_edit(&mut self, name: &str, input: &Value) {
        let path_key = match name {
            "Edit" | "MultiEdit" | "Write" => "file_path",
            "NotebookEdit" => "notebook_path",
            _ => return,
        };
        let Some(path) = input.get(path_key).and_then(Value::as_str) else { return };
        let stat = self.meta.edited_files.entry(path.to_string()).or_default();
        if name == "Write" {
            stat.writes += 1;
        } else {
            stat.edits += 1;
        }
    }

    fn process_system(&mut self, line: RawLine, changes: &mut Vec<Change>) {
        let ts = line.timestamp.clone();
        match line.subtype.as_deref() {
            Some("turn_duration") => {
                if let Some(ms) = line.duration_ms {
                    self.push(Item::TurnEnd { ts, duration_ms: ms }, changes);
                }
            }
            Some("compact_boundary") => {
                self.meta.compactions += 1;
                let cm = line.compact_metadata.unwrap_or(Value::Null);
                if let Some(post) = cm.get("postTokens").and_then(Value::as_u64) {
                    self.meta.context_tokens = post;
                }
                self.push(
                    Item::Compact {
                        ts,
                        pre_tokens: cm.get("preTokens").and_then(Value::as_u64),
                        post_tokens: cm.get("postTokens").and_then(Value::as_u64),
                        trigger: cm.get("trigger").and_then(Value::as_str).map(str::to_string),
                    },
                    changes,
                );
            }
            Some("local_command") => {
                let text = match line.content {
                    Some(Value::String(s)) => s,
                    _ => String::new(),
                };
                if !text.trim().is_empty() {
                    self.push(Item::Note { ts, text }, changes);
                }
            }
            _ => {}
        }
    }
}

fn image_from_source(source: Option<&Value>) -> Option<ImageRef> {
    let s = source?;
    if s.get("type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    Some(ImageRef {
        media_type: s.get("media_type").and_then(Value::as_str).unwrap_or("image/png").to_string(),
        data: s.get("data").and_then(Value::as_str)?.to_string(),
    })
}

fn tool_result_from_block(block: &RawBlock) -> ToolResult {
    let mut texts: Vec<String> = Vec::new();
    let mut images: Vec<ImageRef> = Vec::new();
    match &block.content {
        Some(Value::String(s)) => texts.push(s.clone()),
        Some(Value::Array(parts)) => {
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            texts.push(t.to_string());
                        }
                    }
                    Some("image") => {
                        if let Some(img) = image_from_source(p.get("source")) {
                            images.push(img);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let joined = texts.join("\n");
    let truncated = joined.chars().count() > MAX_RESULT_CHARS;
    let text = if truncated { joined.chars().take(MAX_RESULT_CHARS).collect() } else { joined };
    ToolResult { text, is_error: block.is_error.unwrap_or(false), images, truncated }
}

/// Pull `https://claude.ai/artifact/<id>` style links out of free text.
fn collect_artifacts(out: &mut Vec<String>, text: &str) {
    if out.len() >= MAX_ARTIFACTS || !text.contains("claude.ai/") {
        return;
    }
    for marker in ["https://claude.ai/artifact/", "https://claude.ai/code/artifact/"] {
        let mut rest = text;
        while let Some(pos) = rest.find(marker) {
            let tail = &rest[pos..];
            let end = tail
                .find(|c: char| c.is_whitespace() || matches!(c, ')' | ']' | '"' | '\'' | '>' | '<' | ',' | '。' | '、'))
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches('.').to_string();
            if url.len() > marker.len() && !out.contains(&url) {
                out.push(url);
                if out.len() >= MAX_ARTIFACTS {
                    return;
                }
            }
            rest = &tail[end.max(1)..];
        }
    }
}

/// Harness-injected prompts start with a known tag; a person's own prompt almost never does.
fn looks_meta(text: &str) -> bool {
    const TAGS: &[&str] = &[
        "<task-notification>",
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<command-name>",
        "<command-message>",
        "<system-reminder>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "<user-prompt-submit-hook>",
        "<task-reminder>",
        "<ide_",
        "[Request interrupted",
    ];
    let t = text.trim_start();
    TAGS.iter().any(|tag| t.starts_with(tag))
}

/// One-line label for a tool call chip, chosen per tool.
pub fn summarize_tool(name: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let first_line = |v: String| v.lines().next().unwrap_or("").trim().to_string();
    let out = match name {
        "Bash" | "PowerShell" => s("description").or_else(|| s("command").map(first_line)),
        "Read" | "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => s("file_path").or_else(|| s("notebook_path")),
        "Grep" => s("pattern").map(|p| match s("path") {
            Some(path) => format!("{p}  in {path}"),
            None => p,
        }),
        "Glob" => s("pattern"),
        "Agent" | "Task" => s("description").or_else(|| s("prompt").map(first_line)),
        "Skill" => s("skill").map(|sk| match s("args") {
            Some(a) if !a.is_empty() => format!("/{sk} {a}"),
            _ => format!("/{sk}"),
        }),
        "WebFetch" => s("url"),
        "WebSearch" => s("query"),
        "AskUserQuestion" => input
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|q| q.first())
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .map(str::to_string),
        "Artifact" => {
            let action = s("action").unwrap_or_else(|| "publish".into());
            let target = s("file_path").or_else(|| s("url")).or_else(|| s("title")).unwrap_or_default();
            Some(format!("{action} {target}").trim().to_string())
        }
        "TodoWrite" => input
            .get("todos")
            .and_then(Value::as_array)
            .map(|t| format!("{} items", t.len())),
        "ToolSearch" => s("query"),
        _ => None,
    };
    let out = out.unwrap_or_else(|| first_string(input).unwrap_or_default());
    snippet(&first_line(out), 140)
}

fn first_string(v: &Value) -> Option<String> {
    match v {
        Value::Object(map) => map.values().find_map(|x| match x {
            Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
            _ => None,
        }),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

pub fn snippet(text: &str, max_chars: usize) -> String {
    let t = text.trim();
    let mut out: String = t.chars().take(max_chars).collect();
    if t.chars().count() > max_chars {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_all(lines: &[&str], keep: bool) -> (Transcript, Vec<Change>) {
        let mut t = Transcript::new(keep);
        let joined = lines.join("\n") + "\n";
        let changes = t.feed(joined.as_bytes());
        (t, changes)
    }

    #[test]
    fn pairs_tool_results_with_calls() {
        let (t, changes) = feed_all(
            &[
                r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","cwd":"C:\\w","message":{"role":"user","content":"hello"}}"#,
                r#"{"type":"assistant","timestamp":"2026-01-01T00:00:01Z","message":{"model":"claude-x","role":"assistant","content":[{"type":"text","text":"hi"},{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls -la","description":"List files"}}],"usage":{"output_tokens":5}}}"#,
                r#"{"type":"user","timestamp":"2026-01-01T00:00:02Z","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu1","content":"a\nb","is_error":false}]}}"#,
                r#"{"type":"system","subtype":"turn_duration","durationMs":1234,"timestamp":"2026-01-01T00:00:03Z"}"#,
            ],
            true,
        );
        assert_eq!(t.items.len(), 4);
        assert!(matches!(&t.items[2], Item::Tool { summary, result: Some(r), .. } if summary == "List files" && r.text == "a\nb"));
        assert!(changes.contains(&Change::Patched(2)));
        assert_eq!(t.meta.user_turns, 1);
        assert_eq!(t.meta.tool_calls, 1);
        assert_eq!(t.meta.output_tokens, 5);
        assert_eq!(t.meta.model.as_deref(), Some("claude-x"));
        assert_eq!(t.meta.cwd.as_deref(), Some("C:\\w"));
        assert_eq!(t.meta.title().as_deref(), Some("hello"));
    }

    #[test]
    fn partial_lines_wait_for_newline() {
        let mut t = Transcript::new(true);
        let line = r#"{"type":"user","message":{"role":"user","content":"split"}}"#;
        let (a, b) = line.split_at(20);
        assert!(t.feed(a.as_bytes()).is_empty());
        assert_eq!(t.items.len(), 0);
        let c = t.feed(format!("{b}\n").as_bytes());
        assert_eq!(c, vec![Change::Appended(0)]);
        assert_eq!(t.consumed, (line.len() + 1) as u64);
    }

    #[test]
    fn meta_prompts_do_not_count_as_turns() {
        let (t, _) = feed_all(
            &[
                r#"{"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>x</local-command-caveat>"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"<task-notification>done</task-notification>"}}"#,
                r#"{"type":"user","isCompactSummary":true,"message":{"role":"user","content":"This session is being continued"}}"#,
                r#"{"type":"user","message":{"role":"user","content":"real question"}}"#,
                r#"{"type":"ai-title","aiTitle":"A title"}"#,
            ],
            true,
        );
        assert_eq!(t.meta.user_turns, 1);
        assert_eq!(t.meta.first_prompt.as_deref(), Some("real question"));
        assert_eq!(t.meta.title().as_deref(), Some("A title"));
        assert!(matches!(&t.items[0], Item::User { meta: true, .. }));
        assert!(matches!(&t.items[1], Item::User { meta: true, .. }));
        assert!(matches!(&t.items[2], Item::User { compact_summary: true, .. }));
    }

    #[test]
    fn meta_only_mode_keeps_counts_without_items() {
        let (t, changes) = feed_all(
            &[
                r#"{"type":"user","message":{"role":"user","content":"q"}}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t","name":"Read","input":{"file_path":"/x"}}]}}"#,
            ],
            false,
        );
        assert!(changes.is_empty());
        assert!(t.items.is_empty());
        assert_eq!(t.meta.tool_calls, 1);
        assert_eq!(t.meta.user_turns, 1);
    }

    #[test]
    fn tracks_edited_files_and_artifact_links() {
        let (t, _) = feed_all(
            &[
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"a","name":"Edit","input":{"file_path":"/p/a.rs","old_string":"x","new_string":"y"}},{"type":"tool_use","id":"b","name":"Write","input":{"file_path":"/p/a.rs","content":"z"}},{"type":"tool_use","id":"c","name":"Artifact","input":{"file_path":"/p/x.html"}}]}}"#,
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c","content":"Published /p/x.html at https://claude.ai/artifact/AbC123 (Version 1)"}]}}"#,
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"See https://claude.ai/artifact/AbC123 and https://claude.ai/artifact/Zz9。"}]}}"#,
            ],
            false,
        );
        let stat = &t.meta.edited_files["/p/a.rs"];
        assert_eq!((stat.edits, stat.writes), (1, 1));
        assert_eq!(t.meta.artifacts, vec!["https://claude.ai/artifact/AbC123", "https://claude.ai/artifact/Zz9"]);
    }

    #[test]
    fn tracks_context_mode_effort_and_last_tool() {
        let (t, _) = feed_all(
            &[
                r#"{"type":"permission-mode","permissionMode":"plan","sessionId":"s"}"#,
                r#"{"type":"assistant","effort":"xhigh","message":{"model":"claude-opus-5-5","role":"assistant","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Agent","input":{"description":"look around","prompt":"p"}}],"usage":{"input_tokens":2,"cache_creation_input_tokens":1000,"cache_read_input_tokens":20000,"output_tokens":50}}}"#,
                r#"{"type":"permission-mode","permissionMode":"auto","sessionId":"s"}"#,
            ],
            false,
        );
        assert_eq!(t.meta.context_tokens, 21_052);
        assert_eq!(t.meta.permission_mode.as_deref(), Some("auto"));
        assert_eq!(t.meta.effort.as_deref(), Some("xhigh"));
        assert_eq!(t.meta.last_tool.as_deref(), Some("Agent: look around"));
        assert_eq!(t.meta.last_stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(t.meta.agent_calls, vec!["t1"]);
        assert!(!t.meta.finished);

        let mut agent = Transcript::for_agent_meta();
        let lines = [
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"investigate"}}"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"role":"assistant","stop_reason":null,"content":[{"type":"tool_use","id":"h","name":"SubagentHandback","input":{"report":"done"}}]}}"#,
        ];
        agent.feed((lines.join("\n") + "\n").as_bytes());
        assert!(agent.meta.finished);
        agent.feed(b"{\"type\":\"user\",\"isSidechain\":true,\"message\":{\"role\":\"user\",\"content\":\"one more thing\"}}\n");
        assert!(!agent.meta.finished);
    }

    #[test]
    fn tool_summaries() {
        assert_eq!(summarize_tool("Read", &serde_json::json!({"file_path":"/a/b.rs"})), "/a/b.rs");
        assert_eq!(summarize_tool("Skill", &serde_json::json!({"skill":"crit","args":"plan.md"})), "/crit plan.md");
        assert_eq!(summarize_tool("Bash", &serde_json::json!({"command":"cargo build\n--release"})), "cargo build");
        assert_eq!(summarize_tool("Mystery", &serde_json::json!({"n":1,"q":"hello"})), "hello");
    }
}
