//! Search tools: grep and glob, honoring .gitignore inside a repo.

use super::expand;
use super::git::git_output;
use crate::api::{truncate, MAX_TOOL_OUTPUT};
use std::io::Read;
use std::path::{Path, PathBuf};

// ----------------------------------------------------------- grep/glob ------

const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__", "dist", "build"];

pub fn grep(pattern: &str, path: &str, ignore_case: bool) -> String {
    let re = match regex::RegexBuilder::new(pattern).case_insensitive(ignore_case).build() {
        Ok(r) => r,
        Err(e) => return format!("ERROR: bad regex: {e}"),
    };
    let root = expand(if path.is_empty() { "." } else { path });
    let mut files: Vec<PathBuf> = Vec::new();
    if root.is_file() {
        files.push(root);
    } else if let Some(listed) = git_visible_files(&root) {
        // Inside a repo, git's own list honors .gitignore (no node_modules,
        // target, generated files); the same hidden/build-dir skips and cap
        // as the walker apply on top.
        let skipped = |f: &&PathBuf| {
            f.strip_prefix(&root).unwrap_or(f).parent().is_some_and(|d| {
                d.components().any(|c| {
                    let n = c.as_os_str().to_string_lossy();
                    SKIP_DIRS.contains(&n.as_ref()) || n.starts_with('.') && n.len() > 1
                })
            })
        };
        files.extend(listed.iter().filter(|f| !skipped(f)).filter(|f| f.is_file()).take(5000).cloned());
    } else {
        collect_files(&root, &mut files, 0);
    }
    let mut out = String::new();
    let mut hits = 0usize;
    'outer: for f in files {
        let Ok(fh) = std::fs::File::open(&f) else { continue };
        let mut buf = Vec::new();
        if fh.take(1_000_000).read_to_end(&mut buf).is_err() {
            continue;
        }
        if buf.iter().take(1024).any(|&b| b == 0) {
            continue; // binary
        }
        let text = String::from_utf8_lossy(&buf);
        let rel = f.display().to_string();
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                out.push_str(&format!("{}:{}: {}\n", rel, i + 1, line.trim_end()));
                hits += 1;
                if hits >= 200 {
                    out.push_str("... (200+ matches, stopping)\n");
                    break 'outer;
                }
            }
        }
    }
    if out.is_empty() {
        "(no matches)".into()
    } else {
        truncate(&out, MAX_TOOL_OUTPUT)
    }
}

/// The files git shows under `dir` — tracked, plus untracked ones .gitignore
/// doesn't exclude — joined onto `dir`. None outside a repo or without git.
fn git_visible_files(dir: &Path) -> Option<Vec<PathBuf>> {
    let out = git_output(dir, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"], 20).ok()?;
    if !out.status.success() {
        return None;
    }
    let mut files: Vec<PathBuf> = out
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| dir.join(String::from_utf8_lossy(s).as_ref()))
        .collect();
    files.dedup();
    Some(files)
}

/// Match a relative glob against git's file list (and the directories on it)
/// instead of walking the tree, so ignored trees are neither searched nor
/// returned. Same matching as `glob::glob`: `*` stops at `/`, `**` spans dirs.
fn glob_in_repo(pattern: &str, cwd: &Path, listed: &[PathBuf]) -> std::result::Result<Vec<String>, String> {
    let dotted = pattern.starts_with("./");
    let pat = glob::Pattern::new(pattern.trim_start_matches("./")).map_err(|e| format!("ERROR: bad glob: {e}"))?;
    let opts = glob::MatchOptions { case_sensitive: true, require_literal_separator: true, require_literal_leading_dot: false };
    let mut seen = std::collections::BTreeSet::new();
    for f in listed {
        let Ok(rel) = f.strip_prefix(cwd) else { continue };
        // The file itself and each directory above it are candidates.
        for p in rel.ancestors().filter(|p| !p.as_os_str().is_empty()) {
            seen.insert(p.to_path_buf());
        }
    }
    Ok(seen
        .into_iter()
        .filter(|p| pat.matches_path_with(p, opts) && cwd.join(p).exists())
        .take(500)
        .map(|p| if dotted { format!("./{}", p.display()) } else { p.display().to_string() })
        .collect())
}

/// Entries directly in `dir` that git ignores (dirs as `name/`), or None
/// outside a repo.
pub(super) fn git_ignored_children(dir: &Path) -> Option<std::collections::HashSet<String>> {
    let args = ["ls-files", "-z", "--others", "--ignored", "--exclude-standard", "--directory"];
    let out = git_output(dir, &args, 20).ok().filter(|o| o.status.success())?;
    Some(
        out.stdout
            .split(|&b| b == 0)
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .filter(|s| !s.is_empty() && !s.trim_end_matches('/').contains('/'))
            .collect(),
    )
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 12 || out.len() > 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            if SKIP_DIRS.contains(&name.as_str()) || name.starts_with('.') && name.len() > 1 {
                continue;
            }
            collect_files(&path, out, depth + 1);
        } else {
            out.push(path);
        }
    }
}

pub fn glob_search(pattern: &str) -> String {
    let mut matches: Vec<String> = Vec::new();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let relative = !pattern.starts_with(['/', '~']) && !pattern.split('/').any(|c| c == "..");
    if let Some(listed) = relative.then(|| git_visible_files(&cwd)).flatten() {
        match glob_in_repo(pattern, &cwd, &listed) {
            Ok(m) => matches = m,
            Err(e) => return e,
        }
    } else {
        match glob::glob(pattern) {
        Ok(paths) => {
            for entry in paths.flatten() {
                matches.push(entry.display().to_string());
                if matches.len() >= 500 {
                    break;
                }
            }
        }
        Err(e) => return format!("ERROR: bad glob: {e}"),
        }
    }
    matches.sort();
    if matches.is_empty() {
        "(no files match)".into()
    } else {
        truncate(&matches.join("\n"), MAX_TOOL_OUTPUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::list_files;
    use std::process::Command;

    #[test]
    fn search_tools_honor_gitignore_in_a_repo() {
        let dir = std::env::temp_dir().join(format!("haikucode-gi-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["src", "target/debug", "vendor"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
        }
        let git = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        };
        git(&["init", "-q"]);
        std::fs::write(dir.join(".gitignore"), "target/\n*.log\nvendor/\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(dir.join("new.rs"), "// needle, untracked but not ignored\n").unwrap();
        std::fs::write(dir.join("target/debug/gen.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(dir.join("vendor/dep.rs"), "fn needle() {}\n").unwrap();
        std::fs::write(dir.join("run.log"), "needle\n").unwrap();
        git(&["add", "src/lib.rs", ".gitignore"]);

        let g = grep("needle", &dir.to_string_lossy(), false);
        assert!(g.contains("src/lib.rs") && g.contains("new.rs"), "{g}");
        assert!(!g.contains("target/") && !g.contains("vendor/") && !g.contains("run.log"), "ignored files searched: {g}");

        let listed = git_visible_files(&dir).unwrap();
        let rs = glob_in_repo("**/*.rs", &dir, &listed).unwrap();
        assert_eq!(rs, ["new.rs", "src/lib.rs"]);
        assert_eq!(glob_in_repo("./src/*", &dir, &listed).unwrap(), ["./src/lib.rs"]);
        assert_eq!(glob_in_repo("s*", &dir, &listed).unwrap(), ["src"], "directories match too");
        assert!(glob_in_repo("[", &dir, &listed).unwrap_err().contains("bad glob"));

        let ls = list_files(&dir.to_string_lossy());
        assert!(ls.contains("src/") && ls.contains("new.rs"), "{ls}");
        assert!(!ls.lines().any(|l| l == "target/" || l == "vendor/" || l == "run.log"), "{ls}");
        let last = ls.lines().last().unwrap();
        assert!(last.starts_with("(3 git-ignored not shown:") && last.contains("target/"), "{ls}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
