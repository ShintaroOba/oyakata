//! Derive "which repository is this session working in" from a working directory.

use serde::Serialize;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RepoInfo {
    /// Normalized grouping key (lower-cased, forward slashes).
    pub key: String,
    /// Display name: `owner/name` for ghq-style checkouts, otherwise the directory name.
    pub name: String,
    /// Git root if one was found by walking up from the cwd. For a linked worktree this is
    /// the main checkout, so worktree sessions are listed under their repository.
    pub root: Option<String>,
    /// The cwd relative to the checkout root, when the session ran in a subdirectory.
    pub subdir: Option<String>,
    /// The linked worktree the cwd is in (its checkout root), when it is not the main one.
    /// File tree, Git view and diffs of the session use this instead of `root`.
    pub worktree: Option<String>,
    /// Branch checked out in that worktree.
    pub branch: Option<String>,
}

pub fn detect(cwd: &str) -> RepoInfo {
    let path = PathBuf::from(cwd);
    let mut cur: Option<&Path> = Some(path.as_path());
    while let Some(p) = cur {
        // `.git` is a directory for a normal checkout and a file for a worktree.
        let dot_git = p.join(".git");
        if dot_git.exists() {
            let subdir = path
                .strip_prefix(p)
                .ok()
                .filter(|s| !s.as_os_str().is_empty())
                .map(|s| s.to_string_lossy().replace('\\', "/"));
            if dot_git.is_file() {
                if let Some((main, branch)) = linked_worktree(p, &dot_git) {
                    return RepoInfo {
                        key: normalize(&main),
                        name: display_name(&main),
                        root: Some(main.to_string_lossy().into_owned()),
                        subdir,
                        worktree: Some(p.to_string_lossy().into_owned()),
                        branch,
                    };
                }
            }
            return RepoInfo {
                key: normalize(p),
                name: display_name(p),
                root: Some(p.to_string_lossy().into_owned()),
                subdir,
                worktree: None,
                branch: None,
            };
        }
        cur = p.parent();
    }
    RepoInfo {
        key: normalize(&path),
        name: display_name(&path),
        root: None,
        subdir: None,
        worktree: None,
        branch: None,
    }
}

/// A linked worktree's `.git` file says `gitdir: <main>/.git/worktrees/<name>`, and that
/// directory has a `commondir` file pointing back at the main `.git`. Returns the main
/// checkout and the worktree's branch. Submodules also have a `.git` file but no
/// `commondir`, so they stay repositories of their own.
fn linked_worktree(root: &Path, dot_git: &Path) -> Option<(PathBuf, Option<String>)> {
    let text = std::fs::read_to_string(dot_git).ok()?;
    let gitdir = text.lines().find_map(|l| l.strip_prefix("gitdir:")).map(str::trim)?;
    let gitdir = {
        let g = PathBuf::from(gitdir);
        if g.is_absolute() {
            g
        } else {
            root.join(g)
        }
    };
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = lexical_normalize(&gitdir.join(common.trim()));
    if common.file_name().map(|n| n != ".git").unwrap_or(true) {
        return None; // a bare repository or an unusual layout: leave it alone
    }
    let main = common.parent()?.to_path_buf();
    let branch = std::fs::read_to_string(gitdir.join("HEAD"))
        .ok()
        .and_then(|h| h.trim().strip_prefix("ref: refs/heads/").map(str::to_string));
    Some((main, branch))
}

/// Resolve `.` and `..` without touching the filesystem (paths may be on another drive
/// letter spelling, and canonicalize would add `\\?\` on Windows).
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
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

    #[test]
    fn linked_worktrees_belong_to_their_main_repository() {
        let tmp = std::env::temp_dir().join(format!("oyk-repo-{}", std::process::id()));
        let main = tmp.join("github.com").join("acme").join("widget");
        let wt = tmp.join("worktrees").join("widget").join("fix-1");
        let admin = main.join(".git").join("worktrees").join("fix-1");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/oyakata/fix-1\n").unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        // A submodule-style .git file (no commondir) stays its own repository.
        let sub = tmp.join("sub");
        let sub_admin = main.join(".git").join("modules").join("sub");
        std::fs::create_dir_all(&sub_admin).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join(".git"), format!("gitdir: {}\n", sub_admin.display())).unwrap();

        let r = detect(&wt.join("src").to_string_lossy());
        assert_eq!(r.name, "acme/widget");
        assert_eq!(r.key, normalize(&main));
        assert_eq!(r.root.as_deref(), Some(main.to_string_lossy().as_ref()));
        assert_eq!(r.worktree.as_deref(), Some(wt.to_string_lossy().as_ref()));
        assert_eq!(r.branch.as_deref(), Some("oyakata/fix-1"));
        assert_eq!(r.subdir.as_deref(), Some("src"));

        let s = detect(&sub.to_string_lossy());
        assert_eq!(s.worktree, None);
        assert_eq!(s.root.as_deref(), Some(sub.to_string_lossy().as_ref()));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
