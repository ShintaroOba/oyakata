//! GitHub Copilot CLI. Sessions are `$COPILOT_HOME/session-state/<uuid>/events.jsonl`
//! (append-only typed events: `user.message`, `assistant.message`, `tool.execution_*`, …) with
//! a `workspace.yaml` beside them. Turns run with `copilot -p … --output-format json`.

use super::canon::{self, Usage};
use super::{str_of, AgentKind, Discovered, ExecCommand, ExecEvent, ExecOpts};
use serde_json::Value;
use std::fs;
use std::path::Path;

pub fn discover(data_dir: &Path) -> Vec<Discovered> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(data_dir.join("session-state")) else { return out };
    for e in rd.flatten() {
        let dir = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !dir.is_dir() {
            continue;
        }
        let events = dir.join("events.jsonl");
        if !events.is_file() {
            continue;
        }
        let ws = fs::read_to_string(dir.join("workspace.yaml")).unwrap_or_default();
        let field = |k: &str| ws.lines().find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(':')).map(|v| v.trim().trim_matches('"').trim_matches('\'').to_string())).filter(|v| !v.is_empty());
        out.push(Discovered {
            id: name,
            agent: AgentKind::Copilot,
            path: events,
            project_dir: "copilot".into(),
            cwd: field("cwd"),
            title: field("name"),
            updated_ms: None,
        });
    }
    out
}

/// Incremental converter over `events.jsonl` lines.
#[derive(Default)]
pub struct Converter {
    cwd: Option<String>,
    model: Option<String>,
}

impl Converter {
    pub fn convert_line(&mut self, raw: &[u8]) -> Vec<Value> {
        let Ok(ev) = serde_json::from_slice::<Value>(raw) else { return Vec::new() };
        let ts = str_of(&ev, "timestamp");
        let data = ev.get("data").cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();
        match str_of(&ev, "type").unwrap_or("") {
            "session.start" | "session.resume" => {
                if let Some(c) = data.pointer("/context/cwd").and_then(Value::as_str) {
                    self.cwd = Some(c.to_string());
                }
                out.push(canon::session(ts, self.cwd.as_deref(), str_of(&data, "copilotVersion")));
            }
            "session.model_change" => {
                if let Some(m) = str_of(&data, "newModel") {
                    self.model = Some(m.to_string());
                }
            }
            "user.message" => {
                let text = str_of(&data, "content").unwrap_or("");
                if !text.trim().is_empty() {
                    out.push(canon::user(ts, text, self.cwd.as_deref(), canon::looks_injected(text)));
                }
            }
            "assistant.message" => {
                if let Some(m) = str_of(&data, "model") {
                    self.model = Some(m.to_string());
                }
                let mut blocks = Vec::new();
                if let Some(r) = str_of(&data, "reasoning").filter(|r| !r.trim().is_empty()) {
                    blocks.push(canon::thinking(r));
                }
                let text = str_of(&data, "content").unwrap_or("");
                if !text.trim().is_empty() {
                    blocks.push(canon::text(text));
                }
                let requests: Vec<&Value> = data.get("toolRequests").and_then(Value::as_array).map(|a| a.iter().collect()).unwrap_or_default();
                for r in &requests {
                    let id = str_of(r, "toolCallId").unwrap_or("");
                    let name = str_of(r, "name").unwrap_or("tool");
                    blocks.push(canon::tool_use(id, name, r.get("arguments").cloned().unwrap_or(Value::Null)));
                }
                if !blocks.is_empty() {
                    let stop = if requests.is_empty() { "end_turn" } else { "tool_use" };
                    out.push(canon::assistant(ts, self.model.as_deref(), blocks, None, Some(stop)));
                }
            }
            "assistant.reasoning" => {
                if let Some(r) = str_of(&data, "content").filter(|r| !r.trim().is_empty()) {
                    out.push(canon::assistant(ts, self.model.as_deref(), vec![canon::thinking(r)], None, None));
                }
            }
            "tool.execution_complete" => {
                let id = str_of(&data, "toolCallId").unwrap_or("");
                let ok = data.get("success").and_then(Value::as_bool).unwrap_or(true);
                let text = data
                    .pointer("/result/content")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or_else(|| data.pointer("/error/message").and_then(Value::as_str).map(str::to_string))
                    .or_else(|| str_of(&data, "error").map(str::to_string))
                    .unwrap_or_default();
                out.push(canon::tool_results(ts, vec![canon::tool_result(id, &text, !ok)]));
            }
            "session.usage_checkpoint" => out.push(canon::turn_end(ts, 0)),
            "session.shutdown" => {
                if let Some(td) = data.get("tokenDetails") {
                    let n = |k: &str| td.pointer(&format!("/{k}/tokenCount")).and_then(Value::as_u64).unwrap_or(0);
                    let usage = Usage { input: n("input"), cache_read: n("cache_read"), cache_write: n("cache_write"), output: n("output") };
                    if usage != Usage::default() {
                        out.push(canon::usage_only(ts, self.model.as_deref(), usage));
                    }
                }
            }
            _ => {}
        }
        out
    }
}

/// `copilot --output-format json [--resume <id> | --session-id <id>] -C <cwd> …` with the
/// prompt piped on stdin, which Copilot CLI treats as the non-interactive prompt.
pub fn exec_command(opts: &ExecOpts) -> ExecCommand {
    let mut args: Vec<String> = vec!["--output-format".into(), "json".into(), "-C".into(), opts.cwd.display().to_string()];
    if let Some(id) = &opts.resume {
        args.push("--resume".into());
        args.push(id.clone());
    } else if let Some(id) = &opts.session_id {
        args.push("--session-id".into());
        args.push(id.clone());
    }
    args.extend(super::permission_args(AgentKind::Copilot, &opts.mode));
    if let Some(m) = &opts.model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(e) = &opts.effort {
        args.push("--reasoning-effort".into());
        args.push(e.clone());
    }
    ExecCommand { args, stdin: Some(opts.prompt.trim_end().to_string()) }
}

pub fn exec_event(v: &Value) -> ExecEvent {
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    match str_of(v, "type").unwrap_or("") {
        "session.start" | "session.resume" => str_of(&data, "sessionId").map(|s| ExecEvent::SessionId(s.to_string())).unwrap_or(ExecEvent::Ignore),
        "user.message" => ExecEvent::TurnStarted,
        "assistant.message" => {
            let text = str_of(&data, "content").unwrap_or("");
            if text.trim().is_empty() {
                ExecEvent::Ignore
            } else {
                ExecEvent::Message(text.to_string())
            }
        }
        "tool.execution_start" => {
            let name = str_of(&data, "toolName").unwrap_or("tool");
            let args = data.get("arguments").cloned().unwrap_or(Value::Null);
            ExecEvent::Tool(format!("{name}: {}", crate::transcript::summarize_tool(name, &args)))
        }
        "result" => ExecEvent::TurnDone { error: None },
        "error" => ExecEvent::Error(str_of(&data, "message").or_else(|| str_of(v, "message")).unwrap_or("error").to_string()),
        _ => ExecEvent::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Item, Transcript};
    use serde_json::json;

    fn fixture_dir() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/copilot")
    }

    #[test]
    fn discovers_session_state_dirs() {
        let found = discover(&{
            let tmp = std::env::temp_dir().join(format!("oyk-copilot-{}", std::process::id()));
            let dst = tmp.join("session-state/1fc398c2-e820-4e06-9dfd-8a9205f95437");
            fs::create_dir_all(&dst).unwrap();
            for f in ["events.jsonl", "workspace.yaml"] {
                fs::copy(fixture_dir().join("1fc398c2-e820-4e06-9dfd-8a9205f95437").join(f), dst.join(f)).unwrap();
            }
            fs::create_dir_all(tmp.join("session-state/.session-operation-locks")).unwrap();
            tmp
        });
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "1fc398c2-e820-4e06-9dfd-8a9205f95437");
        assert_eq!(found[0].cwd.as_deref(), Some("C:\\Temp\\agents"));
        assert_eq!(found[0].title.as_deref(), Some("say hi"));
    }

    #[test]
    fn converts_real_events_with_a_tool_call() {
        let bytes = fs::read(fixture_dir().join("1fc398c2-e820-4e06-9dfd-8a9205f95437/events.jsonl")).unwrap();
        let mut conv = Converter::default();
        let mut lines = Vec::new();
        for raw in bytes.split(|&b| b == b'\n') {
            lines.extend(conv.convert_line(raw));
        }
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&lines));
        assert_eq!(t.meta.cwd.as_deref(), Some("C:\\Temp\\agents"));
        assert_eq!(t.meta.version.as_deref(), Some("1.0.93"));
        assert_eq!(t.meta.user_turns, 2);
        assert_eq!(t.meta.model.as_deref(), Some("claude-sonnet-5.5"));
        assert_eq!(t.meta.tool_calls, 1);
        let tool = t.items.iter().find_map(|i| match i {
            Item::Tool { name, result, summary, .. } => Some((name.clone(), result.clone(), summary.clone())),
            _ => None,
        });
        let (name, result, summary) = tool.expect("a tool item");
        assert_eq!(name, "powershell");
        assert_eq!(summary, "Echo a message");
        assert!(result.unwrap().text.starts_with("hello-from-copilot"));
        assert!(t.meta.context_tokens > 0, "usage from session.shutdown");
        assert_eq!(t.meta.last_text_snippet.as_deref(), Some("The command printed `hello-from-copilot`."));
    }

    #[test]
    fn exec_commands_and_events() {
        let opts = ExecOpts { cwd: "C:/w".into(), prompt: "hi".into(), resume: Some("s1".into()), session_id: None, model: Some("gpt-5".into()), mode: "auto".into(), effort: Some("high".into()) };
        let c = exec_command(&opts);
        assert_eq!(c.args, ["--output-format", "json", "-C", "C:/w", "--resume", "s1", "--allow-all-tools", "--model", "gpt-5", "--reasoning-effort", "high"]);
        assert_eq!(c.stdin.as_deref(), Some("hi"));
        assert_eq!(exec_event(&json!({"type":"tool.execution_start","data":{"toolName":"powershell","arguments":{"command":"ls","description":"List"}}})), ExecEvent::Tool("powershell: List".into()));
        assert_eq!(exec_event(&json!({"type":"result","data":{}})), ExecEvent::TurnDone { error: None });
    }
}
