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
