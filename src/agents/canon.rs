//! Builders for the canonical transcript lines (Claude Code's JSONL shape) that every agent
//! adapter emits. `transcript::Transcript::feed` consumes the bytes `to_jsonl` produces.

use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub output: u64,
}

impl Usage {
    pub fn to_value(&self) -> Value {
        json!({
            "input_tokens": self.input,
            "cache_creation_input_tokens": self.cache_write,
            "cache_read_input_tokens": self.cache_read,
            "output_tokens": self.output,
        })
    }
}

fn opt(v: Option<&str>) -> Value {
    v.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null)
}

/// A prompt typed by the user. `meta` marks harness-injected context the UI greys out.
pub fn user(ts: Option<&str>, text: &str, cwd: Option<&str>, meta: bool) -> Value {
    let mut v = json!({ "type": "user", "timestamp": opt(ts), "message": { "role": "user", "content": text } });
    if let Some(c) = cwd {
        v["cwd"] = Value::String(c.to_string());
    }
    if meta {
        v["isMeta"] = Value::Bool(true);
    }
    v
}

/// A `user` line carrying tool results (the shape Claude Code uses for them).
pub fn tool_results(ts: Option<&str>, blocks: Vec<Value>) -> Value {
    json!({ "type": "user", "timestamp": opt(ts), "message": { "role": "user", "content": blocks } })
}

pub fn tool_result(id: &str, text: &str, is_error: bool) -> Value {
    json!({ "type": "tool_result", "tool_use_id": id, "content": text, "is_error": is_error })
}

pub fn text(t: &str) -> Value {
    json!({ "type": "text", "text": t })
}

pub fn thinking(t: &str) -> Value {
    json!({ "type": "thinking", "thinking": t })
}

pub fn tool_use(id: &str, name: &str, input: Value) -> Value {
    json!({ "type": "tool_use", "id": id, "name": name, "input": input })
}

/// An assistant line. `stop` is `end_turn` for a final answer and `tool_use` while tools run.
pub fn assistant(ts: Option<&str>, model: Option<&str>, blocks: Vec<Value>, usage: Option<Usage>, stop: Option<&str>) -> Value {
    let mut msg = json!({ "role": "assistant", "content": blocks });
    if let Some(m) = model {
        msg["model"] = Value::String(m.to_string());
    }
    if let Some(u) = usage {
        msg["usage"] = u.to_value();
    }
    if let Some(s) = stop {
        msg["stop_reason"] = Value::String(s.to_string());
    }
    json!({ "type": "assistant", "timestamp": opt(ts), "message": msg })
}

/// Token usage that arrives separately from the message it belongs to.
pub fn usage_only(ts: Option<&str>, model: Option<&str>, usage: Usage) -> Value {
    assistant(ts, model, Vec::new(), Some(usage), None)
}

pub fn title(t: &str) -> Value {
    json!({ "type": "ai-title", "aiTitle": t })
}

/// Session-level facts (cwd, agent version) that the parser picks up from any
/// user/assistant/system line.
pub fn session(ts: Option<&str>, cwd: Option<&str>, version: Option<&str>) -> Value {
    let mut v = json!({ "type": "system", "subtype": "session_meta", "timestamp": opt(ts) });
    if let Some(c) = cwd {
        v["cwd"] = Value::String(c.to_string());
    }
    if let Some(ver) = version {
        v["version"] = Value::String(ver.to_string());
    }
    v
}

pub fn turn_end(ts: Option<&str>, duration_ms: u64) -> Value {
    json!({ "type": "system", "subtype": "turn_duration", "durationMs": duration_ms, "timestamp": opt(ts) })
}

pub fn permission_mode(mode: &str) -> Value {
    json!({ "type": "permission-mode", "permissionMode": mode })
}

pub fn to_jsonl(lines: &[Value]) -> Vec<u8> {
    let mut out = Vec::new();
    for l in lines {
        if let Ok(mut b) = serde_json::to_vec(l) {
            out.append(&mut b);
            out.push(b'\n');
        }
    }
    out
}

/// Epoch milliseconds → `YYYY-MM-DDTHH:MM:SS.mmmZ`, the timestamp form Claude Code writes.
pub fn ms_to_iso(ms: u64) -> String {
    let secs = ms / 1000;
    let millis = ms % 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    format!("{year:04}-{month:02}-{d:02}T{h:02}:{m:02}:{s:02}.{millis:03}Z")
}

/// Harness-injected prompts (`<environment_context>…`, `<session_context>…`) that are not
/// something the person typed.
pub fn looks_injected(text: &str) -> bool {
    let t = text.trim_start();
    if !t.starts_with('<') {
        return false;
    }
    let tag: String = t[1..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    matches!(
        tag.as_str(),
        "environment_context" | "user_instructions" | "skills_instructions" | "session_context" | "current_datetime" | "turn_aborted" | "system-reminder" | "AGENTS" | "agents_md" | "permissions_instructions" | "collaboration_mode_instructions"
    ) || tag.ends_with("_context") || tag.ends_with("_instructions")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_timestamps() {
        assert_eq!(ms_to_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(ms_to_iso(1_791_422_480_623), "2026-10-08T01:21:20.623Z");
        assert_eq!(ms_to_iso(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn injected_prompts_are_recognized() {
        assert!(looks_injected("<environment_context>\n<cwd>x</cwd>"));
        assert!(looks_injected("  <session_context>..."));
        assert!(!looks_injected("<div>hello</div>"));
        assert!(!looks_injected("say hi"));
    }

    #[test]
    fn lines_feed_the_transcript_parser() {
        let lines = vec![
            user(Some("2026-01-01T00:00:00Z"), "hello", Some("C:/w"), false),
            assistant(Some("2026-01-01T00:00:01Z"), Some("m"), vec![tool_use("c1", "shell", json!({"command": "ls"}))], None, Some("tool_use")),
            tool_results(Some("2026-01-01T00:00:02Z"), vec![tool_result("c1", "a\nb", false)]),
            assistant(Some("2026-01-01T00:00:03Z"), Some("m"), vec![text("done")], Some(Usage { input: 10, cache_read: 5, cache_write: 0, output: 2 }), Some("end_turn")),
        ];
        let mut t = crate::transcript::Transcript::new(true);
        t.feed(&to_jsonl(&lines));
        assert_eq!(t.items.len(), 3);
        assert_eq!(t.meta.user_turns, 1);
        assert_eq!(t.meta.tool_calls, 1);
        assert_eq!(t.meta.cwd.as_deref(), Some("C:/w"));
        assert_eq!(t.meta.context_tokens, 17);
        assert_eq!(t.meta.title().as_deref(), Some("hello"));
    }
}
