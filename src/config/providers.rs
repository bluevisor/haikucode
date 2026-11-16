//! Provider presets: sign-in methods, preset/price tables, key env vars,
//! address checks and the context-window table.

use super::*;

/// ChatGPT sign-ins talk to the Codex backend, not api.openai.com.
pub const CHATGPT_CODEX_URL: &str = "https://chatgpt.com/backend-api/codex";

/// One way to authenticate with a provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AuthMethod {
    /// A pay-as-you-go API key.
    #[default]
    Api,
    /// A subscription login in the browser (OAuth).
    Sub,
    /// A coding-plan key, sent to the plan's own endpoint.
    Plan,
    /// No credentials (a local or open server).
    None,
}

impl AuthMethod {
    pub fn id(self) -> &'static str {
        match self {
            AuthMethod::Api => "api",
            AuthMethod::Sub => "sub",
            AuthMethod::Plan => "plan",
            AuthMethod::None => "none",
        }
    }

    pub fn parse(s: &str) -> AuthMethod {
        match s.trim() {
            "sub" | "subscription" | "oauth" => AuthMethod::Sub,
            "plan" | "coding_plan" => AuthMethod::Plan,
            "none" => AuthMethod::None,
            _ => AuthMethod::Api,
        }
    }
}

/// The sign-in methods `provider` offers, the default (API key) first.
pub fn auth_methods(provider: &str) -> Vec<AuthMethod> {
    if !is_builtin_provider(provider) {
        return vec![AuthMethod::Api, AuthMethod::None];
    }
    let mut v = vec![AuthMethod::Api];
    if crate::auth::sub_supported(provider) {
        v.push(AuthMethod::Sub);
    }
    if coding_plan_url(provider).is_some() {
        v.push(AuthMethod::Plan);
    }
    v
}

/// A provider's subscription coding plan that is used with a key of its own
/// on a separate endpoint (Z.ai's GLM Coding Plan).
pub fn coding_plan_url(provider: &str) -> Option<&'static str> {
    match provider {
        "zai" => Some("https://api.z.ai/api/coding/paas/v4"),
        _ => None,
    }
}

/// What the coding plan is called, for the UI.
pub fn coding_plan_label(provider: &str) -> &'static str {
    match provider {
        "zai" => "GLM Coding Plan",
        _ => "coding plan",
    }
}

/// Where to get an API key for a preset, for the key prompt.
pub fn key_url(provider: &str, plan: bool) -> Option<&'static str> {
    Some(match (provider, plan) {
        ("zai", true) => "z.ai/subscribe (then z.ai/manage-apikey/apikey-list)",
        ("deepseek", _) => "platform.deepseek.com/api_keys",
        ("openai", _) => "platform.openai.com/api-keys",
        ("anthropic", _) => "console.anthropic.com/settings/keys",
        ("groq", _) => "console.groq.com/keys",
        ("openrouter", _) => "openrouter.ai/settings/keys",
        ("google", _) => "aistudio.google.com/apikey",
        ("zai", _) => "z.ai/manage-apikey/apikey-list",
        ("vercel", _) => "vercel.com → AI Gateway → API Keys",
        _ => return None,
    })
}

/// Why a preset has no subscription sign-in, when people often look for one.
pub fn no_sub_reason(provider: &str) -> Option<&'static str> {
    match provider {
        "anthropic" => Some("Claude Pro/Max sign-in isn't available: Anthropic only allows it in its own apps"),
        "google" => Some("Gemini sign-in isn't available yet: it needs Google's Code Assist API"),
        _ => None,
    }
}

/// Is `name` one of the built-in presets in `PROVIDERS`?
pub fn is_builtin_provider(name: &str) -> bool {
    PROVIDERS.iter().any(|(n, _, _)| *n == name)
}

/// Can `name` name a custom provider? Short, lowercase-ish, no spaces, not a
/// preset's name (those keep their own key env vars and API quirks).
pub fn valid_custom_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 32 {
        return Err("a provider name is 1 to 32 characters".into());
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.') {
        return Err(format!("'{name}': use letters, digits, '-', '_' or '.'"));
    }
    if is_builtin_provider(name) {
        return Err(format!("'{name}' is a built-in provider"));
    }
    Ok(())
}

/// Validate API base addresses before saving or attaching credentials.
pub fn normalize_provider_url(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('/');
    if !(value.starts_with("http://") || value.starts_with("https://")) {
        return Err("The URL must start with http:// or https:// and include a host".into());
    }
    let parsed = url::Url::parse(value).map_err(|_| "Enter a full API base URL, e.g. http://localhost:1234/v1".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("The URL must start with http:// or https:// and include a host".into());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("Put credentials in the API key field, not in the URL".into());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() || value.chars().any(char::is_whitespace) {
        return Err("Use an API base URL without spaces, query parameters or a fragment".into());
    }
    if ["/chat/completions", "/models", "/responses"].iter().any(|suffix| parsed.path().ends_with(suffix)) {
        return Err("Use the API base URL (e.g. https://server.example/v1), without /chat/completions, /models or /responses".into());
    }
    Ok(parsed.as_str().trim_end_matches('/').to_string())
}

/// Provider presets: (name, base_url, default model) — the single source for
/// the first-run wizard and the `/config` panel's provider row.
pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("deepseek", "https://api.deepseek.com", "deepseek-flash"),
    ("openai", "https://api.openai.com/v1", "gpt-4o-mini"),
    ("anthropic", "https://api.anthropic.com/v1", "claude-opus-5-5"),
    ("groq", "https://api.groq.com/openai/v1", "llama-3.3-70b-versatile"),
    ("openrouter", "https://openrouter.ai/api/v1", "openai/gpt-4o-mini"),
    ("google", "https://generativelanguage.googleapis.com/v1beta/openai", "gemini-2.5-flash"),
    ("zai", "https://api.z.ai/api/paas/v4", "glm-5.2"),
    ("vercel", "https://ai-gateway.vercel.sh/v1", "anthropic/claude-sonnet-5.5"),
];

/// Built-in price lists for the presets: (name, in, out, cached, currency),
/// per 1M tokens, for each preset's default model. The cost counter follows
/// the active provider, so each preset carries its own list; a custom
/// provider without a user-set `prices` entry has no known price here and its
/// cost readout is hidden.
pub const PRICES: &[(&str, f64, f64, f64, &str)] = &[
    ("deepseek", 0.15, 0.60, DEEPSEEK_CACHE_HIT_PRICE, "USD"),
    ("openai", 0.15, 0.60, 0.075, "USD"),
    ("anthropic", 15.0, 75.0, 1.50, "USD"),
    ("groq", 0.59, 0.79, 0.059, "USD"),
    ("openrouter", 0.15, 0.60, 0.075, "USD"),
    ("google", 0.30, 2.50, 0.03, "USD"),
    ("zai", 0.60, 2.20, 0.06, "USD"),
    ("vercel", 3.00, 15.0, 0.30, "USD"),
];

/// The built-in price list for a preset name, if it has one.
pub fn built_in_prices(name: &str) -> Option<PriceList> {
    PRICES
        .iter()
        .find(|(n, ..)| *n == name)
        .map(|(_, i, o, c, cur)| PriceList {
            price_in: *i,
            price_out: *o,
            price_cached_in: Some(*c),
            price_currency: cur.to_string(),
        })
}

/// Env vars that may supply the API key for `provider`, highest priority
/// first — only the configured provider's key is ever picked up, so a machine
/// with several providers' keys exported can't hand the wrong one to this
/// one. HAIKUCODE_API_KEY is the explicit universal override and always wins.
pub(super) fn key_env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "deepseek" => &["HAIKUCODE_API_KEY", "DEEPSEEK_API_KEY"],
        "openai" => &["HAIKUCODE_API_KEY", "OPENAI_API_KEY"],
        "anthropic" => &["HAIKUCODE_API_KEY", "ANTHROPIC_API_KEY"],
        "groq" => &["HAIKUCODE_API_KEY", "GROQ_API_KEY"],
        "openrouter" => &["HAIKUCODE_API_KEY", "OPENROUTER_API_KEY"],
        "google" => &["HAIKUCODE_API_KEY", "GOOGLE_API_KEY", "GEMINI_API_KEY"],
        "zai" => &["HAIKUCODE_API_KEY", "ZAI_API_KEY", "ZHIPUAI_API_KEY"],
        "vercel" => &["HAIKUCODE_API_KEY", "AI_GATEWAY_API_KEY"],
        _ => &["HAIKUCODE_API_KEY"],
    }
}

/// Best-known context window (in tokens) for a model id, used when the
/// provider's `/models` endpoint doesn't advertise one (DeepSeek's now does,
/// as `context_window: 1048576`; older deployments and most OpenAI-compatible
/// servers return only `id`/`object`/`owned_by`). Matched by substring so
/// version suffixes need no enumerating. Conservative 128k fallback.
pub fn known_context_window(model: &str) -> u32 {
    let m = model.to_ascii_lowercase();
    if m.contains("deepseek") {
        // The API exposes v4 Flash under the bare alias `deepseek-flash` (no
        // "v4"), so match the family by either tag — otherwise the ctx bar and
        // auto-compaction would treat a 1M model as 128k and saturate 8x early.
        if m.contains("v4") || m.contains("flash") {
            1_000_000 // v4-pro and v4-flash both advertise 1M
        } else {
            128_000 // V3 / chat / reasoner
        }
    } else if m.contains("glm-5") || m.contains("glm-4-plus") {
        1_000_000 // GLM-5 / 5.1 / 5.2 / 4-plus all advertise 1M
    } else if m.contains("glm") {
        128_000 // GLM-4-Flash / GLM-4-Air / etc.
    } else if m.contains("claude") {
        // Haiku 4.5 is 200k; the current Opus/Sonnet/Fable models are 1M.
        if m.contains("haiku") { 200_000 } else { 1_000_000 }
    } else {
        // Unknown model: assume the conservative window rather than the 1M
        // default, or auto-compaction would wait until ~800k tokens and the
        // provider would reject the request long before that.
        128_000
    }
}
