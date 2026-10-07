//! OYAKATA — a browser-based command post for Claude Code sessions across repositories.
//!
//! `oyakata`            start (or reuse) the daemon and open the browser
//! `oyakata serve`      run the server in the foreground
//! `oyakata status`     show whether a daemon is running
//! `oyakata stop`       stop the daemon
//! `oyakata install`    install the `/oyakata` skill into Claude Code
//! `oyakata new`        start a session OYAKATA owns and attach this terminal to it
//! `oyakata attach`     attach this terminal to a session OYAKATA owns
//! `oyakata sessions`   list sessions OYAKATA owns

mod client;
mod console;
mod gitops;
mod index;
mod live;
mod paths;
mod repo;
mod runner;
mod server;
mod transcript;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_PORT: u16 = 4848;
const SKILL_MD: &str = include_str!("../skills/oyakata/SKILL.md");

#[derive(Parser)]
#[command(name = "oyakata", version, about = "Browser-based command post for Claude Code sessions across repositories")]
struct Cli {
    #[command(flatten)]
    common: Common,
    /// Session id to open first (defaults to $CLAUDE_CODE_SESSION_ID when run from inside Claude Code)
    #[arg(long, global = true)]
    focus: Option<String>,
    /// Do not open a browser window
    #[arg(long, global = true)]
    no_open: bool,
    /// Run the server in this process instead of spawning a detached daemon
    #[arg(long)]
    foreground: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Args, Clone)]
struct Common {
    /// Port to listen on
    #[arg(long, global = true, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Address to bind
    #[arg(long, global = true, default_value = "127.0.0.1")]
    bind: String,
    /// Claude Code config directory (default: $CLAUDE_CONFIG_DIR or ~/.claude)
    #[arg(long, global = true)]
    claude_dir: Option<PathBuf>,
    /// Path to the `claude` executable used for sessions started from the browser
    #[arg(long, global = true)]
    claude: Option<PathBuf>,
    /// Extra folder whose direct subfolders are listed as repositories (repeatable)
    #[arg(long = "repo-root", global = true)]
    repo_roots: Vec<PathBuf>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server in the foreground
    Serve,
    /// Show whether a daemon is running on the port
    Status,
    /// Stop the running daemon
    Stop,
    /// Install the /oyakata skill into Claude Code (~/.claude/skills/oyakata)
    Install {
        /// Directory to install the skill into
        #[arg(long)]
        skills_dir: Option<PathBuf>,
        /// Overwrite an existing, different SKILL.md
        #[arg(long)]
        force: bool,
    },
    /// Start a session owned by OYAKATA from this terminal, then attach to it
    New {
        /// Working directory (default: current directory)
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Model, e.g. claude-opus-5-5
        #[arg(long)]
        model: Option<String>,
        /// Permission mode: default | acceptEdits | auto | plan | bypassPermissions
        #[arg(long)]
        mode: Option<String>,
        /// Effort level: low | medium | high | xhigh | max
        #[arg(long)]
        effort: Option<String>,
        /// Print the session id and return instead of attaching
        #[arg(long)]
        no_attach: bool,
        /// The first prompt
        #[arg(trailing_var_arg = true, required = true)]
        prompt: Vec<String>,
    },
    /// Attach this terminal to a session OYAKATA owns (the browser can keep using it too)
    Attach {
        /// Session id
        id: String,
        /// If the session has ended, let OYAKATA resume it (`claude --resume`) on the first prompt
        #[arg(long)]
        resume: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(long)]
        effort: Option<String>,
    },
    /// List sessions OYAKATA owns right now
    Sessions,
    /// (internal) Type stdin into the console of a Claude Code session running in a terminal
    #[command(hide = true)]
    TypeInto {
        #[arg(long)]
        pid: u32,
        #[arg(long)]
        proc_start: Option<u64>,
        /// Press Esc instead of typing text
        #[arg(long)]
        escape: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let claude_dir = paths::claude_dir(cli.common.claude_dir.clone());
    match cli.cmd {
        Some(Cmd::Serve) => {
            let url = (!cli.no_open).then(|| page_url(&cli.common, cli.focus.as_deref()));
            run_server(&cli.common, claude_dir, url)
        }
        Some(Cmd::Status) => {
            match probe(&cli.common) {
                Some(v) => {
                    println!("running: {}", base_url(&cli.common));
                    println!("{}", serde_json::to_string_pretty(&v)?);
                }
                None => println!("not running on {}", base_url(&cli.common)),
            }
            Ok(())
        }
        Some(Cmd::Stop) => {
            if probe(&cli.common).is_none() {
                println!("not running on {}", base_url(&cli.common));
                return Ok(());
            }
            let (status, body) = http_request(&cli.common, "POST", "/api/shutdown")?;
            if !(200..300).contains(&status) {
                bail!("daemon refused to stop (HTTP {status}): {}", body.trim());
            }
            // Wait until the port is actually free so callers can restart right away.
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline && probe(&cli.common).is_some() {
                std::thread::sleep(Duration::from_millis(200));
            }
            println!("stopped");
            Ok(())
        }
        Some(Cmd::Install { skills_dir, force }) => install(&claude_dir, skills_dir, force),
        Some(Cmd::New { cwd, model, mode, effort, no_attach, prompt }) => {
            let c = ensure_daemon(&cli.common, &claude_dir)?;
            let cwd = cwd.map(|p| p.display().to_string()).unwrap_or_else(client::current_dir_string);
            let args = client::StartArgs { cwd, prompt: prompt.join(" "), resume: None, model, mode, effort };
            let id = client::start(&c, &args)?;
            println!("session {id}\n{}", page_url(&cli.common, Some(&id)));
            if no_attach {
                return Ok(());
            }
            client::attach(&c, &id, false, &args)
        }
        Some(Cmd::Attach { id, resume, model, mode, effort }) => {
            let c = ensure_daemon(&cli.common, &claude_dir)?;
            let defaults = client::StartArgs { cwd: client::current_dir_string(), prompt: String::new(), resume: None, model, mode, effort };
            client::attach(&c, &id, resume, &defaults)
        }
        Some(Cmd::Sessions) => {
            let c = client::Client { bind: cli.common.bind.clone(), port: cli.common.port };
            if !c.is_running() {
                println!("not running on {}", base_url(&cli.common));
                return Ok(());
            }
            client::list_sessions(&c)
        }
        Some(Cmd::TypeInto { pid, proc_start, escape }) => console::helper_main(pid, proc_start, escape),
        None => open_flow(cli, claude_dir),
    }
}

/// A client for the daemon, starting the daemon first if nothing answers on the port.
fn ensure_daemon(common: &Common, claude_dir: &Path) -> Result<client::Client> {
    let c = client::Client { bind: common.bind.clone(), port: common.port };
    if c.is_running() {
        return Ok(c);
    }
    spawn_daemon(common, claude_dir)?;
    if !client::wait_until_running(&c, 10) {
        bail!("the daemon did not come up within 10s; see {}", claude_dir.join("oyakata.log").display());
    }
    Ok(c)
}

/// Default command: make sure a daemon is running, then open the browser on the right session.
fn open_flow(cli: Cli, claude_dir: PathBuf) -> Result<()> {
    let focus = cli
        .focus
        .clone()
        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok().filter(|s| !s.is_empty()));
    let url = page_url(&cli.common, focus.as_deref());

    if probe(&cli.common).is_some() {
        println!("OYAKATA is already running at {url}");
        if !cli.no_open {
            open_browser(&url)?;
        }
        return Ok(());
    }

    if cli.foreground {
        let open = (!cli.no_open).then_some(url);
        return run_server(&cli.common, claude_dir, open);
    }

    spawn_daemon(&cli.common, &claude_dir)?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if probe(&cli.common).is_some() {
            println!("OYAKATA is open at {url}");
            if !cli.no_open {
                open_browser(&url)?;
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    bail!(
        "the daemon did not come up within 10s; see {}",
        claude_dir.join("oyakata.log").display()
    )
}

fn base_url(c: &Common) -> String {
    format!("http://{}:{}", c.bind, c.port)
}

fn page_url(c: &Common, focus: Option<&str>) -> String {
    match focus {
        Some(id) => format!("{}/#/s/{id}", base_url(c)),
        None => format!("{}/", base_url(c)),
    }
}

#[tokio::main]
async fn run_server(c: &Common, claude_dir: PathBuf, open_url: Option<String>) -> Result<()> {
    if !claude_dir.join("projects").is_dir() {
        bail!(
            "no Claude Code data at {} (expected a projects/ directory; use --claude-dir)",
            claude_dir.display()
        );
    }
    let started = Instant::now();
    let mut state = index::State::new(claude_dir.clone());
    state.ghq_root = gitops::ghq_root(&paths::home_dir());
    state.repo_roots = c.repo_roots.iter().filter(|p| p.is_dir()).cloned().collect();
    let n = state.initial_scan();
    let live = state.live.len();
    eprintln!(
        "oyakata {}: indexed {n} sessions ({live} live) from {} in {:.2}s",
        env!("CARGO_PKG_VERSION"),
        claude_dir.display(),
        started.elapsed().as_secs_f64()
    );
    let claude_exe = runner::find_claude(c.claude.clone());
    match &claude_exe {
        Some(exe) => eprintln!("claude executable: {}", exe.display()),
        None => eprintln!("claude executable not found; starting sessions from the browser is disabled (use --claude <path>)"),
    }
    let runners = Arc::new(runner::RunnerRegistry::new(claude_exe));

    let (tx, _rx) = tokio::sync::broadcast::channel(512);
    let app: server::Shared = Arc::new(server::App {
        state: Arc::new(Mutex::new(state)),
        tx,
        shutdown: Arc::new(tokio::sync::Notify::new()),
        started_at_ms: now_ms(),
        runners: runners.clone(),
        default_permission_mode: default_permission_mode(&claude_dir),
    });

    let addr = format!("{}:{}", c.bind, c.port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("bind {addr} (is something else using the port? try --port)"))?;
    eprintln!("listening on {}", base_url(c));

    tokio::spawn(server::poll_loop(app.clone(), Duration::from_millis(1000)));
    tokio::spawn(server::run_events_loop(app.clone()));

    if let Some(url) = open_url {
        if let Err(e) = open_browser(&url) {
            eprintln!("could not open a browser: {e}; open {url} yourself");
        }
    }

    // Shutdown: `oyakata stop` or Ctrl-C flips a watch channel. The server drains briefly,
    // but open browser tabs hold SSE streams forever, so we stop waiting after a second.
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let shutdown = app.shutdown.clone();
    tokio::spawn(async move {
        tokio::select! {
            _ = shutdown.notified() => {},
            _ = tokio::signal::ctrl_c() => {},
        }
        eprintln!("shutting down");
        let _ = stop_tx.send(true);
    });
    let mut graceful_rx = stop_rx.clone();
    let server = axum::serve(listener, server::router(app)).with_graceful_shutdown(async move {
        let _ = graceful_rx.wait_for(|v| *v).await;
    });
    let mut deadline_rx = stop_rx.clone();
    tokio::select! {
        r = server => { r?; }
        _ = async move {
            let _ = deadline_rx.wait_for(|v| *v).await;
            tokio::time::sleep(Duration::from_secs(1)).await;
        } => {}
    }
    // Sessions started from the browser die with the daemon; end them cleanly so Claude Code
    // flushes its transcript and they can be resumed later.
    runners.stop_all().await;
    Ok(())
}

/// `permissions.defaultMode` from Claude Code's settings, used as the default for sessions
/// started from the browser.
fn default_permission_mode(claude_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(claude_dir.join("settings.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("permissions")?.get("defaultMode")?.as_str().map(str::to_string)
}

/// Start `oyakata serve` as a detached process whose output goes to `<claude_dir>/oyakata.log`.
fn spawn_daemon(c: &Common, claude_dir: &Path) -> Result<()> {
    let exe = std::env::current_exe().context("locate own executable")?;
    let log_path = claude_dir.join("oyakata.log");
    let log = fs::File::create(&log_path).with_context(|| format!("create {}", log_path.display()))?;
    let mut args: Vec<String> = vec![
        "serve".into(),
        "--no-open".into(),
        "--port".into(),
        c.port.to_string(),
        "--bind".into(),
        c.bind.clone(),
    ];
    if let Some(dir) = &c.claude_dir {
        args.push("--claude-dir".into());
        args.push(dir.display().to_string());
    }
    if let Some(exe) = &c.claude {
        args.push("--claude".into());
        args.push(exe.display().to_string());
    }
    for r in &c.repo_roots {
        args.push("--repo-root".into());
        args.push(r.display().to_string());
    }

    let build = |breakaway: bool| {
        let mut cmd = Command::new(&exe);
        cmd.args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().expect("clone log handle")))
            .stderr(Stdio::from(log.try_clone().expect("clone log handle")));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
            let mut flags = DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP;
            if breakaway {
                flags |= CREATE_BREAKAWAY_FROM_JOB;
            }
            cmd.creation_flags(flags);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let _ = breakaway;
            cmd.process_group(0);
        }
        cmd
    };

    // Breaking away from a job object keeps the daemon alive after the caller's job (for
    // example Claude Code's Bash tool) is torn down; not every job allows it, so retry without.
    match build(true).spawn() {
        Ok(_) => Ok(()),
        Err(_) => {
            build(false).spawn().context("spawn daemon")?;
            Ok(())
        }
    }
}

fn install(claude_dir: &Path, skills_dir: Option<PathBuf>, force: bool) -> Result<()> {
    let dir = skills_dir.unwrap_or_else(|| claude_dir.join("skills").join("oyakata"));
    let target = dir.join("SKILL.md");
    if target.exists() && !force {
        let current = fs::read_to_string(&target)?;
        if current == SKILL_MD {
            println!("skill already up to date: {}", target.display());
        } else {
            println!(
                "{} exists and differs from the bundled skill; re-run with --force to overwrite",
                target.display()
            );
        }
    } else {
        fs::create_dir_all(&dir)?;
        fs::write(&target, SKILL_MD)?;
        println!("installed skill: {}", target.display());
    }
    println!();
    println!("Use it from any Claude Code session with: /oyakata");
    println!("Optional, to make every session prefer Mermaid diagrams, add to ~/.claude/CLAUDE.md:");
    println!("  図は ```mermaid フェンスで書く（OYAKATA がブラウザで描画する）。ASCIIアートで図を描かない。");
    Ok(())
}

/// Ask a running daemon for /api/health. Uses a hand-rolled HTTP/1.0 request so the CLI
/// stays free of an HTTP client dependency.
fn probe(c: &Common) -> Option<Value> {
    let (status, body) = http_request(c, "GET", "/api/health").ok()?;
    if status != 200 {
        return None;
    }
    let v: Value = serde_json::from_str(&body).ok()?;
    (v.get("name")?.as_str()? == "oyakata").then_some(v)
}

/// Minimal HTTP/1.0 client for talking to the daemon, so the CLI needs no HTTP crate.
/// Sends the `X-Oyakata` header the server requires on mutating calls.
fn http_request(c: &Common, method: &str, path: &str) -> Result<(u16, String)> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    let addr = format!("{}:{}", c.bind, c.port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| anyhow!("unresolvable bind address"))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_millis(400))?;
    s.set_read_timeout(Some(Duration::from_secs(2)))?;
    s.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        s,
        "{method} {path} HTTP/1.0\r\nHost: {}\r\nX-Oyakata: cli\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        c.bind
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

fn open_browser(url: &str) -> Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    cmd.spawn().context("launch browser")?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
