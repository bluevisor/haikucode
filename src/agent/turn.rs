//! One user turn: the model/tool loop, Stop hooks, `!cmd` runs and /rewind.

use super::compact::estimate_tokens;
use super::tool_calls::parallel_safe;
use super::{preview, GenerationStats, TurnMark, UiEvent, Worker, AUTO_STEPS};
use crate::api::{self, Message};
use crate::hooks::Outcome;
use crate::tools;
use std::sync::atomic::Ordering;

impl Worker {
    pub(super) fn run_turn(&mut self, text: String, images: Vec<String>, typed: String) {
        self.generation = GenerationStats::default();
        // Drop images a previous interrupted turn queued but never consumed,
        // so they can't surface mid-way through an unrelated turn.
        self.pending_images.clear();
        self.recent_calls.clear();
        if !self.quiet {
            self.turns.push(TurnMark { at: self.messages.len(), head: self.checkpoint_head(), prompt: typed });
        }
        self.messages.push(Message::user_with_images(text, images));
        self.run_loop();
    }

    /// HEAD of the working-directory repo when edits are checkpointed there.
    fn checkpoint_head(&self) -> Option<String> {
        let cwd = std::env::current_dir().ok()?;
        if !self.cfg.auto_commit || !tools::in_git_repo(&cwd) {
            return None;
        }
        tools::git_run(&cwd, &["rev-parse", "HEAD"], 10).ok().map(|s| s.trim().to_string())
    }

    /// Go back to just before turn `n`: undo haikucode's checkpoints since then
    /// (files first: if git refuses, the conversation is left alone too), then
    /// drop that prompt and everything after it and hand it back to the composer.
    pub(super) fn rewind(&mut self, n: usize) {
        let Some(mark) = self.turns.get(n).cloned() else {
            let _ = self.ui.send(UiEvent::Error("that prompt is no longer in the conversation.".into()));
            return;
        };
        let files = match &mark.head {
            Some(head) => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
                match tools::revert_checkpoints_since(&cwd, head) {
                    Ok(0) => "no file changes to undo".to_string(),
                    Ok(k) => format!("reverted {k} checkpoint commit(s)"),
                    Err(e) => {
                        let _ = self.ui.send(UiEvent::Error(format!("rewind stopped, nothing changed: {e}")));
                        return;
                    }
                }
            }
            None => "files untouched (no git checkpoints for that prompt)".to_string(),
        };
        self.messages.truncate(mark.at);
        self.turns.truncate(n);
        let est = estimate_tokens(&self.messages);
        self.last_prompt = est;
        self.save_session();
        let _ = self.ui.send(UiEvent::Context(est));
        let _ = self.ui.send(UiEvent::Notice(format!(
            "rewound to before prompt {}: {files}; conversation trimmed to {} messages.",
            n + 1,
            self.messages.len()
        )));
        let _ = self.ui.send(UiEvent::Rewound(mark.prompt));
    }

    /// Stop hooks run after the model's final reply. Exit 2 means "not done":
    /// the hook's reason goes back to the model and the loop continues, at most
    /// a few times so a hook that can never be satisfied can't spin forever.
    pub(super) fn run_stop_hooks(&mut self) {
        if !self.hooks.has("Stop") || self.quiet {
            return;
        }
        for _ in 0..3 {
            if self.cancel.load(Ordering::Relaxed) {
                return;
            }
            let last = self.messages.iter().rev().find(|m| m.role == "assistant").map(|m| m.content.clone());
            let payload = serde_json::json!({ "last_assistant_message": last.unwrap_or_default() });
            match self.hooks.run("Stop", "", &payload) {
                Outcome::Block(reason) => {
                    let _ = self.ui.send(UiEvent::Notice(format!("stop hook: {}", crate::api::truncate(&reason, 200))));
                    self.messages.push(Message::user(format!("[Stop hook feedback — address this before finishing]\n{reason}")));
                    self.run_loop();
                }
                Outcome::Continue { context } => {
                    if !context.trim().is_empty() {
                        let _ = self.ui.send(UiEvent::Notice(crate::api::truncate(context.trim(), 400)));
                    }
                    return;
                }
            }
        }
    }

    /// `!cmd`: run a shell command on the user's behalf. Output is shown like a
    /// tool result and recorded in the conversation so the model can build on it.
    pub(super) fn run_shell(&mut self, cmd: &str) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        let _ = self.ui.send(UiEvent::ToolStart { name: "bash".into(), summary: cmd.to_string() });
        let out = crate::tools::bash(cmd, 120, &cwd, &self.cancel);
        let ok = !out.starts_with("ERROR");
        let _ = self.ui.send(UiEvent::ToolResult { ok, preview: preview(&out, 40) });
        self.messages.push(Message::user(format!(
            "[The user ran a shell command directly]\n$ {cmd}\n{}",
            crate::api::truncate(&out, 20000)
        )));
    }

    /// The model/tool loop over `self.messages`. Returns the final assistant
    /// text (the reply with no tool calls). When `self.quiet` is set (inside a
    /// sub-agent) assistant tokens aren't streamed to the UI as the main reply,
    /// but tool activity still shows so the user can follow along and approve.
    pub(super) fn run_loop(&mut self) -> String {
        let quiet = self.quiet;
        let max = if self.cfg.max_tool_calls == 0 {
            AUTO_STEPS
        } else {
            self.cfg.max_tool_calls as usize
        };
        // A failed or fruitless mid-turn compaction isn't retried every step.
        let mut compact_failed = false;
        for _ in 0..max {
            if self.cancel.load(Ordering::Relaxed) {
                let _ = self.ui.send(UiEvent::Notice("(interrupted)".into()));
                return String::new();
            }
            // A long turn can outgrow the window between user prompts, so check
            // before every request, not only when a new turn starts. Tool results
            // appended since the last response aren't in `last_prompt` yet, hence
            // the estimate. Sub-agents run on their own short-lived history.
            if !quiet && !compact_failed {
                let used = self.last_prompt.max(estimate_tokens(&self.messages));
                if used as f64 >= 0.8 * self.ctx_limit().max(1) as f64 {
                    let _ = self.ui.send(UiEvent::Notice(
                        "context nearly full mid-turn — compacting…".into(),
                    ));
                    // When the whole conversation was summarized it ends on the
                    // assistant's acknowledgement; hand the turn back explicitly.
                    if !self.compact(None) {
                        compact_failed = true;
                    } else {
                        if self.messages.last().map(|m| m.role.as_str()) == Some("assistant") {
                            self.messages.push(Message::user(
                                "[Context was compacted mid-task.] Continue the task from the summary.",
                            ));
                        }
                        // Still over the line: compacting again this turn would
                        // only burn calls (the model re-reads what was summarized).
                        if estimate_tokens(&self.messages) as f64 >= 0.8 * self.ctx_limit().max(1) as f64 {
                            compact_failed = true;
                        }
                    }
                }
            }
            let ui = self.ui.clone();
            let ui2 = self.ui.clone();
            let ui3 = self.ui.clone();
            // Time generation on the worker, excluding the wait for first
            // content and any tool/approval time. Reset on request retries.
            let generation_start = std::cell::Cell::new(None);
            let started = &generation_start;
            let record_start = move |t: &str| {
                if !t.is_empty() && started.get().is_none() {
                    started.set(Some(std::time::Instant::now()));
                }
            };
            let res = api::chat_resilient(
                &self.http,
                &self.cfg,
                &self.messages,
                &self.tools,
                &self.cancel,
                move |t| {
                    record_start(t);
                    if !quiet {
                        let _ = ui.send(UiEvent::Token(t.to_string()));
                    }
                },
                move |t| {
                    record_start(t);
                    if !quiet {
                        let _ = ui2.send(UiEvent::Reasoning(t.to_string()));
                    }
                },
                move |m| {
                    started.set(None);
                    let _ = ui3.send(UiEvent::ResetLive);
                    let _ = ui3.send(UiEvent::Notice(m.to_string()));
                },
            );
            let (content, calls, usage, native) = match res {
                Ok(x) => x,
                Err(e) => {
                    let _ = self.ui.send(UiEvent::Error(e.to_string()));
                    return String::new();
                }
            };
            if let Some(u) = usage {
                self.last_prompt = u.prompt_tokens;
                let _ = self.ui.send(UiEvent::Usage(u.spent()));
            }
            if !quiet {
                let _ = self.ui.send(UiEvent::AssistantCommit);
                if !self.cancel.load(Ordering::Relaxed) {
                    if let (Some(u), Some(start)) = (usage, generation_start.get()) {
                        self.generation.add(u.completion_tokens, start.elapsed());
                    }
                }
            }

            // Esc during streaming: chat_stream stops early and returns what
            // had accumulated so far. Half-streamed tool calls must not enter
            // history (their args may be truncated and they'll never get
            // results, which the API rejects on the next request) — keep only
            // the partial text and end the turn.
            if self.cancel.load(Ordering::Relaxed) {
                if !content.is_empty() {
                    self.messages.push(Message {
                        role: "assistant".into(),
                        content,
                        images: Vec::new(),
                        tool_calls: None,
                        tool_call_id: None,
                        // Partial chat reasoning still has to go back to
                        // DeepSeek; a half-signed native turn must not.
                        native: native.filter(|n| !api::native_is_history_bound(n)),
                    });
                }
                let _ = self.ui.send(UiEvent::Notice("(interrupted)".into()));
                return String::new();
            }

            let mut msg = Message {
                role: "assistant".into(),
                content: content.clone(),
                images: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                native,
            };
            if !calls.is_empty() {
                msg.tool_calls = Some(
                    calls
                        .iter()
                        .cloned()
                        .enumerate()
                        .map(|(i, c)| c.into_tool_call(i))
                        .collect(),
                );
            }
            self.messages.push(msg);

            if calls.is_empty() {
                return content;
            }
            // Once the assistant message above is in history, every one of its
            // tool_call_ids must get a tool result or the API rejects the next
            // request — so an Esc here answers the skipped calls instead of
            // returning with the history dangling.
            let mut interrupted = false;
            let mut i = 0;
            while i < calls.len() {
                if interrupted || self.cancel.load(Ordering::Relaxed) {
                    if !interrupted {
                        interrupted = true;
                        let _ = self.ui.send(UiEvent::Notice("(interrupted)".into()));
                    }
                    let c = &calls[i];
                    let id = if c.id.is_empty() { format!("call_{i}") } else { c.id.clone() };
                    self.messages
                        .push(Message::tool(id, "(interrupted by user; tool not run)".into()));
                    i += 1;
                    continue;
                }
                // Consecutive read-only calls go out together; the rest run one
                // at a time, in order (approvals, writes, sub-agents, MCP).
                let run = calls[i..].iter().take_while(|c| parallel_safe(&c.name)).count();
                if run >= 2 {
                    self.handle_parallel(&calls[i..i + run], i);
                    i += run;
                } else {
                    self.handle_call(calls[i].clone(), i);
                    i += 1;
                }
            }
            if interrupted {
                return String::new();
            }
            // A long turn used to live only in memory until it ended: a crash
            // lost it, and nothing outside could inspect it. Save as it goes —
            // at most every 20s, since a Pi's SD card shouldn't take a full
            // rewrite every round. Sub-agents run on a swapped-in history.
            if !quiet && self.last_save.elapsed() >= std::time::Duration::from_secs(20) {
                self.save_session();
                self.last_save = std::time::Instant::now();
            }
            // view_image queues images to hand to the model on the next call;
            // add them as a user message after the tool results (which must
            // immediately follow their tool calls).
            if !self.pending_images.is_empty() {
                let imgs = std::mem::take(&mut self.pending_images);
                self.messages.push(Message::user_with_images(
                    "(attached image(s) from view_image)",
                    imgs,
                ));
            }
        }
        let _ = self.ui.send(UiEvent::Notice("(stopped: hit max steps)".into()));
        String::new()
    }
}
