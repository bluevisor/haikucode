//! bash: one-shot commands (in the sandbox) and background jobs.

use crate::api::{truncate, MAX_TOOL_OUTPUT};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ----------------------------------------------------------------- bash -----

/// `sh -c command`, inside the OS sandbox when one is configured (see
/// `sandbox`). Returns the command, what must outlive its spawn, the active
/// sandbox (to explain failures), and a one-time "unavailable" warning.
fn shell(command: &str, cwd: &Path) -> (Command, Option<crate::sandbox::Guard>, Option<crate::sandbox::Sandbox>, Option<String>) {
    match crate::sandbox::Sandbox::current(cwd) {
        Some(sb) => {
            let (cmd, guard, warn) = sb.shell(command);
            (cmd, Some(guard), warn.is_none().then_some(sb), warn)
        }
        None => {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(command);
            (cmd, None, None, None)
        }
    }
}

pub fn bash(command: &str, timeout: u64, cwd: &Path, cancel: &AtomicBool) -> String {
    // `_guard` keeps the Linux sandbox ruleset open until the child is spawned.
    let (mut cmd, _guard, sandbox, warning) = shell(command, cwd);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group so we can kill the whole tree on timeout or interrupt.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("ERROR: failed to spawn shell: {e}"),
    };
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    // Poll on a short interval so an Esc (cancel) interrupts a long-running
    // command instead of blocking the worker until the timeout elapses.
    let deadline = Instant::now() + Duration::from_secs(timeout.max(1));
    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(output)) => {
                let mut out = String::new();
                out.push_str(&String::from_utf8_lossy(&output.stdout));
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.trim().is_empty() {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str("[stderr]\n");
                    out.push_str(&stderr);
                }
                let code = output.status.code().unwrap_or(-1);
                out = out.trim_end().to_string();
                out.push_str(&format!("\n[exit {code}]"));
                if let Some(note) = sandbox.as_ref().and_then(|sb| sb.explain(&out, code)) {
                    out.push('\n');
                    out.push_str(&note);
                }
                if let Some(w) = &warning {
                    out = format!("{w}\n{out}");
                }
                return truncate(out.trim(), MAX_TOOL_OUTPUT);
            }
            Ok(Err(e)) => return format!("ERROR: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // Negative pid = kill whole process group (sh + grandchildren).
                // Routed through `sh -c` to match bash_kill (a bare
                // `Command::new("kill")` mis-targets the group in some sandboxes).
                let kill = |pid: u32| {
                    let _ = Command::new("sh").arg("-c").arg(format!("kill -KILL -{pid}")).status();
                };
                if cancel.load(Ordering::Relaxed) {
                    kill(pid);
                    let _ = rx.recv_timeout(Duration::from_millis(500));
                    return "ERROR: interrupted by user".to_string();
                }
                if Instant::now() >= deadline {
                    kill(pid);
                    // Give the waiter thread a moment to reap the killed child.
                    let _ = rx.recv_timeout(Duration::from_millis(500));
                    return format!("ERROR: command timed out after {timeout}s");
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return "ERROR: shell waiter disconnected".to_string();
            }
        }
    }
}

// -------------------------------------------------------- background jobs ---

/// A shell command running detached from the agent loop. Output accumulates
/// in `buf`; `read_to` tracks how much bash_output has already returned.
struct Job {
    pid: u32,
    command: String,
    buf: Arc<Mutex<String>>,
    exit: Arc<Mutex<Option<i32>>>,
    read_to: usize,
}

const JOB_BUF_MAX: usize = 1_000_000;

fn jobs() -> &'static Mutex<HashMap<u32, Job>> {
    static JOBS: OnceLock<Mutex<HashMap<u32, Job>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(HashMap::new()))
}

const MAX_BG_JOBS: usize = 64;

pub fn bash_background(command: &str, cwd: &Path) -> String {
    static NEXT_ID: AtomicU32 = AtomicU32::new(1);
    // `_guard` keeps the Linux sandbox ruleset open until the child is spawned.
    let (mut cmd, _guard, sandbox, warning) = shell(command, cwd);
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Own process group, so bash_kill can take out the whole pipeline.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return format!("ERROR: failed to spawn shell: {e}"),
    };
    let pid = child.id();
    let buf = Arc::new(Mutex::new(String::new()));
    let exit = Arc::new(Mutex::new(None));
    let pipes: [Option<Box<dyn Read + Send>>; 2] = [
        child.stdout.take().map(|p| Box::new(p) as _),
        child.stderr.take().map(|p| Box::new(p) as _),
    ];
    for pipe in pipes.into_iter().flatten() {
        let buf = buf.clone();
        std::thread::spawn(move || {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(pipe);
            let mut line = String::new();
            while reader.read_line(&mut line).map(|n| n > 0).unwrap_or(false) {
                let mut b = buf.lock().unwrap();
                if b.len() < JOB_BUF_MAX {
                    b.push_str(&line);
                }
                line.clear();
            }
        });
    }
    {
        let exit = exit.clone();
        std::thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
            *exit.lock().unwrap() = Some(code);
        });
    }
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut map = jobs().lock().unwrap();
    // Evict finished jobs when the table gets large, to avoid unbounded memory.
    if map.len() >= MAX_BG_JOBS {
        map.retain(|_, j| j.exit.lock().unwrap().is_none());
    }
    map.insert(
        id,
        Job { pid, command: command.to_string(), buf, exit, read_to: 0 },
    );
    drop(map);
    let mut msg = format!("Started background job {id} (pid {pid}). Poll with bash_output, stop with bash_kill.");
    if let Some(sb) = &sandbox {
        msg.push_str(&format!(" It runs in the {}.", sb.describe()));
    }
    if let Some(w) = warning {
        msg = format!("{w}\n{msg}");
    }
    msg
}

/// Output produced since the last bash_output call, plus run status.
pub fn bash_output(id: u64) -> String {
    if id > u32::MAX as u64 {
        return format!("ERROR: invalid job id {id} (max {})", u32::MAX);
    }
    let mut map = jobs().lock().unwrap();
    let Some(job) = map.get_mut(&(id as u32)) else {
        return format!("ERROR: no background job {id}");
    };
    let buf = job.buf.lock().unwrap();
    let new = buf[job.read_to..].to_string();
    job.read_to = buf.len();
    drop(buf);
    let status = match *job.exit.lock().unwrap() {
        Some(code) => format!("[exited {code}]"),
        None => "[running]".to_string(),
    };
    let header = format!("job {id}: {}", truncate(&job.command, 100));
    let body = if new.is_empty() { "(no new output)".to_string() } else { truncate(&new, MAX_TOOL_OUTPUT) };
    format!("{header}\n{body}\n{status}")
}

pub fn bash_kill(id: u64) -> String {
    if id > u32::MAX as u64 {
        return format!("ERROR: invalid job id {id} (max {})", u32::MAX);
    }
    let map = jobs().lock().unwrap();
    let Some(job) = map.get(&(id as u32)) else {
        return format!("ERROR: no background job {id}");
    };
    if job.exit.lock().unwrap().is_some() {
        return format!("job {id} has already exited");
    }
    let pid = job.pid;
    drop(map);
    // Negative pid = the whole process group (sh + its children).
    let _ = Command::new("sh").arg("-c").arg(format!("kill -KILL -{pid}")).status();
    format!("killed job {id} (process group {pid})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_job_lifecycle() {
        let cwd = std::env::temp_dir();
        let start = bash_background("echo hello; sleep 30", &cwd);
        assert!(start.contains("Started background job"), "got: {start}");
        let id: u64 = start
            .split_whitespace()
            .nth(3)
            .and_then(|s| s.parse().ok())
            .expect("job id in start message");
        std::thread::sleep(Duration::from_millis(300));
        let out = bash_output(id);
        assert!(out.contains("hello"), "got: {out}");
        assert!(out.contains("[running]"), "got: {out}");
        // Second poll returns only new output.
        let out2 = bash_output(id);
        assert!(out2.contains("(no new output)"), "got: {out2}");
        let killed = bash_kill(id);
        assert!(killed.contains("killed job"), "got: {killed}");
        std::thread::sleep(Duration::from_millis(300));
        assert!(bash_output(id).contains("[exited"), "job should be dead");
        assert!(bash_output(999).starts_with("ERROR"));
    }

    #[test]
    fn bash_cancel_interrupts_long_command() {
        // A long sleep must be killed within ~poll interval of the cancel flag
        // flipping (the Esc-interrupt path), not run to completion.
        let cwd = std::env::temp_dir();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            c2.store(true, Ordering::Relaxed);
        });
        let start = Instant::now();
        let out = bash("sleep 30", 30, &cwd, &cancel);
        let elapsed = start.elapsed();
        assert!(out.contains("interrupted by user"), "got: {out}");
        assert!(elapsed < Duration::from_secs(3), "took too long: {elapsed:?}");
    }

    #[test]
    fn bash_runs_without_cancel() {
        let cwd = std::env::temp_dir();
        let cancel = Arc::new(AtomicBool::new(false));
        let out = bash("echo hi", 10, &cwd, &cancel);
        assert!(out.contains("hi"), "got: {out}");
        assert!(out.contains("[exit 0]"), "got: {out}");
    }
}
