//! Live session registry: Claude Code writes `~/.claude/sessions/<pid>.json` for every
//! running interactive session (status busy/idle, cwd, display name). Entries whose
//! process is gone are ignored, so a crash never leaves a ghost "live" session.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawLive {
    pid: u32,
    session_id: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    started_at: Option<u64>,
    #[serde(default)]
    updated_at: Option<u64>,
    #[serde(default)]
    status_updated_at: Option<u64>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    waiting_for: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    /// Process creation time as a Windows FILETIME, written as a decimal string.
    #[serde(default)]
    proc_start: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LiveInfo {
    pub pid: u32,
    pub session_id: String,
    pub cwd: Option<String>,
    /// `busy` while the model is working, `idle` while waiting for the user,
    /// `waiting` while blocked on a prompt (see `waiting_for`).
    pub status: String,
    pub waiting_for: Option<String>,
    pub name: Option<String>,
    pub started_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub status_updated_at: Option<u64>,
    pub version: Option<String>,
    pub kind: Option<String>,
    /// `cli` for a terminal; IDE extensions and SDK hosts use other values.
    pub entrypoint: Option<String>,
    /// Too large for a JavaScript number, and only the server needs it.
    #[serde(skip)]
    pub proc_start: Option<u64>,
    /// An interactive session in a terminal, so OYAKATA can type into it (`console`).
    pub typeable: bool,
}

/// Read the registry directory and return live sessions keyed by session id.
pub fn read_registry(dir: &Path) -> HashMap<String, LiveInfo> {
    let mut out: HashMap<String, LiveInfo> = HashMap::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = serde_json::from_str::<RawLive>(&text) else {
            continue;
        };
        if !pid_alive(raw.pid) {
            continue;
        }
        let typeable = cfg!(windows)
            && raw.kind.as_deref().map_or(true, |k| k == "interactive")
            && raw.entrypoint.as_deref().map_or(true, |e| e == "cli");
        let proc_start = match &raw.proc_start {
            Some(serde_json::Value::String(s)) => s.parse().ok(),
            Some(v) => v.as_u64(),
            None => None,
        };
        let info = LiveInfo {
            pid: raw.pid,
            session_id: raw.session_id,
            cwd: raw.cwd,
            status: raw.status.unwrap_or_else(|| "unknown".into()),
            waiting_for: raw.waiting_for,
            name: raw.name,
            started_at: raw.started_at,
            updated_at: raw.updated_at,
            status_updated_at: raw.status_updated_at,
            version: raw.version,
            kind: raw.kind,
            entrypoint: raw.entrypoint,
            proc_start,
            typeable,
        };
        let newer = match out.get(&info.session_id) {
            Some(prev) => info.updated_at >= prev.updated_at,
            None => true,
        };
        if newer {
            out.insert(info.session_id.clone(), info);
        }
    }
    out
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    // SAFETY: plain Win32 calls with a valid out-pointer; the handle is closed before returning.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(handle, &mut code);
        CloseHandle(handle);
        ok != 0 && code == STILL_ACTIVE as u32
    }
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill with signal 0 only probes for existence.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
pub fn pid_alive(_pid: u32) -> bool {
    true
}
