//! Codex CLI (OpenAI). Sessions are append-only rollout files
//! `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-<timestamp>-<uuid>.jsonl`; every line is
//! `{"timestamp", "type", "payload"}` where `type` is `session_meta`, `turn_context`,
//! `response_item` (the model's own items: messages, function calls and outputs, reasoning),
//! `event_msg`, `token_usage_record`, … Turns are driven with `codex exec --json` and
//! `codex exec resume --json <id>`; the prompt goes in on stdin (`-`).

use super::canon::{self, Usage};
use super::{str_of, AgentKind, Discovered, ExecCommand, ExecEvent, ExecOpts};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub fn discover(data_dir: &Path) -> Vec<Discovered> {
    let mut out = Vec::new();
    walk(&data_dir.join("sessions"), 0, &mut out);
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<Discovered>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 4 {
                walk(&p, depth + 1, out);
            }
            continue;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
        let Some(stem) = name.strip_prefix("rollout-").and_then(|s| s.strip_suffix(".jsonl")) else { continue };
        let Some(id) = session_id_from_stem(stem) else { continue };
        let first = super::first_line(&p);
        let cwd = first.as_ref().and_then(|v| v.pointer("/payload/cwd")).and_then(Value::as_str).map(str::to_string);
        out.push(Discovered { id, agent: AgentKind::Codex, path: p, project_dir: "codex".into(), cwd, title: None, updated_ms: None });
    }
}

/// `rollout-2026-10-08T10-22-25-<uuid>` → the trailing UUID.
fn session_id_from_stem(stem: &str) -> Option<String> {
    if stem.len() < 36 {
        return None;
    }
    let id = &stem[stem.len() - 36..];
    let ok = id.chars().enumerate().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    ok.then(|| id.to_lowercase())
}

/// Incremental converter: feed one rollout line, get canonical lines back.
#[derive(Default)]
pub struct Converter {
    model: Option<String>,
    cwd: Option<String>,
    /// Tool names by call id, so outputs can be attached without the UI needing the name.
    calls: HashMap<String, String>,
}

impl Converter {
    pub fn convert_line(&mut self, raw: &[u8]) -> Vec<Value> {
        let Ok(line) = serde_json::from_slice::<Value>(raw) else { return Vec::new() };
        let ts = str_of(&line, "timestamp").map(str::to_string);
        let ts = ts.as_deref();
        let payload = line.get("payload").cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();
        match str_of(&line, "type").unwrap_or("") {
            "session_meta" => {
                self.cwd = str_of(&payload, "cwd").map(str::to_string);
                out.push(canon::session(ts, self.cwd.as_deref(), str_of(&payload, "cli_version")));
                if let Some(name) = str_of(&payload, "thread_name").filter(|s| !s.trim().is_empty()) {
                    out.push(canon::title(name));
                }
            }
            "turn_context" => {
                if let Some(m) = str_of(&payload, "model") {
                    self.model = Some(m.to_string());
                }
                if self.cwd.is_none() {
                    self.cwd = str_of(&payload, "cwd").map(str::to_string);
                }
                let sandbox = payload.pointer("/sandbox_policy/type").and_then(Value::as_str).unwrap_or("");
                let mode = match sandbox {
                    "read-only" => "plan",
                    "danger-full-access" => "bypassPermissions",
                    "workspace-write" => "acceptEdits",
                    _ => "",
                };
                if !mode.is_empty() {
                    out.push(canon::permission_mode(mode));
                }
            }
            "response_item" => self.response_item(ts, &payload, &mut out),
            "event_msg" => match str_of(&payload, "type").unwrap_or("") {
                "task_complete" | "turn_complete" => {
                    out.push(canon::turn_end(ts, payload.get("duration_ms").and_then(Value::as_u64).unwrap_or(0)));
                }
                "thread_name_updated" | "thread_name" => {
                    if let Some(name) = str_of(&payload, "name").or_else(|| str_of(&payload, "thread_name")).filter(|s| !s.trim().is_empty()) {
                        out.push(canon::title(name));
                    }
                }
                _ => {}
            },
            "token_usage_record" => {
                let u = payload.get("turn_token_usage").or_else(|| payload.get("usage")).cloned().unwrap_or(Value::Null);
                if let Some(usage) = usage_of(&u) {
                    out.push(canon::usage_only(ts, self.model.as_deref(), usage));
                }
            }
            "compacted" => out.push(json!({ "type": "system", "subtype": "compact_boundary", "timestamp": ts, "compactMetadata": { "trigger": "auto" } })),
            _ => {}
        }
        out
    }

    fn response_item(&mut self, ts: Option<&str>, item: &Value, out: &mut Vec<Value>) {
        let model = self.model.clone();
        match str_of(item, "type").unwrap_or("") {
            "message" => {
                let role = str_of(item, "role").unwrap_or("");
                let text = content_text(item.get("content"));
                if text.trim().is_empty() {
                    return;
                }
                match role {
                    "user" => out.push(canon::user(ts, &text, self.cwd.as_deref(), canon::looks_injected(&text))),
                    "assistant" => out.push(canon::assistant(ts, model.as_deref(), vec![canon::text(&text)], None, Some("end_turn"))),
                    _ => {}
                }
            }
            "reasoning" => {
                let mut parts: Vec<String> = Vec::new();
                for s in item.get("summary").and_then(Value::as_array).into_iter().flatten() {
                    if let Some(t) = str_of(s, "text") {
                        parts.push(t.to_string());
                    }
                }
                let t = parts.join("\n\n");
                if !t.trim().is_empty() {
                    out.push(canon::assistant(ts, model.as_deref(), vec![canon::thinking(&t)], None, None));
                }
            }
            "function_call" => {
                let name = str_of(item, "name").unwrap_or("tool").to_string();
                let id = str_of(item, "call_id").unwrap_or("").to_string();
                let input = match str_of(item, "arguments") {
                    Some(a) => serde_json::from_str::<Value>(a).unwrap_or_else(|_| json!({ "input": a })),
                    None => Value::Null,
                };
                self.calls.insert(id.clone(), name.clone());
                out.push(canon::assistant(ts, model.as_deref(), vec![canon::tool_use(&id, &name, input)], None, Some("tool_use")));
            }
            "custom_tool_call" => {
                let name = str_of(item, "name").unwrap_or("tool").to_string();
                let id = str_of(item, "call_id").unwrap_or("").to_string();
                let input = json!({ "input": str_of(item, "input").unwrap_or("") });
                self.calls.insert(id.clone(), name.clone());
                out.push(canon::assistant(ts, model.as_deref(), vec![canon::tool_use(&id, &name, input)], None, Some("tool_use")));
            }
            "local_shell_call" => {
                let id = str_of(item, "call_id").unwrap_or("").to_string();
                let cmd: Vec<String> = item
                    .pointer("/action/command")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                    .unwrap_or_default();
                self.calls.insert(id.clone(), "shell".into());
                out.push(canon::assistant(ts, model.as_deref(), vec![canon::tool_use(&id, "shell", json!({ "command": cmd.join(" ") }))], None, Some("tool_use")));
            }
            "web_search_call" => {
                let id = str_of(item, "id").unwrap_or("").to_string();
                let q = item.pointer("/action/query").and_then(Value::as_str).unwrap_or("");
                out.push(canon::assistant(ts, model.as_deref(), vec![canon::tool_use(&id, "web_search", json!({ "query": q }))], None, Some("tool_use")));
            }
            "function_call_output" | "custom_tool_call_output" => {
                let id = str_of(item, "call_id").unwrap_or("").to_string();
                let output = output_text(item.get("output"));
                let is_error = output.trim_start().starts_with("Error") || output.contains("exit code") && !output.contains("exit code 0");
                out.push(canon::tool_results(ts, vec![canon::tool_result(&id, &output, is_error)]));
            }
            _ => {}
        }
    }
}

/// Text of a Codex message's content items (`input_text` / `output_text` / `text`).
fn content_text(content: Option<&Value>) -> String {
    let mut parts = Vec::new();
    match content {
        Some(Value::String(s)) => parts.push(s.clone()),
        Some(Value::Array(items)) => {
            for it in items {
                if matches!(str_of(it, "type"), Some("input_text") | Some("output_text") | Some("text")) {
                    if let Some(t) = str_of(it, "text") {
                        parts.push(t.to_string());
                    }
                }
            }
        }
        _ => {}
    }
    parts.join("\n")
}

/// `function_call_output.output` is a string, or `{content: string | [{text}]}`.
fn output_text(output: Option<&Value>) -> String {
    match output {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(map)) => match map.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(items)) => items.iter().filter_map(|i| str_of(i, "text")).collect::<Vec<_>>().join("\n"),
            _ => serde_json::to_string(output.unwrap()).unwrap_or_default(),
        },
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

fn usage_of(u: &Value) -> Option<Usage> {
    if !u.is_object() {
        return None;
    }
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cached = n("cached_input_tokens");
    Some(Usage {
        input: n("input_tokens").saturating_sub(cached),
        cache_read: cached,
        cache_write: n("cache_write_input_tokens"),
        output: n("output_tokens"),
    })
}

/// `codex exec --json … -` (prompt on stdin) or `codex exec resume --json <id> -`.
pub fn exec_command(opts: &ExecOpts) -> ExecCommand {
    let mut args: Vec<String> = vec!["exec".into()];
    if let Some(id) = &opts.resume {
        args.push("resume".into());
        args.push("--json".into());
        args.push(id.clone());
    } else {
        args.push("--json".into());
        args.push("--skip-git-repo-check".into());
        args.push("-C".into());
        args.push(opts.cwd.display().to_string());
    }
    // Config overrides are accepted by both `exec` and `exec resume`.
    let sandbox = match opts.mode.as_str() {
        "plan" => "read-only",
        "bypassPermissions" => "danger-full-access",
        _ => "workspace-write",
    };
    args.push("-c".into());
    args.push(format!("sandbox_mode=\"{sandbox}\""));
    args.push("-c".into());
    args.push("approval_policy=\"never\"".into());
    if let Some(m) = &opts.model {
        args.push("-c".into());
        args.push(format!("model=\"{m}\""));
    }
    if let Some(e) = &opts.effort {
        args.push("-c".into());
        args.push(format!("model_reasoning_effort=\"{e}\""));
    }
    args.push("-".into());
    ExecCommand { args, stdin: Some(opts.prompt.clone()) }
}

pub fn exec_event(v: &Value) -> ExecEvent {
    match str_of(v, "type").unwrap_or("") {
        "thread.started" => str_of(v, "thread_id").map(|s| ExecEvent::SessionId(s.to_string())).unwrap_or(ExecEvent::Ignore),
        "turn.started" => ExecEvent::TurnStarted,
        "turn.completed" => ExecEvent::TurnDone { error: None },
        "turn.failed" => ExecEvent::TurnDone { error: Some(v.pointer("/error/message").and_then(Value::as_str).unwrap_or("turn failed").to_string()) },
        "error" => ExecEvent::Error(str_of(v, "message").unwrap_or("error").to_string()),
        "item.started" | "item.updated" | "item.completed" => {
            let item = v.get("item").cloned().unwrap_or(Value::Null);
            match str_of(&item, "type").unwrap_or("") {
                "command_execution" => ExecEvent::Tool(format!("shell: {}", str_of(&item, "command").unwrap_or("").lines().next().unwrap_or(""))),
                "file_change" => ExecEvent::Tool("apply_patch".into()),
                "mcp_tool_call" => ExecEvent::Tool(format!("{}: {}", str_of(&item, "server").unwrap_or("mcp"), str_of(&item, "tool").unwrap_or(""))),
                "web_search" => ExecEvent::Tool(format!("web_search: {}", str_of(&item, "query").unwrap_or(""))),
                "agent_message" => ExecEvent::Message(str_of(&item, "text").unwrap_or("").to_string()),
                "error" => ExecEvent::Error(str_of(&item, "message").unwrap_or("error").to_string()),
                _ => ExecEvent::Ignore,
            }
        }
        _ => ExecEvent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Item, Transcript};

    const FIXTURE: &str = "tests/fixtures/codex/rollout-2026-10-08T10-22-25-01a1191a-be38-7192-9d4a-d6e939ff6c5d.jsonl";

    fn fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE)
    }

    #[test]
    fn discovers_rollouts_by_uuid() {
        assert_eq!(session_id_from_stem("2026-10-08T10-22-25-01a1191a-be38-7192-9d4a-d6e939ff6c5d").as_deref(), Some("01a1191a-be38-7192-9d4a-d6e939ff6c5d"));
        assert_eq!(session_id_from_stem("not-a-rollout"), None);
        let tmp = std::env::temp_dir().join(format!("oyk-codex-{}", std::process::id()));
        let day = tmp.join("sessions/2026/10/08");
        fs::create_dir_all(&day).unwrap();
        fs::copy(fixture_path(), day.join("rollout-2026-10-08T10-22-25-01a1191a-be38-7192-9d4a-d6e939ff6c5d.jsonl")).unwrap();
        let found = discover(&tmp);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "01a1191a-be38-7192-9d4a-d6e939ff6c5d");
        assert_eq!(found[0].cwd.as_deref(), Some("C:\\Temp\\agents\\work"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn converts_a_real_rollout() {
        let bytes = fs::read(fixture_path()).unwrap();
        let mut conv = Converter::default();
        let mut lines = Vec::new();
        for raw in bytes.split(|&b| b == b'\n') {
            lines.extend(conv.convert_line(raw));
        }
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&lines));
        assert_eq!(t.meta.cwd.as_deref(), Some("C:\\Temp\\agents\\work"));
        assert_eq!(t.meta.version.as_deref(), Some("0.161.0"));
        assert_eq!(t.meta.model.as_deref(), Some("fake-1"));
        assert_eq!(t.meta.user_turns, 2, "two real prompts; the environment context is meta");
        assert_eq!(t.meta.first_prompt.as_deref(), Some("say hi"));
        assert_eq!(t.meta.assistant_messages, 2);
        assert_eq!(t.meta.permission_mode.as_deref(), Some("plan"), "read-only sandbox");
        assert!(t.meta.context_tokens > 0);
        assert!(t.items.iter().any(|i| matches!(i, Item::TurnEnd { .. })));
        assert!(t.items.iter().any(|i| matches!(i, Item::User { meta: true, .. })));
    }

    #[test]
    fn converts_tool_calls_and_outputs() {
        let mut conv = Converter::default();
        let call = br#"{"timestamp":"2026-01-01T00:00:00Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"ls\",\"-la\"]}","call_id":"c1"}}"#;
        let output = br#"{"timestamp":"2026-01-01T00:00:01Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"a\nb"}}"#;
        let mut lines = conv.convert_line(call);
        lines.extend(conv.convert_line(output));
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&lines));
        assert_eq!(t.meta.tool_calls, 1);
        assert!(matches!(&t.items[0], Item::Tool { name, result: Some(r), .. } if name == "shell" && r.text == "a\nb"));
    }

    #[test]
    fn exec_commands() {
        let opts = ExecOpts { cwd: "C:/w".into(), prompt: "hi\nthere".into(), resume: None, session_id: None, model: Some("gpt-5".into()), mode: "auto".into(), effort: None };
        let c = exec_command(&opts);
        assert_eq!(c.args[..6], ["exec", "--json", "--skip-git-repo-check", "-C", "C:/w", "-c"]);
        assert!(c.args.contains(&"sandbox_mode=\"workspace-write\"".to_string()));
        assert!(c.args.contains(&"model=\"gpt-5\"".to_string()));
        assert_eq!(c.args.last().map(String::as_str), Some("-"));
        assert_eq!(c.stdin.as_deref(), Some("hi\nthere"));
        let r = exec_command(&ExecOpts { resume: Some("abc".into()), mode: "plan".into(), ..opts });
        assert_eq!(r.args[..4], ["exec", "resume", "--json", "abc"]);
        assert!(r.args.contains(&"sandbox_mode=\"read-only\"".to_string()));
    }

    #[test]
    fn exec_events() {
        assert_eq!(exec_event(&json!({"type":"thread.started","thread_id":"t1"})), ExecEvent::SessionId("t1".into()));
        assert_eq!(exec_event(&json!({"type":"turn.completed","usage":{}})), ExecEvent::TurnDone { error: None });
        assert_eq!(exec_event(&json!({"type":"item.started","item":{"type":"command_execution","command":"npm test"}})), ExecEvent::Tool("shell: npm test".into()));
        assert_eq!(exec_event(&json!({"type":"turn.failed","error":{"message":"boom"}})), ExecEvent::TurnDone { error: Some("boom".into()) });
    }
}
