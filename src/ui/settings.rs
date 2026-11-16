use super::*;

impl App {
    pub(super) fn preview_theme(&mut self, idx: usize, prev: String) {
        self.set_palette(palette_by_name(THEMES[idx]));
        self.mode = Mode::ThemeSelect { cursor: idx, prev };
    }

    /// Commit the theme at `idx`: persist it and close the picker.
    fn commit_theme(&mut self, idx: usize) {
        let name = THEMES[idx];
        self.set_palette(palette_by_name(name));
        crate::config::Config::persist_theme(name);
        self.push(Kind::Notice, format!("theme set to {name}"));
        self.mode = Mode::Idle;
    }

    pub(super) fn on_key_settings(&mut self, key: KeyEvent, h: &Handles) {
        let (cur, editing) = match &self.mode {
            Mode::Settings { cursor, edit } => (*cursor, edit.clone()),
            _ => return,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(mut buf) = editing {
            match key.code {
                KeyCode::Enter => {
                    self.mode = Mode::Settings { cursor: cur, edit: None };
                    self.commit_setting(SETTING_ROWS[cur], buf, h);
                }
                KeyCode::Esc => self.mode = Mode::Settings { cursor: cur, edit: None },
                KeyCode::Char('u') if ctrl => {
                    self.mode = Mode::Settings { cursor: cur, edit: Some(String::new()) };
                }
                KeyCode::Backspace => {
                    buf.pop();
                    self.mode = Mode::Settings { cursor: cur, edit: Some(buf) };
                }
                KeyCode::Char(c) if !ctrl => {
                    buf.push(caps_char(&key, c));
                    self.mode = Mode::Settings { cursor: cur, edit: Some(buf) };
                }
                _ => {}
            }
            return;
        }
        let n = SETTING_ROWS.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::Settings { cursor: (cur + n - 1) % n, edit: None };
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::Settings { cursor: (cur + 1) % n, edit: None };
            }
            KeyCode::Enter | KeyCode::Right => self.activate_setting(cur, 1, h),
            KeyCode::Left => self.activate_setting(cur, -1, h),
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Idle,
            _ => {}
        }
    }

    /// Act on the `/config` row at cursor position `cur`: cycle choice rows by
    /// `dir`, or open text rows in the edit buffer. Choice changes apply (and
    /// persist) immediately.
    pub(super) fn activate_setting(&mut self, cur: usize, dir: i32, h: &Handles) {
        let cycle = |i: usize, n: usize| ((i as i32 + dir).rem_euclid(n as i32)) as usize;
        let edit_with = |s: String| Mode::Settings { cursor: cur, edit: Some(s) };
        match SETTING_ROWS[cur] {
            0 => {
                // The presets, then the user's own providers.
                let choices = self.settings.provider_choices();
                let i = choices
                    .iter()
                    .position(|(n, _, _)| *n == self.settings.provider)
                    .map(|i| cycle(i, choices.len()))
                    .unwrap_or(0);
                let (p, b, m) = choices[i].clone();
                self.switch_provider(p, b, m, h);
            }
            1 => self.mode = edit_with(self.settings.base_url.clone()),
            2 => self.mode = edit_with(self.settings.model.clone()),
            3 => self.mode = edit_with(String::new()),
            4 => {
                // Sign-in has steps (a key, a browser login, then the
                // model): run the /provider wizard's for this provider.
                let p = self.settings.provider.clone();
                self.setup_provider(&p);
            }
            5 => {
                let levels = crate::config::REASONING_LEVELS;
                let i = levels.iter().position(|l| *l == self.settings.reasoning).unwrap_or(0);
                self.set_reasoning(levels[cycle(i, levels.len())], h);
            }
            6 => {
                let next = if dir >= 0 { (self.perm() + 1) % 3 } else { (self.perm() + 2) % 3 };
                self.set_perm(next);
            }
            7 => {
                let v = !self.settings.auto_commit;
                self.settings.auto_commit = v;
                let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::AutoCommit(v)));
            }
            8 => {
                let i = THEMES
                    .iter()
                    .position(|t| *t == self.palette.name)
                    .map(|i| cycle(i, THEMES.len()))
                    .unwrap_or(0);
                self.set_palette(palette_by_name(THEMES[i]));
                self.settings.theme = THEMES[i].to_string();
                Config::persist_theme(THEMES[i]);
            }
            9 => self.mode = edit_with(self.ctx_limit.to_string()),
            10 => self.mode = edit_with(setting_max_tool_calls(self.settings.max_tool_calls)),
            // A pick-list, USD first. The currency belongs to the active
            // provider's price list: cycling it relabels the cost, it doesn't
            // convert, and it patches that provider's list (not a global one).
            11 => {
                let code = money::cycle_code(&self.settings.price_currency, dir);
                self.patch(ConfigPatch::PriceCurrency(code), h);
                self.sync_prices();
            }
            _ => {}
        }
    }

    /// Commit a text edit from the `/config` panel. `row` indexes
    /// `SETTING_LABELS` (not the cursor position).
    pub(super) fn commit_setting(&mut self, row: usize, val: String, h: &Handles) {
        let val = val.trim().to_string();
        match row {
            1 => {
                if !val.is_empty() {
                    let v = val.trim_end_matches('/').to_string();
                    self.settings.base_url = v.clone();
                    let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::BaseUrl(v)));
                }
            }
            2 => {
                if !val.is_empty() {
                    self.model_info = val.clone();
                    let _ = h.cmd_tx.send(WorkerCmd::SetModel(val));
                }
            }
            3 => {
                // Enter on an empty field used to save an empty key, wiping the
                // saved one; every later launch then fell into the setup wizard.
                if val.is_empty() {
                    self.push(Kind::Notice, "api key unchanged".to_string());
                    return;
                }
                self.settings.api_key = val.clone();
                let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::ApiKey(val)));
            }
            9 => {
                let Ok(v) = val.trim().replace(['_', ','], "").parse::<u32>().map(|n| n.max(1)) else {
                    self.push(Kind::ErrorK, format!("context window must be a number of tokens; kept {}", self.ctx_limit));
                    return;
                };
                self.settings.set_context_window(v);
                self.ctx_limit = v;
                let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::ContextWindow(v)));
            }
            10 => {
                let v = helpers::parse_max_tool_calls(&val);
                self.settings.max_tool_calls = v;
                let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::MaxToolCalls(v)));
            }
            _ => {}
        }
    }

    pub(super) fn on_key_select(&mut self, key: KeyEvent, h: &Handles) {
        let Some(ref mut picker) = self.picker else {
            self.mode = Mode::Idle;
            return;
        };
        // Movement keys (arrows, Ctrl+j/k, PageUp/PageDown) are handled up
        // front rather than as match arms: a shared arm like
        // `KeyCode::Up | KeyCode::Char('k') if ctrl` applies its guard to
        // `Up` too, so plain arrow keys fell through to `_ => {}` and did
        // nothing. Bare j/k stay filter input (the list is type-to-filter).
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('e') && matches!(picker.action, PickAction::Provider) {
            let name = picker.filtered().get(picker.cursor)
                .and_then(|&i| picker.items[i].split_whitespace().next()).unwrap_or("").to_string();
            self.edit_provider(&name);
            return;
        }
        if let Some(delta) = picker_step_for_key(&key) {
            picker.step(delta);
            return;
        }

        match key.code {
            KeyCode::Esc => self.select_escape(h),
            KeyCode::Enter => {
                let filtered = picker.filtered();
                let mut provider_pick = None;
                let mut auth_pick = None;
                let mut model_pick = None;
                // A model that isn't listed: take the typed name as is.
                if filtered.is_empty() && matches!(picker.action, PickAction::Model) {
                    let typed = picker.filter.trim().to_string();
                    if !typed.is_empty() {
                        model_pick = Some(typed);
                    }
                }
                if let Some(&idx) = filtered.get(picker.cursor) {
                    let item = picker.items[idx].clone();
                    match picker.action {
                        PickAction::Provider => provider_pick = Some(item),
                        PickAction::Auth => auth_pick = Some(idx),
                        PickAction::Model => model_pick = Some(item),
                        PickAction::Rewind => {
                            let n = item.split_once('.').and_then(|(n, _)| n.parse::<usize>().ok());
                            if let Some(n) = n.filter(|&n| n > 0) {
                                let _ = h.cmd_tx.send(WorkerCmd::Rewind(n - 1));
                            }
                        }
                        PickAction::Resume => {
                            if let Some(id) = item.split_whitespace().next() {
                                let _ = h.cmd_tx.send(WorkerCmd::Resume(id.to_string()));
                            }
                        }
                    }
                }
                self.mode = Mode::Idle;
                self.picker = None;
                if let Some(item) = provider_pick {
                    self.provider_picked(&item, h);
                }
                if let Some(idx) = auth_pick {
                    self.auth_picked(idx, h);
                }
                if let Some(model) = model_pick {
                    let _ = h.cmd_tx.send(WorkerCmd::SetModel(model.clone()));
                    if let Some(mut d) = self.wizard.take().filter(|d| d.waiting_models) {
                        d.model = model;
                        self.wizard_done(d, h);
                    }
                }
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                let len = picker.filtered().len();
                picker.clamp(len.max(1));
            }
            // Ctrl-modified chars are shortcuts, not filter input.
            KeyCode::Char(c) if !ctrl => {
                picker.filter.push(caps_char(&key, c));
                let len = picker.filtered().len();
                picker.clamp(len.max(1));
            }
            _ => {}
        }
    }

    /// Esc in a picker: close it, or in the provider wizard go back a step
    /// (the model step ends the setup on the model already in use).
    pub(super) fn select_escape(&mut self, h: &Handles) {
        let action = self.picker.as_ref().map(|p| p.action);
        self.picker = None;
        self.clear_busy();
        let Some(mut d) = self.wizard.take() else { return };
        d.error = None;
        match action {
            Some(PickAction::Model) if d.waiting_models => self.wizard_done(d, h),
            Some(PickAction::Auth) if d.adding => {
                self.wizard = Some(d);
                self.ask(ProviderStep::BaseUrl);
            }
            Some(PickAction::Auth) => self.open_provider_picker(),
            _ => {
                self.wizard = Some(d);
                self.cancel_wizard();
            }
        }
    }

    pub(super) fn on_key_themeselect(&mut self, key: KeyEvent, _h: &Handles) {
        let (cursor, prev) = match &self.mode {
            Mode::ThemeSelect { cursor, prev } => (*cursor, prev.clone()),
            _ => return,
        };
        match key.code {
            KeyCode::Esc => {
                self.set_palette(palette_by_name(&prev));
                self.mode = Mode::Idle;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let idx = cursor.saturating_sub(1).max(0);
                self.preview_theme(idx, prev);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let idx = (cursor + 1).min(THEMES.len() - 1);
                self.preview_theme(idx, prev);
            }
            KeyCode::Enter => self.commit_theme(cursor),
            _ => {}
        }
    }

    /// `/statusline` with no argument: the picker, starting from what's shown.
    pub(super) fn open_statusline_picker(&mut self) {
        self.mode = Mode::StatuslineSelect {
            cursor: 0,
            items: statusline::picker_items(&self.statusline),
            prev: self.statusline.clone(),
        };
    }

    /// Save `segments` (the default row when they equal it) and show them.
    pub(super) fn set_statusline(&mut self, segments: Vec<String>) {
        let default = segments.iter().map(String::as_str).eq(statusline::DEFAULT.iter().copied());
        let saved = if default { Vec::new() } else { segments.clone() };
        crate::config::Config::persist_statusline(&saved);
        self.settings.statusline = saved;
        self.statusline = segments;
        self.clock_min = -1;
        self.push(Kind::Notice, format!("status bar: {}", self.statusline.join(" ")));
    }

    pub(super) fn cancel_statusline(&mut self) {
        if let Mode::StatuslineSelect { prev, .. } = std::mem::replace(&mut self.mode, Mode::Idle) {
            self.statusline = prev;
        }
    }

    /// Up/down move, Space toggles, Shift or Alt+Up/Down (or [ ]) move the
    /// row, Enter saves, Esc restores. The row previews every change.
    pub(super) fn on_key_statusline(&mut self, key: KeyEvent) {
        let Mode::StatuslineSelect { cursor, items, .. } = &mut self.mode else { return };
        let n = items.len();
        let shift = key.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') if shift => {
                if *cursor > 0 {
                    items.swap(*cursor, *cursor - 1);
                    *cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') if shift => {
                if *cursor + 1 < n {
                    items.swap(*cursor, *cursor + 1);
                    *cursor += 1;
                }
            }
            KeyCode::Char('[') if *cursor > 0 => {
                items.swap(*cursor, *cursor - 1);
                *cursor -= 1;
            }
            KeyCode::Char(']') if *cursor + 1 < n => {
                items.swap(*cursor, *cursor + 1);
                *cursor += 1;
            }
            KeyCode::Up | KeyCode::Char('k') => *cursor = cursor.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => *cursor = (*cursor + 1).min(n.saturating_sub(1)),
            KeyCode::Char(' ') => {
                let on = !items[*cursor].1;
                // Keep at least one item: an empty row would read as the default.
                if on || items.iter().filter(|(_, o)| *o).count() > 1 {
                    items[*cursor].1 = on;
                }
            }
            KeyCode::Enter => {
                let chosen: Vec<String> = items.iter().filter(|(_, on)| *on).map(|(s, _)| s.clone()).collect();
                self.mode = Mode::Idle;
                self.set_statusline(chosen);
                return;
            }
            KeyCode::Esc => {
                self.cancel_statusline();
                return;
            }
            _ => return,
        }
        self.statusline = items.iter().filter(|(_, on)| *on).map(|(s, _)| s.clone()).collect();
    }

    /// Display value for one `/config` row.
    pub(super) fn setting_value(&self, row: usize) -> String {
        let s = &self.settings;
        match row {
            0 => s.provider.clone(),
            1 => s.base_url.clone(),
            2 => self.model_info.clone(),
            3 => {
                if s.api_key.is_empty() {
                    "(not set)".to_string()
                } else {
                    let tail: String = s
                        .api_key
                        .chars()
                        .rev()
                        .take(4)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    if s.key_from_env {
                        format!("…{tail} (from env — overrides the saved key)")
                    } else {
                        format!("…{tail}")
                    }
                }
            }
            4 => {
                let how = crate::config::method_short(&s.provider, s.auth());
                let more = if crate::config::auth_methods(&s.provider).len() > 1 { " (Enter to change)" } else { "" };
                if s.has_credentials() {
                    format!("{how}{more}")
                } else {
                    format!("{how} — not set up (Enter to set up)")
                }
            }
            5 => s.reasoning.clone(),
            6 => perm_name(self.perm()).to_string(),
            7 => if s.auto_commit { "on".into() } else { "off".into() },
            8 => self.palette.name.to_string(),
            9 => self.ctx_limit.to_string(), // the window in effect (pinned, detected or table)
            10 => setting_max_tool_calls(s.max_tool_calls),
            11 => {
                // Cycled off USD while the active provider still runs the
                // built-in (USD-quoted) list: the prices are relabelled, not
                // converted, so say where the real ones go.
                if s.price_currency != "USD" && !s.prices.contains_key(&s.provider) {
                    format!("{} (built-in list is still USD: set prices.{} in config.json)", s.price_currency, s.provider)
                } else {
                    s.price_currency.clone()
                }
            }
            _ => String::new(),
        }
    }
}
