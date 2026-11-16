//! `ConfigPatch`: one `/config` change, applied live and mirrored to disk.

use super::*;

/// One settings change from the `/config` panel. The worker applies it to its
/// live config and mirrors it to disk (each variant maps onto one field).
#[derive(Clone)]
pub enum ConfigPatch {
    Provider { provider: String, base_url: String, model: String },
    /// Add a custom provider, or replace the one with the same name.
    AddProvider(CustomProvider),
    /// Forget a custom provider (its saved key goes with it).
    RemoveProvider(String),
    /// Save the API key for one provider, active or not (the `/provider`
    /// wizard stores it before switching, so the switch finds it).
    ProviderKey { provider: String, key: String },
    BaseUrl(String),
    ApiKey(String),
    /// `/effort` or the /config row: one of `REASONING_LEVELS`.
    Reasoning(String),
    Permission(String),
    AutoCommit(bool),
    ContextWindow(u32),
    MaxToolCalls(u32),
    /// One provider's sign-in method, active or not (the `/provider` wizard
    /// sets it before switching).
    ProviderAuth { provider: String, mode: String },
    /// A subscription login finished: keep its token.
    SignedIn { provider: String, token: OAuthToken },
    /// Forget a provider's subscription login (back to its API key).
    SignOut(String),
    /// ISO code the price list is quoted in (see `Config::price_currency`).
    PriceCurrency(String),
}

impl Config {
    pub fn apply_patch(&mut self, p: &ConfigPatch) {
        match p {
            ConfigPatch::Provider { provider, base_url, model } => {
                // Remember what the provider being left was on (its model,
                // a preset's proxy URL), so switching back restores it.
                self.sync_active_custom();
                self.provider = provider.clone();
                self.base_url = base_url.clone();
                self.model = model.clone();
                // Swap to the new provider's saved key (or empty) and its
                // sign-in method.
                self.resolve_key();
                self.sync_auth_mode();
                // The cost counter follows the provider: its price list (the
                // user-set one, else the built-in table, else hidden).
                self.sync_prices();
            }
            ConfigPatch::AddProvider(cp) => {
                self.custom_providers.retain(|c| c.name != cp.name);
                self.custom_providers.push(cp.clone());
            }
            ConfigPatch::ProviderKey { provider, key } => {
                self.api_keys.insert(provider.clone(), key.clone());
                if &self.provider == provider {
                    self.api_key = key.clone();
                    self.key_from_env = false;
                }
            }
            ConfigPatch::RemoveProvider(name) => {
                self.custom_providers.retain(|c| &c.name != name);
                self.api_keys.remove(name);
                self.oauth.remove(name);
                self.auth_modes.remove(name);
            }
            ConfigPatch::ProviderAuth { provider, mode } => {
                let m = AuthMethod::parse(mode);
                if m == AuthMethod::Api {
                    self.auth_modes.remove(provider);
                } else if auth_methods(provider).contains(&m) {
                    self.auth_modes.insert(provider.clone(), m.id().to_string());
                }
                self.sync_auth_mode();
            }
            ConfigPatch::SignedIn { provider, token } => {
                self.oauth.insert(provider.clone(), token.clone());
            }
            ConfigPatch::SignOut(provider) => {
                self.oauth.remove(provider);
                if self.auth_for(provider) == AuthMethod::Sub {
                    self.auth_modes.remove(provider);
                }
                self.sync_auth_mode();
            }
            ConfigPatch::BaseUrl(u) => {
                self.base_url = u.clone();
                self.sync_active_custom();
            }
            ConfigPatch::ApiKey(k) => {
                self.api_key = k.clone();
                self.api_keys.insert(self.provider.clone(), k.clone());
                self.key_from_env = false;
            }
            ConfigPatch::Reasoning(r) => self.reasoning = normalize_reasoning(r),
            ConfigPatch::Permission(m) => self.permission = m.clone(),
            ConfigPatch::AutoCommit(b) => self.auto_commit = *b,
            ConfigPatch::ContextWindow(n) => self.set_context_window(*n),
            ConfigPatch::MaxToolCalls(n) => self.max_tool_calls = *n,
            ConfigPatch::PriceCurrency(c) => {
                let c = c.trim().to_ascii_uppercase();
                let c = if c.is_empty() {
                    default_price_currency()
                } else {
                    c
                };
                // The currency belongs to the active provider's list: the
                // user-set one, else the built-in table, else a fresh list
                // (a custom server whose currency was cycled in /config).
                // A model with its own currency keeps it there instead.
                let key = self.model_settings_key().filter(|k| self.models[*k].price_currency.is_some()).cloned();
                if let Some(k) = key {
                    self.models.entry(k).or_default().price_currency = Some(c);
                } else {
                    let mut list = self.price_list().unwrap_or_default();
                    list.price_currency = c;
                    self.prices.insert(self.provider.clone(), list);
                }
                self.sync_prices();
            }
        }
    }

    /// Mirror one patch to disk, preserving every other field.
    pub fn persist_patch(p: &ConfigPatch) {
        let mut disk = Config::load_disk();
        disk.apply_patch(p);
        let _ = disk.save();
    }
}
