use super::*;

impl App {
    pub fn on_key(&mut self, key: KeyEvent, h: &Handles) {
        // Any key gives up a mouse selection: it is a momentary view of the
        // text, and keeping the highlight after the user moves on only
        // confuses the next frame.
        self.select = None;
        // Cycle the permission mode in any state, except while a masked prompt
        // or an ask_user question is capturing every key. Shift+Tab is the
        // primary binding: it arrives as BackTab on ANSI terminals, or as
        // Tab+SHIFT under the Kitty keyboard protocol (which setup_terminal
        // pushes) — so both spellings must be recognized, and neither may fall
        // through to the composer's Tab autocomplete. The Pi's framebuffer
        // console can't report Shift+Tab at all (its keymap has no shift
        // binding for Tab), so Ctrl+P is the console-safe alias.
        if !matches!(self.mode, Mode::Password { .. } | Mode::Question { .. } | Mode::ProviderInput { .. }) {
            let shift_tab = key.code == KeyCode::BackTab
                || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT));
            let ctrl_p = matches!(key.code, KeyCode::Char('p') | KeyCode::Char('P'))
                && key.modifiers.contains(KeyModifiers::CONTROL);
            if shift_tab || ctrl_p {
                self.cycle_perm();
                return;
            }
        }
        // Ctrl+T: full thinking/tool output vs brief summaries.
        if matches!(key.code, KeyCode::Char('t') | KeyCode::Char('T'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && !matches!(self.mode, Mode::Password { .. } | Mode::Question { .. } | Mode::ProviderInput { .. })
        {
            self.toggle_thinking();
            return;
        }
        // Ctrl+L: full repaint in every mode (advertised in /help; recovers
        // from a desynced screen after e.g. a background job wrote to the tty).
        if matches!(key.code, KeyCode::Char('l') | KeyCode::Char('L'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.force_clear = true;
            self.dirty();
            return;
        }
        match self.mode {
            Mode::Password { .. } => self.on_key_password(key),
            Mode::Question { .. } => self.on_key_question(key),
            Mode::Approval(_) => self.on_key_approval(key, h),
            Mode::Settings { .. } => self.on_key_settings(key, h),
            Mode::Select => self.on_key_select(key, h),
            Mode::ProviderInput { step } => self.on_key_provider_input(step, key, h),
            Mode::ThemeSelect { .. } => self.on_key_themeselect(key, h),
            Mode::StatuslineSelect { .. } => self.on_key_statusline(key),
            Mode::Busy => self.on_key_busy(key, h),
            Mode::Idle => self.on_key_idle(key, h),
        }
    }

    pub(super) fn do_esc(&mut self, h: &Handles) {
        match self.mode {
            Mode::Password { .. } => self.cancel_password(),
            Mode::Question { .. } => self.cancel_question(),
            Mode::ProviderInput { step } => self.wizard_back(step, h),
            Mode::Select => self.select_escape(h),
            Mode::Approval(_) => {
                let _ = h.appr_tx.send(ApprovalResponse::No);
                self.interrupt(h);
            }
            Mode::Settings { .. } | Mode::ThemeSelect { .. } => {
                self.clear_busy();
                self.picker = None;
            }
            Mode::StatuslineSelect { .. } => self.cancel_statusline(),
            Mode::Busy => self.interrupt(h),
            Mode::Idle => {
                self.busy_since = None;
                self.suggestion = None;
            }
        }
    }

    fn on_key_password(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.cancel_password(),
            KeyCode::Enter => {
                let val = std::mem::take(&mut self.pw_input);
                if let Some(tx) = self.pw_reply.take() {
                    let _ = tx.send(Some(val));
                }
                self.resume_busy();
            }
            KeyCode::Backspace => {
                self.pw_input.pop();
            }
            KeyCode::Char(c) => {
                self.pw_input.push(caps_char(&key, c));
            }
            _ => {}
        }
    }

    fn cancel_password(&mut self) {
        if let Some(tx) = self.pw_reply.take() {
            let _ = tx.send(None);
        }
        self.pw_input.clear();
        self.resume_busy();
    }

    fn on_key_question(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.cancel_question(),
            KeyCode::Enter => {
                let val = std::mem::take(&mut self.q_input);
                if let Some(tx) = self.q_reply.take() {
                    let _ = tx.send(Some(val));
                }
                self.resume_busy();
            }
            KeyCode::Backspace => {
                self.q_input.pop();
            }
            KeyCode::Char(c) => {
                self.q_input.push(caps_char(&key, c));
            }
            _ => {}
        }
    }

    fn cancel_question(&mut self) {
        if let Some(tx) = self.q_reply.take() {
            let _ = tx.send(None);
        }
        self.q_input.clear();
        self.resume_busy();
    }

    pub(super) fn cycle_perm(&mut self) {
        let next = match self.perm() {
            crate::agent::PERM_ASK => crate::agent::PERM_AUTO,
            crate::agent::PERM_AUTO => crate::agent::PERM_PLAN,
            _ => crate::agent::PERM_ASK,
        };
        self.set_perm(next);
    }

    /// Switch the permission mode and save it, so the next session starts in
    /// the same mode (like effort and the brief view).
    pub(super) fn set_perm(&mut self, p: u8) {
        self.perm.store(p, Ordering::Relaxed);
        let name = perm_name(p);
        self.settings.permission = name.to_string();
        Config::persist_patch(&ConfigPatch::Permission(name.to_string()));
    }

    pub(super) fn perm(&self) -> u8 {
        self.perm.load(Ordering::Relaxed)
    }

    fn on_key_approval(&mut self, key: KeyEvent, h: &Handles) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                let _ = h.appr_tx.send(ApprovalResponse::Yes);
                self.set_busy();
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                let _ = h.appr_tx.send(ApprovalResponse::No);
                self.set_busy();
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                // The worker flips the shared flag itself; save the mode too.
                self.set_perm(crate::agent::PERM_AUTO);
                let _ = h.appr_tx.send(ApprovalResponse::Always);
                self.set_busy();
            }
            KeyCode::Char('p') | KeyCode::Char('P') => {
                if let Some(rule) = self.appr_rule.take() {
                    let _ = h.appr_tx.send(ApprovalResponse::Rule(rule));
                    self.set_busy();
                }
            }
            KeyCode::Esc => {
                // Esc = deny AND stop the turn (Claude Code / Codex semantics);
                // N denies but lets the model carry on.
                let _ = h.appr_tx.send(ApprovalResponse::No);
                self.interrupt(h);
            }
            _ => {}
        }
    }

    fn on_key_busy(&mut self, key: KeyEvent, h: &Handles) {
        // Esc / Ctrl+C / Ctrl+D stop the turn; everything else is composer
        // editing, so the readline keys work while the agent runs too. Ctrl+D
        // keeps its readline meaning (delete-char) while there is a draft, the
        // same as idle — interrupting has Esc and Ctrl+C.
        if key.code == KeyCode::Esc {
            self.interrupt(h);
            return;
        }
        if ctrl_c_or_d(&key) {
            if matches!(key.code, KeyCode::Char('d')) && !self.input.is_empty() {
                self.delete_forward();
            } else {
                self.interrupt(h);
            }
            return;
        }
        if self.on_key_readline(key) {
            return;
        }
        match key.code {
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => self.insert_newline(),
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => self.paste_clipboard_image(),
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_char(caps_char(&key, c));
            }
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    let pos = self.byte_at(self.cursor);
                    self.input.remove(pos);
                }
            }
            KeyCode::Enter => {
                if !self.enter_inserts_newline(&key) {
                    self.queue_input();
                }
            }
            // Ctrl/Alt+←/→ move by word, as idle — Ctrl+←/→ is the word move
            // that survives Haiku's Terminal, so it can't go char-only here.
            KeyCode::Left | KeyCode::Right
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.cursor = if key.code == KeyCode::Left { self.prev_word() } else { self.next_word() };
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.char_len()),
            KeyCode::Home => self.cursor = self.line_bounds().0,
            KeyCode::End => self.cursor = self.line_bounds().1,
            // Ctrl/Alt+↑/↓ roll the transcript while the agent runs; plain
            // ↑/↓ walk a multi-line draft, then the prompt history (as idle).
            KeyCode::Up | KeyCode::Down
                if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.scroll_lines(if key.code == KeyCode::Up { -SCROLL_STEP } else { SCROLL_STEP })
            }
            KeyCode::Up => {
                if !self.move_row(false) {
                    self.history_prev()
                }
            }
            KeyCode::Down => {
                if !self.move_row(true) {
                    self.history_next()
                }
            }
            KeyCode::PageUp => self.scroll_page(true),
            KeyCode::PageDown => self.scroll_page(false),
            _ => {}
        }
    }

    /// Arms — or fires — the Ctrl+C/Ctrl+D (and empty-composer Esc) quit. True
    /// means the app is quitting and the caller should stop handling the key.
    fn double_press_quit(&mut self) -> bool {
        let now = Instant::now();
        if let Some(t) = self.last_ctrl_c {
            if now.duration_since(t) < DOUBLE_PRESS_TIMEOUT {
                self.should_quit = true;
                return true;
            }
        }
        self.last_ctrl_c = Some(now);
        false
    }

    fn interrupt(&mut self, h: &Handles) {
        h.shared.cancel.store(true, Ordering::Relaxed);
        // The half-typed draft stays in the composer (it used to be queued,
        // which sent it as the next prompt the moment the turn ended), and
        // queued follow-ups are held until the user speaks again.
        self.queue_paused = true;
        self.clear_busy();
        // Clear the suggestion so the user sees the hint again.
        self.suggestion = None;
    }

    fn on_key_idle(&mut self, key: KeyEvent, h: &Handles) {
        // Ctrl+C / Ctrl+D quit on a double press. Draft text in the composer
        // must not swallow the first press (it used to: with a non-empty line
        // the key fell through to the `Char` arm's CONTROL guard and did
        // nothing at all) — a single press clears the line, Codex/Claude-Code
        // style, and still arms the timer so the second press quits. Ctrl+D on
        // a non-empty line is readline's delete-char instead (as in a shell):
        // only an empty line arms the quit.
        if ctrl_c_or_d(&key) {
            if matches!(key.code, KeyCode::Char('d')) && !self.input.is_empty() {
                self.delete_forward();
                return;
            }
            if self.double_press_quit() {
                return;
            }
            self.clear_input();
            return;
        }
        // Esc clears the line instead of quitting on a single press; with an
        // empty composer it still arms the double-press exit, as before.
        if key.code == KeyCode::Esc {
            if !self.input.is_empty() {
                self.clear_input();
                return;
            }
            self.double_press_quit();
            return;
        }
        self.last_ctrl_c = None;

        if self.on_key_readline(key) {
            return;
        }

        match key.code {
            // Ctrl+J is a literal line feed: a newline everywhere, including the
            // Pi's framebuffer console where Shift+Enter is plain Enter.
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => self.insert_newline(),
            KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.paste_clipboard_image();
                self.suggest_idx = 0;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if c == '/' && self.input.is_empty() {
                    self.suggest_idx = 0;
                }
                self.insert_char(caps_char(&key, c));
                self.suggest_idx = 0;
            }
            KeyCode::Tab => {
                if !self.input.starts_with('/') || self.input.contains(char::is_whitespace) {
                    // Cycle suggestion
                    if let Some(ref s) = self.suggestion {
                        if !self.input.is_empty() {
                            self.complete();
                            return;
                        }
                        self.input = s.clone();
                        self.cursor = self.char_len();
                        self.suggestion = None;
                        return;
                    }
                    self.complete();
                } else {
                    let sugg = self.slash_suggestions();
                    if !sugg.is_empty() {
                        let idx = self.suggest_idx.min(sugg.len() - 1);
                        self.input = sugg[idx].0.to_string();
                        self.cursor = self.char_len();
                        self.suggest_idx = 0;
                    }
                }
            }
            KeyCode::Backspace => self.on_key_edit(key),
            KeyCode::Delete => self.on_key_edit(key),
            // Modified arrows: Alt/Ctrl+←/→ move by word, Alt/Ctrl+↑/↓ roll the
            // transcript a few lines — the scroll gesture that works on every
            // terminal (any terminal without mouse capture sends no
            // wheel events). Plain ↑/↓ are the draft rows and history below.
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
                if key.modifiers.contains(KeyModifiers::ALT)
                    || key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.suggest_idx = 0;
                match key.code {
                    KeyCode::Left => self.cursor = self.prev_word(),
                    KeyCode::Right => self.cursor = self.next_word(),
                    KeyCode::Up => self.scroll_lines(-SCROLL_STEP),
                    KeyCode::Down => self.scroll_lines(SCROLL_STEP),
                    _ => {}
                }
            }
            KeyCode::Left => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                }
            }
            KeyCode::Right => {
                if self.cursor < self.char_len() {
                    self.cursor += 1;
                }
            }
            KeyCode::Home => self.cursor = self.line_bounds().0,
            KeyCode::End => self.cursor = self.line_bounds().1,
            // While the `/` palette is open, plain arrows pick its entry.
            KeyCode::Up | KeyCode::Down if !self.slash_suggestions().is_empty() => {
                let n = self.slash_suggestions().len();
                let cur = self.suggest_idx.min(n - 1);
                self.suggest_idx = if key.code == KeyCode::Up { (cur + n - 1) % n } else { (cur + 1) % n };
            }
            // Plain arrows move between the rows of a multi-line draft; on its
            // first/last row (and so always for a one-line draft) they walk
            // the prompt history, like a shell.
            KeyCode::Up => {
                if !self.move_row(false) {
                    self.history_prev()
                }
            }
            KeyCode::Down => {
                if !self.move_row(true) {
                    self.history_next()
                }
            }
            KeyCode::Enter => {
                if !self.enter_inserts_newline(&key) {
                    self.last_ctrl_c = None;
                    // `/mo` + Enter runs the highlighted `/model`; a command
                    // typed out in full runs as typed.
                    // `/effort hi` + Enter runs `/effort high`; a bare `/effort `
                    // runs as typed (listing the options).
                    let sugg = self.slash_suggestions();
                    let typed_option = self.input.contains(' ') && !self.input.ends_with(' ');
                    let picks = !sugg.is_empty() && (!self.input.contains(' ') || typed_option);
                    if picks && !sugg.iter().any(|(c, _)| *c == self.input) {
                        self.input = sugg[self.suggest_idx.min(sugg.len() - 1)].0.to_string();
                        self.suggest_idx = 0;
                        // An option that still needs its argument: fill the
                        // line and let the user finish it.
                        if self.input.ends_with(' ') {
                            self.cursor = self.char_len();
                            return;
                        }
                    }
                    self.submit(h);
                }
            }
            // Scroll the output transcript; the input field is untouched.
            KeyCode::PageUp => self.scroll_page(true),
            KeyCode::PageDown => self.scroll_page(false),
            _ => {}
        }
    }
}
