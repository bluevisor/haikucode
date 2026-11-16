//! Git helpers: auto-checkpoint commits, version bump, context/status, rewind.

use super::expand;
use crate::api::truncate;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

// --------------------------------------------------------------- git --------

/// True if `dir` is inside a git work tree.
pub fn in_git_repo(dir: &Path) -> bool {
    git_cmd(dir, &["rev-parse", "--is-inside-work-tree"], 10)
        .map(|s| s.success())
        .unwrap_or(false)
}

fn git_cmd(dir: &Path, args: &[&str], timeout_secs: u64) -> std::io::Result<std::process::ExitStatus> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    run_proc(&mut c, timeout_secs).map(|o| o.status)
}

/// Like git_status but captures stdout (used for `rev-parse`, `status`, etc.).
pub(super) fn git_output(dir: &Path, args: &[&str], timeout_secs: u64) -> std::io::Result<std::process::Output> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    run_proc(&mut c, timeout_secs)
}

/// Spawn a command, wait with timeout, kill the process group on timeout.
/// Returns the captured Output (stdout + stderr) and the exit status.
fn run_proc(cmd: &mut Command, timeout_secs: u64) -> std::io::Result<std::process::Output> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = child.wait_with_output();
        let _ = tx.send(r);
    });
    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(e)) => Err(e),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let _ = Command::new("kill").arg("-KILL").arg(format!("-{pid}")).status();
            let _ = rx.recv_timeout(Duration::from_millis(500));
            Err(std::io::Error::new(std::io::ErrorKind::TimedOut, format!("timed out after {timeout_secs}s")))
        }
        Err(e) => Err(std::io::Error::other(e.to_string())),
    }
}

/// Whether edit checkpoints also bump Cargo.toml's patch version. Opt-in with
/// `"auto_bump_version": true` in config.json or the project's
/// `.haikucode/settings(.local).json` (the most specific layer wins): in someone
/// else's crate a version bump per edit is noise, not a checkpoint.
fn auto_bump_version() -> bool {
    crate::config::settings_layers()
        .iter()
        .rev()
        .find_map(|(_, v)| v.get("auto_bump_version").and_then(|b| b.as_bool()))
        .unwrap_or(false)
}

/// Bump the `[package]` patch version in `dir/Cargo.toml`. Returns the new
/// version string, or None if there's no Cargo.toml or no `[package]` version
/// to bump (a workspace root, `version.workspace = true`).
pub fn bump_cargo_version(dir: &Path) -> Option<String> {
    let path = dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&path).ok()?;
    let trailing_nl = text.ends_with('\n');
    // Match `version = "x.y.z"` and bump z. (?m) enables multi-line mode so ^
    // matches at the start of any line, not just the start of the string.
    let re = regex::Regex::new(r#"(?m)^(\s*version\s*=\s*")(\d+)\.(\d+)\.(\d+)(")"#).ok()?;
    let mut bumped = String::new();
    let mut new_version: Option<String> = None;
    let mut first = true;
    // Only the [package] table's own version: a `[dependencies.foo]` table
    // has a `version = "x.y.z"` line of exactly the same shape.
    let mut in_package = false;
    for line in text.lines() {
        if !first {
            bumped.push('\n');
        }
        first = false;
        let t = line.trim_start();
        if t.starts_with('[') {
            in_package = t.trim_end().trim_end_matches(|c: char| c != ']') == "[package]";
        }
        if new_version.is_none() && in_package {
            if let Some(caps) = re.captures(line) {
                let z: u64 = caps[4].parse().ok()?;
                let v = format!("{}.{}.{}", &caps[2], &caps[3], z + 1);
                bumped.push_str(&format!("{}{}{}", &caps[1], v, &caps[5]));
                new_version = Some(v);
                continue;
            }
        }
        bumped.push_str(line);
    }
    let new_version = new_version?;
    if trailing_nl {
        bumped.push('\n');
    }
    std::fs::write(&path, &bumped).ok()?;
    Some(new_version)
}

/// Commit the given paths to the repo at `dir` as an edit checkpoint. With
/// `auto_bump_version` on, also bumps Cargo.toml's patch version into it.
/// Returns a short note like ` [committed a1b2c3d]` for the tool result, or ""
/// when there was nothing to commit / not a repo. Best-effort: never surfaces an
/// error.
pub fn git_autocommit(dir: &Path, paths: &[String], message: &str) -> String {
    git_autocommit_with(dir, paths, message, auto_bump_version())
}

/// `git_autocommit` with the version-bump choice made by the caller (tests
/// pin it, so they don't depend on the settings of the repo they run in).
fn git_autocommit_with(dir: &Path, paths: &[String], message: &str, bump: bool) -> String {
    if paths.is_empty() || !in_git_repo(dir) {
        return String::new();
    }
    // Tools expand `~` before touching the filesystem; git does not, so the
    // model's raw path strings must be expanded the same way here.
    let paths: Vec<PathBuf> = paths.iter().map(|p| expand(p)).collect();

    // Stage the edited files first, so we can check if there's anything to commit.
    let mut add = Command::new("git");
    add.arg("-C").arg(dir).arg("add").arg("--");
    for p in &paths {
        add.arg(p);
    }
    if run_proc(&mut add, 10).map(|o| !o.status.success()).unwrap_or(true) {
        return String::new();
    }

    // Only bump Cargo.toml when the edited files actually changed. Scope the
    // check to those files: anything the user had staged themselves is not
    // ours to judge (or, below, to commit).
    let staged = {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir).args(["diff", "--cached", "--quiet", "--"]);
        for p in &paths {
            c.arg(p);
        }
        c.stdout(Stdio::null()).stderr(Stdio::null());
        run_proc(&mut c, 10)
            .map(|o| !o.status.success()) // --quiet exits 1 when there ARE differences
            .unwrap_or(false)
    };
    if !staged {
        return String::new(); // nothing changed
    }

    // Bump Cargo.toml version if opted in, and add it to the commit.
    let cargo_toml = dir.join("Cargo.toml");
    let version_bumped = if cargo_toml.exists() && bump {
        bump_cargo_version(dir)
    } else {
        None
    };
    if version_bumped.is_some() {
        let mut add_cargo = Command::new("git");
        add_cargo.arg("-C").arg(dir).arg("add").arg("--").arg(&cargo_toml);
        run_proc(&mut add_cargo, 10).ok();
    }

    // A fresh Pi may have no user.name/email; retry with a fallback identity
    // only if the first attempt fails (so configured identity is preserved).
    // Capture stderr so we can surface merge-conflict and other commit failures.
    let run_commit = |ident: bool| -> (bool, String) {
        let mut c = Command::new("git");
        c.arg("-C").arg(dir);
        if ident {
            c.args(["-c", "user.name=haikucode", "-c", "user.email=haikucode@localhost"]);
        }
        // Commit only the edited files (+ the Cargo.toml bump): with explicit
        // paths git ignores whatever else the user has staged, so a checkpoint
        // never sweeps their half-prepared commit into a "haikucode: edit" one.
        c.args(["commit", "--no-verify", "-m", message, "--"]);
        for p in &paths {
            c.arg(p);
        }
        if version_bumped.is_some() {
            c.arg(&cargo_toml);
        }
        run_proc(&mut c, 30).map(|o| {
            (o.status.success(), String::from_utf8_lossy(&o.stderr).trim().to_string())
        }).unwrap_or_default()
    };
    let (ok, err) = run_commit(false);
    let committed = ok || {
        let (ok2, err2) = run_commit(true);
        if !ok2 && err2.is_empty() {
            return if err.is_empty() {
                String::new()
            } else {
                format!(" [commit skipped: {err}]")
            };
        }
        ok2
    };
    if !committed {
        return if err.is_empty() {
            String::new()
        } else {
            format!(" [commit skipped: {err}]")
        };
    }
    let hash = git_output(dir, &["rev-parse", "--short", "HEAD"], 10)
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut note = if hash.is_empty() {
        " [committed]".to_string()
    } else {
        format!(" [committed {hash}]")
    };
    if let Some(v) = version_bumped {
        note.push_str(&format!(" version → {v}"));
    }
    note
}

/// Recent git history + working-tree status for `dir`, as a startup context
/// clue. None outside a repo.
pub fn git_context(dir: &Path) -> Option<String> {
    if !in_git_repo(dir) {
        return None;
    }
    let run = |args: &[&str]| {
        git_output(dir, args, 10)
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let branch = run(&["rev-parse", "--abbrev-ref", "HEAD"]);
    let log = run(&["log", "--oneline", "-15"]);
    let status = run(&["status", "--short"]);
    let mut out = String::new();
    if !branch.is_empty() {
        out.push_str(&format!("branch: {branch}\n"));
    }
    if !log.is_empty() {
        out.push_str(&format!("recent commits:\n{log}\n"));
    }
    if !status.is_empty() {
        out.push_str(&format!("uncommitted changes:\n{}\n", truncate(&status, 2000)));
    }
    (!out.trim().is_empty()).then(|| out.trim().to_string())
}

/// Branch + clean/dirty state for the banner, e.g. `main · 3 changed` or
/// `main · clean`. `sep` joins them (mode-aware). None outside a repo.
pub fn git_status_line(dir: &Path, sep: &str) -> Option<String> {
    if !in_git_repo(dir) {
        return None;
    }
    let branch = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())?;
    let changed = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
        .unwrap_or(0);
    let state = if changed == 0 {
        "clean".to_string()
    } else {
        format!("{changed} changed")
    };
    Some(format!("{branch}{sep}{state}"))
}

/// Current branch name and whether the working tree is dirty, for the UI title.
/// `None` when `dir` is not inside a git repo.
/// Full SHA of HEAD, or None outside a repo / before the first commit.
pub fn git_rev(dir: &Path) -> Option<String> {
    let out = git_output(dir, &["rev-parse", "--verify", "HEAD"], 10).ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `git <args>` with captured stdout, for the TUI's /diff and /undo. Returns
/// Err(stderr) on a non-zero exit.
pub fn git_run(dir: &Path, args: &[&str], timeout_secs: u64) -> std::result::Result<String, String> {
    match git_output(dir, args, timeout_secs) {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// Undo haikucode's own checkpoint commits made after `base` (and `/undo`'s
/// reverts of them), newest first, as one new commit. Anything else committed
/// in between is left alone. Returns how many commits were reverted (0 when
/// there was nothing to undo). Refuses with staged changes, since the commit
/// would sweep them in; on a conflict everything is put back as it was.
pub fn revert_checkpoints_since(dir: &Path, base: &str) -> std::result::Result<usize, String> {
    let log = git_run(dir, &["log", "--format=%H %s", &format!("{base}..HEAD")], 10)?;
    let shas: Vec<&str> = log
        .lines()
        .filter_map(|l| l.split_once(' '))
        .filter(|(_, subject)| subject.starts_with("haikucode:") || subject.starts_with("Revert \"haikucode:"))
        .map(|(sha, _)| sha)
        .collect();
    if shas.is_empty() {
        return Ok(0);
    }
    let staged_clean = |d: &Path| {
        git_output(d, &["diff", "--cached", "--quiet"], 10).map(|o| o.status.success()).unwrap_or(false)
    };
    if !staged_clean(dir) {
        return Err("you have staged changes; commit or unstage them first".into());
    }
    let mut args = vec!["revert", "--no-commit"];
    args.extend(shas.iter().copied());
    if let Err(e) = git_run(dir, &args, 60) {
        let _ = git_run(dir, &["revert", "--abort"], 10);
        return Err(e);
    }
    if staged_clean(dir) {
        // The checkpoints cancel out (e.g. already undone): nothing to commit.
        let _ = git_run(dir, &["revert", "--quit"], 10);
        return Ok(0);
    }
    let msg = format!("haikucode: rewind (reverted {} checkpoint(s))", shas.len());
    let commit = |ident: bool| {
        let mut a: Vec<&str> = Vec::new();
        if ident {
            a.extend(["-c", "user.name=haikucode", "-c", "user.email=haikucode@localhost"]);
        }
        a.extend(["commit", "--no-verify", "-m", &msg]);
        git_run(dir, &a, 30)
    };
    match commit(false).or_else(|_| commit(true)) {
        Ok(_) => Ok(shas.len()),
        Err(e) => {
            let _ = git_run(dir, &["revert", "--abort"], 10);
            Err(e)
        }
    }
}

pub fn git_head(dir: &Path) -> Option<(String, bool)> {
    if !in_git_repo(dir) {
        return None;
    }
    let branch = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())?;
    let dirty = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    Some((branch, dirty))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_autocommit_leaves_the_users_own_staged_changes_alone() {
        let dir = std::env::temp_dir().join(format!("haikucode-ac-staged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        git(&["init", "-q"]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-q", "-m", "init"]);
        // The user has staged their own work-in-progress...
        std::fs::write(dir.join("mine.txt"), "wip").unwrap();
        git(&["add", "mine.txt"]);
        // ...and haikucode edits a different file.
        let edited = dir.join("edited.txt");
        std::fs::write(&edited, "hello").unwrap();
        let note = git_autocommit(&dir, &[edited.to_string_lossy().into_owned()], "haikucode: edit edited.txt");
        assert!(note.contains("[committed"), "{note}");
        let files = git(&["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files.trim(), "edited.txt", "only haikucode's file is in the checkpoint");
        assert_eq!(git(&["diff", "--cached", "--name-only"]), "mine.txt", "the user's staging survives");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_autocommit_checkpoints_edit() {
        let dir = std::env::temp_dir().join(format!("haikucode_git_test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap()
        };
        if !git(&["init", "-q"]).status.success() {
            return; // no git available; skip
        }
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        assert!(in_git_repo(&dir));
        let f = dir.join("x.txt");
        std::fs::write(&f, "hi\n").unwrap();
        let note = git_autocommit(&dir, &[f.to_string_lossy().into()], "haikucode: write x.txt");
        assert!(note.contains("committed"), "got: {note:?}");
        // A second call with no change commits nothing.
        let note2 = git_autocommit(&dir, &[f.to_string_lossy().into()], "haikucode: noop");
        assert!(note2.is_empty(), "got: {note2:?}");
        let log = git(&["log", "--oneline"]);
        assert!(String::from_utf8_lossy(&log.stdout).contains("write x.txt"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bump_cargo_version_increments_patch() {
        let dir = std::env::temp_dir().join(format!("haikucode_bump_test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("Cargo.toml");
        std::fs::write(&toml, "[package]\nname = \"t\"\nversion = \"3.14.159\"\nedition = \"2021\"\n").unwrap();
        let new = bump_cargo_version(&dir).expect("bump should succeed");
        assert_eq!(new, "3.14.160");
        let content = std::fs::read_to_string(&toml).unwrap();
        assert!(content.contains("version = \"3.14.160\""));
        // Bump again.
        let new2 = bump_cargo_version(&dir).expect("second bump");
        assert_eq!(new2, "3.14.161");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rewind_reverts_only_haikucodes_checkpoints() {
        let dir = std::env::temp_dir().join(format!("haikucode-rewind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        let commit = |msg: &str| git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qam", msg]);
        git(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "base\n").unwrap();
        std::fs::write(dir.join("b.txt"), "base\n").unwrap();
        git(&["add", "."]);
        commit("init");
        let base = git(&["rev-parse", "HEAD"]);
        // Two haikucode checkpoints with the user's own commit in between.
        std::fs::write(dir.join("a.txt"), "haikucode 1\n").unwrap();
        commit("haikucode: edit a.txt");
        std::fs::write(dir.join("b.txt"), "mine\n").unwrap();
        commit("user: my change");
        std::fs::write(dir.join("a.txt"), "haikucode 2\n").unwrap();
        commit("haikucode: edit a.txt");

        assert_eq!(revert_checkpoints_since(&dir, &base), Ok(2));
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).unwrap();
        assert_eq!(read("a.txt"), "base\n", "haikucode's edits are undone");
        assert_eq!(read("b.txt"), "mine\n", "the user's commit is kept");
        assert!(git(&["log", "-1", "--format=%s"]).starts_with("haikucode: rewind"));
        // Rewinding again to the same point has nothing left to undo.
        assert_eq!(revert_checkpoints_since(&dir, &git(&["rev-parse", "HEAD"])), Ok(0));

        // Staged work of the user's own blocks a rewind instead of being swept in.
        std::fs::write(dir.join("a.txt"), "haikucode 3\n").unwrap();
        commit("haikucode: edit a.txt");
        std::fs::write(dir.join("b.txt"), "staged\n").unwrap();
        git(&["add", "b.txt"]);
        assert!(revert_checkpoints_since(&dir, &base).is_err());
        assert_eq!(read("a.txt"), "haikucode 3\n", "nothing changed on refusal");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bump_cargo_version_only_touches_the_package_table() {
        let dir = std::env::temp_dir().join(format!("haikucode_bump_tables-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("Cargo.toml");
        // A dependency table ahead of [package] has the same `version = ` shape.
        std::fs::write(
            &toml,
            "[dependencies.serde]\nversion = \"1.0.200\"\n\n[package] # the crate\nname = \"t\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(bump_cargo_version(&dir).as_deref(), Some("0.1.1"));
        let content = std::fs::read_to_string(&toml).unwrap();
        assert!(content.contains("version = \"1.0.200\""), "dependency untouched: {content}");
        assert!(content.contains("version = \"0.1.1\""));
        // A workspace root has no [package] version to bump.
        std::fs::write(&toml, "[workspace.package]\nversion = \"2.0.0\"\n").unwrap();
        assert!(bump_cargo_version(&dir).is_none());
        assert!(std::fs::read_to_string(&toml).unwrap().contains("\"2.0.0\""));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn bump_cargo_version_no_cargo_toml() {
        let dir = std::env::temp_dir().join(format!("haikucode_bump_nocargo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(bump_cargo_version(&dir).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn git_autocommit_bumps_version() {
        let dir = std::env::temp_dir().join(format!("haikucode_git_bump_test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap()
        };
        if !git(&["init", "-q"]).status.success() {
            std::fs::remove_dir_all(&dir).ok();
            return; // no git available; skip
        }
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        let toml = dir.join("Cargo.toml");
        std::fs::write(&toml, "[package]\nname = \"t\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
        // Initial commit so the repo has a HEAD.
        git(&["add", "Cargo.toml"]);
        git(&["commit", "-q", "-m", "init"]);
        // Now edit a file and autocommit.
        let f = dir.join("x.txt");
        std::fs::write(&f, "hi\n").unwrap();
        let note = git_autocommit_with(&dir, &[f.to_string_lossy().into()], "haikucode: write x.txt", true);
        assert!(note.contains("committed"), "got: {note:?}");
        assert!(note.contains("version → 0.1.1"), "got: {note:?}");
        let content = std::fs::read_to_string(&toml).unwrap();
        assert!(content.contains("version = \"0.1.1\""));
        std::fs::remove_dir_all(&dir).ok();
    }
}
