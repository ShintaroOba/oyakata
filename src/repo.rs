//! Derive "which repository is this session working in" from a working directory.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoInfo {
    /// Normalized grouping key (lower-cased, forward slashes).
    pub key: String,
    /// Display name: `owner/name` for ghq-style checkouts, otherwise the directory name.
    pub name: String,
    /// Git root if one was found by walking up from the cwd.
    pub root: Option<String>,
    /// The cwd relative to the git root, when the session ran in a subdirectory.
    pub subdir: Option<String>,
}

pub fn detect(cwd: &str) -> RepoInfo {
    let path = PathBuf::from(cwd);
    let mut cur: Option<&Path> = Some(path.as_path());
    while let Some(p) = cur {
        // `.git` is a directory for a normal checkout and a file for a worktree.
        if p.join(".git").exists() {
            let subdir = path
                .strip_prefix(p)
                .ok()
                .filter(|s| !s.as_os_str().is_empty())
                .map(|s| s.to_string_lossy().replace('\\', "/"));
            return RepoInfo {
                key: normalize(p),
                name: display_name(p),
                root: Some(p.to_string_lossy().into_owned()),
                subdir,
            };
        }
        cur = p.parent();
    }
    RepoInfo {
        key: normalize(&path),
        name: display_name(&path),
        root: None,
        subdir: None,
    }
}

/// `.../github.com/<owner>/<name>` → `owner/name`; anything else → last component.
fn display_name(root: &Path) -> String {
    let comps: Vec<String> = root
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let n = comps.len();
    if n >= 3 && looks_like_host(&comps[n - 3]) {
        return format!("{}/{}", comps[n - 2], comps[n - 1]);
    }
    comps
        .last()
        .cloned()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

fn looks_like_host(s: &str) -> bool {
    !s.starts_with('.')
        && s.contains('.')
        && !s.contains(' ')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

pub fn normalize(p: &Path) -> String {
    let mut s = p.to_string_lossy().replace('\\', "/");
    while s.ends_with('/') && s.len() > 1 {
        s.pop();
    }
    if cfg!(windows) {
        s = s.to_lowercase();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ghq_layout_becomes_owner_slash_name() {
        assert_eq!(display_name(Path::new("C:/Users/x/ghq/github.com/acme/widget")), "acme/widget");
        assert_eq!(display_name(Path::new("/home/x/ghq/gitlab.example.com/team/app")), "team/app");
    }

    #[test]
    fn dot_directories_are_not_hosts() {
        assert_eq!(display_name(Path::new("C:/Users/x/.claude/notes/journal")), "journal");
        assert_eq!(display_name(Path::new("C:/Users/x/workspaces/widget-2")), "widget-2");
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(normalize(Path::new("/a/b/")), "/a/b");
    }
}
