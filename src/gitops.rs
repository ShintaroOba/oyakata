//! Repository view backend: file tree, file contents, diffs, log, branches, and the three
//! write operations (commit, push, pull). Everything shells out to `git`, which keeps the
//! binary small and makes the behaviour match what the user sees in a terminal.

use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const MAX_TEXT_FILE: u64 = 2 * 1024 * 1024;
const MAX_SHOW_BYTES: usize = 400 * 1024;

pub struct GitOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl GitOutput {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
    pub fn combined(&self) -> String {
        let mut s = self.stdout.trim().to_string();
        if !self.stderr.trim().is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(self.stderr.trim());
        }
        s
    }
}

fn git_command(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(root).arg("--no-pager");
    c.env("GIT_TERMINAL_PROMPT", "0").env("LC_ALL", "C").env("GIT_PAGER", "cat");
    c.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

pub fn run(root: &Path, args: &[&str]) -> Result<GitOutput> {
    let out = git_command(root)
        .args(args)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    Ok(GitOutput {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

fn run_ok(root: &Path, args: &[&str]) -> Result<String> {
    let o = run(root, args)?;
    if !o.ok() {
        bail!("git {} failed: {}", args.join(" "), o.stderr.trim());
    }
    Ok(o.stdout)
}

/// Like `run`, but kills git if it exceeds the timeout (network operations).
pub fn run_timeout(root: &Path, args: &[&str], timeout: Duration) -> Result<GitOutput> {
    let mut child = git_command(root)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn git {}", args.join(" ")))?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            let out = child.wait_with_output()?;
            return Ok(GitOutput {
                status: status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            bail!("git {} timed out after {}s", args.join(" "), timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---- status ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct StatusEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub index: String,
    pub worktree: String,
    pub staged: bool,
    pub unstaged: bool,
    pub untracked: bool,
    pub conflict: bool,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct RepoStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub detached: bool,
    pub no_commits: bool,
    pub head: Option<String>,
    pub head_subject: Option<String>,
    pub entries: Vec<StatusEntry>,
}

pub fn status(root: &Path) -> Result<RepoStatus> {
    let raw = run_ok(root, &["status", "--porcelain=v1", "-b", "-z", "--untracked-files=all"])?;
    let mut st = RepoStatus::default();
    let mut parts = raw.split('\0').peekable();
    if let Some(header) = parts.next() {
        parse_header(header, &mut st);
    }
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, path) = entry.split_at(3);
        let x = xy.chars().next().unwrap_or(' ');
        let y = xy.chars().nth(1).unwrap_or(' ');
        let mut orig = None;
        if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
            orig = parts.next().map(str::to_string);
        }
        let untracked = x == '?' && y == '?';
        let conflict = matches!((x, y), ('U', _) | (_, 'U') | ('A', 'A') | ('D', 'D'));
        st.entries.push(StatusEntry {
            path: path.to_string(),
            orig_path: orig,
            index: x.to_string(),
            worktree: y.to_string(),
            staged: !untracked && x != ' ' && x != '?',
            unstaged: untracked || (y != ' ' && y != '?') || untracked,
            untracked,
            conflict,
        });
    }
    st.entries.sort_by(|a, b| a.path.cmp(&b.path));
    if !st.no_commits {
        if let Ok(line) = run_ok(root, &["log", "-1", "--format=%h%x1f%s"]) {
            let mut it = line.trim_end().split('\x1f');
            st.head = it.next().map(str::to_string).filter(|s| !s.is_empty());
            st.head_subject = it.next().map(str::to_string);
        }
    }
    Ok(st)
}

fn parse_header(header: &str, st: &mut RepoStatus) {
    // "## main...origin/main [ahead 1, behind 2]" | "## HEAD (no branch)" | "## No commits yet on main"
    let h = header.trim_start_matches("## ").trim();
    if let Some(rest) = h.strip_prefix("No commits yet on ") {
        st.no_commits = true;
        st.branch = Some(rest.to_string());
        return;
    }
    if h.starts_with("HEAD (no branch)") {
        st.detached = true;
        st.branch = Some("HEAD".into());
        return;
    }
    let (names, tracking) = match h.find(" [") {
        Some(i) => (&h[..i], Some(&h[i + 2..h.len().saturating_sub(1)])),
        None => (h, None),
    };
    match names.split_once("...") {
        Some((b, u)) => {
            st.branch = Some(b.to_string());
            st.upstream = Some(u.to_string());
        }
        None => st.branch = Some(names.to_string()),
    }
    if let Some(t) = tracking {
        for part in t.split(',') {
            let part = part.trim();
            if let Some(n) = part.strip_prefix("ahead ") {
                st.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                st.behind = n.parse().unwrap_or(0);
            }
        }
    }
}

// ---- tree & files ---------------------------------------------------------------------------

/// Tracked plus untracked-but-not-ignored files, relative to the root with forward slashes.
pub fn tree(root: &Path) -> Result<Vec<String>> {
    let raw = run_ok(root, &["ls-files", "-co", "--exclude-standard", "-z"])?;
    let mut files: Vec<String> = raw.split('\0').filter(|s| !s.is_empty()).map(str::to_string).collect();
    files.sort();
    files.dedup();
    Ok(files)
}

/// Resolve `rel` under `root`, refusing anything that escapes the root.
pub fn safe_join(root: &Path, rel: &str) -> Result<PathBuf> {
    let rel = rel.replace('\\', "/");
    let p = Path::new(&rel);
    if p.is_absolute() || rel.starts_with('/') || rel.contains(':') {
        bail!("path must be relative to the repository");
    }
    for c in p.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => bail!("path escapes the repository"),
        }
    }
    Ok(root.join(p))
}

#[derive(Debug, Clone, Serialize)]
pub struct FileContent {
    pub path: String,
    pub size: u64,
    pub mtime_ms: u64,
    pub binary: bool,
    pub truncated: bool,
    pub mime: String,
    pub content: Option<String>,
    /// Line ending the file used, so a save can keep it (`\r\n` or `\n`).
    pub eol: String,
}

pub fn mtime_ms(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn read_text_file(path: &Path, display_path: &str) -> Result<FileContent> {
    let md = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
    if md.is_dir() {
        bail!("{} is a directory", display_path);
    }
    let mime = mime_for(path);
    let size = md.len();
    let mtime = mtime_ms(&md);
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let probe = &bytes[..bytes.len().min(8000)];
    let binary = probe.contains(&0) || mime.starts_with("image/") || mime == "application/pdf";
    if binary {
        return Ok(FileContent {
            path: display_path.to_string(),
            size,
            mtime_ms: mtime,
            binary: true,
            truncated: false,
            mime,
            content: None,
            eol: "\n".into(),
        });
    }
    let truncated = size > MAX_TEXT_FILE;
    let slice = if truncated { &bytes[..MAX_TEXT_FILE as usize] } else { &bytes[..] };
    let text = String::from_utf8_lossy(slice).into_owned();
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    Ok(FileContent {
        path: display_path.to_string(),
        size,
        mtime_ms: mtime,
        binary: false,
        truncated,
        mime,
        content: Some(text.replace("\r\n", "\n")),
        eol: eol.into(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct WriteResult {
    pub path: String,
    pub size: u64,
    pub mtime_ms: u64,
}

/// Save a text file. When `base_mtime_ms` is given and the file changed on disk since then,
/// refuse so the caller can show a conflict instead of clobbering someone else's edit.
pub fn write_text_file(path: &Path, content: &str, eol: &str, base_mtime_ms: Option<u64>) -> Result<WriteResult> {
    if let Some(base) = base_mtime_ms {
        if let Ok(md) = std::fs::metadata(path) {
            let now = mtime_ms(&md);
            if now != base {
                bail!("conflict: the file changed on disk (mtime {now} != {base})");
            }
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let data = if eol == "\r\n" { content.replace('\n', "\r\n") } else { content.to_string() };
    let tmp = path.with_extension(format!("{}.oyakata-tmp", path.extension().and_then(|e| e.to_str()).unwrap_or("")));
    std::fs::write(&tmp, data.as_bytes()).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path).or_else(|_| {
        // Windows refuses to rename over a file that is open elsewhere; fall back to a plain write.
        let _ = std::fs::remove_file(&tmp);
        std::fs::write(path, data.as_bytes())
    })
    .with_context(|| format!("save {}", path.display()))?;
    let md = std::fs::metadata(path)?;
    Ok(WriteResult { path: path.display().to_string(), size: md.len(), mtime_ms: mtime_ms(&md) })
}

#[derive(Debug, Clone, Serialize)]
pub struct DirEntry {
    pub name: String,
    pub dir: bool,
    pub size: u64,
}

/// Plain directory listing for folders that are not git repositories.
pub fn list_dir(path: &Path) -> Result<Vec<DirEntry>> {
    let rd = std::fs::read_dir(path).with_context(|| format!("list {}", path.display()))?;
    let mut out: Vec<DirEntry> = rd
        .flatten()
        .filter_map(|e| {
            let md = e.metadata().ok()?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name == ".git" || name == "node_modules" || name == "target" {
                return None;
            }
            Some(DirEntry { name, dir: md.is_dir(), size: md.len() })
        })
        .collect();
    out.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(out)
}

pub fn mime_for(path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "txt" | "log" | "" => "text/plain; charset=utf-8",
        _ => "text/plain; charset=utf-8",
    }
    .to_string()
}

// ---- diff, log, branches -------------------------------------------------------------------

pub fn diff(root: &Path, rel: &str, staged: bool) -> Result<String> {
    let args: Vec<&str> = if staged {
        vec!["diff", "--cached", "--no-color", "--", rel]
    } else {
        vec!["diff", "--no-color", "--", rel]
    };
    let out = run_ok(root, &args)?;
    if !out.trim().is_empty() {
        return Ok(out);
    }
    // Untracked files have no diff; synthesize an all-added one so the viewer has something.
    let tracked = run(root, &["ls-files", "--error-unmatch", "--", rel])?.ok();
    if tracked {
        return Ok(String::new());
    }
    let full = safe_join(root, rel)?;
    let file = read_text_file(&full, rel)?;
    let Some(content) = file.content else { return Ok(String::new()) };
    let mut s = format!("--- /dev/null\n+++ b/{rel}\n@@ -0,0 +1,{} @@\n", content.lines().count());
    for line in content.lines() {
        s.push('+');
        s.push_str(line);
        s.push('\n');
    }
    Ok(s)
}

/// Diff of the whole working tree (unstaged + staged + untracked), for the "変更" tab.
pub fn diff_all(root: &Path) -> Result<String> {
    let mut s = run_ok(root, &["diff", "--no-color", "HEAD"])
        .or_else(|_| run_ok(root, &["diff", "--no-color"]))?;
    let st = status(root)?;
    for e in st.entries.iter().filter(|e| e.untracked) {
        if let Ok(d) = diff(root, &e.path, false) {
            s.push_str(&d);
        }
    }
    Ok(s)
}

#[derive(Debug, Clone, Serialize)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub refs: String,
}

pub fn log(root: &Path, n: usize) -> Result<Vec<Commit>> {
    let n = n.clamp(1, 500).to_string();
    let out = run(root, &["log", "-n", &n, "--format=%H%x1f%h%x1f%an%x1f%aI%x1f%s%x1f%D%x1e"])?;
    if !out.ok() {
        return Ok(Vec::new());
    }
    Ok(out
        .stdout
        .split('\x1e')
        .filter(|s| !s.trim().is_empty())
        .filter_map(|rec| {
            let f: Vec<&str> = rec.trim_start_matches(['\n', '\r']).split('\x1f').collect();
            (f.len() >= 5).then(|| Commit {
                hash: f[0].to_string(),
                short: f[1].to_string(),
                author: f[2].to_string(),
                date: f[3].to_string(),
                subject: f[4].to_string(),
                refs: f.get(5).unwrap_or(&"").trim().to_string(),
            })
        })
        .collect())
}

pub fn show(root: &Path, hash: &str) -> Result<String> {
    if !hash.chars().all(|c| c.is_ascii_hexdigit()) || hash.len() < 4 {
        bail!("invalid commit hash");
    }
    let mut out = run_ok(
        root,
        &["show", "--no-color", "--stat", "-p", "--format=commit %H%nAuthor: %an <%ae>%nDate:   %aI%n%n    %s%n%n%b", hash],
    )?;
    if out.len() > MAX_SHOW_BYTES {
        let mut cut = MAX_SHOW_BYTES;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n… (truncated)\n");
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct Branch {
    pub name: String,
    pub upstream: Option<String>,
    pub current: bool,
}

pub fn branches(root: &Path) -> Result<Vec<Branch>> {
    let out = run_ok(root, &["branch", "--format=%(refname:short)%09%(upstream:short)%09%(HEAD)"])?;
    Ok(out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            Branch {
                name: f.first().unwrap_or(&"").to_string(),
                upstream: f.get(1).filter(|s| !s.is_empty()).map(|s| s.to_string()),
                current: f.get(2).map(|s| s.trim() == "*").unwrap_or(false),
            }
        })
        .collect())
}

// ---- write operations ----------------------------------------------------------------------

pub fn commit(root: &Path, message: &str, paths: &[String]) -> Result<String> {
    if message.trim().is_empty() {
        bail!("commit message is empty");
    }
    let add = if paths.is_empty() {
        run(root, &["add", "-A"])?
    } else {
        for p in paths {
            safe_join(root, p)?;
        }
        let mut args: Vec<&str> = vec!["add", "--"];
        args.extend(paths.iter().map(String::as_str));
        run(root, &args)?
    };
    if !add.ok() {
        bail!("git add failed: {}", add.combined());
    }
    let out = run(root, &["commit", "-m", message])?;
    if !out.ok() {
        bail!("git commit failed: {}", out.combined());
    }
    Ok(out.combined())
}

pub fn push(root: &Path) -> Result<String> {
    let out = run_timeout(root, &["push"], Duration::from_secs(180))?;
    if out.ok() {
        return Ok(out.combined());
    }
    let err = out.combined();
    if err.contains("no upstream") || err.contains("has no upstream branch") || err.contains("set-upstream") {
        let retry = run_timeout(root, &["push", "-u", "origin", "HEAD"], Duration::from_secs(180))?;
        if retry.ok() {
            return Ok(retry.combined());
        }
        bail!("git push failed: {}", retry.combined());
    }
    bail!("git push failed: {err}")
}

pub fn pull(root: &Path) -> Result<String> {
    let out = run_timeout(root, &["pull", "--ff-only"], Duration::from_secs(180))?;
    if !out.ok() {
        bail!("git pull failed: {}", out.combined());
    }
    Ok(out.combined())
}

// ---- discovery -----------------------------------------------------------------------------

/// ghq's root (`git config ghq.root`), falling back to `~/ghq` when it exists.
pub fn ghq_root(home: &Path) -> Option<PathBuf> {
    let out = git_command(home).args(["config", "--global", "--get", "ghq.root"]).output().ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !s.is_empty() {
            let p = PathBuf::from(expand_tilde(&s, home));
            if p.is_dir() {
                return Some(p);
            }
        }
    }
    let fallback = home.join("ghq");
    fallback.is_dir().then_some(fallback)
}

fn expand_tilde(s: &str, home: &Path) -> String {
    match s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => s.to_string(),
    }
}

/// Repositories under a ghq root: `<root>/<host>/<owner>/<name>` that contain `.git`.
pub fn scan_ghq(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for host in read_dirs(root) {
        for owner in read_dirs(&host) {
            for repo in read_dirs(&owner) {
                if repo.join(".git").exists() {
                    out.push(repo);
                }
            }
        }
    }
    out
}

/// Repositories directly under a plain folder (plus the folder itself if it is a repo).
pub fn scan_folder(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if root.join(".git").exists() {
        out.push(root.to_path_buf());
    }
    for child in read_dirs(root) {
        if child.join(".git").exists() {
            out.push(child);
        }
    }
    out
}

fn read_dirs(p: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(p) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.file_name().map(|n| n.to_string_lossy().starts_with('.')).unwrap_or(false))
        .collect();
    v.sort();
    v
}

pub fn is_repo(root: &Path) -> bool {
    root.join(".git").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_header() {
        let mut st = RepoStatus::default();
        parse_header("## main...origin/main [ahead 2, behind 1]", &mut st);
        assert_eq!(st.branch.as_deref(), Some("main"));
        assert_eq!(st.upstream.as_deref(), Some("origin/main"));
        assert_eq!((st.ahead, st.behind), (2, 1));
        let mut st = RepoStatus::default();
        parse_header("## HEAD (no branch)", &mut st);
        assert!(st.detached);
        let mut st = RepoStatus::default();
        parse_header("## No commits yet on main", &mut st);
        assert!(st.no_commits);
    }

    #[test]
    fn safe_join_rejects_escapes() {
        let root = Path::new("/r");
        assert!(safe_join(root, "../x").is_err());
        assert!(safe_join(root, "/etc/passwd").is_err());
        assert!(safe_join(root, "C:/x").is_err());
        assert!(safe_join(root, "src/main.rs").is_ok());
    }
}
