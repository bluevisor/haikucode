use super::*;

impl App {
    pub fn handle_event(&mut self, ev: UiEvent, h: &Handles) {
        match ev {
            UiEvent::Token(t) => {
                if !t.is_empty() { self.awaiting_response = false; }
                self.tool_running = None;
                self.current_activity = None;
                self.live.push_str(&t);
                self.dirty();
            }
            UiEvent::Reasoning(t) => {
                if !t.is_empty() { self.awaiting_response = false; }
                self.tool_running = None;
                self.current_activity = None;
                self.live_reasoning.push_str(&t);
                self.dirty();
            }
            UiEvent::ResetLive => {
                self.flush_live();
            }
            UiEvent::AssistantCommit => {
                self.flush_live();
                if let Some(last) = self.transcript.last() {
                    if last.kind == Kind::Assistant && last.text.is_empty() {
                        self.transcript.pop();
                        // Drop the separator that was inserted for the entry we
                        // just removed, so no stray blank is left behind.
                        if self.transcript.last().map(|l| l.kind) == Some(Kind::Blank) {
                            self.transcript.pop();
                        }
                    }
                }
            }
            UiEvent::ToolStart { name, summary } => {
                self.awaiting_response = false;
                self.tool_running = Some(name.clone());
                self.current_activity = Some(brief::action(&name, &summary));
                self.flush_live();
                self.push(Kind::Tool, format!("{name} {summary}"));
            }
            UiEvent::Diff(d) => {
                // Flush any live assistant text before showing a diff preview so
                // the diff doesn't interleave with streaming tokens mid-sentence.
                self.flush_live();
                for (i, ln) in d.lines().enumerate() {
                    let kind = if ln.starts_with('+') {
                        Kind::DiffAdd
                    } else if ln.starts_with('-') {
                        Kind::DiffDel
                    } else {
                        Kind::DiffCtx
                    };
                    self.transcript.push(TLine { kind, text: ln.replace('\t', "    "), lead: i == 0, color: None });
                }
                self.after_push();
            }
            UiEvent::ToolResult { ok, preview } => {
                self.tool_running = None;
                self.current_activity = None;
                self.flush_live();
                if preview.is_empty() {
                    return;
                }
                let kind = if ok { Kind::ToolResult } else { Kind::ToolErr };
                self.push(kind, preview);
            }
            UiEvent::Approval { desc, rule } => {
                self.appr_rule = rule;
                // Flush any preceding assistant text before showing the prompt.
                self.flush_live();
                self.mode = Mode::Approval(desc);
            }
            UiEvent::PasswordRequest { prompt, reply } => {
                self.pw_input.clear();
                self.pw_reply = Some(reply);
                self.mode = Mode::Password { prompt };
            }
            UiEvent::Question { prompt, reply } => {
                self.q_input.clear();
                self.q_reply = Some(reply);
                self.mode = Mode::Question { prompt };
            }
            UiEvent::McpPrompts(rows) => {
                self.mcp_palette = rows
                    .into_iter()
                    .map(|(c, d)| -> (&'static str, &'static str) {
                        (Box::leak(c.into_boxed_str()), Box::leak(d.into_boxed_str()))
                    })
                    .collect();
            }
            UiEvent::McpPromptReady { shown, text } => self.send_prompt_as(shown, text, h),
            UiEvent::ModelList(ids) if self.wizard.as_ref().is_some_and(|d| d.waiting_models) => {
                self.last_models = ids.clone();
                self.wizard_models(ids);
            }
            UiEvent::ModelListFailed(e) if self.wizard.as_ref().is_some_and(|d| d.waiting_models) => {
                self.wizard_models_failed(&e);
            }
            UiEvent::ModelListFailed(e) => self.push(Kind::ErrorK, format!("could not fetch models: {e}")),
            UiEvent::SignedIn { provider, token } => self.on_signed_in(provider, Some(token), String::new(), h),
            UiEvent::SignInFailed { provider, error } => self.on_signed_in(provider, None, error, h),
            UiEvent::ModelList(ids) => {
                let current = ids.iter().position(|m| *m == self.model_info);
                self.picker = Some(Picker {
                    title: "Pick a model (type to filter)".into(),
                    items: ids.clone(),
                    current,
                    filter: String::new(),
                    cursor: current.unwrap_or(0),
                    scroll: 0,
                    action: PickAction::Model,
                    hint: None,
                });
                if let Some(p) = self.picker.as_mut() {
                    let len = p.items.len();
                    p.clamp(len); // scroll the current model into view
                }
                self.last_models = ids;
                self.mode = Mode::Select;
            }
            UiEvent::ModelChanged(m) => {
                self.settings.model = m.clone();
                self.settings.sync_active_custom();
                self.sync_prices(); // a model can carry its own prices
                self.model_info = m;
            }
            UiEvent::Usage(s) => {
                self.last_prompt_tokens = s.prompt as u32;
                self.add_spent(s);
            }
            UiEvent::Spend(s) => self.add_spent(s),
            UiEvent::Context(n) => {
                self.last_prompt_tokens = n;
            }
            UiEvent::ContextLimit(n) => {
                self.ctx_limit = n;
            }
            UiEvent::BalanceCleared => self.balance = None,
            UiEvent::Balance(b) => {
                // A balance in another currency (DeepSeek bills China-region
                // accounts in CNY) needs no nudge: the status line tags both
                // figures with their ISO codes. The old notice told people to
                // switch the price currency, which relabels USD prices as yuan.
                self.balance = Some(b);
            }
            UiEvent::TurnList(items) => {
                if items.is_empty() {
                    self.picker = None;
                    self.mode = Mode::Idle;
                } else {
                    self.picker = Some(Picker {
                        title: "Rewind to before which prompt? (edits since then are undone)".into(),
                        items,
                        current: None,
                        filter: String::new(),
                        cursor: 0,
                        scroll: 0,
                        action: PickAction::Rewind,
                        hint: None,
                    });
                    self.mode = Mode::Select;
                }
            }
            UiEvent::Rewound(prompt) => {
                // Hand the prompt back so it can be edited and re-sent.
                self.input = prompt;
                self.cursor = self.char_len();
                self.git_checked_at = None;
            }
            UiEvent::SessionList(items) => {
                if items.is_empty() {
                    self.picker = None;
                    self.mode = Mode::Idle;
                } else {
                    self.picker = Some(Picker {
                        title: "Resume which session?".into(),
                        items,
                        current: None,
                        filter: String::new(),
                        cursor: 0,
                        scroll: 0,
                        action: PickAction::Resume,
                        hint: None,
                    });
                    self.mode = Mode::Select;
                }
            }
            UiEvent::SessionSwitched { label, replay } => {
                self.transcript.clear();
                self.reset_session_counters();
                self.replay(replay);
                self.push(Kind::Notice, format!("resumed session {label}"));
                self.session_label = label;
                self.git_checked_at = None;
                self.dirty();
            }
            UiEvent::SessionLabel(label) => {
                self.session_label = label;
            }
            UiEvent::Notice(msg) => {
                self.push(Kind::Notice, msg);
            }
            UiEvent::Progress(msg) => {
                self.current_activity = Some(msg.clone());
                self.push(Kind::Progress, msg);
            }
            UiEvent::Error(msg) => {
                self.awaiting_response = false;
                self.current_activity = None;
                self.push(Kind::ErrorK, msg);
            }
            UiEvent::TurnDone => {
                self.awaiting_response = false;
                self.tool_running = None;
                self.current_activity = None;
                self.flush_live();
                // Keep an open picker alive: the worker sends TurnDone right
                // after ModelList, and clear_busy would drop back to Idle.
                if matches!(self.mode, Mode::Select) && self.picker.is_some() {
                    self.busy_since = None;
                } else {
                    self.clear_busy();
                }
                // The worker may reply with YOLO toggled; sync the UI.
                self.perm = h.shared.perm.clone();
                // Dispatch the next queued message, if any — through `dispatch`
                // so it is echoed, `@file`s attach, and `/cmd` / `!cmd` route
                // exactly as if typed now. Not after an Esc: the user stopped
                // the agent, so the queue waits for their next message.
                if self.queue_paused {
                    self.queue_paused = false;
                    if !self.queued.is_empty() {
                        self.push(
                            Kind::Notice,
                            format!("{} queued message(s) held — they send after your next message.", self.queued.len()),
                        );
                    }
                } else if !self.queued.is_empty() && self.mode == Mode::Idle {
                    let next = self.queued.remove(0);
                    self.dispatch(next, h);
                }
            }
            UiEvent::Suggestion(s) => {
                self.suggestion = Some(s);
            }
        }
    }

    #[allow(dead_code)]
    fn model_short(&self) -> &str {
        self.model_info.rsplit('/').next().unwrap_or(&self.model_info)
    }
}
