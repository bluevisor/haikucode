//! Context-window sizing, compaction and token estimation.

use super::{spend, UiEvent, Worker};
use crate::api::{self, Message};
use crate::config::Config;
use std::sync::atomic::Ordering;

impl Worker {
    /// What a detected window is keyed by.
    fn ctx_key(cfg: &Config) -> String {
        format!("{}\n{}\n{}", cfg.provider, cfg.base_url, cfg.model)
    }

    /// Set the context window for the current model. The built-in table is
    /// instant; a background call then refines it from the provider's
    /// `/models` metadata for providers that advertise one (see `ctx_limit`).
    /// A user-pinned window (per-model entry or `context_window`) is used as-is.
    pub(super) fn refresh_context_window(&mut self) {
        // Whatever was detected belonged to the previous provider/model.
        let gen = self.ctx_gen.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut d) = self.ctx_detected.lock() {
            *d = None;
        }
        if let Some(n) = self.cfg.model_context_window() {
            let _ = self.ui.send(UiEvent::ContextLimit(n));
            return;
        }
        if self.cfg.context_window_explicit {
            // Re-announce it: the previous model may have had its own entry.
            let _ = self.ui.send(UiEvent::ContextLimit(self.cfg.context_window));
            return;
        }
        let table = crate::config::known_context_window(&self.cfg.model);
        self.cfg.context_window = table;
        let _ = self.ui.send(UiEvent::ContextLimit(table));
        let ui = self.ui.clone();
        let cfg = self.cfg.clone();
        let detected = self.ctx_detected.clone();
        let current = self.ctx_gen.clone();
        std::thread::spawn(move || {
            let http = api::agent_http();
            if let Some(n) = api::context_window(&http, &cfg, &cfg.model) {
                // A switch since this lookup started: its answer is stale.
                if current.load(Ordering::SeqCst) != gen {
                    return;
                }
                if let Ok(mut d) = detected.lock() {
                    *d = Some((Self::ctx_key(&cfg), n));
                }
                if n != table {
                    let _ = ui.send(UiEvent::ContextLimit(n));
                }
            }
        });
    }

    /// The context window auto-compaction sizes against: the model's own
    /// entry, else a user-pinned value, else what the provider advertised for
    /// this model, else the table.
    pub(super) fn ctx_limit(&self) -> u32 {
        if let Some(n) = self.cfg.model_context_window() {
            return n;
        }
        if !self.cfg.context_window_explicit {
            if let Ok(d) = self.ctx_detected.lock() {
                if let Some((key, n)) = d.as_ref() {
                    if *key == Self::ctx_key(&self.cfg) {
                        return *n;
                    }
                }
            }
        }
        self.cfg.context_window
    }

    /// Auto-compact when the last prompt crossed 80% of the context window.
    /// Does not save the session itself — the caller (User handler) saves
    /// after the turn, avoiding a double-write when compaction runs right
    /// before a new turn.
    pub(super) fn maybe_auto_compact(&mut self) {
        let limit = self.ctx_limit().max(1);
        if self.last_prompt as f64 >= 0.8 * limit as f64 {
            let pct = (self.last_prompt as f64 / limit as f64 * 100.0).round() as u32;
            let _ = self
                .ui
                .send(UiEvent::Notice(format!("context {pct}% full — compacting automatically…")));
            self.compact(None);
        }
    }

    /// Replace older turns with a model-written summary. Keeps the system
    /// prefix (prompt, memory, project context) and the most recent user
    /// exchange verbatim; everything in between is summarized with a plain
    /// (tool-less) completion. Returns true when messages were compacted (the
    /// caller should save the session) and false when there was nothing to do.
    pub(super) fn compact(&mut self, focus: Option<&str>) -> bool {
        let n = self.messages.len();
        if n <= self.system_len + 2 {
            let _ = self.ui.send(UiEvent::Notice("nothing to compact yet.".into()));
            return false;
        }
        // Keep the latest exchange (from the last user message on) verbatim —
        // unless it IS the whole conversation, then summarize everything.
        let mut tail_start = self.messages[self.system_len..]
            .iter()
            .rposition(|m| m.role == "user")
            .map(|i| i + self.system_len)
            .filter(|&i| i > self.system_len)
            .unwrap_or(n);
        // When the latest exchange is itself what blew the window (one turn
        // with hundreds of tool calls), keeping it verbatim compacts nothing
        // and the next request still fails — summarize it too.
        let limit = self.ctx_limit().max(1) as usize;
        if tail_start < n && estimate_tokens(&self.messages[tail_start..]) as usize > limit / 2 {
            tail_start = n;
        }
        let rendered = render_for_summary(&self.messages[self.system_len..tail_start]);
        // The summary request must itself fit the model: ~4 chars per token,
        // leave room for the reply.
        let rendered = api::truncate(&rendered, (limit * 4).saturating_sub(8_000).max(20_000));
        if rendered.trim().is_empty() {
            let _ = self.ui.send(UiEvent::Notice("nothing to compact yet.".into()));
            return false;
        }
        let _ = self.ui.send(UiEvent::Notice("compacting context…".into()));
        let req = vec![
            Message::system(
                "You compress coding-agent conversations. Write a dense summary that lets the \
                 agent continue seamlessly. Preserve: the user's goals and constraints, decisions \
                 made, files/paths touched and how, key file contents or APIs discovered, command \
                 results that matter, and any unresolved problems or next steps. Use terse \
                 bullet points. No preamble.",
            ),
            Message::user(match focus {
                Some(f) if !f.trim().is_empty() => format!(
                    "Summarize this conversation so far. Pay special attention to, and preserve in \
                     detail: {}\n\n{rendered}",
                    f.trim()
                ),
                _ => format!("Summarize this conversation so far:\n\n{rendered}"),
            }),
        ];
        let res = api::chat_plain(&self.http, &self.cfg, &req, &self.cancel);
        if let Ok((_, Some(u))) = &res {
            let _ = self.ui.send(spend(u));
        }
        match res.map(|(text, _)| text) {
            Ok(summary) if !summary.trim().is_empty() => {
                let mut new = self.messages[..self.system_len].to_vec();
                new.push(Message::user(format!(
                    "[Earlier conversation was compacted. Summary:]\n{}",
                    summary.trim()
                )));
                new.push(Message {
                    role: "assistant".into(),
                    content: "Understood — continuing from that summary.".into(),
                    images: Vec::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    native: None,
                });
                // Prompts inside the summary can't be rewound to any more; the
                // kept tail moves up behind the summary.
                let kept_from = new.len();
                self.turns.retain(|t| t.at >= tail_start);
                for t in &mut self.turns {
                    t.at = t.at - tail_start + kept_from;
                }
                new.extend_from_slice(&self.messages[tail_start..]);
                // A client-side summary rewrites the history that signed
                // thinking was bound to: replay the kept turns without it.
                // (Plain chat reasoning stays — DeepSeek wants it back.)
                for m in &mut new {
                    if m.native.as_ref().is_some_and(api::native_is_history_bound) {
                        m.native = None;
                    }
                }
                self.messages = new;
                // Rough size estimate so the UI's ctx bar drops right away.
                let est = estimate_tokens(&self.messages);
                self.last_prompt = est;
                let _ = self.ui.send(UiEvent::Context(est));
                let _ = self.ui.send(UiEvent::Notice(format!(
                    "context compacted: {n} → {} messages.",
                    self.messages.len()
                )));
                true
            }
            Ok(_) => {
                let _ = self.ui.send(UiEvent::Error("compaction failed: empty summary.".into()));
                false
            }
            Err(e) => {
                let _ = self.ui.send(UiEvent::Error(format!("compaction failed: {e}")));
                false
            }
        }
    }
}

/// Render messages as role-tagged plain text for the summarizer. Tool results
/// are clipped hard — the summary only needs their gist, and this keeps the
/// compaction request itself small.
fn render_for_summary(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role.as_str() {
            "tool" => {
                out.push_str(&format!("[tool result] {}\n", api::truncate(&m.content, 500)));
            }
            role => {
                if !m.content.trim().is_empty() {
                    out.push_str(&format!("[{role}] {}\n", api::truncate(&m.content, 4000)));
                }
                // Preserve image context: note how many images were attached so the
                // summarizer knows visual context existed (even if it can't see them).
                if !m.images.is_empty() {
                    let count = m.images.len();
                    out.push_str(&format!(
                        "[{} image{} attached: {}]\n",
                        count,
                        if count > 1 { "s" } else { "" },
                        m.images.iter().map(|i| {
                            i.rsplit('/').next().unwrap_or(i).to_string()
                        }).collect::<Vec<_>>().join(", ")
                    ));
                }
                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        out.push_str(&format!(
                            "[tool call] {}({})\n",
                            c.function.name,
                            api::truncate(&c.function.arguments, 300)
                        ));
                    }
                }
            }
        }
    }
    // Belt and braces: the request must fit in the context window itself.
    api::truncate(&out, 300_000)
}

/// Crude size estimate (~4 chars/token, ~1000 tokens per image) for the
/// post-compaction context bar.
pub(super) fn estimate_tokens(messages: &[Message]) -> u32 {
    let chars: usize = messages
        .iter()
        .map(|m| {
            m.content.len()
                + m.images.len() * 4000
                + m.tool_calls
                    .as_ref()
                    .map(|cs| cs.iter().map(|c| c.function.arguments.len() + 20).sum())
                    .unwrap_or(0)
        })
        .sum();
    (chars / 4) as u32
}
