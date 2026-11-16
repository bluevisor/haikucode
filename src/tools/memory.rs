//! The todo checklist and remember/recall memory notes.

use crate::config::{config_dir, memory_path};
use anyhow::Result;
use std::path::Path;

// --------------------------------------------------------------- todo -------

/// Format the model's plan as a checklist. The returned string is both the
/// tool result (so the model sees the current plan) and the transcript view.
pub fn todo(items: &serde_json::Value) -> String {
    let Some(arr) = items.as_array() else {
        return "ERROR: items must be an array of {text, status}".into();
    };
    if arr.is_empty() {
        return "(plan cleared)".into();
    }
    let mut out = String::new();
    for it in arr {
        let text = it.get("text").and_then(|v| v.as_str()).unwrap_or("");
        let mark = match it.get("status").and_then(|v| v.as_str()).unwrap_or("pending") {
            "completed" => "[x]",
            "in_progress" => "[>]",
            _ => "[ ]",
        };
        out.push_str(&format!("{mark} {text}\n"));
    }
    out.trim_end().to_string()
}

// ------------------------------------------------------------- memory -------
//
// memory.md holds one note per line: `- note` is global (a fact about the user,
// true in every project), `- [/abs/dir] note` belongs to that directory and is
// only shown in sessions there or below. Notes used to be global only, so a
// demo's "haikucode project: … at ./index.html" was injected everywhere and the
// model went looking for a `haikucode/` folder that never existed.

/// The line `remember` appends: tagged with `scope` unless the note is global.
fn memory_line(note: &str, scope: Option<&Path>) -> String {
    match scope {
        Some(dir) => format!("- [{}] {note}", dir.display()),
        None => format!("- {note}"),
    }
}

/// The notes visible from `cwd`, untagged: every global note plus the ones
/// scoped to `cwd` or a directory above it.
fn visible_notes(content: &str, cwd: &Path) -> Vec<String> {
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let Some(rest) = line.strip_prefix("- [") else {
                return Some(line.to_string());
            };
            let (dir, note) = rest.split_once("] ")?;
            cwd.starts_with(dir).then(|| format!("- {note}"))
        })
        .collect()
}

fn notes_here() -> Option<Vec<String>> {
    let content = std::fs::read_to_string(memory_path()).ok()?;
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let notes = visible_notes(&content, &cwd);
    (!notes.is_empty()).then_some(notes)
}

pub fn remember(note: &str, global: bool) -> String {
    if let Err(e) = std::fs::create_dir_all(config_dir()) {
        return format!("ERROR: {e}");
    }
    use std::io::Write;
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let line = memory_line(note, (!global).then_some(cwd.as_path()));
    let path = memory_path();
    // Advisory lockfile to prevent concurrent instances from interleaving
    // appends. create_new is atomic — only one process can own the lock at once.
    let lock = path.with_extension("md.lock");
    if let Err(e) = lock_acquire(&lock) {
        return format!("ERROR: lock busy — another haikucode is writing memory: {e}");
    }
    let res = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    let _ = std::fs::remove_file(&lock);
    match res {
        Ok(()) if global => format!("OK remembered (all projects): {note}"),
        Ok(()) => format!("OK remembered (for {}): {note}", cwd.display()),
        Err(e) => format!("ERROR: {e}"),
    }
}

/// Acquire a lockfile with exponential backoff (up to ~500ms). Returns Ok(())
/// or the last error from create_new.
fn lock_acquire(lock_path: &std::path::Path) -> std::io::Result<()> {
    let mut delay = std::time::Duration::from_millis(10);
    loop {
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(lock_path)
        {
            Ok(_) => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A lock left behind by a killed process (or a power cut — the
                // SD-card scenario this codebase designs for) would otherwise
                // break `remember` forever; an append takes milliseconds, so a
                // lock older than a few seconds is stale.
                let stale = std::fs::metadata(lock_path)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .map(|age| age > std::time::Duration::from_secs(10))
                    .unwrap_or(false);
                if stale {
                    let _ = std::fs::remove_file(lock_path);
                    continue;
                }
                if delay > std::time::Duration::from_millis(200) {
                    return Err(e);
                }
                std::thread::sleep(delay);
                delay *= 2;
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn recall(query: Option<&str>) -> String {
    let Some(notes) = notes_here() else {
        return "(no memories yet)".into();
    };
    match query {
        Some(q) if !q.is_empty() => {
            let ql = q.to_lowercase();
            let filtered: Vec<&str> = notes
                .iter()
                .map(String::as_str)
                .filter(|l| l.to_lowercase().contains(&ql))
                .collect();
            if filtered.is_empty() {
                format!("(no memories matching '{q}')")
            } else {
                filtered.join("\n")
            }
        }
        _ => notes.join("\n"),
    }
}

/// The memory this directory's sessions see (global notes + its own).
pub fn load_memory() -> Result<Option<String>> {
    Ok(notes_here().map(|n| n.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A demo directory's note showed up in every session everywhere, and the
    /// model went hunting for the "haikucode project" it described.
    #[test]
    fn project_notes_stay_in_their_directory() {
        let demo = Path::new("/Users/me/Developer/test");
        let content = [
            memory_line("prefers terse answers", None),
            memory_line("WebGL demo in ./index.html", Some(demo)),
            memory_line("Rust CLI", Some(Path::new("/Users/me/Developer/haikucode"))),
        ]
        .join("\n");

        assert_eq!(
            visible_notes(&content, demo),
            ["- prefers terse answers", "- WebGL demo in ./index.html"]
        );
        assert_eq!(
            visible_notes(&content, &demo.join("src")),
            ["- prefers terse answers", "- WebGL demo in ./index.html"],
            "a subdirectory sees its project's notes"
        );
        assert_eq!(
            visible_notes(&content, Path::new("/Users/me/Developer/testing")),
            ["- prefers terse answers"],
            "a sibling that shares a name prefix does not"
        );
    }

    #[test]
    fn todo_formats_checklist() {
        let items = serde_json::json!([
            {"text": "explore", "status": "completed"},
            {"text": "edit", "status": "in_progress"},
            {"text": "test"},
        ]);
        assert_eq!(todo(&items), "[x] explore\n[>] edit\n[ ] test");
        assert!(todo(&serde_json::json!("nope")).starts_with("ERROR"));
    }
}
