//! Keys, sign-ins and the endpoint: which credential a request carries.

use super::*;

impl Config {
    /// Persist one provider's subscription-login token, preserving the rest of
    /// the file. Goes through the lenient loader so a malformed field elsewhere
    /// can't wipe other saved state.
    pub fn persist_oauth(provider: &str, token: &OAuthToken) {
        let mut disk = Config::load_disk();
        disk.oauth.insert(provider.to_string(), token.clone());
        let _ = disk.save();
    }

    /// The active provider's subscription token, if logged in. Consumed by the
    /// per-provider inference adapters (the request path's auth source).
    #[allow(dead_code)] // wired in by the native inference adapters
    pub fn oauth_token(&self) -> Option<&OAuthToken> {
        self.oauth.get(&self.provider)
    }

    /// The Bearer credential to send for the active provider: the subscription
    /// access token in "sub" mode (when present), otherwise the API key.
    pub fn bearer(&self) -> &str {
        if self.uses_sub() {
            if let Some(t) = self.oauth.get(&self.provider) {
                return &t.access_token;
            }
        }
        &self.api_key
    }

    /// Does this provider accept OpenAI-style `image_url` content parts?
    /// DeepSeek Flash (V4.1) takes images; V4 Pro and Zhipu are text-only and
    /// reject the variant. Most others accept it.
    pub fn supports_images(&self) -> bool {
        match self.provider.as_str() {
            "deepseek" => self.model.to_ascii_lowercase().contains("flash"),
            "zai" => false,
            _ => true,
        }
    }

    /// Resolve the effective API key for the current provider: its saved key,
    /// else its environment variable (so a provider whose key lives only in
    /// the environment works after a switch, as the picker promises).
    pub fn resolve_key(&mut self) {
        // Same precedence as `load`: an exported key (HAIKUCODE_API_KEY, then
        // the provider's own var) overrides the saved one, so a rotated key in
        // the environment still wins after switching away and back.
        if let Some(k) = key_env_vars(&self.provider)
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|k| !k.is_empty())
        {
            self.api_key = k;
            self.key_from_env = true;
        } else {
            self.api_key = self.api_keys.get(&self.provider).cloned().unwrap_or_default();
            self.key_from_env = false;
        }
    }

    /// How `provider` signs in: its saved choice when the provider offers
    /// it, else an API key.
    pub fn auth_for(&self, provider: &str) -> AuthMethod {
        self.auth_modes
            .get(provider)
            .map(|m| AuthMethod::parse(m))
            .filter(|m| auth_methods(provider).contains(m))
            .unwrap_or(AuthMethod::Api)
    }

    /// The active provider's sign-in method.
    pub fn auth(&self) -> AuthMethod {
        self.auth_for(&self.provider)
    }

    /// Mirror the active provider's sign-in method into `auth_mode`.
    pub fn sync_auth_mode(&mut self) {
        self.auth_mode = self.auth().id().to_string();
    }

    /// Is `provider` signed in with a subscription token?
    pub fn signed_in(&self, provider: &str) -> bool {
        self.oauth.get(provider).is_some_and(|t| !t.access_token.is_empty())
    }

    /// Does the active provider send a subscription token (not an API key)?
    pub fn uses_sub(&self) -> bool {
        self.auth() == AuthMethod::Sub && self.signed_in(&self.provider)
    }

    /// Signed in to OpenAI with ChatGPT: requests go to the Codex backend.
    pub fn chatgpt_sub(&self) -> bool {
        self.provider == "openai" && self.uses_sub()
    }

    /// The base URL requests go to: the configured one, except where the
    /// sign-in method has its own endpoint (ChatGPT's Codex backend, a
    /// coding plan's URL).
    pub fn endpoint_base(&self) -> String {
        if self.chatgpt_sub() {
            return std::env::var("HAIKUCODE_CHATGPT_BASE_URL")
                .ok()
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| CHATGPT_CODEX_URL.to_string());
        }
        if self.auth() == AuthMethod::Plan {
            if let Some(u) = coding_plan_url(&self.provider) {
                return u.to_string();
            }
        }
        self.base_url.clone()
    }

    /// Can `provider` make requests as set up: a key for "api" / "plan", a
    /// token for "sub", nothing for a keyless server?
    pub fn has_credentials_for(&self, provider: &str) -> bool {
        match self.auth_for(provider) {
            AuthMethod::Api | AuthMethod::Plan => self.has_key_for(provider),
            AuthMethod::Sub => self.signed_in(provider),
            AuthMethod::None => true,
        }
    }

    /// `has_credentials_for` the active provider (its resolved key counts).
    pub fn has_credentials(&self) -> bool {
        match self.auth() {
            AuthMethod::Api | AuthMethod::Plan => !self.api_key.is_empty(),
            AuthMethod::Sub => self.signed_in(&self.provider),
            AuthMethod::None => true,
        }
    }
}
