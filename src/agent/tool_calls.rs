//! Tool-call execution: dispatch, parallel read-only batches, hooks,
//! permission rules and approvals.

use super::{preview, ApprovalResponse, UiEvent, Worker, PERM_AUTO, PERM_PLAN};
use crate::api::{self, AccumCall, Message};
use crate::hooks::Outcome;
use crate::policy::Decision;
use crate::tools;
use serde_json::Value;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};

impl Worker {
    pub(super) fn handle_call(&mut self, c: AccumCall, idx: usize) {
        let call = Call::new(&c, idx);
        let _ = self.ui.send(UiEvent::ToolStart { name: call.name.clone(), summary: call.summary.clone() });
        let result = match self.settle_early(&call) {
            Some(e) => {
                self.result_event(&e);
                e
            }
            // settle_early returns Some for every unparsable call.
            None => self.run_tool(&call.name, call.args.as_ref().unwrap_or(&Value::Null)),
        };
        self.messages.push(Message::tool(call.id, result));
    }

    /// Run consecutive read-only calls side by side. Everything stateful (the
    /// repeat guard, hooks, deny rules, transcript events, history) stays
    /// sequential and in call order; only the tools' own work overlaps. The
    /// transcript gets each call's start/result pair once the batch is done.
    pub(super) fn handle_parallel(&mut self, batch: &[AccumCall], base: usize) {
        let _ = self.ui.send(UiEvent::Progress(format!(
            "running {} read-only calls in parallel…",
            batch.len()
        )));
        // A call settled up front (bad JSON, repeat, hook veto, deny rule) has
        // its result already; the rest are run below.
        let mut calls: Vec<(Call, Option<String>)> = Vec::with_capacity(batch.len());
        for (k, c) in batch.iter().enumerate() {
            let call = Call::new(c, base + k);
            let early = self.settle_early(&call).or_else(|| {
                let args = call.args.as_ref().unwrap_or(&Value::Null);
                self.pre_tool(&call.name, args).err()
            });
            calls.push((call, early));
        }
        let http = &self.http;
        let mut ran: Vec<Option<String>> = vec![None; calls.len()];
        // Bounded fan-out: a Pi Zero has one core and 512 MB.
        let todo: Vec<usize> = (0..calls.len()).filter(|&k| calls[k].1.is_none()).collect();
        for chunk in todo.chunks(8) {
            let out: Vec<(usize, String)> = std::thread::scope(|sc| {
                let handles: Vec<_> = chunk
                    .iter()
                    .map(|&k| {
                        let (call, _) = &calls[k];
                        let args = call.args.as_ref().unwrap_or(&Value::Null);
                        (k, sc.spawn(move || run_readonly(http, &call.name, args)))
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|(k, h)| (k, h.join().unwrap_or_else(|_| "ERROR: tool panicked".into())))
                    .collect()
            });
            for (k, r) in out {
                ran[k] = Some(r);
            }
        }
        for ((call, early), r) in calls.into_iter().zip(ran) {
            let _ = self.ui.send(UiEvent::ToolStart { name: call.name.clone(), summary: call.summary.clone() });
            let result = match (early, r) {
                (Some(e), _) => e,
                (None, Some(r)) => {
                    let args = call.args.as_ref().unwrap_or(&Value::Null);
                    self.post_tool(&call.name, args, r)
                }
                (None, None) => "ERROR: tool did not run".to_string(),
            };
            self.result_event(&result);
            self.messages.push(Message::tool(call.id, result));
        }
    }

    /// The result for a call that must not run: arguments that aren't JSON, or
    /// the thrashing guard — the same call with the same arguments three times
    /// in a row can't produce a different result, so break the loop instead of
    /// burning tokens (Codex-style). None means go ahead.
    fn settle_early(&mut self, call: &Call) -> Option<String> {
        let name = &call.name;
        let sig = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            name.hash(&mut h);
            call.raw.hash(&mut h);
            h.finish()
        };
        let n = self.recent_calls.len();
        let repeated = n >= 2 && self.recent_calls[n - 1] == sig && self.recent_calls[n - 2] == sig;
        self.recent_calls.push(sig);
        if self.recent_calls.len() > 32 {
            self.recent_calls.remove(0);
        }
        if repeated && name != "bash_output" && name != "ask_user" {
            return Some(format!(
                "ERROR: `{name}` was just called twice with these exact arguments and the result \
                 will not change. Do not repeat it. Try a different approach (other arguments, \
                 another tool, or inspect why it failed), or explain the blocker to the user."
            ));
        }
        if call.args.is_none() {
            return Some(format!(
                "ERROR: could not parse tool arguments as JSON. Re-issue with valid JSON. Received: {}",
                api::truncate(&call.raw, 300)
            ));
        }
        None
    }

    /// What a permission rule's pattern is matched against for this call.
    fn policy_targets(name: &str, args: &Value) -> Vec<String> {
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string);
        match name {
            "bash" => s("command").into_iter().collect(),
            "read_file" | "write_file" | "edit_file" | "list_files" => s("path").into_iter().collect(),
            "multi_edit" => args
                .get("edits")
                .and_then(|e| e.as_array())
                .map(|a| a.iter().filter_map(|e| e.get("path").and_then(|p| p.as_str()).map(str::to_string)).collect())
                .unwrap_or_default(),
            "grep" => s("path").into_iter().collect(),
            "glob" => s("pattern").into_iter().collect(),
            "web_fetch" => s("url").into_iter().collect(),
            "web_search" => s("query").into_iter().collect(),
            "skill" => s("name").into_iter().collect(),
            _ => Vec::new(),
        }
    }

    fn run_tool(&mut self, name: &str, args: &Value) -> String {
        let decision = match self.pre_tool(name, args) {
            Ok(d) => d,
            Err(r) => {
                self.result_event(&r);
                return r;
            }
        };
        let targets = Self::policy_targets(name, args);
        let result = self.run_tool_inner(name, args, &decision, targets.first().map(String::as_str).unwrap_or(""));
        self.post_tool(name, args, result)
    }

    /// PreToolUse hooks (which may veto) and the permission rules. Err is the
    /// refusal to return as the call's result; deny rules are absolute, so
    /// they hold in YOLO mode too.
    fn pre_tool(&mut self, name: &str, args: &Value) -> Result<Decision, String> {
        if self.hooks.has("PreToolUse") {
            match self.hooks.run("PreToolUse", name, &serde_json::json!({ "tool_input": args })) {
                Outcome::Block(reason) => return Err(format!("DENIED by PreToolUse hook: {reason}")),
                Outcome::Continue { context } if !context.trim().is_empty() => {
                    let _ = self.ui.send(UiEvent::Notice(format!("hook: {}", crate::api::truncate(context.trim(), 300))));
                }
                _ => {}
            }
        }
        let targets = Self::policy_targets(name, args);
        let target_refs: Vec<&str> = targets.iter().map(String::as_str).collect();
        match self.policy.check(name, &target_refs) {
            Decision::Deny(rule) => Err(format!(
                "DENIED by permission rule `{rule}` (see /permissions). Do not retry this call."
            )),
            d => Ok(d),
        }
    }

    /// PostToolUse hooks can append feedback (e.g. a formatter's diagnostics).
    fn post_tool(&mut self, name: &str, args: &Value, result: String) -> String {
        if self.hooks.has("PostToolUse") {
            let payload = serde_json::json!({ "tool_input": args, "tool_response": crate::api::truncate(&result, 8000) });
            match self.hooks.run("PostToolUse", name, &payload) {
                Outcome::Block(reason) => {
                    return format!("{result}\n\n[PostToolUse hook feedback]\n{reason}");
                }
                Outcome::Continue { context } if !context.trim().is_empty() => {
                    return format!("{result}\n\n[PostToolUse hook]\n{}", context.trim());
                }
                _ => {}
            }
        }
        result
    }

    fn run_tool_inner(&mut self, name: &str, args: &Value, decision: &Decision, target: &str) -> String {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
        // An allow rule stands in for the user's "yes".
        let pre_approved = matches!(decision, Decision::Allow(_));
        // Plan mode: refuse mutating tools and ask the model to plan instead.
        if self.perm.load(Ordering::Relaxed) == PERM_PLAN
            && matches!(name, "bash" | "write_file" | "edit_file" | "multi_edit" | "bash_kill")
        {
            let r = "[plan mode] Not executed. haikucode is in read-only plan mode — \
                     describe the change you'd make; the user will switch off plan mode to apply it."
                .to_string();
            self.result_event(&r);
            return r;
        }
        match name {
            n if parallel_safe(n) => {
                let r = run_readonly(&self.http, n, args);
                self.result_event(&r);
                r
            }
            "remember" => {
                let global = args.get("global").and_then(|v| v.as_bool()).unwrap_or(false);
                let r = tools::remember(s("note"), global);
                self.result_event(&r);
                r
            }
            "view_image" => {
                let path = s("path");
                // Queueing an image for a text-only model would be dropped on
                // the wire while the model believes it saw it; say so instead.
                let r = if !api::takes_images(&self.cfg) {
                    format!(
                        "ERROR: the current model ({}) can't see images. Check the result another \
                         way (text output, logs, file contents) or ask the user to look.",
                        self.cfg.model
                    )
                } else {
                    match tools::image_data_uri(path) {
                    Ok(uri) => {
                        self.pending_images.push(uri);
                        format!("Loaded image {path} into context.")
                    }
                    Err(e) => e,
                    }
                };
                self.result_event(&r);
                r
            }
            "todo" => {
                let r = tools::todo(args.get("items").unwrap_or(&Value::Null));
                self.result_event(&r);
                r
            }
            "task" => {
                // No nested sub-agents: a sub-agent calling task would recurse.
                if self.quiet {
                    let r = "ERROR: a sub-agent cannot spawn another sub-agent.".to_string();
                    self.result_event(&r);
                    return r;
                }
                let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
                if prompt.trim().is_empty() {
                    let r = "ERROR: task needs a 'prompt' describing the work.".to_string();
                    self.result_event(&r);
                    return r;
                }
                let r = self.run_subagent(prompt);
                self.result_event(&r);
                r
            }
            "ask_user" => {
                let (tx, rx) = mpsc::channel();
                let _ = self.ui.send(UiEvent::Question { prompt: s("question").to_string(), reply: tx });
                let r = match self.wait_reply(&rx) {
                    Ok(Some(ans)) if !ans.trim().is_empty() => {
                        format!("User answered: {}", ans.trim())
                    }
                    _ => "(user declined to answer)".to_string(),
                };
                self.result_event(&r);
                r
            }
            "bash" => {
                let cmd = s("command");
                let background = args.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
                let timeout = args.get("timeout").and_then(|v| v.as_u64()).unwrap_or(120);
                let desc = if background { format!("run in background: {cmd}") } else { format!("run: {cmd}") };
                if !pre_approved && !self.approve(&desc, Some(crate::policy::suggest_rule("bash", cmd))) {
                    let r = "DENIED by user.".to_string();
                    self.result_event(&r);
                    return r;
                }
                let r = if background {
                    tools::bash_background(cmd, &cwd)
                } else {
                    tools::bash(cmd, timeout, &cwd, &self.cancel)
                };
                self.result_event(&r);
                r
            }
            "bash_output" => {
                let r = tools::bash_output(args.get("id").and_then(|v| v.as_u64()).unwrap_or(0));
                self.result_event(&r);
                r
            }
            "bash_kill" => {
                let r = tools::bash_kill(args.get("id").and_then(|v| v.as_u64()).unwrap_or(0));
                self.result_event(&r);
                r
            }
            "write_file" => {
                let path = s("path");
                let content = s("content");
                let (diff, existed) = tools::write_preview(path, content);
                let _ = self.ui.send(UiEvent::Diff(diff));
                let verb = if existed { "overwrite" } else { "create" };
                if !pre_approved
                    && !self.approve(
                        &format!("{verb} {path} ({} bytes)", content.len()),
                        Some(crate::policy::suggest_rule("write_file", path)),
                    )
                {
                    let r = "DENIED by user.".to_string();
                    self.result_event(&r);
                    return r;
                }
                let mut r = tools::write_file(path, content);
                if r.starts_with("OK") {
                    r.push_str(&self.autocommit(&[path], &format!("{verb} {path}")));
                }
                self.result_event(&r);
                r
            }
            "edit_file" => {
                let a = |keys: &[&str]| str_arg(args, keys).unwrap_or("");
                let path = a(&["path", "file_path"]);
                match tools::edit_preview(path, a(&["old_text", "old_string"]), a(&["new_text", "new_string"])) {
                    tools::EditPreview::Err(e) => {
                        self.result_event(&e);
                        e
                    }
                    tools::EditPreview::Ok { diff, new_content } => {
                        let _ = self.ui.send(UiEvent::Diff(diff));
                        if !pre_approved
                            && !self.approve(&format!("edit {path}"), Some(crate::policy::suggest_rule("edit_file", path)))
                        {
                            let r = "DENIED by user.".to_string();
                            self.result_event(&r);
                            return r;
                        }
                        let mut r = tools::apply_write(path, &new_content);
                        if r.starts_with("OK") {
                            r.push_str(&self.autocommit(&[path], &format!("edit {path}")));
                        }
                        self.result_event(&r);
                        r
                    }
                }
            }
            "multi_edit" => {
                let edits = match parse_edits(args) {
                    Ok(e) => e,
                    Err(e) => {
                        let r = format!("ERROR: multi_edit: {e}. Nothing was applied.");
                        self.result_event(&r);
                        return r;
                    }
                };
                match tools::multi_edit_plan(&edits) {
                    Err(e) => {
                        self.result_event(&e);
                        e
                    }
                    Ok(plan) => {
                        let _ = self.ui.send(UiEvent::Diff(plan.diff));
                        let paths: Vec<String> = plan.files.iter().map(|(p, _)| p.clone()).collect();
                        if !pre_approved
                            && !self.approve(
                                &format!("apply {} edits across {} file(s)", edits.len(), paths.len()),
                                Some(crate::policy::suggest_rule("multi_edit", target)),
                            )
                        {
                            let r = "DENIED by user.".to_string();
                            self.result_event(&r);
                            return r;
                        }
                        let mut applied = Vec::new();
                        let mut errs = Vec::new();
                        for (p, content) in &plan.files {
                            let res = tools::apply_write(p, content);
                            if res.starts_with("OK") {
                                applied.push(p.clone());
                            } else {
                                errs.push(res);
                            }
                        }
                        let note = self.autocommit(
                            &applied.iter().map(String::as_str).collect::<Vec<_>>(),
                            &format!("multi_edit: {} file(s)", applied.len()),
                        );
                        let mut r = format!("OK applied edits to {} file(s){note}", applied.len());
                        let noop = edits.iter().filter(|e| e.old_text == e.new_text).count();
                        if noop > 0 {
                            r.push_str(&format!(" (skipped {noop} edit(s) with identical old_text and new_text)"));
                        }
                        if !errs.is_empty() {
                            r.push_str(&format!("\n{} failed:\n{}", errs.len(), errs.join("\n")));
                        }
                        self.result_event(&r);
                        r
                    }
                }
            }
            other if self.mcp.handles(other) => {
                // MCP tools can have side effects; gate them like bash unless
                // auto-approve is on. Plan mode can't tell read from write, so
                // it blocks them all — except the resource tools, which only read.
                let read_only = self.mcp.is_read_only(other);
                if self.perm.load(Ordering::Relaxed) == PERM_PLAN && !read_only {
                    let r = "[plan mode] Not executed. haikucode is in read-only plan mode — \
                             MCP tools may have side effects."
                        .to_string();
                    self.result_event(&r);
                    return r;
                }
                if !pre_approved && !read_only && !self.approve(&format!("call MCP tool {other}"), Some(other.to_string())) {
                    let r = "DENIED by user.".to_string();
                    self.result_event(&r);
                    return r;
                }
                let cancel = self.cancel.clone();
                let r = self.mcp.call(other, args, &cancel);
                self.result_event(&r);
                r
            }
            other => {
                let e = format!("ERROR: unknown tool {other}");
                self.result_event(&e);
                e
            }
        }
    }

    /// Commit just-edited paths as a checkpoint (when auto_commit is on and we
    /// are in a repo). Returns a short note to append to the tool result.
    fn autocommit(&self, paths: &[&str], summary: &str) -> String {
        if !self.cfg.auto_commit {
            return String::new();
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let owned: Vec<String> = paths.iter().map(|s| s.to_string()).collect();
        tools::git_autocommit(&cwd, &owned, &format!("haikucode: {summary}"))
    }

    fn result_event(&self, result: &str) {
        let ok = !result.starts_with("ERROR") && !result.starts_with("DENIED");
        let _ = self.ui.send(UiEvent::ToolResult { ok, preview: preview(result, 12) });
    }

    /// Block on a UI reply channel, but give up when the turn is cancelled
    /// (Esc, or quitting) so the worker can never hang on a prompt nobody will
    /// answer — that used to wedge `join()` at exit.
    fn wait_reply<T>(&self, rx: &Receiver<T>) -> Result<T, ()> {
        loop {
            match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                Ok(v) => return Ok(v),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if self.cancel.load(Ordering::Relaxed) {
                        return Err(());
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(()),
            }
        }
    }

    fn approve(&mut self, desc: &str, rule: Option<String>) -> bool {
        if self.perm.load(Ordering::Relaxed) == PERM_AUTO {
            return true;
        }
        let _ = self.ui.send(UiEvent::Approval { desc: desc.to_string(), rule });
        match self.wait_reply(&self.appr_rx) {
            Ok(ApprovalResponse::Yes) => true,
            Ok(ApprovalResponse::Always) => {
                self.perm.store(PERM_AUTO, Ordering::Relaxed);
                true
            }
            Ok(ApprovalResponse::Rule(rule)) => {
                let path = crate::policy::persist_target();
                match crate::policy::persist_rule(&path, "allow", &rule) {
                    Ok(()) => {
                        if let Some(r) = crate::policy::Rule::parse(&rule) {
                            if !self.policy.allow.contains(&r) {
                                self.policy.allow.push(r);
                            }
                        }
                        let _ = self.ui.send(UiEvent::Notice(format!(
                            "won't ask again for {rule} (saved to {})",
                            path.display()
                        )));
                    }
                    Err(e) => {
                        let _ = self.ui.send(UiEvent::Error(format!("could not save rule {rule}: {e}")));
                    }
                }
                true
            }
            _ => false,
        }
    }
}

/// One tool call from the model, parsed once.
struct Call {
    id: String,
    name: String,
    /// The arguments as sent (`{}` when empty), for errors and the repeat guard.
    raw: String,
    /// None when `raw` isn't valid JSON.
    args: Option<Value>,
    /// The argument shown next to the tool name in the transcript.
    summary: String,
}

impl Call {
    fn new(c: &AccumCall, idx: usize) -> Call {
        let raw = if c.args.trim().is_empty() { "{}".to_string() } else { c.args.clone() };
        let args: Option<Value> = serde_json::from_str(&raw).ok();
        let summary = args
            .as_ref()
            .and_then(|a| {
                ["command", "path", "pattern", "note", "query", "url", "question", "description", "name"]
                    .iter()
                    .find_map(|k| a.get(*k).and_then(|v| v.as_str()))
            })
            .unwrap_or("")
            .to_string();
        Call {
            id: if c.id.is_empty() { format!("call_{idx}") } else { c.id.clone() },
            name: c.name.clone(),
            raw,
            args,
            summary,
        }
    }
}

/// Read-only tools that need no approval, prompt or worker state, so several
/// from one response can run side by side.
pub(super) fn parallel_safe(name: &str) -> bool {
    matches!(name, "read_file" | "list_files" | "grep" | "glob" | "web_fetch" | "web_search" | "recall" | "skill")
}

/// The read-only tools' own work, free of the worker so a batch can run on
/// threads. Also the sequential path for these tools, so both behave alike.
fn run_readonly(http: &ureq::Agent, name: &str, args: &Value) -> String {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).unwrap_or("");
    match name {
        "read_file" => tools::read_file(
            s("path"),
            args.get("start_line").and_then(|v| v.as_u64()),
            args.get("end_line").and_then(|v| v.as_u64()),
        ),
        "list_files" => tools::list_files(s("path")),
        "grep" => tools::grep(
            s("pattern"),
            s("path"),
            args.get("ignore_case").and_then(|v| v.as_bool()).unwrap_or(false),
        ),
        "glob" => tools::glob_search(s("pattern")),
        "web_fetch" => tools::web_fetch(http, s("url")),
        "web_search" => tools::web_search(http, s("query")),
        "recall" => tools::recall(args.get("query").and_then(|v| v.as_str())),
        "skill" => crate::commands::skill_tool(s("name")),
        _ => format!("ERROR: {name} is not a read-only tool"),
    }
}

/// Parse the `edits` argument of multi_edit into typed edit requests.
/// The first string among `keys` in `v`: models trained on other agents send
/// Claude Code's spellings (`file_path`, `old_string`, `new_string`).
fn str_arg<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(|x| x.as_str()))
}

/// multi_edit's edits, all or nothing: a malformed edit is an error naming it,
/// never silently dropped while the rest apply. An edit without a path uses
/// the previous edit's, else the call's top-level `path`. `edits` may also
/// arrive as a JSON string.
fn parse_edits(args: &Value) -> Result<Vec<tools::EditReq>, String> {
    let parsed;
    let edits = match args.get("edits") {
        Some(Value::String(s)) => {
            parsed = serde_json::from_str::<Value>(s).map_err(|_| "'edits' is a string that isn't a JSON array".to_string())?;
            &parsed
        }
        Some(v) => v,
        None => return Err("missing 'edits'".into()),
    };
    let arr = edits.as_array().filter(|a| !a.is_empty()).ok_or("'edits' must be a non-empty array")?;
    let mut path = str_arg(args, &["path", "file_path"]).map(str::to_string);
    let mut out = Vec::with_capacity(arr.len());
    for (i, e) in arr.iter().enumerate() {
        let n = i + 1;
        if let Some(p) = str_arg(e, &["path", "file_path"]) {
            path = Some(p.to_string());
        }
        let path = path.clone().ok_or(format!("edit {n} has no 'path' (give each edit one, or a top-level 'path')"))?;
        let old_text = str_arg(e, &["old_text", "old_string"]).ok_or(format!("edit {n} has no 'old_text'"))?.to_string();
        let new_text = str_arg(e, &["new_text", "new_string"]).ok_or(format!("edit {n} has no 'new_text'"))?.to_string();
        out.push(tools::EditReq { path, old_text, new_text });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_edit_args_fail_loudly_and_paths_carry_over() {
        use serde_json::json;
        let e = |o: &str, n: &str| json!({"old_text": o, "new_text": n});
        // No path anywhere: an error naming the edit (it used to be dropped).
        let err = parse_edits(&json!({"edits": [e("a", "b")]})).unwrap_err();
        assert!(err.contains("edit 1 has no 'path'"), "{err}");
        // A top-level path, inherited by every edit; a later path takes over.
        let ok = parse_edits(&json!({"path": "x.rs", "edits": [
            e("a", "b"),
            {"path": "y.rs", "old_string": "c", "new_string": "d"},
            e("e", "f"),
        ]}))
        .unwrap();
        let paths: Vec<&str> = ok.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["x.rs", "y.rs", "y.rs"]);
        assert_eq!((ok[1].old_text.as_str(), ok[1].new_text.as_str()), ("c", "d"), "Claude Code spellings");
        // A missing field fails the whole batch instead of being skipped.
        let err = parse_edits(&json!({"path": "x", "edits": [e("a", "b"), {"old_text": "c"}]})).unwrap_err();
        assert!(err.contains("edit 2 has no 'new_text'"), "{err}");
        // edits sent as a JSON string still parse.
        let s = json!({"path": "x", "edits": "[{\"old_text\":\"a\",\"new_text\":\"b\"}]"});
        assert_eq!(parse_edits(&s).unwrap().len(), 1);
        assert!(parse_edits(&json!({"edits": []})).is_err());
    }

    #[test]
    fn only_read_only_tools_run_in_parallel() {
        for t in ["read_file", "list_files", "grep", "glob", "web_fetch", "web_search", "recall", "skill"] {
            assert!(parallel_safe(t), "{t}");
        }
        // Anything that writes, prompts, recurses or touches worker state
        // must keep running one at a time, in order.
        for t in [
            "bash", "bash_output", "bash_kill", "write_file", "edit_file", "multi_edit",
            "remember", "view_image", "todo", "task", "ask_user", "mcp__fs__read",
        ] {
            assert!(!parallel_safe(t), "{t}");
        }
    }
}
