use super::*;

impl App {
    pub(super) fn submit(&mut self, h: &Handles) {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        if text.is_empty() {
            return;
        }
        self.history.push(text.clone());
        self.hist_idx = self.history.len();
        self.pending.clear();
        self.suggestion = None;
        self.rebuild_cmd_uses();
        self.scroll_to_bottom();
        self.dispatch(text, h);
    }

    /// Drop the composer's draft line. Unlike `take_input` this keeps it out of
    /// the history (a Ctrl+C/Esc'd draft isn't a command the user ran) and
    /// leaves the queued-input undo buffer alone.
    pub(super) fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
        self.suggest_idx = 0;
        self.suggestion = None;
    }

    pub(super) fn queue_input(&mut self) {
        if let Some(text) = self.take_input() {
            if !text.is_empty() {
                self.queued.push(text);
            }
        }
    }

    fn take_input(&mut self) -> Option<String> {
        let text = std::mem::take(&mut self.input);
        self.cursor = 0;
        if text.is_empty() {
            return None;
        }
        self.history.push(text.clone());
        self.hist_idx = self.history.len();
        self.pending.clear();
        self.rebuild_cmd_uses();
        self.scroll_to_bottom();
        Some(text)
    }



    pub(super) fn set_busy(&mut self) {
        self.awaiting_response = false;
        self.current_activity = None;
        self.mode = Mode::Busy;
        self.busy_since = Some(Instant::now());
    }

    /// Back to Busy after a mid-turn prompt (approval, question, password)
    /// without restarting the elapsed timer — the turn never stopped.
    pub(super) fn resume_busy(&mut self) {
        self.mode = Mode::Busy;
        if self.busy_since.is_none() {
            self.busy_since = Some(Instant::now());
        }
    }

    pub(super) fn clear_busy(&mut self) {
        self.awaiting_response = false;
        self.current_activity = None;
        self.mode = Mode::Idle;
        self.busy_since = None;
        self.tool_running = None;
    }

    pub(super) fn dispatch(&mut self, text: String, h: &Handles) {
        if self.mode == Mode::Busy {
            self.queued.push(text);
            return;
        }
        if let Some(cmd) = text.strip_prefix('/') {
            self.run_command(cmd, h);
            return;
        }
        // `!cmd` runs a shell command directly (Claude Code style): output shows
        // like a tool result and lands in the conversation for the model.
        if let Some(cmd) = text.strip_prefix('!') {
            let cmd = cmd.trim();
            if !cmd.is_empty() && !text.starts_with("!!") {
                self.push(Kind::User, format!("! {cmd}"));
                self.set_busy();
                let _ = h.cmd_tx.send(WorkerCmd::Shell(cmd.to_string()));
                return;
            }
        }
        self.send_prompt(text, h);
    }

    /// Start a turn with `text` as the user's prompt (attachments expanded).
    pub(super) fn send_prompt(&mut self, text: String, h: &Handles) {
        self.send_prompt_as(text.clone(), text, h);
    }

    /// Like `send_prompt`, but the transcript shows `shown` (e.g. `/review`)
    /// instead of the full expanded template the worker receives.
    pub(super) fn send_prompt_as(&mut self, shown: String, text: String, h: &Handles) {
        // Echo the prompt into the transcript (with its full-width band) before
        // the turn starts, so the user can see what they asked for while
        // scrolling back — and so the attachments they referenced are named.
        // A long paste is echoed in part; the model still gets all of it.
        let shown_typed = shown.clone();
        self.push(Kind::User, echo_excerpt(&shown));
        let (task_text, attached) = expand_attachments(&text);
        let (images, img_names) = extract_images(&text);
        let mut all = attached;
        all.extend(img_names);
        if !all.is_empty() {
            self.push(Kind::Notice, format!("attached: {}", all.join(", ")));
        }
        self.set_busy();
        self.awaiting_response = true;
        self.current_activity = Some("waiting for model…".into());
        let _ = h.cmd_tx.send(WorkerCmd::User { text: task_text, images, typed: shown_typed });
    }

    pub(super) fn run_command(&mut self, cmd: &str, h: &Handles) {
        // `/model ` (trailing space) must not become `SetModel("")` and persist
        // an empty model name; a blank argument is no argument.
        let (cmd, _arg) = match cmd.trim().split_once(char::is_whitespace) {
            Some((c, a)) => (c, Some(a.trim()).filter(|a| !a.is_empty())),
            None => (cmd.trim(), None),
        };
        match cmd {
            "model" => {
                if let Some(arg) = _arg {
                    let _ = h.cmd_tx.send(WorkerCmd::SetModel(arg.to_string()));
                } else {
                    let _ = h.cmd_tx.send(WorkerCmd::ListModels);
                    self.mode = Mode::Select;
                }
            }
            "new" => {
                let _ = h.cmd_tx.send(WorkerCmd::New);
                self.transcript.clear();
                self.reset_session_counters();
                self.dirty();
            }
            "config" => {
                self.mode = Mode::Settings { cursor: 0, edit: None };
            }
            "provider" => self.provider_command(_arg, h),
            "effort" => match _arg {
                Some(a) => {
                    let level = crate::config::normalize_reasoning(a);
                    if level == "auto" && !a.eq_ignore_ascii_case("auto") {
                        self.push(Kind::ErrorK, format!("unknown effort '{a}'"));
                        self.show_options("/effort");
                    } else {
                        self.set_reasoning(&level, h);
                        self.push(Kind::Notice, format!("reasoning effort: {level}"));
                    }
                }
                None => {
                    self.push(Kind::Notice, format!("reasoning effort: {}", self.settings.reasoning));
                    self.show_options("/effort");
                }
            },
            "compact" => {
                self.set_busy();
                let _ = h.cmd_tx.send(WorkerCmd::Compact(_arg.map(|a| a.trim().to_string()).filter(|a| !a.is_empty())));
            }
            "diff" => self.show_diff(),
            "undo" => self.undo_checkpoint(h),
            "rewind" => {
                let _ = h.cmd_tx.send(WorkerCmd::ListTurns);
                self.mode = Mode::Select;
            }
            "resume" => match _arg {
                Some(key) => {
                    let _ = h.cmd_tx.send(WorkerCmd::Resume(key.to_string()));
                }
                None => self.open_session_picker(h),
            },
            "rename" => match _arg {
                Some(name) => {
                    let _ = h.cmd_tx.send(WorkerCmd::Rename(name.to_string()));
                }
                None => self.show_options("/rename"),
            },
            "fork" => {
                let _ = h.cmd_tx.send(WorkerCmd::Fork);
            }
            "export" => {
                let _ = h.cmd_tx.send(WorkerCmd::Export(_arg.unwrap_or("").to_string()));
            }
            "sessions" => {
                if _arg.is_none() {
                    self.show_options("/sessions");
                }
                let _ = h.cmd_tx.send(WorkerCmd::Sessions(_arg.unwrap_or("").to_string()));
            }
            "cost" => {
                for line in self.cost_lines() {
                    self.push(Kind::Notice, line);
                }
            }
            "status" => self.show_status(),
            "review" => {
                let base = _arg.map(str::trim).filter(|a| !a.is_empty()).unwrap_or("the default branch (main or master)");
                let task = format!(
                    "Review the code changes in this repository like a careful senior engineer. \
                     First run `git status --short` and `git diff` for uncommitted work, then \
                     `git log --oneline {base}..HEAD` and `git diff {base}...HEAD` for committed \
                     work on this branch (skip whichever is empty; if {base} doesn't exist, review \
                     the last few commits). Report findings ordered by severity: bugs and \
                     correctness first, then security, then simplifications. For each, give \
                     file:line, what is wrong, and a concrete fix. Do not edit any files. End \
                     with a one-line verdict: ship / fix first."
                );
                self.push(Kind::Notice, format!("reviewing changes vs {base}…"));
                let shown = format!("/review{}", _arg.map(|a| format!(" {}", a.trim())).unwrap_or_default());
                self.send_prompt_as(shown, task, h);
            }
            "permissions" | "perms" => self.permissions_command(_arg, h),
            "hooks" => {
                for line in crate::hooks::Hooks::load().describe() {
                    self.push(Kind::Notice, line);
                }
            }
            "commands" | "skills" => {
                let n = self.reload_custom_commands();
                if n == 0 {
                    self.push(
                        Kind::Notice,
                        "no custom commands. Add .haikucode/commands/<name>.md (body = prompt, \
                         $ARGUMENTS = what you type after /name) or ~/config/settings/haikucode/commands/.",
                    );
                } else {
                    self.push(Kind::Notice, format!("{n} custom command(s):"));
                    let rows: Vec<String> = self
                        .custom_cmds
                        .iter()
                        .map(|c| format!("  /{:<14} {}  ({})", c.name, c.description, c.source.display()))
                        .collect();
                    for r in rows {
                        self.push(Kind::Notice, r);
                    }
                }
            }
            "reset" => {
                let _ = h.cmd_tx.send(WorkerCmd::Reset);
                self.transcript.clear();
                self.dirty();
            }
            "auto" | "yolo" => {
                let on = match _arg.map(str::to_ascii_lowercase).as_deref() {
                    Some("on") => true,
                    Some("off") => false,
                    None => self.perm() != crate::agent::PERM_AUTO,
                    Some(other) => {
                        self.push(Kind::ErrorK, format!("unknown option '{other}'"));
                        self.show_options("/yolo");
                        return;
                    }
                };
                let next = if on { crate::agent::PERM_AUTO } else { crate::agent::PERM_ASK };
                self.set_perm(next);
                self.push(Kind::Notice, format!("permissions: {}", perm_name(self.perm())));
            }
            "mcp" => {
                let _ = h.cmd_tx.send(match _arg {
                    None => WorkerCmd::ListMcp,
                    Some(a) => WorkerCmd::Mcp(a.to_string()),
                });
            }
            "memory" => {
                match crate::tools::load_memory() {
                    Ok(Some(text)) => self.push(Kind::Notice, format!("memory:\n{text}")),
                    Ok(None) => self.push(Kind::Notice, String::from("no persistent memory.")),
                    Err(e) => self.push(Kind::ErrorK, format!("{e}")),
                }
            }
            "theme" => {
                if let Some(name) = _arg {
                    let p = palette_by_name(name);
                    self.set_palette(p);
                    crate::config::Config::persist_theme(p.name);
                    self.push(Kind::Notice, format!("theme set to {}", p.name));
                } else {
                    let current = THEMES
                        .iter()
                        .position(|&n| n == self.palette.name)
                        .unwrap_or(0);
                    self.preview_theme(current, self.palette.name.to_string());
                }
            }
            "statusline" => match _arg.map(str::trim).filter(|a| !a.is_empty()) {
                None => self.open_statusline_picker(),
                Some(a) if a.eq_ignore_ascii_case("reset") || a.eq_ignore_ascii_case("default") => {
                    self.set_statusline(statusline::DEFAULT.iter().map(|s| s.to_string()).collect())
                }
                Some(a) => match statusline::parse(a) {
                    Ok(list) => self.set_statusline(list),
                    Err(e) => {
                        self.push(Kind::ErrorK, format!("/statusline: {e}"));
                        self.show_options("/statusline");
                    }
                },
            },
            "init" => {
                self.set_busy();
                let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let task = format!(
                    "Summarise this codebase into a HAIKUCODE.md (or AGENTS.md / CLAUDE.md \
                     if one already exists) that Haiku Code (a coding agent) can read at \
                     startup. Include key architecture, conventions, and safety notes. \
                     Keep it under 300 lines. Use the existing HAIKUCODE.md as a starting \
                     point if it exists. Working directory: {}",
                    cwd.display()
                );
                let _ = h.cmd_tx.send(WorkerCmd::User { text: task, images: vec![], typed: "/init".into() });
            }
            "update" => match _arg {
                None | Some("--force") => {
                    self.push(Kind::Notice, "checking GitHub for a newer Haiku Code…");
                    let _ = h.cmd_tx.send(WorkerCmd::Update(_arg.is_some()));
                }
                Some(other) => {
                    self.push(Kind::ErrorK, format!("unknown option '{other}'"));
                    self.show_options("/update");
                }
            },
            "thinking" => match _arg.map(str::to_ascii_lowercase).as_deref() {
                None => self.toggle_thinking(),
                Some("full" | "on") if !self.show_thinking => self.toggle_thinking(),
                Some("brief" | "off") if self.show_thinking => self.toggle_thinking(),
                Some("full" | "on" | "brief" | "off") => {}
                Some(other) => {
                    self.push(Kind::ErrorK, format!("unknown option '{other}'"));
                    self.show_options("/thinking");
                }
            },
            "clear" => {
                self.transcript.clear();
                self.dirty();
            }
            "help" => self.show_help(),
            "exit" | "quit" | "q" => self.should_quit = true,
            _ => {
                if let Some(c) = self.custom_cmds.iter().find(|c| c.name == cmd) {
                    let body = crate::commands::expand(&c.body, _arg.unwrap_or(""));
                    let shown = format!("/{}{}", c.name, _arg.map(|a| format!(" {}", a.trim())).unwrap_or_default());
                    self.push(Kind::Notice, format!("{shown} → {}", c.source.display()));
                    self.send_prompt_as(shown, body, h);
                } else if cmd.starts_with("mcp__") {
                    // An MCP prompt: the worker fetches it, then hands it back
                    // (McpPromptReady) to send as an ordinary turn.
                    self.set_busy();
                    let _ = h.cmd_tx.send(WorkerCmd::McpPrompt {
                        name: cmd.to_string(),
                        args: _arg.unwrap_or("").to_string(),
                    });
                } else {
                    self.push(Kind::ErrorK, format!("unknown command: /{cmd} (try /help)"));
                }
            }
        }
    }

    /// `/diff`: everything that changed since this session started — the
    /// auto-commit checkpoints plus the uncommitted working tree.
    fn show_diff(&mut self) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        if !crate::tools::in_git_repo(&cwd) {
            self.push(Kind::Notice, "not a git repository.");
            return;
        }
        let base = self.session_rev.clone();
        let (stat, full) = match &base {
            Some(rev) => (
                crate::tools::git_run(&cwd, &["diff", "--stat", rev], 20),
                crate::tools::git_run(&cwd, &["diff", rev], 20),
            ),
            None => (
                crate::tools::git_run(&cwd, &["diff", "--stat", "HEAD"], 20),
                crate::tools::git_run(&cwd, &["diff", "HEAD"], 20),
            ),
        };
        match (stat, full) {
            (Ok(stat), Ok(full)) if stat.trim().is_empty() && full.trim().is_empty() => {
                self.push(Kind::Notice, "no changes since this session started.");
            }
            (Ok(stat), Ok(full)) => {
                let label = base.as_deref().map(|r| format!("since {}", &r[..r.len().min(7)])).unwrap_or_else(|| "uncommitted".into());
                self.push(Kind::Notice, format!("changes {label}:"));
                for l in stat.lines() {
                    self.push(Kind::Notice, format!("  {l}"));
                }
                let mut lines: Vec<&str> = full.lines().collect();
                let total = lines.len();
                if total > 400 {
                    lines.truncate(400);
                }
                let text = lines.join("\n");
                self.handle_diff_text(&text);
                if total > 400 {
                    self.push(Kind::Notice, format!("… {} more lines (run `!git diff` for all)", total - 400));
                }
            }
            (Err(e), _) | (_, Err(e)) => self.push(Kind::ErrorK, format!("git diff failed: {e}")),
        }
    }

    fn handle_diff_text(&mut self, d: &str) {
        for (i, ln) in d.lines().enumerate() {
            let kind = if ln.starts_with('+') {
                Kind::DiffAdd
            } else if ln.starts_with('-') {
                Kind::DiffDel
            } else {
                Kind::DiffCtx
            };
            self.transcript.push(TLine { kind, text: ln.to_string(), lead: i == 0, color: None });
        }
        self.after_push();
    }

    /// `/undo`: revert the most recent haikucode checkpoint commit (only ever a
    /// commit haikucode made, so a user's own commit is never touched).
    fn undo_checkpoint(&mut self, h: &Handles) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
        if !crate::tools::in_git_repo(&cwd) {
            self.push(Kind::Notice, "not a git repository.");
            return;
        }
        let subject = match crate::tools::git_run(&cwd, &["log", "-1", "--format=%h %s"], 10) {
            Ok(s) => s.trim().to_string(),
            Err(e) => {
                self.push(Kind::ErrorK, format!("git log failed: {e}"));
                return;
            }
        };
        let Some((sha, msg)) = subject.split_once(' ') else {
            self.push(Kind::Notice, "nothing to undo.");
            return;
        };
        if !msg.starts_with("haikucode:") {
            self.push(Kind::Notice, format!("last commit isn't a haikucode checkpoint ({subject}); nothing undone."));
            return;
        }
        match crate::tools::git_run(&cwd, &["revert", "--no-edit", "HEAD"], 30) {
            Ok(_) => {
                self.push(Kind::Notice, format!("reverted {subject}"));
                let _ = h.cmd_tx.send(WorkerCmd::Inject(format!(
                    "[The user ran /undo: commit {sha} \"{msg}\" was reverted. The files are back to \
                     their previous state; re-read them before editing again.]"
                )));
                self.git_checked_at = None;
            }
            Err(e) => {
                let _ = crate::tools::git_run(&cwd, &["revert", "--abort"], 10);
                self.push(Kind::ErrorK, format!("revert failed: {e}"));
            }
        }
    }

    pub(super) fn add_spent(&mut self, s: money::Spent) {
        self.sess_prompt += s.prompt;
        self.sess_completion += s.completion;
        self.sess_cached += s.cached;
        self.sess_written += s.written;
    }

    /// Session cost so far, cache hits at the cached-input price; `None` when
    /// the active server's price is unknown (a custom provider without a
    /// price list) — the cost readout is hidden.
    pub(super) fn session_cost(&self) -> Option<f64> {
        let live = self.cost_known.then(|| self.unbanked().cost(self.price_in, self.price_cached_in.unwrap_or(self.price_in * 0.1), self.price_out));
        match live {
            Some(c) => Some(self.banked_cost + c),
            // Priced spend from earlier stays visible on an unpriced server.
            None => (self.banked_cost > 0.0).then_some(self.banked_cost),
        }
    }

    /// Tokens spent since the last bank (see `banked_cost`).
    fn unbanked(&self) -> money::Spent {
        money::Spent {
            prompt: self.sess_prompt.saturating_sub(self.banked_at.prompt),
            completion: self.sess_completion.saturating_sub(self.banked_at.completion),
            cached: self.sess_cached.saturating_sub(self.banked_at.cached),
            written: self.sess_written.saturating_sub(self.banked_at.written),
        }
    }

    /// Price the spend so far at the current list before it changes.
    pub(super) fn bank_cost(&mut self) {
        if let Some(c) = self.session_cost() {
            self.banked_cost = c;
        }
        self.banked_at = money::Spent {
            prompt: self.sess_prompt,
            completion: self.sess_completion,
            cached: self.sess_cached,
            written: self.sess_written,
        };
    }

    /// `/new` and `/resume` start a new session: its counters start at zero.
    pub(super) fn reset_session_counters(&mut self) {
        self.sess_prompt = 0;
        self.sess_completion = 0;
        self.sess_cached = 0;
        self.sess_written = 0;
        self.banked_cost = 0.0;
        self.banked_at = money::Spent::default();
    }

    /// Change the reasoning setting: live in the worker, persisted, and shown.
    pub(super) fn set_reasoning(&mut self, level: &str, h: &Handles) {
        self.settings.reasoning = level.to_string();
        let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::Reasoning(level.to_string())));
    }

    pub(super) fn cost_lines(&self) -> Vec<String> {
        let tokens = self.sess_prompt + self.sess_completion;
        // A custom server without a price list: the cost is hidden.
        let cost = match self.session_cost() {
            Some(c) => format!(" · {}", money::fmt_cost_tagged(c, &self.price_currency)),
            None => String::new(),
        };
        let mut v = vec![format!(
            "session: {} tokens ({} in / {} out){}",
            humanize(tokens),
            humanize(self.sess_prompt),
            humanize(self.sess_completion),
            cost
        )];
        if self.sess_cached + self.sess_written > 0 {
            v.push(format!(
                "cache: {} input tokens read from cache, {} written",
                humanize(self.sess_cached),
                humanize(self.sess_written)
            ));
        }
        v.push(format!(
            "context: {} / {} tokens ({}%)",
            humanize(self.last_prompt_tokens as u64),
            humanize(self.ctx_limit as u64),
            (self.last_prompt_tokens as f64 / self.ctx_limit as f64 * 100.0).round() as u32
        ));
        if let Some(b) = &self.balance {
            v.push(format!("balance: {}", b.render_tagged()));
        }
        v
    }

    fn show_status(&mut self) {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        let git = match crate::tools::git_head(&std::path::PathBuf::from(&cwd)) {
            Some((b, dirty)) => format!("{b}{}", if dirty { " (dirty)" } else { "" }),
            None => "not a repo".into(),
        };
        let policy = crate::policy::Policy::load();
        let hooks = crate::hooks::Hooks::load();
        let mut lines = vec![
            format!("Haiku Code {}", env!("CARGO_PKG_VERSION")),
            format!(
                "model:       {} ({} · {})",
                self.model_info,
                self.settings.provider,
                crate::config::method_short(&self.settings.provider, self.settings.auth())
            ),
            format!("cwd:         {cwd}"),
            format!("git:         {git}"),
            format!("permissions: {} · {} allow / {} deny rule(s)", perm_name(self.perm()), policy.allow.len(), policy.deny.len()),
            format!("hooks:       {}", hooks.hooks.len()),
            format!("mcp servers: {}", self.settings.mcp_servers.len()),
            format!("custom cmds: {}", self.custom_cmds.len()),
            format!("auto-commit: {}", if self.settings.auto_commit { "on" } else { "off" }),
            format!("session:     {}", self.session_label),
        ];
        lines.extend(self.cost_lines());
        for l in lines {
            self.push(Kind::Notice, l);
        }
    }

    /// `/permissions [allow|deny <rule> | remove <rule> | reload]`.
    fn permissions_command(&mut self, arg: Option<&str>, h: &Handles) {
        let arg = arg.map(str::trim).unwrap_or("");
        let (verb, rule) = arg.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((arg, ""));
        match verb {
            "" | "list" => {
                for line in crate::policy::Policy::load().describe() {
                    self.push(Kind::Notice, line);
                }
                self.show_options("/permissions");
            }
            "allow" | "deny" if !rule.is_empty() => {
                if crate::policy::Rule::parse(rule).is_none() {
                    self.push(Kind::ErrorK, format!("bad rule: {rule}"));
                    return;
                }
                let path = crate::policy::persist_target();
                match crate::policy::persist_rule(&path, verb, rule) {
                    Ok(()) => {
                        self.push(Kind::Notice, format!("{verb} {rule} → {}", path.display()));
                        let _ = h.cmd_tx.send(WorkerCmd::ReloadSettings);
                    }
                    Err(e) => self.push(Kind::ErrorK, format!("could not save: {e}")),
                }
            }
            "remove" | "rm" if !rule.is_empty() => {
                let n = crate::policy::remove_rule(rule);
                self.push(Kind::Notice, format!("removed {rule} from {n} file(s)"));
                let _ = h.cmd_tx.send(WorkerCmd::ReloadSettings);
            }
            "reload" => {
                let _ = h.cmd_tx.send(WorkerCmd::ReloadSettings);
            }
            _ => {
                self.push(Kind::ErrorK, format!("/permissions {arg}: missing or unknown option"));
                self.show_options("/permissions");
            }
        }
    }

    /// List a command's options (from `SLASH_OPTIONS`) under a heading.
    pub(super) fn show_options(&mut self, cmd: &str) {
        let rows: Vec<String> = types::options_of(cmd)
            .map(|(o, d)| format!("  {:<22} {d}", o.trim_end()))
            .collect();
        if rows.is_empty() {
            return;
        }
        self.push(Kind::Notice, format!("{cmd} options:"));
        for r in rows {
            self.push(Kind::Notice, r);
        }
    }

    fn show_help(&mut self) {
        self.push(Kind::Notice, String::from("commands:"));
        for (name, desc) in SLASH_COMMANDS {
            self.push(Kind::Notice, format!("  {name:<12} {desc}"));
        }
        if !self.custom_palette.is_empty() {
            self.push(Kind::Notice, String::from("custom commands:"));
            let rows: Vec<String> = self.custom_palette.iter().map(|(n, d)| format!("  {n:<12} {d}")).collect();
            for r in rows {
                self.push(Kind::Notice, r);
            }
        }
        self.push(Kind::Notice, String::from("  @file       attach a file"));
        self.push(Kind::Notice, String::from("  !cmd        run a shell command (output goes to the model too)"));
        self.push(Kind::Notice, "  Shift/Alt+Enter, Ctrl+J, \\+Enter  new line (Enter sends; never inside a word)");
        self.push(Kind::Notice, "  (Haiku's Terminal can't tell Shift+Enter from Enter: use \\+Enter or Ctrl+J)");
        self.push(Kind::Notice, "  ↑/↓         move between draft lines, else browse history");
        self.push(Kind::Notice, "  Home/End    start / end of the line");
        self.push(Kind::Notice, "  Ctrl+A/E    start / end of the line (readline)");
        self.push(Kind::Notice, "  Ctrl+B/F    back / forward a char · Alt+B/F by word");
        self.push(Kind::Notice, "  Ctrl+K/U    delete to the end / start of the line");
        self.push(Kind::Notice, "  Ctrl+W, Alt+Backspace  delete the word before · Ctrl+Y yanks it back");
        self.push(Kind::Notice, "  Ctrl+D      delete the char under the cursor (empty line: quit)");
        self.push(Kind::Notice, "  (Ctrl+<letter> works on every terminal. Alt+<letter> is the fragile");
        self.push(Kind::Notice, "   one: on Haiku it is Terminal's menu key (Alt+F = Find) and needs");
        self.push(Kind::Notice, "   Settings → \"Use Option as Meta key\", on macOS Option-as-Meta,");
        self.push(Kind::Notice, "   and tmux/screen keep Ctrl+B/Ctrl+A for themselves)");
        self.push(Kind::Notice, "  PgUp/PgDn   scroll the output a page");
        self.push(Kind::Notice, "  Ctrl/Alt+↑/↓  scroll the output 3 lines");
        self.push(Kind::Notice, "  drag        select output or input text (releasing copies it)");
        self.push(Kind::Notice, "  click       in the input: put the caret there");
        self.push(Kind::Notice, String::from("  Tab         autocomplete"));
        self.push(Kind::Notice, String::from("  Shift+Tab   cycle permissions (ask / YOLO / plan; remembered)"));
        self.push(Kind::Notice, String::from("  approvals   Y yes · N no · P don't ask again for this pattern · A YOLO (approve everything)"));
        self.push(Kind::Notice, String::from("  Esc         interrupt the agent / clear the line"));
        self.push(Kind::Notice, String::from("  Ctrl+C      clear the line (twice = quit)"));
        self.push(Kind::Notice, String::from("  Ctrl+L      force clear/repaint"));
        self.push(Kind::Notice, String::from("  Ctrl+T      show full thinking / thoughts and live status"));
    }
}
