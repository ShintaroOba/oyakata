//! Gemini CLI (Google). Sessions are `~/.gemini/tmp/<project>/chats/session-<time>-<id>.jsonl`:
//! a metadata line, then message records, interleaved with `{"$set": …}` / `{"$patch": …}` /
//! `{"$rewindTo": …}` edits. Because edits can rewrite earlier messages, the file is
//! re-parsed as a whole whenever it changes. `<project>` is a slug that `~/.gemini/projects.json`
//! maps back to the working directory. Turns run with `gemini -p … --output-format stream-json`.

use super::canon::{self, Usage};
use super::{str_of, AgentKind, Discovered, ExecCommand, ExecEvent, ExecOpts};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// `projects.json`: `{"projects": {"<normalized path>": "<slug>"}}` → slug → path.
fn slug_to_path(data_dir: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Ok(text) = fs::read_to_string(data_dir.join("projects.json")) else { return out };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return out };
    if let Some(map) = v.get("projects").and_then(Value::as_object) {
        for (path, slug) in map {
            if let Some(s) = slug.as_str() {
                out.insert(s.to_string(), path.clone());
            }
        }
    }
    out
}

pub fn discover(data_dir: &Path) -> Vec<Discovered> {
    let mut out = Vec::new();
    let slugs = slug_to_path(data_dir);
    let Ok(projects) = fs::read_dir(data_dir.join("tmp")) else { return out };
    for proj in projects.flatten() {
        let chats = proj.path().join("chats");
        let Ok(files) = fs::read_dir(&chats) else { continue };
        let slug = proj.file_name().to_string_lossy().into_owned();
        for f in files.flatten() {
            let p = f.path();
            if !p.is_file() {
                continue; // subagent transcripts live in subfolders
            }
            let Some(name) = p.file_name().and_then(|n| n.to_str()) else { continue };
            if !name.starts_with("session-") || !(name.ends_with(".jsonl") || name.ends_with(".json")) {
                continue;
            }
            let Some(first) = super::first_line(&p) else { continue };
            let Some(id) = str_of(&first, "sessionId") else { continue };
            out.push(Discovered {
                id: id.to_string(),
                agent: AgentKind::Gemini,
                path: p,
                project_dir: format!("gemini:{slug}"),
                cwd: slugs.get(&slug).cloned(),
                title: None,
                updated_ms: None,
            });
        }
    }
    out
}

/// Whole-file conversion (the format allows edits to earlier messages).
pub fn convert_all(bytes: &[u8]) -> Vec<Value> {
    let mut messages: Vec<Map<String, Value>> = Vec::new();
    let mut title: Option<String> = None;
    let mut version: Option<String> = None;
    let mut started: Option<String> = None;
    for raw in bytes.split(|&b| b == b'\n') {
        let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(raw) else { continue };
        if let Some(Value::Object(set)) = obj.get("$set") {
            if let Some(Value::Array(ms)) = set.get("messages") {
                messages.clear();
                messages.extend(ms.iter().filter_map(|m| m.as_object().cloned()));
            }
            if let Some(s) = set.get("summary").and_then(Value::as_str) {
                title = Some(s.to_string());
            }
            continue;
        }
        if let Some(Value::Object(patch)) = obj.get("$patch") {
            let mut updates: Vec<&Map<String, Value>> = Vec::new();
            if patch.contains_key("id") {
                updates.push(patch);
            }
            for u in patch.get("updates").and_then(Value::as_array).into_iter().flatten() {
                if let Some(o) = u.as_object() {
                    updates.push(o);
                }
            }
            for u in updates {
                let Some(id) = u.get("id").and_then(Value::as_str) else { continue };
                if let Some(m) = messages.iter_mut().find(|m| m.get("id").and_then(Value::as_str) == Some(id)) {
                    for (k, v) in u {
                        if k != "id" {
                            m.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            if let Some(ids) = patch.get("removeIds").and_then(Value::as_array) {
                let gone: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
                messages.retain(|m| !m.get("id").and_then(Value::as_str).map(|id| gone.contains(&id)).unwrap_or(false));
            }
            continue;
        }
        if let Some(id) = obj.get("$rewindTo").and_then(Value::as_str) {
            if let Some(pos) = messages.iter().position(|m| m.get("id").and_then(Value::as_str) == Some(id)) {
                messages.truncate(pos + 1);
            }
            continue;
        }
        if obj.contains_key("sessionId") && !obj.contains_key("type") {
            started = obj.get("startTime").and_then(Value::as_str).map(str::to_string);
            version = obj.get("version").and_then(Value::as_str).map(str::to_string);
            if let Some(s) = obj.get("summary").and_then(Value::as_str) {
                title = Some(s.to_string());
            }
            continue;
        }
        if obj.contains_key("type") {
            messages.push(obj);
        }
    }
    let mut out = vec![canon::session(started.as_deref(), None, version.as_deref())];
    if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
        out.push(canon::title(&t));
    }
    for m in &messages {
        convert_message(m, &mut out);
    }
    out
}

fn parts_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.clone()),
                Value::Object(_) => str_of(p, "text").map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A tool result is `Part[]` with `functionResponse.response.output` (or a plain text part).
fn result_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            let mut out = Vec::new();
            for p in parts {
                if let Some(resp) = p.pointer("/functionResponse/response") {
                    let text = str_of(resp, "output")
                        .or_else(|| str_of(resp, "error"))
                        .or_else(|| str_of(resp, "content"))
                        .map(str::to_string)
                        .unwrap_or_else(|| resp.to_string());
                    out.push(text);
                } else if let Some(t) = str_of(p, "text") {
                    out.push(t.to_string());
                }
            }
            out.join("\n")
        }
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

fn convert_message(m: &Map<String, Value>, out: &mut Vec<Value>) {
    let v = Value::Object(m.clone());
    let ts = str_of(&v, "timestamp");
    match str_of(&v, "type").unwrap_or("") {
        "user" => {
            let text = parts_text(v.get("content"));
            if text.trim().is_empty() {
                return;
            }
            let meta = canon::looks_injected(&text);
            out.push(canon::user(ts, &text, None, meta));
        }
        "gemini" => {
            let model = str_of(&v, "model");
            let mut blocks = Vec::new();
            for th in v.get("thoughts").and_then(Value::as_array).into_iter().flatten() {
                let subject = str_of(th, "subject").unwrap_or("");
                let desc = str_of(th, "description").unwrap_or("");
                let t = if subject.is_empty() { desc.to_string() } else if desc.is_empty() { subject.to_string() } else { format!("{subject}\n{desc}") };
                if !t.trim().is_empty() {
                    blocks.push(canon::thinking(&t));
                }
            }
            let calls: Vec<&Value> = v.get("toolCalls").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
            for c in &calls {
                let id = str_of(c, "id").unwrap_or("");
                let name = str_of(c, "name").unwrap_or("tool");
                blocks.push(canon::tool_use(id, name, c.get("args").cloned().unwrap_or(Value::Null)));
            }
            let text = parts_text(v.get("content"));
            if !text.trim().is_empty() {
                blocks.push(canon::text(&text));
            }
            let usage = v.get("tokens").and_then(|t| {
                let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
                let cached = n("cached");
                (n("total") > 0 || n("input") > 0).then(|| Usage { input: n("input").saturating_sub(cached), cache_read: cached, cache_write: 0, output: n("output") + n("thoughts") })
            });
            let stop = if calls.is_empty() { "end_turn" } else { "tool_use" };
            if !blocks.is_empty() || usage.is_some() {
                out.push(canon::assistant(ts, model, blocks, usage, Some(stop)));
            }
            let mut results = Vec::new();
            for c in &calls {
                let status = str_of(c, "status").unwrap_or("");
                let has_result = c.get("result").map(|r| !r.is_null()).unwrap_or(false);
                if !has_result && !matches!(status, "error" | "cancelled") {
                    continue;
                }
                let text = str_of(c, "resultDisplay").map(str::to_string).filter(|s| !s.is_empty()).unwrap_or_else(|| result_text(c.get("result")));
                results.push(canon::tool_result(str_of(c, "id").unwrap_or(""), &text, matches!(status, "error" | "cancelled")));
            }
            if !results.is_empty() {
                out.push(canon::tool_results(ts, results));
            }
        }
        _ => {}
    }
}

/// `gemini --output-format stream-json [-r <id> | --session-id <id>] …` with the prompt on
/// stdin: a non-TTY stdin puts Gemini CLI in headless mode and is read as the prompt.
pub fn exec_command(opts: &ExecOpts) -> ExecCommand {
    let mut args: Vec<String> = vec!["--output-format".into(), "stream-json".into(), "--skip-trust".into()];
    if let Some(id) = &opts.resume {
        args.push("-r".into());
        args.push(id.clone());
    } else if let Some(id) = &opts.session_id {
        args.push("--session-id".into());
        args.push(id.clone());
    }
    args.extend(super::permission_args(AgentKind::Gemini, &opts.mode));
    if let Some(m) = &opts.model {
        args.push("-m".into());
        args.push(m.clone());
    }
    ExecCommand { args, stdin: Some(opts.prompt.trim_end().to_string()) }
}

pub fn exec_event(v: &Value) -> ExecEvent {
    match str_of(v, "type").unwrap_or("") {
        "init" => str_of(v, "session_id").map(|s| ExecEvent::SessionId(s.to_string())).unwrap_or(ExecEvent::Ignore),
        "message" => match str_of(v, "role") {
            Some("assistant") if v.get("delta").and_then(Value::as_bool) != Some(true) => ExecEvent::Message(str_of(v, "content").unwrap_or("").to_string()),
            Some("user") => ExecEvent::TurnStarted,
            _ => ExecEvent::Ignore,
        },
        "tool_use" => {
            let name = str_of(v, "tool_name").unwrap_or("tool");
            let p = v.get("parameters").cloned().unwrap_or(Value::Null);
            ExecEvent::Tool(format!("{name}: {}", crate::transcript::summarize_tool(name, &p)))
        }
        "result" => ExecEvent::TurnDone {
            error: (str_of(v, "status") == Some("error")).then(|| v.pointer("/error/message").and_then(Value::as_str).unwrap_or("error").to_string()),
        },
        "error" if str_of(v, "severity") == Some("error") => ExecEvent::Error(str_of(v, "message").unwrap_or("error").to_string()),
        _ => ExecEvent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Item, Transcript};
    use serde_json::json;

    fn fixture() -> Vec<u8> {
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gemini/session-2026-10-08T01-22-9ccfe758.jsonl")).unwrap()
    }

    #[test]
    fn converts_a_real_chat_file() {
        let lines = convert_all(&fixture());
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&lines));
        assert_eq!(t.meta.user_turns, 2, "the <session_context> prompt is meta");
        assert_eq!(t.meta.first_prompt.as_deref(), Some("say hi"));
        assert_eq!(t.meta.model.as_deref(), Some("fake-1"));
        assert_eq!(t.meta.assistant_messages, 2);
        assert_eq!(t.meta.context_tokens, 21);
        assert!(t.meta.started_at.as_deref().unwrap().starts_with("2026-10-08"));
    }

    #[test]
    fn applies_patches_and_tool_calls() {
        let file = [
            json!({"sessionId":"s","projectHash":"h","startTime":"2026-01-01T00:00:00Z","lastUpdated":"2026-01-01T00:00:00Z","kind":"main"}),
            json!({"id":"u1","timestamp":"2026-01-01T00:00:01Z","type":"user","content":[{"text":"list files"}]}),
            json!({"id":"g1","timestamp":"2026-01-01T00:00:02Z","type":"gemini","content":"","toolCalls":[{"id":"c1","name":"run_shell_command","args":{"command":"ls"},"status":"executing","timestamp":"2026-01-01T00:00:02Z"}],"model":"gemini-3"}),
            json!({"$patch":{"updates":[{"id":"g1","content":"Here are the files.","toolCalls":[{"id":"c1","name":"run_shell_command","args":{"command":"ls"},"status":"success","result":[{"functionResponse":{"name":"run_shell_command","response":{"output":"a.txt\nb.txt"}}}],"timestamp":"2026-01-01T00:00:02Z"}]}]}}),
            json!({"$set":{"summary":"Listing files"}}),
        ];
        let text = file.iter().map(|v| v.to_string()).collect::<Vec<_>>().join("\n");
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&convert_all(text.as_bytes())));
        assert_eq!(t.meta.title().as_deref(), Some("Listing files"));
        assert_eq!(t.meta.tool_calls, 1);
        assert!(t.items.iter().any(|i| matches!(i, Item::Tool { name, result: Some(r), .. } if name == "run_shell_command" && r.text == "a.txt\nb.txt")));
        assert!(t.items.iter().any(|i| matches!(i, Item::Text { md, .. } if md == "Here are the files.")));
    }

    #[test]
    fn discovers_through_the_project_registry() {
        let tmp = std::env::temp_dir().join(format!("oyk-gemini-{}", std::process::id()));
        let chats = tmp.join("tmp/work/chats");
        fs::create_dir_all(&chats).unwrap();
        fs::write(chats.join("session-2026-10-08T01-22-9ccfe758.jsonl"), fixture()).unwrap();
        fs::write(tmp.join("projects.json"), r#"{"projects":{"c:\\temp\\agents\\work":"work"}}"#).unwrap();
        let found = discover(&tmp);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "9ccfe758-c773-4876-b45b-ada4c7bde42f");
        assert_eq!(found[0].cwd.as_deref(), Some("c:\\temp\\agents\\work"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn exec_commands_and_events() {
        let opts = ExecOpts { cwd: "C:/w".into(), prompt: "hi".into(), resume: None, session_id: Some("u1".into()), model: None, mode: "acceptEdits".into(), effort: None };
        let c = exec_command(&opts);
        assert_eq!(c.args, ["--output-format", "stream-json", "--skip-trust", "--session-id", "u1", "--approval-mode", "auto_edit"]);
        assert_eq!(c.stdin.as_deref(), Some("hi"));
        let r = exec_command(&ExecOpts { resume: Some("u1".into()), session_id: None, ..opts });
        assert!(r.args.windows(2).any(|w| w == ["-r", "u1"]));
        assert_eq!(exec_event(&json!({"type":"init","session_id":"u1","model":"m"})), ExecEvent::SessionId("u1".into()));
        assert_eq!(exec_event(&json!({"type":"result","status":"error","error":{"message":"x"}})), ExecEvent::TurnDone { error: Some("x".into()) });
    }
}
