//! Per-model settings and prices, the wire protocol, provider choices and
//! the context window.

use super::*;

impl Config {
    /// The temperature to request, if any: the configured one, else 0.2 for
    /// DeepSeek (whose docs want a low one for code, and what haikucode always
    /// sent), else none at all. Forcing a low temperature on reasoning models
    /// sends them into repetition loops (Qwen wants 0.6–1.0), and OpenAI's
    /// reasoning models reject any value but 1.
    pub fn temperature(&self) -> Option<f64> {
        self.model_settings()
            .and_then(|m| m.temperature)
            .or(self.temperature)
            .or_else(|| (self.provider == "deepseek").then_some(0.2))
    }

    /// The protocol to speak: the `api` setting, else Anthropic's own
    /// Messages API for the anthropic preset, OpenAI's Responses API for the
    /// openai preset, and chat/completions for everyone else.
    pub fn api_kind(&self) -> ApiKind {
        if self.chatgpt_sub() {
            return ApiKind::Responses; // the Codex backend speaks only Responses
        }
        match self.apis.get(&self.provider).map(|a| a.trim()) {
            Some("messages" | "anthropic") => ApiKind::Messages,
            Some("responses") => ApiKind::Responses,
            Some("chat" | "chat_completions" | "openai") => ApiKind::Chat,
            _ => match self.provider.as_str() {
                "anthropic" => ApiKind::Messages,
                "openai" => ApiKind::Responses,
                _ => ApiKind::Chat,
            },
        }
    }

    /// The price list that applies to `provider`: the user-set one from
    /// `prices`, else the built-in `PRICES` table. `None` for a custom server
    /// with no known price (its cost readout is hidden).
    pub fn price_list_for(&self, provider: &str) -> Option<PriceList> {
        self.prices.get(provider).cloned().or_else(|| built_in_prices(provider))
    }

    /// The active provider's price list (see `price_list_for`).
    pub fn price_list(&self) -> Option<PriceList> {
        self.price_list_for(&self.provider)
    }

    /// The prices the cost readout uses: the active provider's list with the
    /// active model's `models` overrides on top. A model with its own price
    /// gets a cost readout even on a custom server without a list.
    pub fn effective_price_list(&self) -> Option<PriceList> {
        let base = self.price_list();
        let Some(m) = self.model_settings().filter(|m| m.has_price() || (base.is_some() && m.price_currency.is_some())) else {
            return base;
        };
        let mut p = base.unwrap_or_default();
        if let Some(v) = m.price_in { p.price_in = v; }
        if let Some(v) = m.price_out { p.price_out = v; }
        if let Some(v) = m.price_cached_in { p.price_cached_in = Some(v); }
        if let Some(c) = &m.price_currency { p.price_currency = c.trim().to_ascii_uppercase(); }
        Some(p)
    }

    /// Re-derive the single top-level `price_*` fields from the effective
    /// list (provider + model overrides), so they always mirror it. Without
    /// one they're zeroed (and the cost readout is hidden). Call on every
    /// provider *and* model switch.
    pub fn sync_prices(&mut self) {
        match self.effective_price_list() {
            Some(p) => {
                self.price_in = p.price_in;
                self.price_out = p.price_out;
                self.price_cached_in = p.price_cached_in;
                self.price_currency = p.price_currency;
            }
            None => {
                self.price_in = 0.0;
                self.price_out = 0.0;
                self.price_cached_in = None;
                self.price_currency = default_price_currency();
            }
        }
    }

    /// Update only the model field on disk, preserving the rest of the file.
    /// Goes through the lenient loader — a strict parse here could fall back
    /// to defaults on any malformed field and wipe the saved key.
    pub fn persist_model(&self) {
        let mut disk = Config::load_disk();
        disk.model = self.model.clone();
        disk.sync_active_custom();
        let _ = disk.save();
    }

    /// Every provider to choose from: the presets, then the custom ones, as
    /// (name, base_url, model) with each custom provider's saved model.
    pub fn provider_choices(&self) -> Vec<(String, String, String)> {
        PROVIDERS
            .iter()
            .map(|(n, b, m)| {
                // The preset, as last used: its remembered model and proxy URL.
                let p = self.presets.get(*n);
                let base = p.and_then(|p| p.base_url.clone()).unwrap_or_else(|| b.to_string());
                let model = p.map(|p| p.model.clone()).filter(|m| !m.is_empty()).unwrap_or_else(|| m.to_string());
                (n.to_string(), base, model)
            })
            .chain(self.custom_providers.iter().map(|c| (c.name.clone(), c.base_url.clone(), c.model.clone())))
            .collect()
    }

    /// Is there a key for `provider`, saved or in its environment variable?
    pub fn has_key_for(&self, provider: &str) -> bool {
        self.api_keys.get(provider).is_some_and(|k| !k.is_empty())
            || key_env_vars(provider).iter().any(|v| std::env::var(v).is_ok_and(|k| !k.is_empty()))
    }

    pub fn custom_provider(&self, name: &str) -> Option<&CustomProvider> {
        self.custom_providers.iter().find(|c| c.name == name)
    }

    /// A provider set up before custom providers were a list (the setup
    /// wizard's "custom" choice, or a hand-edited file) joins the list, so
    /// switching away from it doesn't lose its address.
    pub(super) fn adopt_active_custom(&mut self) {
        if is_builtin_provider(&self.provider)
            || self.custom_provider(&self.provider).is_some()
            || valid_custom_name(&self.provider).is_err()
            || !self.base_url.starts_with("http")
        {
            return;
        }
        self.custom_providers.push(CustomProvider {
            name: self.provider.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
        });
    }

    /// Mirror the active base URL and model into the active custom
    /// provider's entry, so switching away and back keeps them.
    /// The active model's `models` entry, matched exactly and then
    /// case-insensitively (model ids are usually lowercase, but servers such
    /// as llama-swap echo whatever the user named them).
    pub fn model_settings(&self) -> Option<&ModelSettings> {
        self.model_settings_key().map(|k| &self.models[k])
    }

    pub(super) fn model_settings_key(&self) -> Option<&String> {
        if let Some((k, _)) = self.models.get_key_value(&self.model) {
            return Some(k);
        }
        self.models.keys().find(|k| k.eq_ignore_ascii_case(&self.model))
    }

    /// The active model's own context window, if its `models` entry sets one.
    pub fn model_context_window(&self) -> Option<u32> {
        self.model_settings().and_then(|m| m.context_window)
    }

    /// The window the ctx bar and compaction use before any provider
    /// detection: the active model's entry, else `context_window`.
    pub fn effective_context_window(&self) -> u32 {
        self.model_context_window().unwrap_or(self.context_window)
    }

    /// Pin a context window from `/config`: update the active model's entry
    /// when it has one (that is the value in effect), else the global one.
    pub fn set_context_window(&mut self, n: u32) {
        let n = n.max(1);
        // A window is a property of the model: pinning it globally would
        // carry a 128k window over to an 8k model on the next /model.
        if self.model.is_empty() {
            self.context_window = n;
            self.context_window_explicit = true;
            return;
        }
        let k = self.model_settings_key().cloned().unwrap_or_else(|| self.model.clone());
        self.models.entry(k).or_default().context_window = Some(n);
    }

    pub fn sync_active_custom(&mut self) {
        let (base_url, model) = (self.base_url.clone(), self.model.clone());
        if let Some(c) = self.custom_providers.iter_mut().find(|c| c.name == self.provider) {
            c.base_url = base_url;
            c.model = model;
        } else if let Some((_, preset_url, _)) = PROVIDERS.iter().find(|(n, _, _)| *n == self.provider) {
            let url = (base_url.trim_end_matches('/') != preset_url.trim_end_matches('/') && !base_url.is_empty())
                .then_some(base_url);
            self.presets.insert(self.provider.clone(), PresetState { base_url: url, model });
        }
    }
}
