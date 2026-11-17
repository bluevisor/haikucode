use super::*;

impl App {
    /// The label the `/provider` picker shows for adding a provider.
    pub(super) const ADD_PROVIDER: &'static str = "+ Add a custom provider (any OpenAI-compatible URL)";
    pub(super) const WIZARD_CONTROLS: &'static str = "Enter next/save · Shift+Tab back · Esc cancel · Ctrl+U clear";

    /// How `name` is set up, in a few words: "✓ API key", "✓ ChatGPT
    /// sign-in", or "not set up".
    fn provider_status(&self, name: &str) -> String {
        let s = &self.settings;
        let tick = if self.ascii { "*" } else { "✓" };
        if s.has_credentials_for(name) {
            format!("{tick} {}", crate::config::method_short(name, s.auth_for(name)))
        } else {
            "not set up".into()
        }
    }

    /// The wizard's place in its steps: "step 2 of 3" (a preset: provider,
    /// sign-in, model) or "step 3 of 4" (a new one: name, URL, sign-in, model).
    fn wizard_step(d: &ProviderDraft, step: usize) -> String {
        if d.adding {
            format!("step {step} of 4")
        } else {
            format!("step {} of 3", step.saturating_sub(1).max(1))
        }
    }

    /// `/provider` with no argument: an arrow-key list of every provider
    /// with how it's set up, then "+ Add" at the bottom.
    pub(super) fn open_provider_picker(&mut self) {
        let choices = self.settings.provider_choices();
        let width = choices.iter().map(|(n, _, _)| n.chars().count()).max().unwrap_or(8);
        let statuses: Vec<String> = choices.iter().map(|(n, _, _)| self.provider_status(n)).collect();
        let swidth = statuses.iter().map(|s| s.chars().count()).max().unwrap_or(10);
        let mut items: Vec<String> = choices
            .iter()
            .zip(&statuses)
            .map(|((name, base, model), status)| {
                let custom = if crate::config::is_builtin_provider(name) { "" } else { " (custom)" };
                let host = base.split('/').nth(2).unwrap_or(base);
                let model = if model.is_empty() { "no model yet" } else { model.as_str() };
                format!("{name:<width$}  {status:<swidth$}  {host} · {model}{custom}")
            })
            .collect();
        items.push(Self::ADD_PROVIDER.to_string());
        let current = choices.iter().position(|(n, _, _)| *n == self.settings.provider);
        let mut picker = Picker {
            title: "Choose a provider · Ctrl+E edits a custom server".into(),
            items,
            current,
            filter: String::new(),
            cursor: current.unwrap_or(0),
            scroll: 0,
            action: PickAction::Provider,
            hint: Some("step 1 of 3 · then choose how to sign in (API key or subscription) and the model".into()),
        };
        let len = picker.items.len();
        picker.clamp(len);
        self.picker = Some(picker);
        self.mode = Mode::Select;
    }

    /// The provider picker's choice: start adding one, or go on to how to
    /// sign in to it.
    pub(super) fn provider_picked(&mut self, item: &str, _h: &Handles) {
        if item == Self::ADD_PROVIDER {
            self.wizard = Some(ProviderDraft { adding: true, ..Default::default() });
            self.ask(ProviderStep::Name);
            return;
        }
        let name = item.split_whitespace().next().unwrap_or_default();
        self.setup_provider(name);
    }

    /// Start the wizard at the sign-in step for a known provider.
    pub(super) fn setup_provider(&mut self, name: &str) {
        let Some((p, b, m)) = self.settings.provider_choices().into_iter().find(|(n, _, _)| n == name) else {
            return;
        };
        // The active provider keeps the address and model it's on (a proxy
        // URL set in /config, a model picked since), not the preset's.
        let active = p == self.settings.provider;
        let m = if active && !self.settings.model.is_empty() { self.settings.model.clone() } else { m };
        let b = if active && !self.settings.base_url.is_empty() { self.settings.base_url.clone() } else { b };
        self.wizard = Some(ProviderDraft { name: p, base_url: b, model: m, ..Default::default() });
        self.open_auth_picker();
    }

    /// The sign-in step: every way this provider accepts, with what's saved
    /// for each, the current one highlighted.
    fn open_auth_picker(&mut self) {
        use crate::config::{auth_methods, coding_plan_label, key_tail, key_url, no_sub_reason, AuthMethod};
        use types::AuthChoice;
        let Some(mut d) = self.wizard.take() else { return };
        d.signing_in = false;
        d.waiting_models = false;
        let s = &self.settings;
        let name = d.name.clone();
        let saved_key = s.api_keys.get(&name).filter(|k| !k.is_empty()).map(|k| format!("…{}", key_tail(k)));
        let env_key = (saved_key.is_none() && s.has_key_for(&name)).then(|| "from the environment".to_string());
        // A saved custom server whose address was just changed: its key
        // belongs to the old host, so don't offer to send it to the new one.
        let moved = s.custom_provider(&name).is_some_and(|c| c.base_url != d.base_url);
        let key_note = if moved { None } else { saved_key.or(env_key) };
        if moved && s.has_key_for(&name) && d.error.is_none() {
            d.error = Some("the address changed, so the saved key isn't reused: enter it again, or pick No key".into());
        }
        let sub = crate::auth::sub_label(&name);
        let mut rows: Vec<(AuthChoice, String)> = Vec::new();
        for m in auth_methods(&name) {
            match m {
                AuthMethod::Api => {
                    let get = key_url(&name, false).map(|u| format!(" (get one at {u})")).unwrap_or_default();
                    match &key_note {
                        Some(k) => {
                            rows.push((AuthChoice::Keep(m), format!("API key — use the saved key {k}")));
                            rows.push((AuthChoice::EnterKey(m), "API key — enter a different key".into()));
                        }
                        None => rows.push((AuthChoice::EnterKey(m), format!("API key — paste a key{get}"))),
                    }
                }
                AuthMethod::Sub if s.signed_in(&name) => {
                    rows.push((AuthChoice::Keep(m), format!("{sub} subscription — signed in")));
                    rows.push((AuthChoice::SignIn, format!("{sub} subscription — sign in again (another account)")));
                    rows.push((AuthChoice::SignOut, format!("Sign out of {sub}")));
                }
                AuthMethod::Sub => {
                    let plans = if name == "openai" { " (Plus / Pro / Business)" } else { "" };
                    rows.push((AuthChoice::SignIn, format!("{sub} subscription{plans} — sign in in your browser")));
                }
                AuthMethod::Plan => {
                    let plan = coding_plan_label(&name);
                    match &key_note {
                        Some(k) => {
                            rows.push((AuthChoice::Keep(m), format!("{plan} — use the saved key {k} on the plan's endpoint")));
                            rows.push((AuthChoice::EnterKey(m), format!("{plan} — enter the plan's key")));
                        }
                        None => rows.push((AuthChoice::EnterKey(m), format!("{plan} — paste the plan's key"))),
                    }
                }
                AuthMethod::None => rows.push((AuthChoice::NoKey, "No key — a local or open server".into())),
            }
        }
        // Highlight what's in use now (or, set up for the first time, the
        // default first row).
        let now = s.auth_for(&name);
        let in_use = s.has_credentials_for(&name).then(|| {
            rows.iter().position(|(c, _)| match c {
                AuthChoice::Keep(m) => *m == now,
                AuthChoice::NoKey => now == AuthMethod::None,
                _ => false,
            })
        });
        // A custom server with nothing saved is most often a keyless local
        // one: highlight No key so Enter doesn't demand a key it doesn't need.
        let current = in_use.flatten().or_else(|| {
            (!crate::config::is_builtin_provider(&name) && (moved || !s.has_key_for(&name)))
                .then(|| rows.iter().position(|(c, _)| matches!(c, AuthChoice::NoKey)))
                .flatten()
        });
        let step = Self::wizard_step(&d, 3);
        let ways = if crate::auth::sub_supported(&name) { "API key or subscription".to_string() } else { "API key".into() };
        let hint = match no_sub_reason(&name) {
            Some(why) => format!("{step} · {why}"),
            None => format!("{step} · {ways} · Esc goes back"),
        };
        d.choices = rows.iter().map(|(c, _)| *c).collect();
        let mut picker = Picker {
            title: format!("{name} · How do you sign in?"),
            items: rows.into_iter().map(|(_, l)| l).collect(),
            current,
            filter: String::new(),
            cursor: current.unwrap_or(0),
            scroll: 0,
            action: PickAction::Auth,
            hint: Some(hint),
        };
        let len = picker.items.len();
        picker.clamp(len);
        self.wizard = Some(d);
        self.picker = Some(picker);
        self.mode = Mode::Select;
    }

    /// Act on a sign-in row: keep what's saved, ask for a key, open the
    /// browser, or sign out.
    pub(super) fn auth_picked(&mut self, idx: usize, h: &Handles) {
        use crate::config::AuthMethod;
        use types::AuthChoice;
        let Some(mut d) = self.wizard.take() else { return };
        d.error = None;
        let Some(choice) = d.choices.get(idx).copied() else { return };
        match choice {
            AuthChoice::Keep(m) => {
                d.method = m;
                self.commit_provider(d, h);
            }
            AuthChoice::EnterKey(m) => {
                d.method = m;
                self.wizard = Some(d);
                self.ask(ProviderStep::ApiKey);
            }
            AuthChoice::NoKey => {
                d.method = AuthMethod::None;
                d.api_key.clear();
                // Don't keep a secret for a server that no longer uses it.
                d.clear_key = self.settings.api_keys.get(&d.name).is_some_and(|k| !k.is_empty());
                self.commit_provider(d, h);
            }
            AuthChoice::SignIn => {
                d.method = AuthMethod::Sub;
                d.signing_in = true;
                let _ = h.cmd_tx.send(WorkerCmd::Login(d.name.clone()));
                self.wizard = Some(d);
                self.set_busy();
                self.current_activity = Some("waiting for the browser sign-in… (Esc cancels)".into());
            }
            AuthChoice::SignOut => {
                let label = crate::auth::sub_label(&d.name);
                self.patch(ConfigPatch::SignOut(d.name.clone()), h);
                self.push(Kind::Notice, format!("signed out of {label} for {}", d.name));
                self.wizard = Some(d);
                self.open_auth_picker();
            }
        }
    }

    /// A paste on the sign-in list is a key, not a filter: the row says
    /// "API key — paste a key", so people paste right there. Open the key
    /// prompt with the paste in it (Enter still saves). False when this
    /// provider takes no key, so the paste filters as usual.
    pub(super) fn paste_key_at_auth_picker(&mut self, pasted: &str) -> bool {
        use crate::config::AuthMethod;
        use types::AuthChoice;
        let key = pasted.trim();
        let Some(d) = self.wizard.as_mut() else { return false };
        if key.is_empty() {
            return false;
        }
        // The highlighted row's kind of key (API or coding plan), else the first.
        let at_cursor = self.picker.as_ref().and_then(|p| d.choices.get(p.cursor).copied());
        let method = match at_cursor {
            Some(AuthChoice::EnterKey(m) | AuthChoice::Keep(m)) if matches!(m, AuthMethod::Api | AuthMethod::Plan) => {
                Some(m)
            }
            _ => d.choices.iter().find_map(|c| match c {
                AuthChoice::EnterKey(m) => Some(*m),
                _ => None,
            }),
        };
        let Some(method) = method else { return false };
        d.method = method;
        d.error = None;
        self.picker = None;
        self.ask(ProviderStep::ApiKey);
        self.wiz_input = key.to_string();
        self.wiz_cursor = self.wiz_input.chars().count();
        true
    }

    /// Open a typed step (prefilled where there's something to keep).
    pub(super) fn ask(&mut self, step: ProviderStep) {
        self.wiz_input = self.wizard.as_ref().map(|d| match step {
            ProviderStep::Name => d.name.clone(),
            ProviderStep::BaseUrl => d.base_url.clone(),
            // Never prefill a key, even in a masked field.
            ProviderStep::ApiKey => String::new(),
            // Empty: Enter keeps the draft's model, typing replaces it.
            ProviderStep::Model => String::new(),
        }).unwrap_or_default();
        self.wiz_cursor = self.wiz_input.chars().count();
        self.mode = Mode::ProviderInput { step };
    }

    pub(super) fn edit_provider(&mut self, name: &str) {
        let Some(c) = self.settings.custom_provider(name).cloned() else {
            self.push(Kind::ErrorK, format!("'{name}' is not a custom provider · /provider lists your servers"));
            return;
        };
        self.picker = None;
        self.wizard = Some(ProviderDraft {
            adding: true, editing: true, name: c.name, base_url: c.base_url, model: c.model,
            ..Default::default()
        });
        self.ask(ProviderStep::BaseUrl);
    }

    pub(super) fn cancel_wizard(&mut self) {
        self.wizard = None;
        self.wiz_input.clear();
        self.wiz_cursor = 0;
        self.mode = Mode::Idle;
        self.push(Kind::Notice, String::from("provider setup cancelled"));
    }

    /// Esc in a typed step: back one step (a new provider's name step, or
    /// the end of a setup, cancels).
    pub(super) fn wizard_back(&mut self, step: ProviderStep, h: &Handles) {
        let Some(mut d) = self.wizard.take() else {
            self.mode = Mode::Idle;
            return;
        };
        d.error = None;
        self.wiz_input.clear();
        match step {
            ProviderStep::Name => {
                self.wizard = Some(d);
                self.cancel_wizard();
            }
            // Editing a saved server has no name step to go back to.
            ProviderStep::BaseUrl if d.editing => {
                self.wizard = Some(d);
                self.cancel_wizard();
            }
            ProviderStep::BaseUrl => {
                self.wizard = Some(d);
                self.ask(ProviderStep::Name);
            }
            ProviderStep::ApiKey => {
                self.wizard = Some(d);
                self.open_auth_picker();
            }
            // Already switched: keep the model it has.
            ProviderStep::Model => self.wizard_done(d, h),
        }
    }

    /// The prompt and hint for a typed wizard step: (title, hint).
    pub(super) fn wizard_prompt(&self, step: ProviderStep) -> (String, String) {
        use crate::config::{coding_plan_label, key_url, AuthMethod};
        let d = self.wizard.clone().unwrap_or_default();
        let who = if d.name.is_empty() { "New provider".to_string() } else { d.name.clone() };
        match step {
            ProviderStep::Name => (
                "New provider · Name".into(),
                format!(
                    "{} · a short name, e.g. litellm or ollama (letters, digits, - _ .) · Esc cancels",
                    Self::wizard_step(&d, 1)
                ),
            ),
            ProviderStep::BaseUrl => (
                format!("{}{who} · Base URL", if d.editing { "Edit " } else { "" }),
                format!(
                    "{} · the OpenAI-compatible endpoint, e.g. http://localhost:11434/v1 (not …/chat/completions) · Esc goes back",
                    Self::wizard_step(&d, 2)
                ),
            ),
            ProviderStep::ApiKey => {
                let plan = d.method == AuthMethod::Plan;
                let what = if plan { format!("{} key", coding_plan_label(&d.name)) } else { "API key".into() };
                let get = key_url(&d.name, plan).map(|u| format!(" · get one at {u}")).unwrap_or_default();
                (
                    format!("{who} · {what}"),
                    format!("{}{get} · hidden as you type; paste works · Esc goes back", Self::wizard_step(&d, 3)),
                )
            }
            ProviderStep::Model => {
                let keep = if d.model.is_empty() { String::new() } else { format!(" · Enter keeps {}", d.model) };
                (format!("{who} · Model"), format!("{} · type the model id{keep}", Self::wizard_step(&d, 4)))
            }
        }
    }

    pub(super) fn on_key_provider_input(&mut self, step: ProviderStep, key: KeyEvent, h: &Handles) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.wizard_back(step, h),
            KeyCode::BackTab => self.wizard_back(step, h),
            KeyCode::Enter => {
                let val = self.wiz_input.trim().to_string();
                self.wizard_answer(step, val, h);
            }
            KeyCode::Backspace => {
                if self.wiz_cursor > 0 {
                    self.wiz_cursor -= 1;
                    let pos = self.wiz_input.char_indices().nth(self.wiz_cursor).map(|(i, _)| i).unwrap();
                    self.wiz_input.remove(pos);
                }
            }
            KeyCode::Delete => {
                if let Some((pos, _)) = self.wiz_input.char_indices().nth(self.wiz_cursor) {
                    self.wiz_input.remove(pos);
                }
            }
            KeyCode::Left => self.wiz_cursor = self.wiz_cursor.saturating_sub(1),
            KeyCode::Right => self.wiz_cursor = (self.wiz_cursor + 1).min(self.wiz_input.chars().count()),
            KeyCode::Home => self.wiz_cursor = 0,
            KeyCode::End => self.wiz_cursor = self.wiz_input.chars().count(),
            KeyCode::Char('u') if ctrl => { self.wiz_input.clear(); self.wiz_cursor = 0; }
            KeyCode::Char(c) if !ctrl => {
                let pos = self.wiz_input.char_indices().nth(self.wiz_cursor).map_or(self.wiz_input.len(), |(i, _)| i);
                self.wiz_input.insert(pos, caps_char(&key, c));
                self.wiz_cursor += 1;
            }
            _ => {}
        }
    }

    /// Check one typed answer, then go on to the next step or finish.
    pub(super) fn wizard_answer(&mut self, step: ProviderStep, val: String, h: &Handles) {
        use crate::config::valid_custom_name;
        let Some(mut d) = self.wizard.take() else {
            self.mode = Mode::Idle;
            return;
        };
        d.error = None;
        let retry = |app: &mut Self, mut d: ProviderDraft, e: String| {
            d.error = Some(e);
            app.wizard = Some(d);
            app.mode = Mode::ProviderInput { step }; // keeps what was typed, to fix it
        };
        match step {
            ProviderStep::Name => match valid_custom_name(&val) {
                Ok(()) => {
                    let changed = d.name != val;
                    d.name = val;
                    // Re-adding a saved one starts from its address and model.
                    if let Some(c) = self.settings.custom_provider(&d.name).filter(|_| changed) {
                        d.base_url = c.base_url.clone();
                        d.model = c.model.clone();
                        d.editing = true;
                    }
                    self.wizard = Some(d);
                    self.ask(ProviderStep::BaseUrl);
                }
                Err(e) => retry(self, d, e),
            },
            ProviderStep::BaseUrl => {
                let val = if val.is_empty() { d.base_url.clone() } else { val };
                match crate::config::normalize_provider_url(&val) {
                    Ok(url) => {
                        d.base_url = url;
                        self.wizard = Some(d);
                        self.open_auth_picker();
                    }
                    Err(e) => retry(self, d, e),
                }
            }
            ProviderStep::ApiKey => {
                // A pasted "Bearer sk-…" or "KEY=sk-…" line still works.
                let key = val.trim_start_matches("Bearer ").rsplit('=').next().unwrap_or("").trim().to_string();
                if key.is_empty() {
                    retry(self, d, "paste the key (Esc goes back to pick another way)".into());
                } else if key == "-" {
                    retry(self, d, "to use no key, press Esc and pick No key".into());
                } else if key.chars().any(char::is_whitespace) {
                    retry(self, d, "that has spaces in it: paste just the key".into());
                } else {
                    d.api_key = key;
                    self.wiz_input.clear();
                    self.commit_provider(d, h);
                }
            }
            ProviderStep::Model => {
                let model = if val.is_empty() { d.model.clone() } else { val };
                if model.is_empty() {
                    retry(self, d, "type a model id".into());
                    return;
                }
                self.wiz_input.clear();
                if model != self.settings.model {
                    let _ = h.cmd_tx.send(WorkerCmd::SetModel(model.clone()));
                }
                d.model = model;
                self.wizard_done(d, h);
            }
        }
    }

    /// Save what the wizard collected (a new provider, a key, the sign-in
    /// method), switch to it, and go on to choose the model from the
    /// server's list.
    fn commit_provider(&mut self, d: ProviderDraft, h: &Handles) {
        use crate::config::CustomProvider;
        if d.adding {
            let updated = self.settings.custom_provider(&d.name).is_some();
            self.patch(
                ConfigPatch::AddProvider(CustomProvider {
                    name: d.name.clone(),
                    base_url: d.base_url.clone(),
                    model: d.model.clone(),
                }),
                h,
            );
            self.push(
                Kind::Notice,
                format!("{} provider {}: {}", if updated { "updated" } else { "added" }, d.name, d.base_url),
            );
        }
        if !d.api_key.is_empty() || d.clear_key {
            self.patch(ConfigPatch::ProviderKey { provider: d.name.clone(), key: d.api_key.clone() }, h);
            self.push(Kind::Notice, format!("key {} for {}", if d.clear_key { "removed" } else { "saved" }, d.name));
        }
        // The method belongs to the provider: set it before switching, so
        // the switch already speaks the right way.
        self.patch(ConfigPatch::ProviderAuth { provider: d.name.clone(), mode: d.method.id().into() }, h);
        // Editing a server that isn't the active one saves it without
        // switching away from what's in use.
        if d.editing && self.settings.provider != d.name && !d.model.is_empty() {
            self.wizard = None;
            self.wiz_input.clear();
            self.mode = Mode::Idle;
            self.push(Kind::Notice, format!("saved · /provider {} switches to this server", d.name));
            return;
        }
        self.switch_provider(d.name.clone(), d.base_url.clone(), d.model.clone(), h);
        let mut d = d;
        d.api_key.clear();
        d.waiting_models = true;
        self.push(Kind::Notice, format!("loading {}'s models…", d.name));
        self.wizard = Some(d);
        self.pick_model(h);
    }

    /// The model list arrived for the wizard: the last step.
    pub(super) fn wizard_models(&mut self, ids: Vec<String>) {
        let d = self.wizard.clone().unwrap_or_default();
        let default = crate::config::PROVIDERS.iter().find(|(n, _, _)| *n == d.name).map(|(_, _, m)| *m);
        let current = ids
            .iter()
            .position(|m| *m == self.settings.model)
            .or_else(|| default.and_then(|dm| ids.iter().position(|m| m == dm)));
        let mut picker = Picker {
            title: format!("{} · Choose a model", d.name),
            items: ids,
            current,
            filter: String::new(),
            cursor: current.unwrap_or(0),
            scroll: 0,
            action: PickAction::Model,
            hint: Some(format!(
                "{} · type to filter; a name that isn't listed is used as typed · Esc keeps {}",
                Self::wizard_step(&d, 4),
                if self.settings.model.is_empty() { "none" } else { self.settings.model.as_str() }
            )),
        };
        let len = picker.items.len();
        picker.clamp(len);
        self.picker = Some(picker);
        self.mode = Mode::Select;
    }

    /// The model list didn't come: a refused credential goes back to the
    /// sign-in step, anything else asks for the model id by hand.
    pub(super) fn wizard_models_failed(&mut self, err: &str) {
        let Some(mut d) = self.wizard.take() else { return };
        let short = crate::api::truncate(err.lines().next().unwrap_or(err), 160);
        if err.contains("HTTP 401") || err.contains("HTTP 403") {
            let what = if d.method == crate::config::AuthMethod::Sub { "sign-in" } else { "key" };
            d.error = Some(format!("{} refused the {what} ({short}): pick or enter another", d.name));
            self.wizard = Some(d);
            self.open_auth_picker();
            self.push(Kind::ErrorK, format!("{}: the {what} was refused", self.settings.provider));
        } else {
            d.error = Some(format!("couldn't load the model list ({short}): type the model id"));
            d.model = self.settings.model.clone();
            self.wizard = Some(d);
            self.ask(ProviderStep::Model);
        }
    }

    /// The wizard's end: say what's now in use.
    pub(super) fn wizard_done(&mut self, d: ProviderDraft, _h: &Handles) {
        self.wizard = None;
        self.wiz_input.clear();
        self.mode = Mode::Idle;
        let s = &self.settings;
        let model = if d.model.is_empty() { s.model.clone() } else { d.model.clone() };
        let tick = if self.ascii { "*" } else { "✓" };
        self.push(
            Kind::Notice,
            format!("{tick} {} · {} · {model}", s.provider, crate::config::method_short(&s.provider, s.auth())),
        );
    }

    /// A browser sign-in finished (or didn't).
    pub(super) fn on_signed_in(&mut self, provider: String, token: Option<crate::config::OAuthToken>, error: String, h: &Handles) {
        let waiting = self.wizard.as_ref().is_some_and(|d| d.signing_in && d.name == provider);
        if let Some(token) = token {
            self.settings.apply_patch(&ConfigPatch::SignedIn { provider: provider.clone(), token });
            self.push(Kind::Notice, format!("signed in with {}", crate::auth::sub_label(&provider)));
            if let Some(mut d) = self.wizard.take().filter(|_| waiting) {
                d.signing_in = false;
                self.clear_busy();
                self.commit_provider(d, h);
            }
        } else if waiting {
            if let Some(d) = self.wizard.as_mut() {
                d.error = Some(format!("sign-in didn't finish: {error}"));
            }
            self.clear_busy();
            self.open_auth_picker();
        } else {
            self.push(Kind::ErrorK, format!("sign-in failed: {error}"));
        }
    }

    /// Open the model list for the active provider (as `/model` does).
    fn pick_model(&mut self, h: &Handles) {
        let _ = h.cmd_tx.send(WorkerCmd::ListModels);
        self.mode = Mode::Select;
    }

    /// Make `provider` active with its address and model, and its saved key.
    pub(super) fn switch_provider(&mut self, provider: String, base_url: String, model: String, h: &Handles) {
        self.balance = None; // the old account's; the worker fetches the new one
        self.settings.sync_active_custom(); // remember what the old provider was on
        self.settings.provider = provider.clone();
        self.settings.base_url = base_url.clone();
        self.settings.model = model.clone();
        self.model_info = model.clone();
        self.settings.resolve_key();
        self.settings.sync_auth_mode();
        self.sync_prices(); // the cost counter follows the provider
        let _ = h.cmd_tx.send(WorkerCmd::Patch(ConfigPatch::Provider { provider, base_url, model }));
    }

    /// The cost counter follows the active provider: take its price list (the
    /// user-set one, else the built-in table) or hide the cost (a custom
    /// server with no known price).
    pub(super) fn sync_prices(&mut self) {
        self.bank_cost();
        match self.settings.effective_price_list() {
            Some(p) => {
                self.price_in = p.price_in;
                self.price_out = p.price_out;
                self.price_cached_in = p.price_cached_in;
                self.price_currency = money::Currency::parse(&p.price_currency);
                self.cost_known = true;
            }
            None => self.cost_known = false,
        }
    }

    /// Apply a settings change here and in the worker (which saves it).
    pub(super) fn patch(&mut self, p: ConfigPatch, h: &Handles) {
        self.settings.apply_patch(&p);
        let _ = h.cmd_tx.send(WorkerCmd::Patch(p));
    }

    /// `/provider`: list; `/provider <name>` switches; `/provider add <name>
    /// <base_url> <model>` adds (or updates) an OpenAI-compatible provider and
    /// switches to it; `/provider remove <name>` forgets one.
    pub(super) fn provider_command(&mut self, arg: Option<&str>, h: &Handles) {
        use crate::config::{is_builtin_provider, valid_custom_name, CustomProvider};
        let words: Vec<&str> = arg.unwrap_or("").split_whitespace().collect();
        match words.as_slice() {
            [] => self.open_provider_picker(),
            ["add"] => {
                self.wizard = Some(ProviderDraft { adding: true, ..Default::default() });
                self.ask(ProviderStep::Name);
            }
            ["edit"] => self.edit_provider(&self.settings.provider.clone()),
            ["edit", name] => self.edit_provider(name),
            ["list"] => {
                self.push(Kind::Notice, String::from("providers (● active):"));
                for (name, base, model) in self.settings.provider_choices() {
                    let active = if name == self.settings.provider { "●" } else { " " };
                    let kind = if is_builtin_provider(&name) { "" } else { " (custom)" };
                    let status = self.provider_status(&name);
                    self.push(Kind::Notice, format!("  {active} {name}{kind}  {status}  {base} · {model}"));
                }
                self.push(Kind::Notice, String::from("  /provider <name> switches (or sets it up) · /provider opens the picker"));
                self.show_options("/provider");
            }
            ["add", name, base, model] => {
                if let Err(e) = valid_custom_name(name) {
                    self.push(Kind::ErrorK, e);
                    return;
                }
                let base = match crate::config::normalize_provider_url(base) {
                    Ok(base) => base,
                    Err(e) => { self.push(Kind::ErrorK, e); return; }
                };
                let cp = CustomProvider {
                    name: name.to_string(),
                    base_url: base,
                    model: model.to_string(),
                };
                let (p, b, m) = (cp.name.clone(), cp.base_url.clone(), cp.model.clone());
                let updated = self.settings.custom_provider(name).is_some();
                let changed_address = self.settings.custom_provider(name).is_some_and(|c| c.base_url != cp.base_url);
                self.patch(ConfigPatch::AddProvider(cp), h);
                if changed_address {
                    self.patch(ConfigPatch::ProviderKey { provider: p.clone(), key: String::new() }, h);
                    self.push(Kind::Notice, "address changed · saved key cleared; /provider edit to enter a key".to_string());
                }
                self.push(
                    Kind::Notice,
                    format!("{} provider {p}: {b} · {m}", if updated { "updated" } else { "added" }),
                );
                self.switch_provider(p, b, m, h);
            }
            ["add", ..] => {
                self.push(Kind::ErrorK, String::from("usage: /provider add <name> <base_url> <model>"));
            }
            ["remove" | "rm", name] => {
                if is_builtin_provider(name) {
                    self.push(Kind::ErrorK, format!("'{name}' is built in and can't be removed"));
                    return;
                }
                if self.settings.custom_provider(name).is_none() {
                    self.push(Kind::ErrorK, format!("no custom provider named '{name}'"));
                    return;
                }
                if self.settings.provider == *name {
                    let (p, b, m) = PROVIDERS[0];
                    self.switch_provider(p.into(), b.into(), m.into(), h);
                }
                self.patch(ConfigPatch::RemoveProvider(name.to_string()), h);
                self.push(Kind::Notice, format!("removed provider {name} and its saved key"));
            }
            [name] => match self.settings.provider_choices().into_iter().find(|(n, _, _)| n == name) {
                Some((p, b, m)) if self.settings.has_credentials_for(&p) => self.switch_provider(p, b, m, h),
                // Not set up yet: walk through signing in and the model.
                Some((p, _, _)) => self.setup_provider(&p),
                None => {
                    self.push(Kind::ErrorK, format!("unknown provider '{name}' (/provider lists them)"));
                }
            },
            _ => {
                self.push(Kind::ErrorK, format!("/provider {}: unknown option", words.join(" ")));
                self.show_options("/provider");
            }
        }
    }
}
