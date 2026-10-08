//! OpenCode. Sessions live in a SQLite database (`~/.local/share/opencode/opencode.db`) that
//! OYAKATA reads through `opencode db "<sql>" --format json`, so no SQLite driver is linked.
//! Messages and their parts are JSON blobs in the `message` / `part` tables (the same shapes
//! `opencode export` prints). Turns run with `opencode run --format json [-s <id>]`.

use super::canon::{self, Usage};
use super::{str_of, AgentKind, Discovered, Exe, ExecCommand, ExecEvent, ExecOpts};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Listing {
    pub id: String,
    pub directory: String,
    pub title: String,
    pub parent_id: Option<String>,
    pub updated: u64,
}

fn db_query(exe: &Exe, sql: &str) -> Result<Value> {
    let mut cmd = exe.std_command();
    cmd.args(["db", sql, "--format", "json", "--pure"]);
    cmd.stdin(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let out = cmd.output().context("run opencode db")?;
    if !out.status.success() {
        return Err(anyhow!("opencode db failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    // The CLI may print a banner line before the JSON.
    let start = text.find('[').unwrap_or(0);
    serde_json::from_str(&text[start..]).context("parse opencode db output")
}

pub fn list(exe: &Exe) -> Result<Vec<Listing>> {
    let rows = db_query(exe, "select id, directory, title, parent_id, time_created, time_updated from session order by time_updated desc")?;
    Ok(rows
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            Some(Listing {
                id: str_of(r, "id")?.to_string(),
                directory: str_of(r, "directory").unwrap_or("").to_string(),
                title: str_of(r, "title").unwrap_or("").to_string(),
                parent_id: str_of(r, "parent_id").map(str::to_string),
                updated: r.get("time_updated").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect())
}

pub fn discover(exe: &Exe) -> Vec<Discovered> {
    list(exe)
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.parent_id.is_none())
        .map(|l| Discovered {
            path: PathBuf::from(format!("opencode:{}", l.id)),
            id: l.id,
            agent: AgentKind::Opencode,
            project_dir: "opencode".into(),
            cwd: Some(l.directory).filter(|d| !d.is_empty()),
            title: Some(l.title).filter(|t| !t.is_empty()),
            updated_ms: Some(l.updated),
        })
        .collect()
}

fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The whole transcript of one session, converted. `title` / `directory` come from the
/// listing so a second query is not needed.
pub fn fetch(exe: &Exe, id: &str, title: Option<&str>, directory: Option<&str>) -> Result<Vec<Value>> {
    if !safe_id(id) {
        return Err(anyhow!("bad session id"));
    }
    let sql = format!(
        "select m.id as mid, m.data as message, m.time_created as mtime, p.id as pid, p.data as part, p.time_created as ptime from message m left join part p on p.message_id = m.id where m.session_id = '{id}' order by m.time_created, p.time_created"
    );
    let rows = db_query(exe, &sql)?;
    let mut messages: Vec<(Value, Vec<Value>)> = Vec::new();
    let mut last_mid: Option<String> = None;
    for r in rows.as_array().into_iter().flatten() {
        let mid = str_of(r, "mid").unwrap_or("").to_string();
        if last_mid.as_deref() != Some(mid.as_str()) {
            let mut info: Value = str_of(r, "message").and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
            if info.get("id").is_none() {
                info["id"] = Value::String(mid.clone());
            }
            messages.push((info, Vec::new()));
            last_mid = Some(mid);
        }
        if let Some(mut part) = str_of(r, "part").and_then(|s| serde_json::from_str::<Value>(s).ok()) {
            if part.get("time").is_none() {
                if let Some(t) = r.get("ptime").and_then(Value::as_u64) {
                    part["time"] = serde_json::json!({ "start": t });
                }
            }
            if let Some(last) = messages.last_mut() {
                last.1.push(part);
            }
        }
    }
    Ok(convert(title, directory, &messages))
}

/// `opencode export` output: `{info: Session, messages: [{info: Message, parts: Part[]}]}`.
#[cfg(test)]
pub fn convert_export(export: &Value) -> Vec<Value> {
    let info = export.get("info").cloned().unwrap_or(Value::Null);
    let messages: Vec<(Value, Vec<Value>)> = export
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|m| (m.get("info").cloned().unwrap_or(Value::Null), m.get("parts").and_then(Value::as_array).cloned().unwrap_or_default()))
        .collect();
    convert(str_of(&info, "title"), str_of(&info, "directory"), &messages)
}

fn iso(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_u64).map(canon::ms_to_iso)
}

fn convert(title: Option<&str>, directory: Option<&str>, messages: &[(Value, Vec<Value>)]) -> Vec<Value> {
    let mut out = vec![canon::session(None, directory, None)];
    if let Some(t) = title.filter(|t| !t.trim().is_empty()) {
        out.push(canon::title(t));
    }
    for (info, parts) in messages {
        let ts = iso(info.pointer("/time/created"));
        let role = str_of(info, "role").unwrap_or("");
        if role == "user" {
            let text: Vec<&str> = parts.iter().filter(|p| str_of(p, "type") == Some("text")).filter_map(|p| str_of(p, "text")).collect();
            let text = text.join("\n");
            if !text.trim().is_empty() {
                out.push(canon::user(ts.as_deref(), &text, directory, canon::looks_injected(&text)));
            }
            continue;
        }
        if role != "assistant" {
            continue;
        }
        let model = str_of(info, "modelID");
        let mut blocks: Vec<Value> = Vec::new();
        let flush = |blocks: &mut Vec<Value>, out: &mut Vec<Value>, ts: Option<&str>, stop: Option<&str>| {
            if !blocks.is_empty() {
                out.push(canon::assistant(ts, model, std::mem::take(blocks), None, stop));
            }
        };
        for p in parts {
            let pts = iso(p.pointer("/time/start")).or_else(|| ts.clone());
            match str_of(p, "type").unwrap_or("") {
                "text" => {
                    if let Some(t) = str_of(p, "text").filter(|t| !t.trim().is_empty()) {
                        blocks.push(canon::text(t));
                    }
                }
                "reasoning" => {
                    if let Some(t) = str_of(p, "text").filter(|t| !t.trim().is_empty()) {
                        blocks.push(canon::thinking(t));
                    }
                }
                "tool" => {
                    flush(&mut blocks, &mut out, pts.as_deref(), None);
                    let id = str_of(p, "callID").unwrap_or("");
                    let name = str_of(p, "tool").unwrap_or("tool");
                    let state = p.get("state").cloned().unwrap_or(Value::Null);
                    let input = state.get("input").cloned().unwrap_or(Value::Null);
                    out.push(canon::assistant(pts.as_deref(), model, vec![canon::tool_use(id, name, input)], None, Some("tool_use")));
                    match str_of(&state, "status") {
                        Some("completed") => {
                            let text = str_of(&state, "output").unwrap_or("");
                            out.push(canon::tool_results(iso(state.pointer("/time/end")).as_deref().or(pts.as_deref()), vec![canon::tool_result(id, text, false)]));
                        }
                        Some("error") => {
                            let text = str_of(&state, "error").unwrap_or("error");
                            out.push(canon::tool_results(pts.as_deref(), vec![canon::tool_result(id, text, true)]));
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        flush(&mut blocks, &mut out, ts.as_deref(), Some(if str_of(info, "finish").is_some() { "end_turn" } else { "tool_use" }));
        if let Some(tk) = info.get("tokens") {
            let n = |k: &str| tk.get(k).and_then(Value::as_u64).unwrap_or(0);
            let usage = Usage {
                input: n("input"),
                cache_read: tk.pointer("/cache/read").and_then(Value::as_u64).unwrap_or(0),
                cache_write: tk.pointer("/cache/write").and_then(Value::as_u64).unwrap_or(0),
                output: n("output") + n("reasoning"),
            };
            if usage != Usage::default() {
                out.push(canon::usage_only(iso(info.pointer("/time/completed")).as_deref().or(ts.as_deref()), model, usage));
            }
        }
        if let Some(done) = iso(info.pointer("/time/completed")) {
            let started = info.pointer("/time/created").and_then(Value::as_u64).unwrap_or(0);
            let finished = info.pointer("/time/completed").and_then(Value::as_u64).unwrap_or(started);
            out.push(canon::turn_end(Some(&done), finished.saturating_sub(started)));
        }
    }
    out
}

/// `opencode run --format json --dir <cwd> [-s <id>] … <prompt>`.
pub fn exec_command(opts: &ExecOpts) -> ExecCommand {
    let mut args: Vec<String> = vec!["run".into(), "--format".into(), "json".into(), "--dir".into(), opts.cwd.display().to_string()];
    if let Some(id) = &opts.resume {
        args.push("-s".into());
        args.push(id.clone());
    }
    args.extend(super::permission_args(AgentKind::Opencode, &opts.mode));
    if let Some(m) = &opts.model {
        args.push("-m".into());
        args.push(m.clone());
    }
    if let Some(e) = &opts.effort {
        args.push("--variant".into());
        args.push(e.clone());
    }
    // With no message argument `opencode run` reads the prompt from stdin.
    ExecCommand { args, stdin: Some(opts.prompt.trim_end().to_string()) }
}

pub fn exec_event(v: &Value) -> ExecEvent {
    let part = v.get("part").cloned().unwrap_or(Value::Null);
    match str_of(v, "type").unwrap_or("") {
        "step_start" => ExecEvent::TurnStarted,
        "tool_use" => {
            let name = str_of(&part, "tool").unwrap_or("tool");
            let title = part.pointer("/state/title").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| crate::transcript::summarize_tool(name, part.pointer("/state/input").unwrap_or(&Value::Null)));
            ExecEvent::Tool(format!("{name}: {title}"))
        }
        "text" => ExecEvent::Message(str_of(&part, "text").unwrap_or("").to_string()),
        "error" => {
            let e = v.get("error").cloned().unwrap_or(Value::Null);
            let msg = e.pointer("/data/message").and_then(Value::as_str).or_else(|| str_of(&e, "message")).or_else(|| str_of(&e, "name")).unwrap_or("error");
            ExecEvent::Error(msg.to_string())
        }
        _ => match str_of(v, "sessionID") {
            Some(id) => ExecEvent::SessionId(id.to_string()),
            None => ExecEvent::Ignore,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{Item, Transcript};
    use serde_json::json;

    #[test]
    fn converts_an_export() {
        let text = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/opencode/export.json")).unwrap();
        let export: Value = serde_json::from_str(&text).unwrap();
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&convert_export(&export)));
        assert_eq!(t.meta.title().as_deref(), Some("Hello from the fake model. 1 + 1 = 2."));
        assert_eq!(t.meta.cwd.as_deref(), Some("C:\\Temp\\agents\\work"));
        assert_eq!(t.meta.user_turns, 1);
        assert_eq!(t.meta.first_prompt.as_deref(), Some("\"say hi\""));
        assert_eq!(t.meta.model.as_deref(), Some("fake-1"));
        assert_eq!(t.meta.context_tokens, 21);
        assert!(t.items.iter().any(|i| matches!(i, Item::TurnEnd { .. })));
    }

    #[test]
    fn converts_tool_parts_in_order() {
        let export = json!({
            "info": {"id": "ses_1", "title": "t", "directory": "/w"},
            "messages": [
                {"info": {"id": "m1", "role": "user", "time": {"created": 1000}}, "parts": [{"type": "text", "text": "list"}]},
                {"info": {"id": "m2", "role": "assistant", "modelID": "m", "finish": "stop", "time": {"created": 2000, "completed": 5000}, "tokens": {"input": 5, "output": 2, "reasoning": 0, "cache": {"read": 0, "write": 0}}},
                 "parts": [
                    {"type": "step-start"},
                    {"type": "tool", "callID": "c1", "tool": "bash", "state": {"status": "completed", "input": {"command": "ls"}, "output": "a\nb", "title": "ls", "time": {"start": 2100, "end": 2200}}},
                    {"type": "text", "text": "Two files.", "time": {"start": 2300}},
                    {"type": "step-finish", "reason": "stop"}
                 ]}
            ]
        });
        let mut t = Transcript::new(true);
        t.feed(&canon::to_jsonl(&convert_export(&export)));
        let kinds: Vec<&str> = t.items.iter().map(|i| match i { Item::User { .. } => "user", Item::Tool { .. } => "tool", Item::Text { .. } => "text", Item::TurnEnd { .. } => "end", _ => "other" }).collect();
        assert_eq!(kinds, ["user", "tool", "text", "end"]);
        assert!(matches!(&t.items[1], Item::Tool { name, result: Some(r), .. } if name == "bash" && r.text == "a\nb"));
        assert_eq!(t.meta.context_tokens, 7);
    }

    #[test]
    fn exec_commands_and_events() {
        let opts = ExecOpts { cwd: "/w".into(), prompt: "hi".into(), resume: Some("ses_1".into()), session_id: None, model: Some("fake/fake-1".into()), mode: "auto".into(), effort: None };
        let c = exec_command(&opts);
        assert_eq!(c.args, ["run", "--format", "json", "--dir", "/w", "-s", "ses_1", "--auto", "-m", "fake/fake-1"]);
        assert_eq!(c.stdin.as_deref(), Some("hi"));
        assert_eq!(exec_event(&json!({"type":"step_start","sessionID":"ses_1","part":{}})), ExecEvent::TurnStarted);
        assert_eq!(exec_event(&json!({"type":"text","sessionID":"ses_1","part":{"type":"text","text":"ok"}})), ExecEvent::Message("ok".into()));
        assert_eq!(exec_event(&json!({"type":"tool_use","sessionID":"ses_1","part":{"tool":"bash","state":{"title":"ls"}}})), ExecEvent::Tool("bash: ls".into()));
        assert!(!safe_id("x'; drop table session; --"));
    }
}
