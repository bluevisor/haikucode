//! Tool implementations. Pure-ish: each returns a String result for the model.
//! Approval and event emission live in the agent; preview/diff helpers here let
//! the agent show a change before it is applied.

use std::path::{Path, PathBuf};

mod files;
mod git;
mod images;
mod memory;
mod search;
mod shell;
mod web;

pub use files::{apply_write, edit_preview, list_files, multi_edit_plan, read_file, write_file, write_preview, EditPreview, EditReq};
pub use git::{git_autocommit, git_context, git_head, git_rev, git_run, git_status_line, in_git_repo, revert_checkpoints_since};
pub use images::{image_data_uri, is_image_path};
pub use memory::{load_memory, recall, remember, todo};
pub use search::{glob_search, grep};
pub use shell::{bash, bash_background, bash_kill, bash_output};
pub use web::{web_fetch, web_search};

pub(crate) fn expand(path: &str) -> PathBuf {
    let raw = if let Some(rest) = path.strip_prefix("~/") {
        match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home).join(rest),
            Err(_) => PathBuf::from(path),
        }
    } else if path == "~" {
        match std::env::var("HOME") {
            Ok(home) => PathBuf::from(home),
            Err(_) => PathBuf::from(path),
        }
    } else {
        PathBuf::from(path)
    };
    normalize(&raw)
}

/// Collapse `.` and `..` lexically (without touching the filesystem, so it works
/// for paths that don't exist yet). This doesn't sandbox — a coding agent
/// legitimately reads files all over the box — but it turns a sneaky
/// `~/../../etc/x` into the plain `/etc/x` it really means, so the path the user
/// approves is the path that's used.
fn normalize(p: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => match out.components().next_back() {
                // Pop a real directory name…
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // …but never climb above the filesystem root.
                Some(Component::RootDir) => {}
                // At a relative start, keep the `..` (can't resolve it lexically).
                _ => out.push(".."),
            },
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// Refuse to operate through a symlink: a symlinked path could point outside the
/// intended target. Reads and writes both go through this so the guard is
/// symmetric. Returns `Some(error)` to short-circuit, `None` to proceed.
fn deny_symlink(p: &Path, path: &str, verb: &str) -> Option<String> {
    match std::fs::symlink_metadata(p) {
        Ok(meta) if meta.file_type().is_symlink() => {
            Some(format!("DENIED: {path} is a symlink; {verb} the real path instead."))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_dot_segments() {
        assert_eq!(normalize(Path::new("a/b/../c")), Path::new("a/c"));
        assert_eq!(normalize(Path::new("./a/./b")), Path::new("a/b"));
        assert_eq!(normalize(Path::new("/etc/../etc/passwd")), Path::new("/etc/passwd"));
        // `..` past the start is preserved (can't pop a non-name component).
        assert_eq!(normalize(Path::new("../x")), Path::new("../x"));
        // An absolute root is never climbed past.
        assert_eq!(normalize(Path::new("/../x")), Path::new("/x"));
        // Empty / all-dot paths normalize to ".".
        assert_eq!(normalize(Path::new("")), Path::new("."));
        assert_eq!(normalize(Path::new("a/..")), Path::new("."));
    }

    #[test]
    fn expand_resolves_tilde_and_dots() {
        std::env::set_var("HOME", "/home/pi");
        // The sneaky traversal resolves to the plain path it really means.
        assert_eq!(expand("~/../../etc/passwd"), PathBuf::from("/etc/passwd"));
        assert_eq!(expand("~/notes.md"), PathBuf::from("/home/pi/notes.md"));
        assert_eq!(expand("~"), PathBuf::from("/home/pi"));
    }
}
