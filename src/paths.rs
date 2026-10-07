use std::path::PathBuf;

/// The user's home directory (`HOME` on Unix, `USERPROFILE` on Windows).
pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Claude Code's config directory. Honors an explicit override first,
/// then `CLAUDE_CONFIG_DIR` (the same variable Claude Code reads), then `~/.claude`.
pub fn claude_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    if let Some(p) = std::env::var_os("CLAUDE_CONFIG_DIR") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    home_dir().join(".claude")
}

/// When OYAKATA is started from a process with a trimmed environment (a tool sandbox, a
/// scheduled task), `PATH` can lack the folders where `git` and `node` live, and every git
/// call fails with "program not found". Append the machine and user `PATH` from the
/// registry — what a fresh terminal would see — keeping the inherited order first.
/// Must run before any thread is spawned.
#[cfg(windows)]
pub fn augment_path() {
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    let before = dirs.len();
    let sources = [
        (HKEY_LOCAL_MACHINE, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment"),
        (HKEY_CURRENT_USER, "Environment"),
    ];
    for (hive, key) in sources {
        let Some(value) = registry::read_expanded(hive, key, "Path") else { continue };
        for d in value.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            let p = PathBuf::from(d);
            let known = dirs.iter().any(|x| same_dir(x, &p));
            if !known && p.is_dir() {
                dirs.push(p);
            }
        }
    }
    if dirs.len() > before {
        if let Ok(joined) = std::env::join_paths(&dirs) {
            std::env::set_var("PATH", joined);
        }
    }
}

#[cfg(not(windows))]
pub fn augment_path() {}

#[cfg(windows)]
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let n = |p: &std::path::Path| p.to_string_lossy().trim_end_matches(['\\', '/']).to_lowercase();
    n(a) == n(b)
}

#[cfg(windows)]
mod registry {
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A string value with `%VAR%` references expanded.
    pub fn read_expanded(hive: HKEY, key: &str, value: &str) -> Option<String> {
        let (k, v) = (wide(key), wide(value));
        let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND;
        let mut size: u32 = 0;
        // SAFETY: valid NUL-terminated strings; a null buffer asks for the required size.
        let rc = unsafe { RegGetValueW(hive, k.as_ptr(), v.as_ptr(), flags, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
        if rc != 0 || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size` bytes.
        let rc = unsafe { RegGetValueW(hive, k.as_ptr(), v.as_ptr(), flags, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
        if rc != 0 {
            return None;
        }
        let raw: Vec<u16> = buf.iter().copied().take_while(|&c| c != 0).chain(std::iter::once(0)).collect();
        // SAFETY: `raw` is NUL-terminated; a zero-length destination asks for the size.
        let need = unsafe { ExpandEnvironmentStringsW(raw.as_ptr(), std::ptr::null_mut(), 0) };
        if need == 0 {
            return Some(String::from_utf16_lossy(&raw[..raw.len() - 1]));
        }
        let mut out = vec![0u16; need as usize];
        // SAFETY: `out` holds `need` characters.
        let n = unsafe { ExpandEnvironmentStringsW(raw.as_ptr(), out.as_mut_ptr(), need) };
        if n == 0 {
            return None;
        }
        Some(String::from_utf16_lossy(&out[..(n as usize).saturating_sub(1)]))
    }
}
