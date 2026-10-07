//! Terminal front-end for sessions the OYAKATA daemon owns: a tiny HTTP/SSE client (no
//! HTTP crate) plus the `new`, `attach` and `sessions` commands. The daemon remains the
//! session's owner, so the browser and any number of terminals can take turns typing.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::Duration;

pub struct Client {
    pub bind: String,
    pub port: u16,
}

impl Client {
    fn connect(&self, timeout: Option<Duration>) -> Result<TcpStream> {
        let addr = format!("{}:{}", self.bind, self.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| anyhow!("unresolvable bind address"))?;
        let s = TcpStream::connect_timeout(&addr, Duration::from_millis(600))?;
        s.set_read_timeout(timeout)?;
        s.set_write_timeout(Some(Duration::from_secs(5)))?;
        Ok(s)
    }

    /// One HTTP/1.0 request; returns (status, body). Always sends the `X-Oyakata` header
    /// that the daemon requires on mutating calls.
    pub fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<(u16, String)> {
        let mut s = self.connect(Some(Duration::from_secs(200)))?;
        let body = body.unwrap_or("");
        write!(
            s,
            "{method} {path} HTTP/1.0\r\nHost: {}\r\nX-Oyakata: cli\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            self.bind,
            body.len()
        )?;
        let mut buf = String::new();
        s.read_to_string(&mut buf)?;
        let (head, body) = buf.split_once("\r\n\r\n").ok_or_else(|| anyhow!("malformed HTTP response"))?;
        let status: u16 = head
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow!("malformed HTTP status line"))?;
        Ok((status, body.to_string()))
    }

    pub fn get_json(&self, path: &str) -> Result<Value> {
        let (status, body) = self.request("GET", path, None)?;
        let v: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            bail!("{}", v.get("error").and_then(Value::as_str).unwrap_or(&format!("HTTP {status}")));
        }
        Ok(v)
    }

    pub fn post_json(&self, path: &str, body: &Value) -> Result<Value> {
        let (status, text) = self.request("POST", path, Some(&body.to_string()))?;
        let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !(200..300).contains(&status) {
            bail!("{}", v.get("error").and_then(Value::as_str).unwrap_or(&format!("HTTP {status}")));
        }
        Ok(v)
    }

    pub fn is_running(&self) -> bool {
        self.get_json("/api/health").map(|v| v.get("name").and_then(Value::as_str) == Some("oyakata")).unwrap_or(false)
    }

    /// Stream `/api/events`, sending `(event, data)` pairs until the connection closes.
    pub fn sse(&self, tx: mpsc::Sender<Msg>) -> Result<()> {
        let mut s = self.connect(None)?;
        write!(s, "GET /api/events HTTP/1.0\r\nHost: {}\r\nX-Oyakata: cli\r\nAccept: text/event-stream\r\n\r\n", self.bind)?;
        let mut reader = BufReader::new(s);
        // headers
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                bail!("event stream closed before headers");
            }
            if line == "\r\n" || line == "\n" {
                break;
            }
        }
        let mut event = String::new();
        let mut data = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line)? == 0 {
                break;
            }
            let l = line.trim_end_matches(['\r', '\n']);
            if l.is_empty() {
                if !data.is_empty() {
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        if tx.send(Msg::Event(event.clone(), v)).is_err() {
                            break;
                        }
                    }
                }
                event.clear();
                data.clear();
            } else if let Some(rest) = l.strip_prefix("event:") {
                event = rest.trim().to_string();
            } else if let Some(rest) = l.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest.trim_start());
            }
        }
        Ok(())
    }
}

pub enum Msg {
    Event(String, Value),
    Input(String),
    Eof,
    StreamClosed,
}

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const YEL: &str = "\x1b[33m";
const CYA: &str = "\x1b[36m";
const RED: &str = "\x1b[31m";
const MAG: &str = "\x1b[35m";
const RST: &str = "\x1b[0m";

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn print_item(it: &Value, echo_user: bool) {
    match s(it, "t") {
        "user" => {
            if it.get("meta").and_then(Value::as_bool).unwrap_or(false) || it.get("compact_summary").and_then(Value::as_bool).unwrap_or(false) {
                return;
            }
            if echo_user {
                println!("{CYA}あなた>{RST} {}", s(it, "text"));
            }
        }
        "text" => println!("{YEL}Claude>{RST}\n{}\n", s(it, "md").trim_end()),
        "tool" => println!("  {DIM}⚙ {}: {}{RST}", s(it, "name"), s(it, "summary")),
        "compact" => println!("  {DIM}(コンテキストを圧縮){RST}"),
        _ => {}
    }
}

fn print_pending(p: &Value) {
    let tool = s(p, "tool_name");
    let input = p.get("input").cloned().unwrap_or(Value::Null);
    println!();
    match tool {
        "AskUserQuestion" => {
            for (qi, q) in input.get("questions").and_then(Value::as_array).into_iter().flatten().enumerate() {
                println!("{MAG}? {}{RST}{}", s(q, "question"), if qi == 0 { "" } else { "" });
                for (i, o) in q.get("options").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    println!("  {}) {}{}", i + 1, s(o, "label"), if s(o, "description").is_empty() { String::new() } else { format!(" — {}", s(o, "description")) });
                }
            }
            println!("{BOLD}番号（複数は 1,3 のように）か自由記述 >{RST} ");
        }
        "ExitPlanMode" => {
            println!("{MAG}┌ 計画の承認{RST}");
            for l in s(&input, "plan").lines() {
                println!("{MAG}│{RST} {l}");
            }
            println!("{MAG}└{RST} {BOLD}[y] 承認して進める  [n] 修正を依頼（続けて理由を入力）>{RST} ");
        }
        _ => {
            println!("{MAG}┌ 許可の確認: {tool}{RST} {}", s(p, "description"));
            let detail = match tool {
                "Bash" | "PowerShell" => s(&input, "command").to_string(),
                "Read" | "Edit" | "Write" | "MultiEdit" => s(&input, "file_path").to_string(),
                "WebFetch" => s(&input, "url").to_string(),
                _ => serde_json::to_string(&input).unwrap_or_default(),
            };
            for l in detail.lines().take(20) {
                println!("{MAG}│{RST} {l}");
            }
            let suggest = p.get("suggestions").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(false);
            println!(
                "{MAG}└{RST} {BOLD}[y] 許可  {}[n] 拒否  （それ以外の入力は拒否理由として送る）>{RST} ",
                if suggest { "[a] 以後も許可  " } else { "" }
            );
        }
    }
}

/// Turn a typed line into a permission response body for the pending request.
fn answer_for(p: &Value, line: &str) -> Value {
    let tool = s(p, "tool_name");
    let id = s(p, "request_id");
    let line = line.trim();
    match tool {
        "AskUserQuestion" => {
            let mut answers = serde_json::Map::new();
            let questions = p.get("input").and_then(|i| i.get("questions")).and_then(Value::as_array).cloned().unwrap_or_default();
            for q in &questions {
                let options: Vec<&str> = q.get("options").and_then(Value::as_array).into_iter().flatten().map(|o| s(o, "label")).collect();
                let picked: Vec<String> = line
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map(|t| match t.parse::<usize>() {
                        Ok(n) if n >= 1 && n <= options.len() => options[n - 1].to_string(),
                        _ => t.to_string(),
                    })
                    .collect();
                answers.insert(s(q, "question").to_string(), Value::String(picked.join(", ")));
            }
            json!({ "request_id": id, "behavior": "allow", "answers": answers })
        }
        "ExitPlanMode" => match line {
            "y" | "Y" | "yes" => json!({ "request_id": id, "behavior": "allow" }),
            "n" | "N" | "no" => json!({ "request_id": id, "behavior": "deny", "message": "修正してください" }),
            other => json!({ "request_id": id, "behavior": "deny", "message": other }),
        },
        _ => match line {
            "y" | "Y" | "yes" => json!({ "request_id": id, "behavior": "allow" }),
            "a" | "A" => json!({ "request_id": id, "behavior": "allow", "apply_suggestions": true }),
            "n" | "N" | "no" => json!({ "request_id": id, "behavior": "deny" }),
            other => json!({ "request_id": id, "behavior": "deny", "message": other }),
        },
    }
}

fn find_run(client: &Client, id: &str) -> Option<Value> {
    client
        .get_json("/api/run")
        .ok()?
        .get("runs")?
        .as_array()?
        .iter()
        .find(|r| s(r, "session_id") == id)
        .cloned()
}

fn find_session(client: &Client, id: &str) -> Option<Value> {
    client
        .get_json("/api/sessions")
        .ok()?
        .get("sessions")?
        .as_array()?
        .iter()
        .find(|x| s(x, "id") == id)
        .cloned()
}

pub struct StartArgs {
    pub cwd: String,
    pub prompt: String,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub effort: Option<String>,
}

pub fn start(client: &Client, a: &StartArgs) -> Result<String> {
    let v = client.post_json(
        "/api/run/start",
        &json!({ "cwd": a.cwd, "prompt": a.prompt, "resume": a.resume, "model": a.model, "permission_mode": a.mode, "effort": a.effort }),
    )?;
    Ok(s(&v, "session_id").to_string())
}

pub fn list_sessions(client: &Client) -> Result<()> {
    let runs = client.get_json("/api/run")?;
    let sessions = client.get_json("/api/sessions")?;
    let titles: std::collections::HashMap<String, String> = sessions
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|x| (s(x, "id").to_string(), x.get("title").and_then(Value::as_str).unwrap_or(s(x, "first_prompt")).to_string()))
        .collect();
    let runs = runs.get("runs").and_then(Value::as_array).cloned().unwrap_or_default();
    if runs.is_empty() {
        println!("OYAKATA が持っているセッションはありません。`oyakata new \"指示\"` で始められます。");
        return Ok(());
    }
    println!("{:<38} {:<8} {:<6} {}", "session", "status", "turns", "title / cwd");
    for r in runs {
        let id = s(&r, "session_id");
        let title = titles.get(id).cloned().unwrap_or_default();
        println!(
            "{:<38} {:<8} {:<6} {}\n{:<38} {:<8} {:<6} {DIM}{}{RST}",
            id,
            s(&r, "status"),
            r.get("turns").and_then(Value::as_u64).unwrap_or(0),
            title.chars().take(60).collect::<String>(),
            "",
            "",
            "",
            s(&r, "cwd")
        );
    }
    Ok(())
}

/// Interactive loop: print what happens in the session, send typed lines as prompts, and
/// answer permission prompts and questions from the keyboard.
pub fn attach(client: &Client, id: &str, allow_resume: bool, defaults: &StartArgs) -> Result<()> {
    // Fetching the session also makes the daemon keep its items in memory, which is what
    // turns file growth into `append` events for us.
    let detail = client.get_json(&format!("/api/sessions/{id}")).ok();
    let mut loaded = detail.is_some();
    let mut seen: usize = 0;
    if let Some(d) = &detail {
        let items = d.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        let skip = items.len().saturating_sub(12);
        if skip > 0 {
            println!("{DIM}… {skip} 件省略（ブラウザでは全文が見られます）{RST}");
        }
        for it in &items[skip..] {
            print_item(it, true);
        }
        seen = items.len();
    }
    let mut run = find_run(client, id);
    let running = run.as_ref().map(|r| s(r, "status") != "exited").unwrap_or(false);
    if !running {
        if let Some(sess) = find_session(client, id) {
            if s(&sess, "owner") == "terminal" {
                bail!("このセッションはターミナル側の claude が実行中です。そちらを終了してから、もう一度 attach すると OYAKATA が引き継ぎます。");
            }
        }
        if !allow_resume {
            bail!("セッション {id} は OYAKATA の管理下にありません。`oyakata attach --resume {id}` で引き継げます。");
        }
        println!("{BOLD}このセッションは終了しています。最初の指示を入力すると OYAKATA が引き継いで再開します。{RST}");
        print!("> ");
        std::io::stdout().flush()?;
        let mut first = String::new();
        if std::io::stdin().read_line(&mut first)? == 0 || first.trim().is_empty() {
            bail!("指示が空なので中止しました");
        }
        let cwd = detail
            .as_ref()
            .and_then(|d| d.get("session"))
            .and_then(|x| x.get("cwd"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| defaults.cwd.clone());
        start(
            client,
            &StartArgs { cwd, prompt: first.trim().to_string(), resume: Some(id.to_string()), model: defaults.model.clone(), mode: defaults.mode.clone(), effort: defaults.effort.clone() },
        )?;
        run = find_run(client, id);
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    {
        let tx = tx.clone();
        let c = Client { bind: client.bind.clone(), port: client.port };
        std::thread::spawn(move || {
            let _ = c.sse(tx.clone());
            let _ = tx.send(Msg::StreamClosed);
        });
    }
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            let mut line = String::new();
            loop {
                line.clear();
                match stdin.lock().read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = tx.send(Msg::Eof);
                        break;
                    }
                    Ok(_) => {
                        if tx.send(Msg::Input(line.trim_end_matches(['\r', '\n']).to_string())).is_err() {
                            break;
                        }
                    }
                }
            }
        });
    }

    let mut status = run.as_ref().map(|r| s(r, "status").to_string()).unwrap_or_else(|| "busy".into());
    let mut pending: Option<Value> = run.as_ref().and_then(|r| r.get("pending")).and_then(Value::as_array).and_then(|a| a.first().cloned());
    let mut eof = false;
    // When stdin is closed (scripted use), leave once the turn ends, but give the transcript
    // poller a moment to deliver the final text first.
    let mut closing_at: Option<std::time::Instant> = None;
    println!("{DIM}接続: {id}  （/quit で端末だけ離脱、/stop でセッション終了。ブラウザからも同じ会話に送れます）{RST}");
    if let Some(p) = &pending {
        print_pending(p);
    } else if status == "idle" {
        print!("> ");
        std::io::stdout().flush()?;
    } else {
        println!("{DIM}（Claude が作業中… そのまま入力すると、次の区切りで Claude に渡します）{RST}");
    }

    loop {
        let msg = match closing_at {
            Some(deadline) => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    break;
                }
                match rx.recv_timeout(left) {
                    Ok(m) => m,
                    Err(mpsc::RecvTimeoutError::Timeout) => break,
                    Err(_) => break,
                }
            }
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        // A brand-new session has no transcript file until Claude writes the first line;
        // keep trying to load it so the daemon starts streaming its items to us.
        if !loaded {
            if let Ok(d) = client.get_json(&format!("/api/sessions/{id}")) {
                loaded = true;
                let items = d.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                for it in &items {
                    print_item(it, false);
                }
                seen = items.len();
            }
        }
        match msg {
            Msg::Event(kind, data) => match kind.as_str() {
                "append" if s(&data, "session") == id => {
                    let start = data.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
                    let items = data.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
                    for (k, it) in items.iter().enumerate() {
                        if start + k < seen {
                            continue;
                        }
                        print_item(it, false);
                    }
                    seen = seen.max(start + items.len());
                }
                "patch" if s(&data, "session") == id => {
                    if let Some(it) = data.get("item") {
                        if let Some(r) = it.get("result") {
                            if r.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                                println!("  {RED}✗ {}: {}{RST}", s(it, "name"), s(r, "text").lines().next().unwrap_or(""));
                            }
                        }
                    }
                }
                "run" => {
                    let r = data.get("run").cloned().unwrap_or(Value::Null);
                    if s(&r, "session_id") != id {
                        continue;
                    }
                    let new_status = s(&r, "status").to_string();
                    let new_pending = r.get("pending").and_then(Value::as_array).and_then(|a| a.first().cloned());
                    if new_status == "exited" {
                        println!("{DIM}セッションが終了しました。{}{RST}", s(&r, "last_error"));
                        break;
                    }
                    if new_pending.as_ref().map(|p| s(p, "request_id")) != pending.as_ref().map(|p| s(p, "request_id")) {
                        pending = new_pending;
                        if let Some(p) = &pending {
                            print_pending(p);
                        }
                    }
                    if new_status == "idle" && status != "idle" {
                        if eof {
                            closing_at = Some(std::time::Instant::now() + Duration::from_millis(2500));
                        } else {
                            print!("> ");
                            std::io::stdout().flush()?;
                        }
                    }
                    status = new_status;
                }
                _ => {}
            },
            Msg::Input(line) => {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }
                if line == "/quit" || line == "/q" {
                    println!("{DIM}端末を離れます。セッションは OYAKATA が持ったままです。{RST}");
                    break;
                }
                if line == "/stop" {
                    client.post_json(&format!("/api/run/{id}/stop"), &json!({}))?;
                    println!("セッションを終了しました。");
                    break;
                }
                if line == "/status" {
                    println!("status: {status}");
                    continue;
                }
                if let Some(p) = pending.take() {
                    let body = answer_for(&p, &line);
                    if let Err(e) = client.post_json(&format!("/api/run/{id}/permission"), &body) {
                        println!("{RED}{e}{RST}");
                        pending = Some(p);
                    }
                    continue;
                }
                match client.post_json(&format!("/api/run/{id}/send"), &json!({ "text": line })) {
                    Ok(_) => {
                        if status == "busy" {
                            println!("{DIM}（作業中なので、次の区切りで Claude に渡します）{RST}");
                        }
                        status = "busy".into();
                    }
                    Err(e) => println!("{RED}{e}{RST}"),
                }
            }
            Msg::Eof => {
                eof = true;
                if status == "idle" && pending.is_none() {
                    closing_at = Some(std::time::Instant::now() + Duration::from_millis(2500));
                }
            }
            Msg::StreamClosed => {
                println!("{DIM}OYAKATA との接続が切れました（常駐が停止した可能性があります）。{RST}");
                break;
            }
        }
    }
    Ok(())
}

pub fn current_dir_string() -> String {
    std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_else(|_| ".".into())
}

pub fn wait_until_running(client: &Client, secs: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if client.is_running() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

#[allow(dead_code)]
pub fn context_err<T>(r: Result<T>, what: &str) -> Result<T> {
    r.with_context(|| what.to_string())
}
